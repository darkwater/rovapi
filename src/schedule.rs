use std::{
    fmt,
    fs::{self, File, OpenOptions, TryLockError},
    io::{BufReader, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    download::DownloadValidators,
    gtfs::{GtfsArchive, ImportLimits},
    storage::{ImportSummary, SqliteStore, StorageError},
};

const ACTIVE_FILE: &str = "active.json";
const ACTIVE_TEMP_FILE: &str = ".active.json.tmp";
const LOCK_FILE: &str = ".ovapi.lock";
const MAX_ACTIVE_FILE_BYTES: u64 = 4 * 1024;
const DOWNLOAD_STATE_FILE: &str = "static-feed.json";
const DOWNLOAD_STATE_TEMP_FILE: &str = ".static-feed.json.tmp";
const MAX_DOWNLOAD_STATE_BYTES: u64 = 16 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct DownloadState {
    url: String,
    validators: DownloadValidators,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ScheduleVersion(String);

impl ScheduleVersion {
    pub fn parse(value: impl Into<String>) -> Result<Self, ScheduleError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid {
            return Err(ScheduleError::InvalidVersion(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn database_filename(&self) -> String {
        format!("{}.sqlite", self.0)
    }
}

impl<'de> Deserialize<'de> for ScheduleVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActiveSchedule {
    pub version: ScheduleVersion,
    pub database: String,
}

pub struct DataDirectory {
    root: PathBuf,
    schedules: PathBuf,
    _lock: File,
}

#[derive(Debug)]
pub enum ScheduleError {
    AlreadyRunning,
    InvalidActiveFile(&'static str),
    InvalidDownloadState(&'static str),
    InvalidVersion(String),
    Io(std::io::Error),
    Metadata(serde_json::Error),
    MissingDatabase(PathBuf),
    Storage(StorageError),
}

impl DataDirectory {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ScheduleError> {
        let root = root.as_ref().to_path_buf();
        let schedules = root.join("schedules");
        fs::create_dir_all(&schedules)?;
        fs::create_dir_all(root.join("snapshots"))?;
        fs::create_dir_all(root.join("state"))?;

        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(LOCK_FILE))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(ScheduleError::AlreadyRunning),
            Err(TryLockError::Error(error)) => return Err(ScheduleError::Io(error)),
        }

        Ok(Self {
            root,
            schedules,
            _lock: lock,
        })
    }

    pub fn staging_path(&self, version: &ScheduleVersion) -> PathBuf {
        self.schedules
            .join(format!(".{}.sqlite.building", version.as_str()))
    }

    pub fn database_path(&self, version: &ScheduleVersion) -> PathBuf {
        self.schedules.join(version.database_filename())
    }

    pub fn snapshot_path(&self, version: &ScheduleVersion) -> PathBuf {
        self.root
            .join("snapshots")
            .join(format!("{}.gtfs.zip", version.as_str()))
    }

    pub fn download_validators(&self, url: &str) -> Result<DownloadValidators, ScheduleError> {
        let path = self.root.join("state").join(DOWNLOAD_STATE_FILE);
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DownloadValidators::default());
            }
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.len() > MAX_DOWNLOAD_STATE_BYTES {
            return Err(ScheduleError::InvalidDownloadState(
                "feed download state is too large",
            ));
        }
        let state: DownloadState = serde_json::from_reader(BufReader::new(file))?;
        if state.url == url {
            Ok(state.validators)
        } else {
            Ok(DownloadValidators::default())
        }
    }

    pub fn save_download_validators(
        &self,
        url: &str,
        validators: DownloadValidators,
    ) -> Result<(), ScheduleError> {
        let state_directory = self.root.join("state");
        let temporary_path = state_directory.join(DOWNLOAD_STATE_TEMP_FILE);
        let final_path = state_directory.join(DOWNLOAD_STATE_FILE);
        let temporary = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary_path)?;
        let mut writer = BufWriter::new(temporary);
        serde_json::to_writer(
            &mut writer,
            &DownloadState {
                url: url.to_owned(),
                validators,
            },
        )?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(temporary_path, final_path)?;
        sync_directory(&state_directory)?;
        Ok(())
    }

    /// Imports a GTFS ZIP into a new immutable schedule version and activates it.
    ///
    /// Failed imports remove their private staging database and SQLite sidecars,
    /// so the same version can be retried after correcting the feed.
    pub fn import_and_activate<R: Read + Seek>(
        &self,
        version: &ScheduleVersion,
        reader: R,
        limits: ImportLimits,
    ) -> Result<(ActiveSchedule, ImportSummary), ScheduleError> {
        if self.database_path(version).exists() {
            return Err(ScheduleError::InvalidActiveFile(
                "schedule version already exists",
            ));
        }

        let staging = self.staging_path(version);
        remove_staging_files(&staging)?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        let mut archive = GtfsArchive::open(reader, limits).map_err(StorageError::from)?;
        let mut store = SqliteStore::create(&staging)?;
        let summary = store.import_gtfs(&mut archive)?;
        store.set_metadata("schedule_version", version.as_str())?;
        let imported_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();
        store.set_metadata("imported_at_unix", &imported_at_unix)?;
        store.prepare_for_activation()?;
        let active = self.install_and_activate(version)?;
        staging_guard.disarm();
        Ok((active, summary))
    }

    /// Moves a finalized staging database into place and activates it.
    pub fn install_and_activate(
        &self,
        version: &ScheduleVersion,
    ) -> Result<ActiveSchedule, ScheduleError> {
        let staging = self.staging_path(version);
        if !staging.is_file() {
            return Err(ScheduleError::MissingDatabase(staging));
        }
        let database = self.database_path(version);
        if database.exists() {
            return Err(ScheduleError::InvalidActiveFile(
                "schedule version already exists",
            ));
        }
        fs::rename(&staging, &database)?;
        sync_directory(&self.schedules)?;
        self.activate(version)
    }

    pub fn activate(&self, version: &ScheduleVersion) -> Result<ActiveSchedule, ScheduleError> {
        let database_path = self.database_path(version);
        if !database_path.is_file() {
            return Err(ScheduleError::MissingDatabase(database_path));
        }
        // Opening verifies that the schema is supported before publishing it.
        drop(SqliteStore::open_read_only(&database_path)?);

        let active = ActiveSchedule {
            version: version.clone(),
            database: version.database_filename(),
        };
        let temporary_path = self.root.join(ACTIVE_TEMP_FILE);
        let active_path = self.root.join(ACTIVE_FILE);
        let temporary = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary_path)?;
        let mut writer = BufWriter::new(temporary);
        serde_json::to_writer(&mut writer, &active)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(temporary_path, active_path)?;
        sync_directory(&self.root)?;
        Ok(active)
    }

    pub fn active(&self) -> Result<Option<ActiveSchedule>, ScheduleError> {
        let path = self.root.join(ACTIVE_FILE);
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.len() > MAX_ACTIVE_FILE_BYTES {
            return Err(ScheduleError::InvalidActiveFile(
                "active metadata is too large",
            ));
        }
        let active: ActiveSchedule = serde_json::from_reader(BufReader::new(file))?;
        let expected = active.version.database_filename();
        if active.database != expected {
            return Err(ScheduleError::InvalidActiveFile(
                "database filename does not match version",
            ));
        }
        if !self.schedules.join(&active.database).is_file() {
            return Err(ScheduleError::MissingDatabase(
                self.schedules.join(&active.database),
            ));
        }
        Ok(Some(active))
    }
}

struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = remove_staging_files(&self.path);
    }
}

