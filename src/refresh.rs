use std::{
    fmt,
    fs::File,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::{
    AppState,
    download::{DownloadError, DownloadOutcome, FeedDownloader},
    gtfs::ImportLimits,
    schedule::{DataDirectory, ScheduleError, ScheduleVersion},
    storage::{SqliteReader, StorageError},
};

pub const STATIC_FEED_URL: &str = "https://gtfs.ovapi.nl/nl/gtfs-nl.zip";
const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);
const REFRESH_SECOND_UTC: u64 = 4 * 60 * 60 + 15 * 60;

#[derive(Debug)]
pub enum RefreshError {
    Download(DownloadError),
    Schedule(ScheduleError),
    Storage(StorageError),
    Worker(tokio::task::JoinError),
}

/// Checks the Dutch static feed forever while leaving the current schedule in
/// service whenever a refresh fails.
pub async fn run_static_refresh(data: Arc<DataDirectory>, state: AppState) {
    loop {
        let delay = match refresh_once(Arc::clone(&data), &state).await {
            Ok(()) => duration_until_daily_refresh(SystemTime::now()),
            Err(error) => {
                error!(%error, "static GTFS refresh failed; keeping current schedule");
                RETRY_INTERVAL
            }
        };
        sleep(delay).await;
    }
}

pub async fn refresh_once(data: Arc<DataDirectory>, state: &AppState) -> Result<(), RefreshError> {
    let candidate = data.download_candidate_path();
    if candidate.exists() {
        std::fs::remove_file(&candidate).map_err(ScheduleError::from)?;
    }

    let validators = data.verified_download_validators(STATIC_FEED_URL)?;
    info!(url = STATIC_FEED_URL, "checking static GTFS feed");
    let downloader = FeedDownloader::new(MAX_SNAPSHOT_BYTES)?;
    let (version, snapshot) = match downloader
        .download(STATIC_FEED_URL, &candidate, &validators)
        .await?
    {
        DownloadOutcome::NotModified => {
            info!(url = STATIC_FEED_URL, "static GTFS feed has not changed");
            let Some(cached) = data.cached_download(STATIC_FEED_URL)? else {
                // This should only be possible for legacy validator state. The
                // next check will be unconditional because no digest is bound
                // to those validators.
                return Ok(());
            };
            cached
        }
        DownloadOutcome::Downloaded(downloaded) => {
            let version = ScheduleVersion::parse(format!("nl-{}", downloaded.sha256))?;
            let snapshot = data.install_snapshot(&candidate, &version)?;
            data.save_download_state(STATIC_FEED_URL, downloaded.validators, &downloaded.sha256)?;
            info!(
                version = version.as_str(),
                bytes = downloaded.bytes,
                path = %snapshot.display(),
                "static GTFS snapshot stored"
            );
            (version, snapshot)
        }
    };

    if data
        .active()?
        .is_some_and(|active| active.version == version)
        && state.is_ready()
    {
        info!(
            version = version.as_str(),
            "static GTFS schedule is already active"
        );
        return Ok(());
    }

    if !data.database_path(&version).is_file() {
        let import_data = Arc::clone(&data);
        let import_version = version.clone();
        let import_snapshot = snapshot.clone();
        let summary = tokio::task::spawn_blocking(move || {
            let file = File::open(import_snapshot).map_err(ScheduleError::from)?;
            import_data.import_and_install(&import_version, file, ImportLimits::default())
        })
        .await??;
        info!(
            version = version.as_str(),
            stops = summary.stops,
            routes = summary.routes,
            trips = summary.trips,
            stop_times = summary.stop_times,
            "static GTFS schedule imported"
        );
    } else {
        warn!(
            version = version.as_str(),
            "schedule is already installed; reusing it"
        );
    }

    let reader = SqliteReader::open(data.database_path(&version)).await?;
    data.activate(&version)?;
    drop(state.replace_schedule(reader));
    info!(version = version.as_str(), "static GTFS schedule activated");
    Ok(())
}

fn duration_until_daily_refresh(now: SystemTime) -> Duration {
    let now = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let seconds_today = now % (24 * 60 * 60);
    let seconds = if seconds_today < REFRESH_SECOND_UTC {
        REFRESH_SECOND_UTC - seconds_today
    } else {
        24 * 60 * 60 - seconds_today + REFRESH_SECOND_UTC
    };
    Duration::from_secs(seconds)
}

impl fmt::Display for RefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Download(error) => write!(formatter, "{error}"),
            Self::Schedule(error) => write!(formatter, "{error}"),
            Self::Storage(error) => write!(formatter, "{error}"),
            Self::Worker(error) => write!(formatter, "GTFS import worker failed: {error}"),
        }
    }
}

impl std::error::Error for RefreshError {}

impl From<DownloadError> for RefreshError {
    fn from(value: DownloadError) -> Self {
        Self::Download(value)
    }
}
impl From<ScheduleError> for RefreshError {
    fn from(value: ScheduleError) -> Self {
        Self::Schedule(value)
    }
}
impl From<StorageError> for RefreshError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}
impl From<tokio::task::JoinError> for RefreshError {
    fn from(value: tokio::task::JoinError) -> Self {
        Self::Worker(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_checks_for_0415_utc() {
        let day = 24 * 60 * 60;
        assert_eq!(
            duration_until_daily_refresh(UNIX_EPOCH + Duration::from_secs(4 * 60 * 60)),
            Duration::from_secs(15 * 60)
        );
        assert_eq!(
            duration_until_daily_refresh(UNIX_EPOCH + Duration::from_secs(5 * 60 * 60)),
            Duration::from_secs(day - 45 * 60)
        );
    }
}
