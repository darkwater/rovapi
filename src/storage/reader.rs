use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, oneshot};

use crate::gtfs::{GtfsDate, GtfsTime};

use super::{
    NearbyStop, ScheduledDeparture, SqliteStore, StopRepository, StorageError, StoredStop,
};

const COMMAND_CAPACITY: usize = 64;

/// Async handle to a read-only SQLite connection owned by a blocking worker.
#[derive(Clone, Debug)]
pub struct SqliteReader {
    sender: mpsc::Sender<Command>,
}

enum Command {
    Stop {
        source_id: String,
        response: oneshot::Sender<Result<Option<StoredStop>, StorageError>>,
    },
    SearchStops {
        query: String,
        limit: usize,
        response: oneshot::Sender<Result<Vec<StoredStop>, StorageError>>,
    },
    NearbyStops {
        latitude: f64,
        longitude: f64,
        radius_metres: f64,
        limit: usize,
        response: oneshot::Sender<Result<Vec<NearbyStop>, StorageError>>,
    },
    ScheduledDepartures {
        stop_source_id: String,
        date: GtfsDate,
        after: GtfsTime,
        limit: usize,
        response: oneshot::Sender<Result<Vec<ScheduledDeparture>, StorageError>>,
    },
}

impl SqliteReader {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = PathBuf::from(path.as_ref());
        let (sender, mut receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (startup_sender, startup_receiver) = oneshot::channel();

        std::thread::Builder::new()
            .name("ovapi-sqlite-reader".to_owned())
            .spawn(move || {
                let store = match SqliteStore::open_read_only(path) {
                    Ok(store) => {
                        let _ = startup_sender.send(Ok(()));
                        store
                    }
                    Err(error) => {
                        let _ = startup_sender.send(Err(error));
                        return;
                    }
                };

                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        Command::Stop {
                            source_id,
                            response,
                        } => {
                            let _ = response.send(store.stop(&source_id));
                        }
                        Command::SearchStops {
                            query,
                            limit,
                            response,
                        } => {
                            let _ = response.send(store.search_stops(&query, limit));
                        }
                        Command::NearbyStops {
                            latitude,
                            longitude,
                            radius_metres,
                            limit,
                            response,
                        } => {
                            let _ = response.send(store.nearby_stops(
                                latitude,
                                longitude,
                                radius_metres,
                                limit,
                            ));
                        }
                        Command::ScheduledDepartures {
                            stop_source_id,
                            date,
                            after,
                            limit,
                            response,
                        } => {
                            let _ = response.send(store.scheduled_departures(
                                &stop_source_id,
                                date,
                                after,
                                limit,
                            ));
                        }
                    }
                }
            })
            .map_err(StorageError::WorkerStart)?;

        startup_receiver
            .await
            .map_err(|_| StorageError::WorkerStopped)??;
        Ok(Self { sender })
    }

    pub async fn stop(
        &self,
        source_id: impl Into<String>,
    ) -> Result<Option<StoredStop>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::Stop {
                source_id: source_id.into(),
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn search_stops(
        &self,
        query: impl Into<String>,
        limit: usize,
    ) -> Result<Vec<StoredStop>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::SearchStops {
                query: query.into(),
                limit,
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn nearby_stops(
        &self,
        latitude: f64,
        longitude: f64,
        radius_metres: f64,
        limit: usize,
    ) -> Result<Vec<NearbyStop>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::NearbyStops {
                latitude,
                longitude,
                radius_metres,
                limit,
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn scheduled_departures(
        &self,
        stop_source_id: impl Into<String>,
        date: GtfsDate,
        after: GtfsTime,
        limit: usize,
    ) -> Result<Vec<ScheduledDeparture>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::ScheduledDepartures {
                stop_source_id: stop_source_id.into(),
                date,
                after,
                limit,
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::storage::StopInput;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn serves_queries_without_blocking_the_async_runtime() {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ovapi-reader-{}-{sequence}.sqlite",
            std::process::id()
        ));
        let mut store = SqliteStore::create(&path).unwrap();
        store
            .insert_stop(StopInput {
                source_id: "ut-centraal",
                code: Some("UT"),
                name: "Utrecht Centraal",
                latitude: Some(52.0893),
                longitude: Some(5.1103),
                location_type: Some(1),
                parent_source_id: None,
                platform_code: None,
            })
            .unwrap();
        store.prepare_for_activation().unwrap();

        let reader = SqliteReader::open(&path).await.unwrap();
        assert_eq!(
            reader.stop("ut-centraal").await.unwrap().unwrap().name,
            "Utrecht Centraal"
        );
        assert_eq!(reader.search_stops("utrecht", 10).await.unwrap().len(), 1);
        assert_eq!(
            reader
                .nearby_stops(52.09, 5.11, 1_000.0, 10)
                .await
                .unwrap()
                .len(),
            1
        );

        drop(reader);
        let _ = fs::remove_file(path);
    }
}
