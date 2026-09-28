use std::{
    collections::HashMap,
    fmt,
    io::{Read, Seek},
    path::Path,
    time::{Duration, Instant},
};

use rovapi_models::{
    AgencyId, FeedInfo as StoredFeedInfo, NearbyStop, Route as StoredRoute, RouteId,
    ScheduleMetadata as StoredScheduleMetadata, ScheduledDeparture, ScheduledStopCall, ServiceId,
    ShapeId, ShapePoint as StoredShapePoint, Stop as StoredStop, StopId, Trip as StoredTrip,
    TripId,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Statement, Transaction, params};

use crate::gtfs::{GtfsArchive, GtfsDate, GtfsError, GtfsTime, Stop, ValidationReport, validate};

const SCHEMA: &str = include_str!("../../migrations/0001_schedule.sql");
const CALENDAR_SCHEMA: &str = include_str!("../../migrations/0002_calendar.sql");
const SHAPE_SCHEMA: &str = include_str!("../../migrations/0003_shapes.sql");
const TRANSFER_SCHEMA: &str = include_str!("../../migrations/0004_transfers.sql");
const FEED_INFO_SCHEMA: &str = include_str!("../../migrations/0005_feed_info.sql");
const SCHEMA_VERSION: u32 = 5;
const EARTH_RADIUS_METRES: f64 = 6_371_000.0;

#[derive(Clone, Debug, PartialEq)]
pub struct StopInput<'a> {
    pub source_id: &'a str,
    pub code: Option<&'a str>,
    pub name: &'a str,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub location_type: Option<u8>,
    pub parent_source_id: Option<&'a str>,
    pub platform_code: Option<&'a str>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportSummary {
    pub feed_info: u64,
    pub agencies: u64,
    pub stops: u64,
    pub routes: u64,
    pub calendars: u64,
    pub calendar_dates: u64,
    pub shape_points: u64,
    pub transfers: u64,
    pub trips: u64,
    pub stop_times: u64,
}

pub trait ScheduleRepository {
    fn stop(&self, source_id: &str) -> Result<Option<StoredStop>, StorageError>;
    fn search_stops(&self, query: &str, limit: usize) -> Result<Vec<StoredStop>, StorageError>;
    fn nearby_stops(
        &self,
        latitude: f64,
        longitude: f64,
        radius_metres: f64,
        limit: usize,
    ) -> Result<Vec<NearbyStop>, StorageError>;
    fn scheduled_departures(
        &self,
        stop_source_id: &str,
        date: GtfsDate,
        after: GtfsTime,
        limit: usize,
    ) -> Result<Vec<ScheduledDeparture>, StorageError>;
    fn route(&self, source_id: &str) -> Result<Option<StoredRoute>, StorageError>;
    fn search_routes(&self, query: &str, limit: usize) -> Result<Vec<StoredRoute>, StorageError>;
    fn route_trips(
        &self,
        route_source_id: &str,
        date: GtfsDate,
        limit: usize,
    ) -> Result<Vec<StoredTrip>, StorageError>;
    fn trip(&self, source_id: &str) -> Result<Option<StoredTrip>, StorageError>;
    fn trip_stops(&self, trip_source_id: &str) -> Result<Vec<ScheduledStopCall>, StorageError>;
    fn trip_shape(&self, trip_source_id: &str) -> Result<Vec<StoredShapePoint>, StorageError>;
    fn schedule_metadata(&self) -> Result<StoredScheduleMetadata, StorageError>;
}

pub struct SqliteStore {
    connection: Connection,
}

#[derive(Debug)]
pub enum StorageError {
    Gtfs(GtfsError),
    InvalidCoordinate,
    InvalidReaderPoolSize,
    InvalidRadius,
    Integrity(String),
    InvalidReference {
        entity: &'static str,
        source_id: String,
    },
    Sqlite(rusqlite::Error),
    QueryTimedOut,
    UnsupportedSchema {
        actual: u32,
        expected: u32,
    },
    Validation(ValidationReport),
    WorkerStart(std::io::Error),
    WorkerStopped,
}

