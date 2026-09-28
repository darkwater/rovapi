use std::{env, error::Error, fs::File, io, path::PathBuf};

use ovapi::{
    AppState, Config,
    gtfs::ImportLimits,
    router,
    schedule::{DataDirectory, ScheduleVersion},
    storage::SqliteReader,
};
use tokio::net::TcpListener;
use tracing::info;
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
                agencies = summary.agencies,
                stops = summary.stops,
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
        Command::Help => {
            print_usage();
            return Ok(());
        }
        Command::Serve => {}
    }

    let config = Config::from_env()?;
    let data_directory = DataDirectory::open(&config.data_directory)?;
    let state = match data_directory.active()? {
        Some(active) => {
            let database = data_directory.database_path(&active.version);
            info!(version = active.version.as_str(), path = %database.display(), "loading schedule");
            AppState::with_schedule(SqliteReader::open(database).await?)
        }
        None => {
            info!(path = %config.data_directory.display(), "no active schedule found");
            AppState::new()
        }
    };
    let listener = TcpListener::bind(config.bind_address).await?;
    info!(address = %config.bind_address, "listening");

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

enum Command {
    Serve,
    Import { gtfs_zip: PathBuf, version: String },
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
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: ovapi [import <gtfs.zip> <version>]",
        )),
    }
}

fn print_usage() {
    println!(
        "ovapi\n\nUSAGE:\n    ovapi\n    ovapi import <gtfs.zip> <version>\n\nENVIRONMENT:\n    OVAPI_DATA_DIR       Data directory (default: data)\n    OVAPI_BIND_ADDRESS   Listen address (default: 127.0.0.1:3000)\n    RUST_LOG             Tracing filter"
    );
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("ovapi=info,tower_http=info"));

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
