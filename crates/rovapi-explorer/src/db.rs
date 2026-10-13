use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, SyncSender},
    thread,
};

use eframe::egui;
use rovapi::{
    schedule::ActiveSchedule,
    storage::{ScheduleRepository, SqliteStore},
};
use rovapi_models::{Route, ScheduledStopCall, ShapePoint, Trip};
use rusqlite::{Connection, OpenFlags, params};

pub const PAGE_SIZE: i64 = 100;
const MAX_SHAPE_POINTS: i64 = 30_000;
const MAX_STOP_CALLS: i64 = 10_000;

pub fn active_database_path(data_dir: &std::path::Path) -> Result<Option<PathBuf>, String> {
    let active_path = data_dir.join("active.json");
    let metadata = match std::fs::metadata(&active_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read {}: {error}", active_path.display())),
    };
    if metadata.len() > 4096 {
        return Err(format!(
            "Active schedule metadata is too large: {}",
            active_path.display()
        ));
    }
    let content = std::fs::read(&active_path)
        .map_err(|error| format!("Cannot read {}: {error}", active_path.display()))?;
    let active: ActiveSchedule = serde_json::from_slice(&content)
        .map_err(|error| format!("Invalid {}: {error}", active_path.display()))?;
    if active.database != format!("{}.sqlite", active.version.as_str()) {
        return Err(format!(
            "Invalid database filename in {}",
            active_path.display()
        ));
    }
    let database = data_dir.join("schedules").join(active.database);
    if !database.is_file() {
        return Err(format!(
            "Active schedule database is missing: {}",
            database.display()
        ));
    }
    Ok(Some(database))
}

#[derive(Clone, Debug)]
pub struct RouteRow {
    pub id: i64,
    pub route: Route,
}

#[derive(Clone, Debug)]
pub struct TripRow {
    pub id: i64,
    pub trip: Trip,
}

#[derive(Clone, Debug)]
pub struct ShapeSummary {
    pub id: String,
    pub trip_count: i64,
}

#[derive(Clone, Debug)]
pub struct TripDetail {
    pub id: i64,
    pub trip: Trip,
    pub calls: Vec<ScheduledStopCall>,
    pub shape: Vec<ShapePoint>,
}

