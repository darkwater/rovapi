BEGIN IMMEDIATE;

CREATE TABLE service_calendars (
    service_source_id  TEXT PRIMARY KEY,
    monday             INTEGER NOT NULL CHECK (monday IN (0, 1)),
    tuesday            INTEGER NOT NULL CHECK (tuesday IN (0, 1)),
    wednesday          INTEGER NOT NULL CHECK (wednesday IN (0, 1)),
    thursday           INTEGER NOT NULL CHECK (thursday IN (0, 1)),
    friday             INTEGER NOT NULL CHECK (friday IN (0, 1)),
    saturday           INTEGER NOT NULL CHECK (saturday IN (0, 1)),
    sunday             INTEGER NOT NULL CHECK (sunday IN (0, 1)),
    start_date         INTEGER NOT NULL,
    end_date           INTEGER NOT NULL,
    CHECK (start_date <= end_date)
) STRICT, WITHOUT ROWID;

CREATE TABLE service_exceptions (
    service_source_id  TEXT NOT NULL,
    date               INTEGER NOT NULL,
    exception_type     INTEGER NOT NULL CHECK (exception_type IN (1, 2)),
    PRIMARY KEY (service_source_id, date)
) STRICT, WITHOUT ROWID;

CREATE INDEX service_exceptions_by_date
    ON service_exceptions(date, service_source_id, exception_type);

PRAGMA user_version = 2;
COMMIT;
