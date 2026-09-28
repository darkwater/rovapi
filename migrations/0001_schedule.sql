BEGIN IMMEDIATE;

CREATE TABLE feed_metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE agencies (
    id              INTEGER PRIMARY KEY,
    source_id       TEXT NOT NULL UNIQUE,
    name            TEXT NOT NULL,
    url             TEXT NOT NULL,
    timezone        TEXT NOT NULL
) STRICT;

CREATE TABLE stops (
    id                  INTEGER PRIMARY KEY,
    source_id           TEXT NOT NULL UNIQUE,
    code                TEXT,
    name                TEXT NOT NULL,
    latitude            REAL,
    longitude           REAL,
    location_type       INTEGER CHECK (location_type BETWEEN 0 AND 4),
    parent_source_id    TEXT,
    platform_code       TEXT,
    CHECK ((latitude IS NULL) = (longitude IS NULL)),
    CHECK (latitude IS NULL OR latitude BETWEEN -90.0 AND 90.0),
    CHECK (longitude IS NULL OR longitude BETWEEN -180.0 AND 180.0)
) STRICT;

CREATE VIRTUAL TABLE stop_search USING fts5(
    stop_id UNINDEXED,
    name,
    code,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE VIRTUAL TABLE stop_spatial USING rtree(
    stop_id,
    min_latitude,
    max_latitude,
    min_longitude,
    max_longitude
);

CREATE TABLE routes (
    id                  INTEGER PRIMARY KEY,
    source_id           TEXT NOT NULL UNIQUE,
    agency_source_id    TEXT,
    short_name          TEXT,
    long_name           TEXT,
    route_type          INTEGER NOT NULL,
    color               TEXT,
    text_color          TEXT
) STRICT;

CREATE TABLE trips (
    id                  INTEGER PRIMARY KEY,
    source_id           TEXT NOT NULL UNIQUE,
    route_id            INTEGER NOT NULL REFERENCES routes(id),
    service_source_id   TEXT NOT NULL,
    headsign            TEXT,
    short_name          TEXT,
    direction_id        INTEGER,
    shape_source_id     TEXT
) STRICT;

CREATE INDEX trips_by_route ON trips(route_id);
CREATE INDEX trips_by_service ON trips(service_source_id);

CREATE TABLE stop_times (
    trip_id                    INTEGER NOT NULL REFERENCES trips(id),
    stop_id                    INTEGER NOT NULL REFERENCES stops(id),
    stop_sequence              INTEGER NOT NULL,
    arrival_service_seconds    INTEGER,
    departure_service_seconds  INTEGER,
    stop_headsign              TEXT,
    pickup_type                INTEGER,
    drop_off_type              INTEGER,
    timepoint                  INTEGER,
    PRIMARY KEY (trip_id, stop_sequence)
) STRICT, WITHOUT ROWID;

CREATE INDEX stop_times_by_stop ON stop_times(stop_id, departure_service_seconds);

PRAGMA user_version = 1;
COMMIT;