fn remove_staging_files(path: &Path) -> Result<(), std::io::Error> {
    for suffix in ["", "-wal", "-shm"] {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(suffix);
        match fs::remove_file(PathBuf::from(candidate)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => write!(formatter, "the OVAPI data directory is already in use"),
            Self::InvalidActiveFile(message) => {
                write!(formatter, "invalid active schedule metadata: {message}")
            }
            Self::InvalidDownloadState(message) => {
                write!(formatter, "invalid feed download state: {message}")
            }
            Self::InvalidVersion(version) => {
                write!(formatter, "invalid schedule version {version:?}")
            }
            Self::Io(error) => write!(formatter, "schedule data I/O error: {error}"),
            Self::Metadata(error) => write!(formatter, "invalid schedule metadata: {error}"),
            Self::MissingDatabase(path) => {
                write!(
                    formatter,
                    "schedule database does not exist: {}",
                    path.display()
                )
            }
            Self::Storage(error) => write!(formatter, "invalid schedule database: {error}"),
        }
    }
}

impl std::error::Error for ScheduleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Metadata(error) => Some(error),
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ScheduleError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ScheduleError {
    fn from(error: serde_json::Error) -> Self {
        Self::Metadata(error)
    }
}

impl From<StorageError> for ScheduleError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        sync::atomic::{AtomicU64, Ordering},
    };

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::storage::ScheduleRepository;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn gtfs(stop_time_stop_id: &str) -> Cursor<Vec<u8>> {
        let stop_times = format!(
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\ntrip-1,10:00:00,10:01:00,{stop_time_stop_id},1\n"
        );
        let files = [
            (
                "agency.txt",
                "agency_id,agency_name,agency_url,agency_timezone\nNL,Example,https://example.nl,Europe/Amsterdam\n",
            ),
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon\nstop-1,Centraal,52.0907,5.1214\n",
            ),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_type\nroute-1,NL,8,3\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id\nroute-1,weekday,trip-1\n",
            ),
            (
                "calendar.txt",
                "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekday,1,1,1,1,1,0,0,20260901,20260930\n",
            ),
        ];
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            for (name, contents) in files {
                writer
                    .start_file(name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(contents.as_bytes()).unwrap();
            }
            writer
                .start_file("stop_times.txt", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(stop_times.as_bytes()).unwrap();
            writer.finish().unwrap();
        }
        output.set_position(0);
        output
    }

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!("ovapi-data-{}-{sequence}", std::process::id())))
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn installs_and_recovers_an_active_schedule() {
        let temporary = TempDirectory::new();
        let data = DataDirectory::open(&temporary.0).unwrap();
        let version = ScheduleVersion::parse("2026-09-28T120000Z").unwrap();
        SqliteStore::create(data.staging_path(&version))
            .unwrap()
            .prepare_for_activation()
            .unwrap();

        let installed = data.install_and_activate(&version).unwrap();
        assert_eq!(installed.version, version);
        assert_eq!(data.active().unwrap(), Some(installed));
        assert!(data.database_path(&version).is_file());
        assert!(!data.staging_path(&version).exists());
    }

    #[test]
    fn imports_and_activates_gtfs_without_leaving_staging_files() {
        let temporary = TempDirectory::new();
        let data = DataDirectory::open(&temporary.0).unwrap();
        let version = ScheduleVersion::parse("2026-09-28").unwrap();

        let (active, summary) = data
            .import_and_activate(&version, gtfs("stop-1"), ImportLimits::default())
            .unwrap();

        assert_eq!(active.version, version);
        assert_eq!(summary.stop_times, 1);
        assert!(data.database_path(&version).is_file());
        assert!(!data.staging_path(&version).exists());
        assert_eq!(data.active().unwrap(), Some(active));
        let store = SqliteStore::open_read_only(data.database_path(&version)).unwrap();
        let metadata = store.schedule_metadata().unwrap();
        assert_eq!(metadata.schedule_version.as_deref(), Some("2026-09-28"));
        assert!(metadata.imported_at_unix.is_some());
    }

    #[test]
    fn failed_import_removes_staging_database() {
        let temporary = TempDirectory::new();
        let data = DataDirectory::open(&temporary.0).unwrap();
        let version = ScheduleVersion::parse("invalid-feed").unwrap();

        let result =
            data.import_and_activate(&version, gtfs("missing-stop"), ImportLimits::default());

        assert!(matches!(
            result,
            Err(ScheduleError::Storage(StorageError::Validation(_)))
        ));
        assert!(!data.staging_path(&version).exists());
        assert!(!data.database_path(&version).exists());
    }

    #[test]
    fn persists_download_validators_per_feed_url() {
        let temporary = TempDirectory::new();
        let data = DataDirectory::open(&temporary.0).unwrap();
        let validators = DownloadValidators {
            etag: Some("\"version-1\"".to_owned()),
            last_modified: Some("Mon, 28 Sep 2026 10:00:00 GMT".to_owned()),
        };

        data.save_download_validators("https://example.nl/gtfs.zip", validators.clone())
            .unwrap();

        assert_eq!(
            data.download_validators("https://example.nl/gtfs.zip")
                .unwrap(),
            validators
        );
        assert_eq!(
            data.download_validators("https://other.example/gtfs.zip")
                .unwrap(),
            DownloadValidators::default()
        );
    }

    #[test]
    fn prevents_two_writers_for_one_data_directory() {
        let temporary = TempDirectory::new();
        let _first = DataDirectory::open(&temporary.0).unwrap();
        assert!(matches!(
            DataDirectory::open(&temporary.0),
            Err(ScheduleError::AlreadyRunning)
        ));
    }

    #[test]
    fn rejects_versions_that_could_escape_the_schedule_directory() {
        for version in ["", ".", "..", "../escape", "with/slash", "white space"] {
            assert!(ScheduleVersion::parse(version).is_err(), "{version:?}");
        }
    }

    #[test]
    fn rejects_unsafe_versions_in_active_metadata() {
        let temporary = TempDirectory::new();
        let data = DataDirectory::open(&temporary.0).unwrap();
        fs::write(
            temporary.0.join(ACTIVE_FILE),
            r#"{"version":"../escape","database":"../escape.sqlite"}"#,
        )
        .unwrap();

        assert!(matches!(data.active(), Err(ScheduleError::Metadata(_))));
    }
}