#[derive(Debug)]
pub enum Query {
    Open(PathBuf),
    Routes { filter: String, after: i64 },
    Route { source_id: String },
    Trips { route_id: i64, after: i64 },
    Shape { id: String },
    Trip { source_id: String },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Slot {
    Open,
    Routes,
    Route,
    Trips,
    Shape(String),
    Trip,
}

impl Query {
    pub fn slot(&self) -> Slot {
        match self {
            Self::Open(_) => Slot::Open,
            Self::Routes { .. } => Slot::Routes,
            Self::Route { .. } => Slot::Route,
            Self::Trips { .. } => Slot::Trips,
            Self::Shape { id } => Slot::Shape(id.clone()),
            Self::Trip { .. } => Slot::Trip,
        }
    }
}

#[derive(Debug)]
pub enum Data {
    Open {
        path: PathBuf,
        version: Option<String>,
        feed: Option<String>,
    },
    Routes {
        rows: Vec<RouteRow>,
        has_more: bool,
    },
    Route {
        row: RouteRow,
        shapes: Vec<ShapeSummary>,
    },
    Trips {
        rows: Vec<TripRow>,
        has_more: bool,
    },
    Shape {
        id: String,
        points: Vec<ShapePoint>,
    },
    Trip(TripDetail),
}

#[derive(Debug)]
pub struct Request {
    pub session: u64,
    pub generation: u64,
    pub sequence: u64,
    pub query: Query,
}

#[derive(Debug)]
pub struct ResultMessage {
    pub session: u64,
    pub generation: u64,
    pub sequence: u64,
    pub slot: Slot,
    pub result: Result<Data, String>,
}

pub fn start(ctx: egui::Context) -> (SyncSender<Request>, Receiver<ResultMessage>) {
    let (requests, request_rx) = std::sync::mpsc::sync_channel::<Request>(16);
    let (result_tx, results) = std::sync::mpsc::sync_channel::<ResultMessage>(16);
    thread::Builder::new()
        .name("rovapi-explorer-db".into())
        .spawn(move || {
            let mut connection: Option<Connection> = None;
            let mut store: Option<SqliteStore> = None;
            let mut current_session = 0;
            while let Ok(request) = request_rx.recv() {
                let slot = request.query.slot();
                let result = match request.query {
                    Query::Open(path) => match open(path) {
                        Ok((new_connection, new_store, data)) => {
                            connection = Some(new_connection);
                            store = Some(new_store);
                            current_session = request.session;
                            Ok(data)
                        }
                        Err(error) => Err(error),
                    },
                    query if request.session == current_session => match (&connection, &store) {
                        (Some(connection), Some(store)) => execute(connection, store, query),
                        _ => Err("No database is open".into()),
                    },
                    _ => continue,
                };
                ctx.request_repaint();
                if result_tx
                    .send(ResultMessage {
                        session: request.session,
                        generation: request.generation,
                        sequence: request.sequence,
                        slot,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .expect("start explorer database worker");
    (requests, results)
}

fn open(path: PathBuf) -> Result<(Connection, SqliteStore, Data), String> {
    if !path.is_file() {
        return Err(format!("Database file not found: {}", path.display()));
    }
    let store = SqliteStore::open_read_only(&path).map_err(|e| e.to_string())?;
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let meta = store.schedule_metadata().map_err(|e| e.to_string())?;
    let feed = meta.feed.map(|f| f.publisher_name);
    let data = Data::Open {
        path,
        version: meta.schedule_version,
        feed,
    };
    Ok((connection, store, data))
}

fn execute(connection: &Connection, store: &SqliteStore, query: Query) -> Result<Data, String> {
    match query {
        Query::Routes { filter, after } => {
            let mut statement = connection.prepare("SELECT id, source_id FROM routes WHERE id > ?1 AND ( ?2 = '' OR instr(lower(source_id), lower(?2)) > 0 OR instr(lower(coalesce(short_name,'')), lower(?2)) > 0 OR instr(lower(coalesce(long_name,'')), lower(?2)) > 0 ) ORDER BY id LIMIT ?3").map_err(|e| e.to_string())?;
            let ids = statement
                .query_map(params![after, filter, PAGE_SIZE + 1], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?;
            let mut rows = Vec::new();
            for id in ids {
                let (id, source_id) = id.map_err(|e| e.to_string())?;
                if let Some(route) = store.route(&source_id).map_err(|e| e.to_string())? {
                    rows.push(RouteRow { id, route });
                }
            }
            let has_more = rows.len() as i64 > PAGE_SIZE;
            rows.truncate(PAGE_SIZE as usize);
            Ok(Data::Routes { rows, has_more })
        }
        Query::Route { source_id } => {
            let id: i64 = connection
                .query_row(
                    "SELECT id FROM routes WHERE source_id = ?1",
                    [&source_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let route = store
                .route(&source_id)
                .map_err(|e| e.to_string())?
                .ok_or("Route no longer exists")?;
            let mut statement = connection.prepare("SELECT shape_source_id, count(*) FROM trips WHERE route_id = ?1 AND shape_source_id IS NOT NULL GROUP BY shape_source_id ORDER BY count(*) DESC, shape_source_id LIMIT 500").map_err(|e| e.to_string())?;
            let shapes = statement
                .query_map([id], |row| {
                    Ok(ShapeSummary {
                        id: row.get(0)?,
                        trip_count: row.get(1)?,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(Data::Route {
                row: RouteRow { id, route },
                shapes,
            })
        }
        Query::Trips { route_id, after } => {
            let mut statement = connection.prepare("SELECT id, source_id FROM trips WHERE route_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3").map_err(|e| e.to_string())?;
            let ids = statement
                .query_map(params![route_id, after, PAGE_SIZE + 1], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?;
            let mut rows = Vec::new();
            for id in ids {
                let (id, source_id) = id.map_err(|e| e.to_string())?;
                if let Some(trip) = store.trip(&source_id).map_err(|e| e.to_string())? {
                    rows.push(TripRow { id, trip });
                }
            }
            let has_more = rows.len() as i64 > PAGE_SIZE;
            rows.truncate(PAGE_SIZE as usize);
            Ok(Data::Trips { rows, has_more })
        }
        Query::Shape { id } => {
            let mut statement = connection.prepare("SELECT sequence, latitude, longitude, distance_traveled FROM shape_points WHERE shape_source_id = ?1 ORDER BY sequence LIMIT ?2").map_err(|e| e.to_string())?;
            let points = statement
                .query_map(params![id, MAX_SHAPE_POINTS + 1], |row| {
                    Ok(ShapePoint {
                        sequence: row.get(0)?,
                        latitude: row.get(1)?,
                        longitude: row.get(2)?,
                        distance_traveled: row.get(3)?,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            if points.len() as i64 > MAX_SHAPE_POINTS {
                return Err("Shape exceeds the 30,000 point display limit".into());
            }
            Ok(Data::Shape { id, points })
        }
        Query::Trip { source_id } => {
            let (id, stop_count, shape_count): (i64, i64, i64) = connection.query_row("SELECT t.id, (SELECT count(*) FROM stop_times WHERE trip_id = t.id), (SELECT count(*) FROM shape_points WHERE shape_source_id = t.shape_source_id) FROM trips t WHERE source_id = ?1", [&source_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(|e| e.to_string())?;
            if stop_count > MAX_STOP_CALLS {
                return Err("Trip exceeds the 10,000 stop-call display limit".into());
            }
            if shape_count > MAX_SHAPE_POINTS {
                return Err("Trip shape exceeds the 30,000 point display limit".into());
            }
            let trip = store
                .trip(&source_id)
                .map_err(|e| e.to_string())?
                .ok_or("Trip no longer exists")?;
            let calls = store.trip_stops(&source_id).map_err(|e| e.to_string())?;
            let shape = store.trip_shape(&source_id).map_err(|e| e.to_string())?;
            Ok(Data::Trip(TripDetail {
                id,
                trip,
                calls,
                shape,
            }))
        }
        Query::Open(_) => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovapi::gtfs::{GtfsArchive, ImportLimits};
    use std::{
        fs,
        io::{Cursor, Write},
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
    };
    use zip::{ZipWriter, write::SimpleFileOptions};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn discovers_the_active_database_without_a_data_directory_lock() {
        let root = std::env::temp_dir().join(format!(
            "rovapi-explorer-active-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let schedules = root.join("schedules");
        fs::create_dir_all(&schedules).unwrap();
        let database = schedules.join("feed-v1.sqlite");
        fs::write(&database, []).unwrap();
        fs::write(
            root.join("active.json"),
            br#"{"version":"feed-v1","database":"feed-v1.sqlite"}"#,
        )
        .unwrap();
        assert_eq!(active_database_path(&root).unwrap(), Some(database));
        fs::write(
            root.join("active.json"),
            br#"{"version":"feed-v1","database":"../other.sqlite"}"#,
        )
        .unwrap();
        assert!(active_database_path(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_route_trip_shape_and_service_day_times_without_modifying_database() {
        let path = std::env::temp_dir().join(format!(
            "rovapi-explorer-test-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/static_feed");
        let mut zip = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut zip);
            for entry in fs::read_dir(fixture).unwrap() {
                let entry = entry.unwrap();
                writer
                    .start_file(
                        entry.file_name().to_str().unwrap(),
                        SimpleFileOptions::default(),
                    )
                    .unwrap();
                writer.write_all(&fs::read(entry.path()).unwrap()).unwrap();
            }
            writer.finish().unwrap();
        }
        zip.set_position(0);
        let mut archive = GtfsArchive::open(zip, ImportLimits::default()).unwrap();
        let mut writer = SqliteStore::create(&path).unwrap();
        writer.import_gtfs(&mut archive).unwrap();
        writer.prepare_for_activation().unwrap();
        let before = fs::metadata(&path).unwrap().len();
        let (connection, store, _) = open(path.clone()).unwrap();
        let routes = execute(
            &connection,
            &store,
            Query::Routes {
                filter: "route-8".into(),
                after: 0,
            },
        )
        .unwrap();
        let Data::Routes { rows, has_more } = routes else {
            panic!("expected routes")
        };
        assert_eq!(rows.len(), 1);
        assert!(!has_more);
        let route_id = rows[0].id;
        let Data::Route { shapes, .. } = execute(
            &connection,
            &store,
            Query::Route {
                source_id: "route-8".into(),
            },
        )
        .unwrap() else {
            panic!("expected route")
        };
        assert_eq!(shapes[0].id, "shape-8");
        let Data::Trips { rows, .. } =
            execute(&connection, &store, Query::Trips { route_id, after: 0 }).unwrap()
        else {
            panic!("expected trips")
        };
        assert_eq!(rows[0].trip.source_id, "trip-late");
        let Data::Trip(trip) = execute(
            &connection,
            &store,
            Query::Trip {
                source_id: "trip-late".into(),
            },
        )
        .unwrap() else {
            panic!("expected trip")
        };
        assert!(!trip.shape.is_empty());
        assert!(trip.calls.iter().any(|call| {
            call.scheduled_departure
                .as_deref()
                .is_some_and(|time| time.starts_with("25:"))
        }));
        drop(store);
        drop(connection);
        assert_eq!(fs::metadata(&path).unwrap().len(), before);
        fs::remove_file(path).unwrap();
    }
}
