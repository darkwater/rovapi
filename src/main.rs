use std::error::Error;

use ovapi::{AppState, Config, router, schedule::DataDirectory, storage::SqliteReader};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

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
