use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::{mpsc, oneshot};

use crate::gtfs::{GtfsDate, GtfsTime};

use super::{
    NearbyStop, ScheduleRepository, ScheduledDeparture, ScheduledStopCall, SqliteStore,
    StorageError, StoredRoute, StoredScheduleMetadata, StoredShapePoint, StoredStop, StoredTrip,
};

const DEFAULT_READER_COUNT: usize = 4;
const COMMAND_CAPACITY_PER_READER: usize = 16;

/// Async handle to a pool of read-only SQLite connections on blocking workers.
#[derive(Clone, Debug)]
pub struct SqliteReader {
    pool: Arc<ReaderPool>,
}

#[derive(Debug)]
struct ReaderPool {
    senders: Vec<mpsc::Sender<Command>>,
    next: AtomicUsize,
    live_workers: Arc<AtomicUsize>,
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
    Route {
        source_id: String,
        response: oneshot::Sender<Result<Option<StoredRoute>, StorageError>>,
    },
    SearchRoutes {
        query: String,
        limit: usize,
        response: oneshot::Sender<Result<Vec<StoredRoute>, StorageError>>,
    },
    RouteTrips {
        route_source_id: String,
        date: GtfsDate,
        limit: usize,
        response: oneshot::Sender<Result<Vec<StoredTrip>, StorageError>>,
    },
    Trip {
        source_id: String,
        response: oneshot::Sender<Result<Option<StoredTrip>, StorageError>>,
    },
    TripStops {
        trip_source_id: String,
        response: oneshot::Sender<Result<Vec<ScheduledStopCall>, StorageError>>,
    },
    TripShape {
        trip_source_id: String,
        response: oneshot::Sender<Result<Vec<StoredShapePoint>, StorageError>>,
    },
    ScheduleMetadata {
        response: oneshot::Sender<Result<StoredScheduleMetadata, StorageError>>,
    },
}

struct LiveWorkerGuard(Arc<AtomicUsize>);

impl LiveWorkerGuard {
    fn new(live_workers: Arc<AtomicUsize>) -> Self {
        live_workers.fetch_add(1, Ordering::Release);
        Self(live_workers)
    }
}

impl Drop for LiveWorkerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

impl SqliteReader {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Self::open_with_pool_size(path, DEFAULT_READER_COUNT).await
    }

    pub async fn open_with_pool_size(
        path: impl AsRef<Path>,
        pool_size: usize,
    ) -> Result<Self, StorageError> {
        if pool_size == 0 {
            return Err(StorageError::InvalidReaderPoolSize);
        }

        let path = PathBuf::from(path.as_ref());
        let live_workers = Arc::new(AtomicUsize::new(0));
        let mut senders = Vec::with_capacity(pool_size);

        for worker_index in 0..pool_size {
            let (sender, mut receiver) = mpsc::channel(COMMAND_CAPACITY_PER_READER);
            let (startup_sender, startup_receiver) = oneshot::channel();
            let worker_path = path.clone();
            let worker_live_workers = Arc::clone(&live_workers);
            std::thread::Builder::new()
                .name(format!("ovapi-sqlite-reader-{worker_index}"))
                .spawn(move || {
                    let store = match SqliteStore::open_read_only(worker_path) {
                        Ok(store) => store,
                        Err(error) => {
                            let _ = startup_sender.send(Err(error));
                            return;
                        }
                    };
                    let _guard = LiveWorkerGuard::new(Arc::clone(&worker_live_workers));
                    if startup_sender.send(Ok(())).is_err() {
                        return;
                    }

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
                            Command::Route {
                                source_id,
                                response,
                            } => {
                                let _ = response.send(store.route(&source_id));
                            }
                            Command::SearchRoutes {
                                query,
                                limit,
                                response,
                            } => {
                                let _ = response.send(store.search_routes(&query, limit));
                            }
                            Command::RouteTrips {
                                route_source_id,
                                date,
                                limit,
                                response,
                            } => {
                                let _ =
                                    response.send(store.route_trips(&route_source_id, date, limit));
                            }
                            Command::Trip {
                                source_id,
                                response,
                            } => {
                                let _ = response.send(store.trip(&source_id));
                            }
                            Command::TripStops {
                                trip_source_id,
                                response,
                            } => {
                                let _ = response.send(store.trip_stops(&trip_source_id));
                            }
                            Command::TripShape {
                                trip_source_id,
                                response,
                            } => {
                                let _ = response.send(store.trip_shape(&trip_source_id));
                            }
                            Command::ScheduleMetadata { response } => {
                                let _ = response.send(store.schedule_metadata());
                            }
                        }
                    }
                })
                .map_err(StorageError::WorkerStart)?;

            startup_receiver
                .await
                .map_err(|_| StorageError::WorkerStopped)??;
            senders.push(sender);
        }

        Ok(Self {
            pool: Arc::new(ReaderPool {
                senders,
                next: AtomicUsize::new(0),
                live_workers,
            }),
        })
    }

    pub fn is_healthy(&self) -> bool {
        self.pool.live_workers.load(Ordering::Acquire) == self.pool.senders.len()
            && self.pool.senders.iter().all(|sender| !sender.is_closed())
    }

    fn sender(&self) -> &mpsc::Sender<Command> {
        let index = self.pool.next.fetch_add(1, Ordering::Relaxed) % self.pool.senders.len();
        &self.pool.senders[index]
    }

    pub async fn stop(
        &self,
        source_id: impl Into<String>,
    ) -> Result<Option<StoredStop>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
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
        self.sender()
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
        self.sender()
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
        self.sender()
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

    pub async fn route(
        &self,
        source_id: impl Into<String>,
    ) -> Result<Option<StoredRoute>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
            .send(Command::Route {
                source_id: source_id.into(),
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn search_routes(
        &self,
        query: impl Into<String>,
        limit: usize,
    ) -> Result<Vec<StoredRoute>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
            .send(Command::SearchRoutes {
                query: query.into(),
                limit,
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn route_trips(
        &self,
        route_source_id: impl Into<String>,
        date: GtfsDate,
        limit: usize,
    ) -> Result<Vec<StoredTrip>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
            .send(Command::RouteTrips {
                route_source_id: route_source_id.into(),
                date,
                limit,
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn trip(
        &self,
        source_id: impl Into<String>,
    ) -> Result<Option<StoredTrip>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
            .send(Command::Trip {
                source_id: source_id.into(),
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn trip_stops(
        &self,
        trip_source_id: impl Into<String>,
    ) -> Result<Vec<ScheduledStopCall>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
            .send(Command::TripStops {
                trip_source_id: trip_source_id.into(),
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn trip_shape(
        &self,
        trip_source_id: impl Into<String>,
    ) -> Result<Vec<StoredShapePoint>, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
            .send(Command::TripShape {
                trip_source_id: trip_source_id.into(),
                response,
            })
            .await
            .map_err(|_| StorageError::WorkerStopped)?;
        receiver.await.map_err(|_| StorageError::WorkerStopped)?
    }

    pub async fn schedule_metadata(&self) -> Result<StoredScheduleMetadata, StorageError> {
        let (response, receiver) = oneshot::channel();
        self.sender()
            .send(Command::ScheduleMetadata { response })
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

        let reader = SqliteReader::open_with_pool_size(&path, 2).await.unwrap();
        assert!(reader.is_healthy());
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

    #[tokio::test]
    async fn rejects_an_empty_reader_pool() {
        let error = SqliteReader::open_with_pool_size("unused.sqlite", 0)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::InvalidReaderPoolSize));
    }
}
