use std::{
    fmt,
    fs::{self, File, OpenOptions, TryLockError},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize};

use crate::storage::{SqliteStore, StorageError};

const ACTIVE_FILE: &str = "active.json";
const ACTIVE_TEMP_FILE: &str = ".active.json.tmp";
const LOCK_FILE: &str = ".ovapi.lock";
const MAX_ACTIVE_FILE_BYTES: u64 = 4 * 1024;

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
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
