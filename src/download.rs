use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{
    Client, StatusCode,
    header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{fs, io::AsyncWriteExt};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DownloadValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadedFeed {
    pub bytes: u64,
    /// SHA-256 of the decoded response body (the GTFS ZIP bytes).
    pub sha256: String,
    pub validators: DownloadValidators,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadOutcome {
    Downloaded(DownloadedFeed),
    NotModified,
}

#[derive(Clone, Debug)]
pub struct FeedDownloader {
    client: Client,
    max_bytes: u64,
}

#[derive(Debug)]
pub enum DownloadError {
    DestinationExists(PathBuf),
    Http(reqwest::Error),
    Io(std::io::Error),
    TooLarge { limit: u64 },
}

impl FeedDownloader {
    pub fn new(max_bytes: u64) -> Result<Self, DownloadError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(15 * 60))
            .user_agent(concat!(
                "rovapi/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/darkwater/rovapi)"
            ))
            .build()?;
        Ok(Self { client, max_bytes })
    }

    pub async fn download(
        &self,
        url: &str,
        destination: &Path,
        previous: &DownloadValidators,
    ) -> Result<DownloadOutcome, DownloadError> {
        let temporary = temporary_path(destination);
        let result = self
            .download_to_temporary(url, destination, &temporary, previous)
            .await;
        match result {
            Ok(DownloadOutcome::Downloaded(downloaded)) => {
                if let Err(error) = fs::rename(&temporary, destination).await {
                    let _ = fs::remove_file(&temporary).await;
                    return Err(error.into());
                }
                sync_parent_directory(destination).await?;
                Ok(DownloadOutcome::Downloaded(downloaded))
            }
            Ok(DownloadOutcome::NotModified) => {
                let _ = fs::remove_file(&temporary).await;
                Ok(DownloadOutcome::NotModified)
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary).await;
                Err(error)
            }
        }
    }

    async fn download_to_temporary(
        &self,
        url: &str,
        destination: &Path,
        temporary: &Path,
        previous: &DownloadValidators,
    ) -> Result<DownloadOutcome, DownloadError> {
        let mut response = self.request(url, previous).send().await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(DownloadOutcome::NotModified);
        }
        response.error_for_status_ref()?;
        if destination.exists() {
            return Err(DownloadError::DestinationExists(destination.to_owned()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_bytes)
        {
            return Err(DownloadError::TooLarge {
                limit: self.max_bytes,
            });
        }

        let validators = DownloadValidators {
            etag: header_text(&response, ETAG),
            last_modified: header_text(&response, LAST_MODIFIED),
        };
        let mut file = fs::File::create(temporary).await?;
        let mut bytes = 0_u64;
        let mut digest = Sha256::new();
        while let Some(chunk) = response.chunk().await? {
            write_limited(&mut file, &mut bytes, self.max_bytes, &chunk).await?;
            digest.update(&chunk);
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);

        Ok(DownloadOutcome::Downloaded(DownloadedFeed {
            bytes,
            sha256: format!("{:x}", digest.finalize()),
            validators,
        }))
    }

    fn request(&self, url: &str, previous: &DownloadValidators) -> reqwest::RequestBuilder {
        let mut request = self.client.get(url);
        if let Some(etag) = &previous.etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = &previous.last_modified {
            request = request.header(IF_MODIFIED_SINCE, last_modified);
        }
        request
    }
}

#[cfg(unix)]
async fn sync_parent_directory(path: &Path) -> Result<(), DownloadError> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("download destination has no parent directory"))?
        .to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(parent)?.sync_all())
        .await
        .map_err(std::io::Error::other)??;
    Ok(())
}

#[cfg(not(unix))]
async fn sync_parent_directory(_path: &Path) -> Result<(), DownloadError> {
    Ok(())
}

async fn write_limited(
    file: &mut fs::File,
    bytes: &mut u64,
    limit: u64,
    chunk: &[u8],
) -> Result<(), DownloadError> {
    *bytes = bytes
        .checked_add(chunk.len() as u64)
        .ok_or(DownloadError::TooLarge { limit })?;
    if *bytes > limit {
        return Err(DownloadError::TooLarge { limit });
    }
    file.write_all(chunk).await?;
    Ok(())
}

fn header_text(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn temporary_path(destination: &Path) -> PathBuf {
    let mut path = destination.as_os_str().to_os_string();
    path.push(".part");
    PathBuf::from(path)
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationExists(path) => {
                write!(
                    formatter,
                    "download destination already exists: {}",
                    path.display()
                )
            }
            Self::Http(error) => write!(formatter, "feed download failed: {error}"),
            Self::Io(error) => write!(formatter, "feed download I/O failed: {error}"),
            Self::TooLarge { limit } => {
                write!(formatter, "feed download exceeds the {limit} byte limit")
            }
        }
    }
}

impl std::error::Error for DownloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for DownloadError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn destination(label: &str) -> PathBuf {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "rovapi-download-{label}-{}-{sequence}.zip",
            std::process::id()
        ))
    }

    #[test]
    fn adds_conditional_request_headers() {
        let downloader = FeedDownloader::new(10).unwrap();
        let request = downloader
            .request(
                "https://example.nl/gtfs.zip",
                &DownloadValidators {
                    etag: Some("\"v1\"".to_owned()),
                    last_modified: Some("Mon, 28 Sep 2026 10:00:00 GMT".to_owned()),
                },
            )
            .build()
            .unwrap();
        assert_eq!(request.headers()[IF_NONE_MATCH], "\"v1\"");
        assert_eq!(
            request.headers()[IF_MODIFIED_SINCE],
            "Mon, 28 Sep 2026 10:00:00 GMT"
        );
    }

    #[tokio::test]
    async fn enforces_the_actual_streamed_size() {
        let path = destination("limit");
        let mut file = fs::File::create(&path).await.unwrap();
        let mut bytes = 0;
        write_limited(&mut file, &mut bytes, 10, b"01234")
            .await
            .unwrap();
        let result = write_limited(&mut file, &mut bytes, 10, b"567890").await;
        assert!(matches!(result, Err(DownloadError::TooLarge { limit: 10 })));
        file.flush().await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"01234");
        let _ = tokio::fs::remove_file(path).await;
    }
}
