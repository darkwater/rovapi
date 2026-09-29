use std::{env, error::Error, fs::File, io, path::PathBuf, sync::Arc};

use rovapi::{
    AppState, Config,
    download::{DownloadOutcome, FeedDownloader},
    gtfs::ImportLimits,
    refresh::run_static_refresh,
    router,
    schedule::{DataDirectory, ScheduleVersion},
    storage::{SqliteReader, StorageError},
};
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    match command_from_args()? {
        Command::Import { gtfs_zip, version } => {
            let version = ScheduleVersion::parse(version)?;
            let data_directory = DataDirectory::open(Config::data_directory_from_env())?;
            let file = File::open(&gtfs_zip)?;
            info!(path = %gtfs_zip.display(), version = version.as_str(), "importing GTFS schedule");
            let (active, summary) =
                data_directory.import_and_activate(&version, file, ImportLimits::default())?;
            info!(
                version = active.version.as_str(),
                feed_info = summary.feed_info,
                agencies = summary.agencies,
                stops = summary.stops,
                stop_groups = summary.stop_groups,
                routes = summary.routes,
                calendars = summary.calendars,
                calendar_dates = summary.calendar_dates,
                shape_points = summary.shape_points,
                transfers = summary.transfers,
                trips = summary.trips,
                stop_times = summary.stop_times,
                "GTFS schedule imported and activated"
            );
            return Ok(());
        }
        Command::Fetch { url, version } => {
            let version = ScheduleVersion::parse(version)?;
            let data_directory = DataDirectory::open(Config::data_directory_from_env())?;
            let snapshot = data_directory.snapshot_path(&version);
            let database = data_directory.database_path(&version);

            if database.is_file() {
                let active = data_directory.activate(&version)?;
                info!(
                    version = active.version.as_str(),
                    path = %database.display(),
                    "existing GTFS schedule activated"
                );
                return Ok(());
            }
            if snapshot.is_file() {
                info!(
                    %url,
                    version = version.as_str(),
                    path = %snapshot.display(),
                    "resuming GTFS import from existing snapshot"
                );
                let file = File::open(&snapshot)?;
                let (active, summary) =
                    data_directory.import_and_activate(&version, file, ImportLimits::default())?;
                info!(
                    version = active.version.as_str(),
                    stops = summary.stops,
                    stop_groups = summary.stop_groups,
                    routes = summary.routes,
                    trips = summary.trips,
                    stop_times = summary.stop_times,
                    "existing GTFS snapshot imported and activated"
                );
                return Ok(());
            }

            let validators = data_directory.download_validators(&url)?;
            info!(%url, version = version.as_str(), "checking GTFS feed");
            let downloader = FeedDownloader::new(1024 * 1024 * 1024)?;
            match downloader.download(&url, &snapshot, &validators).await? {
                DownloadOutcome::NotModified => {
                    info!(%url, "GTFS feed has not changed");
                }
                DownloadOutcome::Downloaded(downloaded) => {
                    info!(%url, bytes = downloaded.bytes, path = %snapshot.display(), "GTFS feed downloaded");
                    let file = File::open(&snapshot)?;
                    let (active, summary) = data_directory.import_and_activate(
                        &version,
                        file,
                        ImportLimits::default(),
                    )?;
                    data_directory.save_download_validators(&url, downloaded.validators)?;
                    info!(
                        version = active.version.as_str(),
                        stops = summary.stops,
                        stop_groups = summary.stop_groups,
                        routes = summary.routes,
                        trips = summary.trips,
                        stop_times = summary.stop_times,
                        "downloaded GTFS schedule imported and activated"
                    );
                }
            }
            return Ok(());
        }
        Command::Activate { version } => {
            let version = ScheduleVersion::parse(version)?;
            let data_directory = DataDirectory::open(Config::data_directory_from_env())?;
            let active = data_directory.activate(&version)?;
            info!(
                version = active.version.as_str(),
                path = %data_directory.database_path(&version).display(),
                "GTFS schedule activated"
            );
            return Ok(());
        }
        Command::Versions => {
            let data_directory = DataDirectory::open(Config::data_directory_from_env())?;
            let active = data_directory.active()?.map(|active| active.version);
            for version in data_directory.installed_versions()? {
                let marker = if active.as_ref() == Some(&version) {
                    "active"
                } else {
                    "installed"
                };
                println!("{}\t{marker}", version.as_str());
            }
            return Ok(());
        }
        Command::Status => {
            let data_directory = DataDirectory::open(Config::data_directory_from_env())?;
            let Some(active) = data_directory.active()? else {
                println!("null");
                return Ok(());
            };
            let reader = SqliteReader::open(data_directory.database_path(&active.version)).await?;
            let metadata = reader.schedule_metadata().await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "active": active,
                    "metadata": metadata,
                }))?
            );
            return Ok(());
        }
        Command::Help => {
            print_usage();
            return Ok(());
        }
        Command::Serve => {}
    }

    let config = Config::from_env()?;
    let data_directory = Arc::new(DataDirectory::open(&config.data_directory)?);
    let state = match data_directory.active()? {
        Some(active) => {
            let database = data_directory.database_path(&active.version);
            info!(version = active.version.as_str(), path = %database.display(), "loading schedule");
            match SqliteReader::open(database).await {
                Ok(reader) => AppState::with_schedule(reader),
                Err(StorageError::UnsupportedSchema { actual, expected }) => {
                    warn!(
                        version = active.version.as_str(),
                        actual_schema = actual,
                        expected_schema = expected,
                        "active schedule uses an unsupported schema; rebuilding in background"
                    );
                    AppState::new()
                }
                Err(error) => return Err(error.into()),
            }
        }
        None => {
            info!(path = %config.data_directory.display(), "no active schedule found");
            AppState::new()
        }
    };
    let listener = TcpListener::bind(config.bind_address).await?;
    info!(address = %config.bind_address, "listening");

    let refresh_task = tokio::spawn(run_static_refresh(
        Arc::clone(&data_directory),
        state.clone(),
    ));

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    refresh_task.abort();

    Ok(())
}