impl SqliteStore {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        let store = Self { connection };
        store.configure(true)?;
        store.connection.execute_batch(SCHEMA)?;
        store.connection.execute_batch(CALENDAR_SCHEMA)?;
        store.connection.execute_batch(SHAPE_SCHEMA)?;
        store.connection.execute_batch(TRANSFER_SCHEMA)?;
        store.connection.execute_batch(FEED_INFO_SCHEMA)?;
        store.ensure_schema_version()?;
        Ok(store)
    }

    pub fn create_in_memory() -> Result<Self, StorageError> {
        let store = Self {
            connection: Connection::open_in_memory()?,
        };
        store.configure(false)?;
        store.connection.execute_batch(SCHEMA)?;
        store.connection.execute_batch(CALENDAR_SCHEMA)?;
        store.connection.execute_batch(SHAPE_SCHEMA)?;
        store.connection.execute_batch(TRANSFER_SCHEMA)?;
        store.connection.execute_batch(FEED_INFO_SCHEMA)?;
        store.ensure_schema_version()?;
        Ok(store)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let store = Self {
            connection: Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?,
        };
        store.configure(false)?;
        store.ensure_schema_version()?;
        Ok(store)
    }

    fn configure(&self, writable: bool) -> Result<(), StorageError> {
        self.connection.pragma_update(None, "foreign_keys", true)?;
        self.connection.busy_timeout(Duration::from_secs(5))?;
        if writable {
            self.connection.pragma_update(None, "journal_mode", "WAL")?;
            self.connection
                .pragma_update(None, "synchronous", "NORMAL")?;
        }
        Ok(())
    }

    pub fn insert_stop(&mut self, stop: StopInput<'_>) -> Result<(), StorageError> {
        validate_coordinates(stop.latitude, stop.longitude)?;
        let transaction = self.connection.transaction()?;
        insert_stop_row(&transaction, stop)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn set_metadata(&self, key: &str, value: &str) -> Result<(), StorageError> {
        self.connection.execute(
            "INSERT INTO feed_metadata(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Validates and imports the core GTFS records in one transaction.
    ///
    /// The target should be a newly created schedule database. Validation is
    /// mandatory; any validation, parsing, reference, or SQLite error leaves the
    /// database unchanged.
    pub fn import_gtfs<R: Read + Seek>(
        &mut self,
        archive: &mut GtfsArchive<R>,
    ) -> Result<ImportSummary, StorageError> {
        let started = Instant::now();
        tracing::info!("validating GTFS archive");
        let validation = validate(archive)?;
        if !validation.is_valid() {
            return Err(StorageError::Validation(validation));
        }
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            stops = validation.counts.stops,
            routes = validation.counts.routes,
            trips = validation.counts.trips,
            stop_times = validation.counts.stop_times,
            "GTFS validation completed; persisting schedule"
        );
        let summary = self.import_validated_gtfs(archive)?;
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            stops = summary.stops,
            routes = summary.routes,
            trips = summary.trips,
            stop_times = summary.stop_times,
            "GTFS schedule persistence completed"
        );
        Ok(summary)
    }

    fn import_validated_gtfs<R: Read + Seek>(
        &mut self,
        archive: &mut GtfsArchive<R>,
    ) -> Result<ImportSummary, StorageError> {
        let transaction = self.connection.transaction()?;

        let mut feed_info_statement = transaction.prepare(
            "INSERT INTO feed_info(
                singleton, publisher_name, publisher_url, feed_lang, default_lang,
                start_date, end_date, source_version, contact_email, contact_url
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let feed_info = archive.visit_feed_info(|feed| {
            feed_info_statement.execute(params![
                feed.feed_publisher_name,
                feed.feed_publisher_url,
                feed.feed_lang,
                empty_to_none(&feed.default_lang),
                feed.feed_start_date.map(GtfsDate::compact),
                feed.feed_end_date.map(GtfsDate::compact),
                empty_to_none(&feed.feed_version),
                empty_to_none(&feed.feed_contact_email),
                empty_to_none(&feed.feed_contact_url),
            ])?;
            Ok::<_, StorageError>(())
        })?;
        drop(feed_info_statement);

        let mut agency_statement = transaction.prepare(
            "INSERT INTO agencies(source_id, name, url, timezone) VALUES (?1, ?2, ?3, ?4)",
        )?;
        let agencies = archive.visit_agencies(|agency| {
            agency_statement.execute(params![
                agency.agency_id,
                agency.agency_name,
                agency.agency_url,
                agency.agency_timezone,
            ])?;
            Ok::<_, StorageError>(())
        })?;
        drop(agency_statement);

        let mut stop_ids = HashMap::new();
        let mut stop_statement = transaction.prepare(
            "INSERT INTO stops (
                source_id, code, name, latitude, longitude, location_type,
                parent_source_id, platform_code
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut stop_search_statement = transaction.prepare(
            "INSERT INTO stop_search(rowid, stop_id, name, code) VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut stop_spatial_statement = transaction.prepare(
            "INSERT INTO stop_spatial(
                stop_id, min_latitude, max_latitude, min_longitude, max_longitude
             ) VALUES (?1, ?2, ?2, ?3, ?3)",
        )?;
        let stops = archive.visit_stops(|stop| {
            let source_id = stop.stop_id.clone();
            let id = insert_gtfs_stop_prepared(
                &transaction,
                &mut stop_statement,
                &mut stop_search_statement,
                &mut stop_spatial_statement,
                stop,
            )?;
            stop_ids.insert(source_id, id);
            Ok::<_, StorageError>(())
        })?;
        drop(stop_statement);
        drop(stop_search_statement);
        drop(stop_spatial_statement);

        let mut route_ids = HashMap::new();
        let mut route_statement = transaction.prepare(
            "INSERT INTO routes(
                source_id, agency_source_id, short_name, long_name, route_type, color, text_color
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let routes = archive.visit_routes(|route| {
            route_statement.execute(params![
                route.route_id,
                empty_to_none(&route.agency_id),
                empty_to_none(&route.route_short_name),
                empty_to_none(&route.route_long_name),
                route.route_type,
                empty_to_none(&route.route_color),
                empty_to_none(&route.route_text_color),
            ])?;
            route_ids.insert(route.route_id, transaction.last_insert_rowid());
            Ok::<_, StorageError>(())
        })?;
        drop(route_statement);

        let mut calendar_statement = transaction.prepare(
            "INSERT INTO service_calendars(
                service_source_id, monday, tuesday, wednesday, thursday,
                friday, saturday, sunday, start_date, end_date
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        let calendars = archive.visit_calendars(|calendar| {
            calendar_statement.execute(params![
                calendar.service_id,
                calendar.monday,
                calendar.tuesday,
                calendar.wednesday,
                calendar.thursday,
                calendar.friday,
                calendar.saturday,
                calendar.sunday,
                calendar.start_date.compact(),
                calendar.end_date.compact(),
            ])?;
            Ok::<_, StorageError>(())
        })?;
        drop(calendar_statement);

        let mut calendar_date_statement = transaction.prepare(
            "INSERT INTO service_exceptions(service_source_id, date, exception_type)
             VALUES (?1, ?2, ?3)",
        )?;
        let calendar_dates = archive.visit_calendar_dates(|exception| {
            calendar_date_statement.execute(params![
                exception.service_id,
                exception.date.compact(),
                exception.exception_type,
            ])?;
            Ok::<_, StorageError>(())
        })?;
        drop(calendar_date_statement);

        let mut shape_statement = transaction.prepare(
            "INSERT INTO shape_points(
                shape_source_id, sequence, latitude, longitude, distance_traveled
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let shape_points = archive.visit_shape_points(|point| {
            shape_statement.execute(params![
                point.shape_id,
                point.shape_pt_sequence,
                point.shape_pt_lat,
                point.shape_pt_lon,
                point.shape_dist_traveled,
            ])?;
            Ok::<_, StorageError>(())
        })?;
        drop(shape_statement);

        let mut trip_ids = HashMap::new();
        let mut trip_statement = transaction.prepare(
            "INSERT INTO trips(
                source_id, route_id, service_source_id, headsign, short_name,
                direction_id, shape_source_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let trips = archive.visit_trips(|trip| {
            let route_id = required_id(&route_ids, "route", &trip.route_id)?;
            trip_statement.execute(params![
                trip.trip_id,
                route_id,
                trip.service_id,
                empty_to_none(&trip.trip_headsign),
                empty_to_none(&trip.trip_short_name),
                trip.direction_id,
                empty_to_none(&trip.shape_id),
            ])?;
            trip_ids.insert(trip.trip_id, transaction.last_insert_rowid());
            Ok::<_, StorageError>(())
        })?;
        drop(trip_statement);

        let mut transfer_statement = transaction.prepare(
            "INSERT INTO transfers(
                from_stop_source_id, to_stop_source_id,
                from_trip_source_id, to_trip_source_id,
                from_route_source_id, to_route_source_id,
                transfer_type, min_transfer_time
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let transfers = archive.visit_transfers(|transfer| {
            transfer_statement.execute(params![
                transfer.from_stop_id,
                transfer.to_stop_id,
                transfer.from_trip_id,
                transfer.to_trip_id,
                transfer.from_route_id,
                transfer.to_route_id,
                transfer.transfer_type,
                transfer.min_transfer_time,
            ])?;
            Ok::<_, StorageError>(())
        })?;
        drop(transfer_statement);

        let mut stop_time_statement = transaction.prepare(
            "INSERT INTO stop_times(
                trip_id, stop_id, stop_sequence, arrival_service_seconds,
                departure_service_seconds, stop_headsign, pickup_type,
                drop_off_type, timepoint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let stop_times = archive.visit_stop_times(|stop_time| {
            let trip_id = required_id(&trip_ids, "trip", &stop_time.trip_id)?;
            let stop_id = required_id(&stop_ids, "stop", &stop_time.stop_id)?;
            stop_time_statement.execute(params![
                trip_id,
                stop_id,
                stop_time.stop_sequence,
                stop_time
                    .arrival_time
                    .map(|time| time.seconds_since_service_day_start()),
                stop_time
                    .departure_time
                    .map(|time| time.seconds_since_service_day_start()),
                empty_to_none(&stop_time.stop_headsign),
                stop_time.pickup_type,
                stop_time.drop_off_type,
                stop_time.timepoint,
            ])?;
            Ok::<_, StorageError>(())
        })?;
        drop(stop_time_statement);

        transaction.commit()?;
        Ok(ImportSummary {
            feed_info,
            agencies,
            stops,
            routes,
            calendars,
            calendar_dates,
            shape_points,
            transfers,
            trips,
            stop_times,
        })
    }

    pub fn schema_version(&self) -> Result<u32, StorageError> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    fn ensure_schema_version(&self) -> Result<(), StorageError> {
        let actual = self.schema_version()?;
        if actual != SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                actual,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// Verifies and closes a completed schedule database for atomic activation.
    ///
    /// This checkpoints WAL contents and switches back to a single-file journal
    /// before closing the final writer connection.
    pub fn prepare_for_activation(self) -> Result<(), StorageError> {
        self.ensure_schema_version()?;
        let integrity: String = self
            .connection
            .pragma_query_value(None, "quick_check", |row| row.get(0))?;
        if integrity != "ok" {
            return Err(StorageError::Integrity(integrity));
        }
        self.connection.execute_batch(
            "PRAGMA optimize;
             PRAGMA wal_checkpoint(TRUNCATE);
             PRAGMA journal_mode=DELETE;",
        )?;
        self.connection
            .close()
            .map_err(|(_, error)| StorageError::Sqlite(error))
    }
}

impl ScheduleRepository for SqliteStore {
    fn stop(&self, source_id: &str) -> Result<Option<StoredStop>, StorageError> {
        Ok(self
            .connection
            .query_row(
                "SELECT source_id, code, name, latitude, longitude, location_type,
                        parent_source_id, platform_code
                 FROM stops WHERE source_id = ?1",
                [source_id],
                map_stop,
            )
            .optional()?)
    }

    fn search_stops(&self, query: &str, limit: usize) -> Result<Vec<StoredStop>, StorageError> {
        let Some(query) = fts_prefix_query(query) else {
            return Ok(Vec::new());
        };
        let mut statement = self.connection.prepare(
            "SELECT s.source_id, s.code, s.name, s.latitude, s.longitude,
                    s.location_type, s.parent_source_id, s.platform_code
             FROM stop_search
             JOIN stops AS s ON s.id = stop_search.rowid
             WHERE stop_search MATCH ?1
             ORDER BY bm25(stop_search), s.name
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![query, bounded_limit(limit)], map_stop)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn nearby_stops(
        &self,
        latitude: f64,
        longitude: f64,
        radius_metres: f64,
        limit: usize,
    ) -> Result<Vec<NearbyStop>, StorageError> {
        validate_coordinates(Some(latitude), Some(longitude))?;
        if !radius_metres.is_finite() || !(0.0..=100_000.0).contains(&radius_metres) {
            return Err(StorageError::InvalidRadius);
        }

        let latitude_delta = radius_metres / 111_320.0;
        let longitude_scale = latitude.to_radians().cos().abs().max(0.01);
        let longitude_delta = radius_metres / (111_320.0 * longitude_scale);
        let mut statement = self.connection.prepare(
            "SELECT s.source_id, s.code, s.name, s.latitude, s.longitude,
                    s.location_type, s.parent_source_id, s.platform_code
             FROM stop_spatial AS spatial
             JOIN stops AS s ON s.id = spatial.stop_id
             WHERE spatial.max_latitude >= ?1 AND spatial.min_latitude <= ?2
               AND spatial.max_longitude >= ?3 AND spatial.min_longitude <= ?4",
        )?;
        let candidates = statement.query_map(
            params![
                latitude - latitude_delta,
                latitude + latitude_delta,
                longitude - longitude_delta,
                longitude + longitude_delta,
            ],
            map_stop,
        )?;
        let mut nearby = candidates
            .map(|candidate| {
                let stop = candidate?;
                let distance_metres = haversine_metres(
                    latitude,
                    longitude,
                    stop.latitude.expect("spatial stops have latitude"),
                    stop.longitude.expect("spatial stops have longitude"),
                );
                Ok(NearbyStop {
                    stop,
                    distance_metres,
                })
            })
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        nearby.retain(|stop| stop.distance_metres <= radius_metres);
        nearby.sort_by(|left, right| left.distance_metres.total_cmp(&right.distance_metres));
        nearby.truncate(bounded_limit(limit) as usize);
        Ok(nearby)
    }

    fn scheduled_departures(
        &self,
        stop_source_id: &str,
        date: GtfsDate,
        after: GtfsTime,
        limit: usize,
    ) -> Result<Vec<ScheduledDeparture>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT t.source_id, r.source_id, r.short_name, r.long_name,
                    COALESCE(st.stop_headsign, t.headsign), st.stop_sequence,
                    st.departure_service_seconds,
                    COALESCE(st.pickup_type, 0), COALESCE(st.drop_off_type, 0),
                    COALESCE(st.timepoint, 1)
             FROM stops AS s
             JOIN stop_times AS st ON st.stop_id = s.id
             JOIN trips AS t ON t.id = st.trip_id
             JOIN routes AS r ON r.id = t.route_id
             WHERE s.source_id = ?1
               AND st.departure_service_seconds IS NOT NULL
               AND COALESCE(st.pickup_type, 0) != 1
               AND st.departure_service_seconds >= ?3
               AND (
                   EXISTS (
                       SELECT 1 FROM service_exceptions AS added
                       WHERE added.service_source_id = t.service_source_id
                         AND added.date = ?2 AND added.exception_type = 1
                   )
                   OR (
                       EXISTS (
                           SELECT 1 FROM service_calendars AS calendar
                           WHERE calendar.service_source_id = t.service_source_id
                             AND ?2 BETWEEN calendar.start_date AND calendar.end_date
                             AND CASE ?4
                                 WHEN 0 THEN calendar.sunday
                                 WHEN 1 THEN calendar.monday
                                 WHEN 2 THEN calendar.tuesday
                                 WHEN 3 THEN calendar.wednesday
                                 WHEN 4 THEN calendar.thursday
                                 WHEN 5 THEN calendar.friday
                                 WHEN 6 THEN calendar.saturday
                             END = 1
                       )
                       AND NOT EXISTS (
                           SELECT 1 FROM service_exceptions AS removed
                           WHERE removed.service_source_id = t.service_source_id
                             AND removed.date = ?2 AND removed.exception_type = 2
                       )
                   )
               )
             ORDER BY st.departure_service_seconds, t.source_id
             LIMIT ?5",
        )?;
        let rows = statement.query_map(
            params![
                stop_source_id,
                date.compact(),
                after.seconds_since_service_day_start(),
                date.weekday() as u8,
                bounded_limit(limit),
            ],
            |row| {
                let seconds = row.get(6)?;
                Ok(ScheduledDeparture {
                    trip_id: TripId::from(row.get::<_, String>(0)?),
                    route_id: RouteId::from(row.get::<_, String>(1)?),
                    route_short_name: row.get(2)?,
                    route_long_name: row.get(3)?,
                    headsign: row.get(4)?,
                    stop_sequence: row.get(5)?,
                    service_date: date.to_string(),
                    scheduled_departure: GtfsTime::from_seconds(seconds).to_string(),
                    scheduled_departure_seconds: seconds,
                    pickup_type: row.get(7)?,
                    drop_off_type: row.get(8)?,
                    timepoint: row.get(9)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn route(&self, source_id: &str) -> Result<Option<StoredRoute>, StorageError> {
        Ok(self
            .connection
            .query_row(
                "SELECT source_id, agency_source_id, short_name, long_name,
                        route_type, color, text_color
                 FROM routes WHERE source_id = ?1",
                [source_id],
                map_route,
            )
            .optional()?)
    }

    fn search_routes(&self, query: &str, limit: usize) -> Result<Vec<StoredRoute>, StorageError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT source_id, agency_source_id, short_name, long_name,
                    route_type, color, text_color
             FROM routes
             WHERE instr(lower(source_id), lower(?1)) > 0
                OR instr(lower(COALESCE(short_name, '')), lower(?1)) > 0
                OR instr(lower(COALESCE(long_name, '')), lower(?1)) > 0
             ORDER BY CASE WHEN lower(short_name) = lower(?1) THEN 0 ELSE 1 END,
                      short_name, long_name, source_id
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![query, bounded_limit(limit)], map_route)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn route_trips(
        &self,
        route_source_id: &str,
        date: GtfsDate,
        limit: usize,
    ) -> Result<Vec<StoredTrip>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT t.source_id, r.source_id, t.service_source_id, t.headsign,
                    t.short_name, t.direction_id, t.shape_source_id
             FROM trips AS t
             JOIN routes AS r ON r.id = t.route_id
             WHERE r.source_id = ?1
               AND (
                   EXISTS (
                       SELECT 1 FROM service_exceptions AS added
                       WHERE added.service_source_id = t.service_source_id
                         AND added.date = ?2 AND added.exception_type = 1
                   )
                   OR (
                       EXISTS (
                           SELECT 1 FROM service_calendars AS calendar
                           WHERE calendar.service_source_id = t.service_source_id
                             AND ?2 BETWEEN calendar.start_date AND calendar.end_date
                             AND CASE ?3
                                 WHEN 0 THEN calendar.sunday
                                 WHEN 1 THEN calendar.monday
                                 WHEN 2 THEN calendar.tuesday
                                 WHEN 3 THEN calendar.wednesday
                                 WHEN 4 THEN calendar.thursday
                                 WHEN 5 THEN calendar.friday
                                 WHEN 6 THEN calendar.saturday
                             END = 1
                       )
                       AND NOT EXISTS (
                           SELECT 1 FROM service_exceptions AS removed
                           WHERE removed.service_source_id = t.service_source_id
                             AND removed.date = ?2 AND removed.exception_type = 2
                       )
                   )
               )
             ORDER BY t.source_id
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                route_source_id,
                date.compact(),
                date.weekday() as u8,
                bounded_limit(limit),
            ],
            map_trip,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn trip(&self, source_id: &str) -> Result<Option<StoredTrip>, StorageError> {
        Ok(self
            .connection
            .query_row(
                "SELECT t.source_id, r.source_id, t.service_source_id, t.headsign,
                        t.short_name, t.direction_id, t.shape_source_id
                 FROM trips AS t
                 JOIN routes AS r ON r.id = t.route_id
                 WHERE t.source_id = ?1",
                [source_id],
                map_trip,
            )
            .optional()?)
    }

    fn trip_stops(&self, trip_source_id: &str) -> Result<Vec<ScheduledStopCall>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT s.source_id, s.code, s.name, s.latitude, s.longitude,
                    s.location_type, s.parent_source_id, s.platform_code,
                    st.stop_sequence, st.arrival_service_seconds,
                    st.departure_service_seconds, st.stop_headsign,
                    st.pickup_type, st.drop_off_type, st.timepoint
             FROM trips AS t
             JOIN stop_times AS st ON st.trip_id = t.id
             JOIN stops AS s ON s.id = st.stop_id
             WHERE t.source_id = ?1
             ORDER BY st.stop_sequence",
        )?;
        let rows = statement.query_map([trip_source_id], |row| {
            let arrival_seconds = row.get(9)?;
            let departure_seconds = row.get(10)?;
            Ok(ScheduledStopCall {
                stop: map_stop(row)?,
                stop_sequence: row.get(8)?,
                scheduled_arrival: format_optional_gtfs_time(arrival_seconds),
                scheduled_arrival_seconds: arrival_seconds,
                scheduled_departure: format_optional_gtfs_time(departure_seconds),
                scheduled_departure_seconds: departure_seconds,
                headsign: row.get(11)?,
                pickup_type: row.get(12)?,
                drop_off_type: row.get(13)?,
                timepoint: row.get(14)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn trip_shape(&self, trip_source_id: &str) -> Result<Vec<StoredShapePoint>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT point.sequence, point.latitude, point.longitude, point.distance_traveled
             FROM trips AS trip
             JOIN shape_points AS point ON point.shape_source_id = trip.shape_source_id
             WHERE trip.source_id = ?1
             ORDER BY point.sequence",
        )?;
        let rows = statement.query_map([trip_source_id], |row| {
            Ok(StoredShapePoint {
                sequence: row.get(0)?,
                latitude: row.get(1)?,
                longitude: row.get(2)?,
                distance_traveled: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn schedule_metadata(&self) -> Result<StoredScheduleMetadata, StorageError> {
        let metadata_value = |key: &str| {
            self.connection
                .query_row(
                    "SELECT value FROM feed_metadata WHERE key = ?1",
                    [key],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        };
        let schedule_version = metadata_value("schedule_version")?;
        let imported_at_unix =
            metadata_value("imported_at_unix")?.and_then(|value| value.parse::<u64>().ok());
        let feed = self
            .connection
            .query_row(
                "SELECT publisher_name, publisher_url, feed_lang, default_lang,
                        start_date, end_date, source_version, contact_email, contact_url
                 FROM feed_info WHERE singleton = 1",
                [],
                |row| {
                    let start_date: Option<u32> = row.get(4)?;
                    let end_date: Option<u32> = row.get(5)?;
                    Ok(StoredFeedInfo {
                        publisher_name: row.get(0)?,
                        publisher_url: row.get(1)?,
                        feed_lang: row.get(2)?,
                        default_lang: row.get(3)?,
                        start_date: start_date.map(format_compact_date),
                        end_date: end_date.map(format_compact_date),
                        source_version: row.get(6)?,
                        contact_email: row.get(7)?,
                        contact_url: row.get(8)?,
                    })
                },
            )
            .optional()?;
        Ok(StoredScheduleMetadata {
            schedule_version,
            imported_at_unix,
            feed,
        })
    }
}

fn insert_gtfs_stop_prepared(
    transaction: &Transaction<'_>,
    stop_statement: &mut Statement<'_>,
    search_statement: &mut Statement<'_>,
    spatial_statement: &mut Statement<'_>,
    stop: Stop,
) -> Result<i64, StorageError> {
    let input = StopInput {
        source_id: &stop.stop_id,
        code: empty_to_none(&stop.stop_code),
        name: &stop.stop_name,
        latitude: stop.stop_lat,
        longitude: stop.stop_lon,
        location_type: stop.location_type,
        parent_source_id: empty_to_none(&stop.parent_station),
        platform_code: empty_to_none(&stop.platform_code),
    };
    validate_coordinates(input.latitude, input.longitude)?;
    stop_statement.execute(params![
        input.source_id,
        input.code,
        input.name,
        input.latitude,
        input.longitude,
        input.location_type,
        input.parent_source_id,
        input.platform_code,
    ])?;
    let id = transaction.last_insert_rowid();
    search_statement.execute(params![id, input.source_id, input.name, input.code])?;
    if let (Some(latitude), Some(longitude)) = (input.latitude, input.longitude) {
        spatial_statement.execute(params![id, latitude, longitude])?;
    }
    Ok(id)
}

fn insert_stop_row(
    transaction: &Transaction<'_>,
    stop: StopInput<'_>,
) -> Result<i64, StorageError> {
    transaction.execute(
        "INSERT INTO stops (
            source_id, code, name, latitude, longitude, location_type,
            parent_source_id, platform_code
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            stop.source_id,
            stop.code,
            stop.name,
            stop.latitude,
            stop.longitude,
            stop.location_type,
            stop.parent_source_id,
            stop.platform_code,
        ],
    )?;
    let id = transaction.last_insert_rowid();
    transaction.execute(
        "INSERT INTO stop_search(rowid, stop_id, name, code) VALUES (?1, ?2, ?3, ?4)",
        params![id, stop.source_id, stop.name, stop.code],
    )?;
    if let (Some(latitude), Some(longitude)) = (stop.latitude, stop.longitude) {
        transaction.execute(
            "INSERT INTO stop_spatial(
                stop_id, min_latitude, max_latitude, min_longitude, max_longitude
             ) VALUES (?1, ?2, ?2, ?3, ?3)",
            params![id, latitude, longitude],
        )?;
    }
    Ok(id)
}

fn required_id(
    ids: &HashMap<String, i64>,
    entity: &'static str,
    source_id: &str,
) -> Result<i64, StorageError> {
    ids.get(source_id)
        .copied()
        .ok_or_else(|| StorageError::InvalidReference {
            entity,
            source_id: source_id.to_owned(),
        })
}

fn empty_to_none(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn map_stop(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredStop> {
    Ok(StoredStop {
        source_id: StopId::from(row.get::<_, String>(0)?),
        code: row.get(1)?,
        name: row.get(2)?,
        latitude: row.get(3)?,
        longitude: row.get(4)?,
        location_type: row.get(5)?,
        parent_source_id: row.get::<_, Option<String>>(6)?.map(StopId::from),
        platform_code: row.get(7)?,
    })
}

fn map_route(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRoute> {
    Ok(StoredRoute {
        source_id: RouteId::from(row.get::<_, String>(0)?),
        agency_source_id: row.get::<_, Option<String>>(1)?.map(AgencyId::from),
        short_name: row.get(2)?,
        long_name: row.get(3)?,
        route_type: row.get(4)?,
        color: row.get(5)?,
        text_color: row.get(6)?,
    })
}

fn map_trip(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTrip> {
    Ok(StoredTrip {
        source_id: TripId::from(row.get::<_, String>(0)?),
        route_id: RouteId::from(row.get::<_, String>(1)?),
        service_id: ServiceId::from(row.get::<_, String>(2)?),
        headsign: row.get(3)?,
        short_name: row.get(4)?,
        direction_id: row.get(5)?,
        shape_id: row.get::<_, Option<String>>(6)?.map(ShapeId::from),
    })
}

fn format_optional_gtfs_time(seconds: Option<u32>) -> Option<String> {
    seconds.map(|seconds| GtfsTime::from_seconds(seconds).to_string())
}

fn format_compact_date(date: u32) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date / 10_000,
        date / 100 % 100,
        date % 100
    )
}

fn validate_coordinates(latitude: Option<f64>, longitude: Option<f64>) -> Result<(), StorageError> {
    match (latitude, longitude) {
        (None, None) => Ok(()),
        (Some(lat), Some(lon))
            if lat.is_finite()
                && lon.is_finite()
                && (-90.0..=90.0).contains(&lat)
                && (-180.0..=180.0).contains(&lon) =>
        {
            Ok(())
        }
        _ => Err(StorageError::InvalidCoordinate),
    }
}

fn bounded_limit(limit: usize) -> i64 {
    limit.clamp(1, 100) as i64
}

fn fts_prefix_query(query: &str) -> Option<String> {
    let terms: Vec<_> = query
        .split_whitespace()
        .map(|term| term.replace('"', "\"\""))
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

fn haversine_metres(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    let latitude_delta = (to_lat - from_lat).to_radians();
    let longitude_delta = (to_lon - from_lon).to_radians();
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let a = (latitude_delta / 2.0).sin().powi(2)
        + from_lat.cos() * to_lat.cos() * (longitude_delta / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_METRES * a.sqrt().asin()
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gtfs(error) => write!(formatter, "GTFS import error: {error}"),
            Self::InvalidCoordinate => write!(formatter, "invalid WGS84 coordinate pair"),
            Self::InvalidReaderPoolSize => {
                write!(
                    formatter,
                    "SQLite reader pool must contain at least one worker"
                )
            }
            Self::InvalidRadius => write!(formatter, "radius must be between 0 and 100000 metres"),
            Self::QueryTimedOut => write!(formatter, "SQLite query timed out while queued"),
            Self::Integrity(message) => {
                write!(formatter, "SQLite integrity check failed: {message}")
            }
            Self::InvalidReference { entity, source_id } => {
                write!(formatter, "unknown {entity} reference {source_id:?}")
            }
            Self::Sqlite(error) => write!(formatter, "SQLite error: {error}"),
            Self::UnsupportedSchema { actual, expected } => write!(
                formatter,
                "unsupported SQLite schema version {actual}; expected {expected}"
            ),
            Self::Validation(report) => write!(
                formatter,
                "GTFS validation failed with {} reported and {} omitted issues",
                report.issues.len(),
                report.omitted_issue_count
            ),
            Self::WorkerStopped => write!(formatter, "SQLite reader worker stopped"),
            Self::WorkerStart(error) => write!(formatter, "failed to start SQLite reader: {error}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gtfs(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::WorkerStart(error) => Some(error),
            _ => None,
        }
    }
}

impl From<GtfsError> for StorageError {
    fn from(error: GtfsError) -> Self {
        Self::Gtfs(error)
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Cursor, Write},
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::gtfs::ImportLimits;

    static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempDatabase(PathBuf);

    impl TempDatabase {
        fn new() -> Self {
            let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(
                std::env::temp_dir()
                    .join(format!("rovapi-{}-{sequence}.sqlite", std::process::id())),
            )
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDatabase {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    const MINIMAL_GTFS: &[(&str, &str)] = &[
        (
            "feed_info.txt",
            "feed_publisher_name,feed_publisher_url,feed_lang,feed_start_date,feed_end_date,feed_version\nExample Publisher,https://example.nl,nl,20260901,20260930,2026-09\n",
        ),
        (
            "agency.txt",
            "agency_id,agency_name,agency_url,agency_timezone\nNL,Example,https://example.nl,Europe/Amsterdam\n",
        ),
        (
            "stops.txt",
            "stop_id,stop_code,stop_name,stop_lat,stop_lon\nstop-1,UT,Centraal,52.0907,5.1214\n",
        ),
        (
            "routes.txt",
            "route_id,agency_id,route_short_name,route_type\nroute-1,NL,8,3\n",
        ),
        (
            "trips.txt",
            "route_id,service_id,trip_id,trip_headsign,shape_id\nroute-1,weekday,trip-1,Centraal,shape-1\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\ntrip-1,25:10:00,25:11:00,stop-1,1\n",
        ),
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekday,1,1,1,1,1,0,0,20260901,20260930\n",
        ),
        ("calendar_dates.txt", "service_id,date,exception_type\n"),
        (
            "shapes.txt",
            "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence,shape_dist_traveled\nshape-1,52.0907,5.1214,1,0\nshape-1,52.1000,5.1300,2,1200.5\n",
        ),
        (
            "transfers.txt",
            "from_stop_id,to_stop_id,transfer_type,min_transfer_time\nstop-1,stop-1,2,180\n",
        ),
    ];

    fn gtfs_archive(overrides: &[(&str, &str)]) -> GtfsArchive<Cursor<Vec<u8>>> {
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            for (name, default_contents) in MINIMAL_GTFS {
                let contents = overrides
                    .iter()
                    .find_map(|(candidate, contents)| (*candidate == *name).then_some(*contents))
                    .unwrap_or(default_contents);
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(contents.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        output.set_position(0);
        GtfsArchive::open(output, ImportLimits::default()).unwrap()
    }

    fn stop<'a>(id: &'a str, name: &'a str, latitude: f64, longitude: f64) -> StopInput<'a> {
        StopInput {
            source_id: id,
            code: Some(id),
            name,
            latitude: Some(latitude),
            longitude: Some(longitude),
            location_type: Some(0),
            parent_source_id: None,
            platform_code: None,
        }
    }

    #[test]
    fn bundled_sqlite_supports_schema_fts_and_rtree() {
        let mut store = SqliteStore::create_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), 5);
        store
            .insert_stop(stop("ut-centraal", "Utrecht Centraal", 52.0893, 5.1103))
            .unwrap();
        store
            .insert_stop(stop("ut-vaartsche", "Vaartsche Rijn", 52.0789, 5.1217))
            .unwrap();

        let search = store.search_stops("utrecht cent", 10).unwrap();
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].source_id, "ut-centraal");

        let nearby = store.nearby_stops(52.09, 5.11, 2_000.0, 10).unwrap();
        assert_eq!(nearby.len(), 2);
        assert_eq!(nearby[0].stop.source_id, "ut-centraal");
        assert!(nearby[0].distance_metres < nearby[1].distance_metres);
    }

    #[test]
    fn exact_distance_filters_rtree_bounding_box_corners() {
        let mut store = SqliteStore::create_in_memory().unwrap();
        store
            .insert_stop(stop("corner", "Bounding box corner", 52.099, 5.1146))
            .unwrap();

        let nearby = store.nearby_stops(52.09, 5.10, 1_000.0, 10).unwrap();
        assert!(nearby.is_empty());
    }

    #[test]
    fn rejects_partial_or_invalid_coordinates_before_sqlite() {
        let mut store = SqliteStore::create_in_memory().unwrap();
        let mut invalid = stop("invalid", "Invalid", 91.0, 5.0);
        assert!(matches!(
            store.insert_stop(invalid.clone()),
            Err(StorageError::InvalidCoordinate)
        ));
        invalid.latitude = None;
        assert!(matches!(
            store.insert_stop(invalid),
            Err(StorageError::InvalidCoordinate)
        ));
    }

    #[test]
    fn imports_core_gtfs_records_in_one_transaction() {
        let mut store = SqliteStore::create_in_memory().unwrap();
        let summary = store.import_gtfs(&mut gtfs_archive(&[])).unwrap();

        assert_eq!(
            summary,
            ImportSummary {
                feed_info: 1,
                agencies: 1,
                stops: 1,
                routes: 1,
                calendars: 1,
                calendar_dates: 0,
                shape_points: 2,
                transfers: 1,
                trips: 1,
                stop_times: 1,
            }
        );
        assert_eq!(store.stop("stop-1").unwrap().unwrap().name, "Centraal");
        let departure: u32 = store
            .connection
            .query_row(
                "SELECT departure_service_seconds FROM stop_times",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(departure, 90_660);
        assert_eq!(
            store
                .route("route-1")
                .unwrap()
                .unwrap()
                .short_name
                .as_deref(),
            Some("8")
        );
        assert_eq!(
            store.search_routes("8", 20).unwrap()[0].source_id,
            "route-1"
        );
        assert_eq!(store.trip("trip-1").unwrap().unwrap().route_id, "route-1");
        let calls = store.trip_stops("trip-1").unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].stop.source_id, "stop-1");
        assert_eq!(calls[0].scheduled_arrival.as_deref(), Some("25:10:00"));
        let shape = store.trip_shape("trip-1").unwrap();
        assert_eq!(shape.len(), 2);
        assert_eq!(shape[1].distance_traveled, Some(1200.5));
        let transfer_time: u32 = store
            .connection
            .query_row("SELECT min_transfer_time FROM transfers", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(transfer_time, 180);
        let source_version: String = store
            .connection
            .query_row("SELECT source_version FROM feed_info", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_version, "2026-09");
        let metadata = store.schedule_metadata().unwrap();
        assert_eq!(
            metadata.feed.unwrap().start_date.as_deref(),
            Some("2026-09-01")
        );
        assert!(metadata.schedule_version.is_none());
    }

    #[test]
    fn scheduled_departures_apply_calendar_ranges_weekdays_and_exceptions() {
        let mut store = SqliteStore::create_in_memory().unwrap();
        store
            .import_gtfs(&mut gtfs_archive(&[
                (
                    "calendar.txt",
                    "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekday,1,0,0,0,0,0,0,20260901,20260930\n",
                ),
                (
                    "calendar_dates.txt",
                    "service_id,date,exception_type\nweekday,20260928,2\nweekday,20260929,1\n",
                ),
            ]))
            .unwrap();

        let removed_monday = store
            .scheduled_departures(
                "stop-1",
                GtfsDate::parse_iso("2026-09-28").unwrap(),
                "00:00:00".parse().unwrap(),
                20,
            )
            .unwrap();
        assert!(removed_monday.is_empty());
        assert!(
            store
                .route_trips("route-1", GtfsDate::parse_iso("2026-09-28").unwrap(), 20,)
                .unwrap()
                .is_empty()
        );

        let added_tuesday = store
            .scheduled_departures(
                "stop-1",
                GtfsDate::parse_iso("2026-09-29").unwrap(),
                "25:00:00".parse().unwrap(),
                20,
            )
            .unwrap();
        assert_eq!(added_tuesday.len(), 1);
        assert_eq!(added_tuesday[0].scheduled_departure, "25:11:00");
        assert_eq!(added_tuesday[0].route_short_name.as_deref(), Some("8"));
        assert_eq!(added_tuesday[0].pickup_type, 0);
        assert_eq!(added_tuesday[0].drop_off_type, 0);
        assert_eq!(added_tuesday[0].timepoint, 1);
        assert_eq!(
            store
                .route_trips("route-1", GtfsDate::parse_iso("2026-09-29").unwrap(), 20,)
                .unwrap()[0]
                .source_id,
            "trip-1"
        );

        let after_departure = store
            .scheduled_departures(
                "stop-1",
                GtfsDate::parse_iso("2026-09-29").unwrap(),
                "25:12:00".parse().unwrap(),
                20,
            )
            .unwrap();
        assert!(after_departure.is_empty());

        store
            .connection
            .execute("UPDATE stop_times SET pickup_type = 1", [])
            .unwrap();
        let no_pickup = store
            .scheduled_departures(
                "stop-1",
                GtfsDate::parse_iso("2026-09-29").unwrap(),
                "25:00:00".parse().unwrap(),
                20,
            )
            .unwrap();
        assert!(no_pickup.is_empty());
    }

    #[test]
    fn invalid_feed_is_rejected_before_writes() {
        let mut store = SqliteStore::create_in_memory().unwrap();
        let result = store.import_gtfs(&mut gtfs_archive(&[(
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\ntrip-1,10:00:00,10:00:00,missing-stop,1\n",
        )]));

        assert!(matches!(result, Err(StorageError::Validation(_))));
        let agency_count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM agencies", [], |row| row.get(0))
            .unwrap();
        assert_eq!(agency_count, 0);
        assert!(store.stop("stop-1").unwrap().is_none());
    }

    #[test]
    fn sqlite_failure_rolls_back_every_imported_table() {
        let mut store = SqliteStore::create_in_memory().unwrap();
        store
            .insert_stop(stop("stop-1", "Existing", 52.0, 5.0))
            .unwrap();

        let result = store.import_gtfs(&mut gtfs_archive(&[]));
        assert!(matches!(result, Err(StorageError::Sqlite(_))));
        let agency_count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM agencies", [], |row| row.get(0))
            .unwrap();
        assert_eq!(agency_count, 0);
        assert_eq!(store.stop("stop-1").unwrap().unwrap().name, "Existing");
    }

    #[test]
    fn completed_database_is_checkpointed_and_read_only_compatible() {
        let database = TempDatabase::new();
        let mut store = SqliteStore::create(database.path()).unwrap();
        store
            .insert_stop(stop("stop-1", "Centraal", 52.0907, 5.1214))
            .unwrap();
        store.prepare_for_activation().unwrap();

        assert!(!PathBuf::from(format!("{}-wal", database.path().display())).exists());
        let reader = SqliteStore::open_read_only(database.path()).unwrap();
        assert_eq!(reader.stop("stop-1").unwrap().unwrap().name, "Centraal");
    }

    #[test]
    fn read_only_open_rejects_an_unknown_schema() {
        let database = TempDatabase::new();
        let connection = Connection::open(database.path()).unwrap();
        connection.pragma_update(None, "user_version", 999).unwrap();
        connection.close().unwrap();

        assert!(matches!(
            SqliteStore::open_read_only(database.path()),
            Err(StorageError::UnsupportedSchema {
                actual: 999,
                expected: 5
            })
        ));
    }
}