enum Command {
    Serve,
    Import { gtfs_zip: PathBuf, version: String },
    Fetch { url: String, version: String },
    Activate { version: String },
    Versions,
    Status,
    Help,
}

fn command_from_args() -> Result<Command, io::Error> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [] => Ok(Command::Serve),
        [flag] if flag == "--help" || flag == "-h" => Ok(Command::Help),
        [command, gtfs_zip, version] if command == "import" => {
            let version = version.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "version must be valid UTF-8")
            })?;
            Ok(Command::Import {
                gtfs_zip: PathBuf::from(gtfs_zip),
                version: version.to_owned(),
            })
        }
        [command, url, version] if command == "fetch" => {
            let url = url.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "URL must be valid UTF-8")
            })?;
            let version = version.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "version must be valid UTF-8")
            })?;
            Ok(Command::Fetch {
                url: url.to_owned(),
                version: version.to_owned(),
            })
        }
        [command, version] if command == "activate" => {
            let version = version.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "version must be valid UTF-8")
            })?;
            Ok(Command::Activate {
                version: version.to_owned(),
            })
        }
        [command] if command == "versions" => Ok(Command::Versions),
        [command] if command == "status" => Ok(Command::Status),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: rovapi [import <gtfs.zip> <version> | fetch <url> <version> | activate <version> | versions | status]",
        )),
    }
}

fn print_usage() {
    println!(
        "rovapi\n\nUSAGE:\n    rovapi\n    rovapi import <gtfs.zip> <version>\n    rovapi fetch <url> <version>\n    rovapi activate <version>\n    rovapi versions\n    rovapi status\n\nENVIRONMENT:\n    ROVAPI_DATA_DIR       Data directory (default: data)\n    ROVAPI_BIND_ADDRESS   Listen address (default: 127.0.0.1:3000)\n    RUST_LOG              Tracing filter"
    );
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("rovapi=info,tower_http=info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    info!("shutdown signal received");
}
