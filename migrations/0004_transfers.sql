BEGIN IMMEDIATE;

CREATE TABLE transfers (
    from_stop_source_id   TEXT NOT NULL,
    to_stop_source_id     TEXT NOT NULL,
    from_trip_source_id   TEXT NOT NULL,
    to_trip_source_id     TEXT NOT NULL,
    from_route_source_id  TEXT NOT NULL,
    to_route_source_id    TEXT NOT NULL,
    transfer_type         INTEGER NOT NULL CHECK (transfer_type BETWEEN 0 AND 5),
    min_transfer_time     INTEGER CHECK (min_transfer_time IS NULL OR min_transfer_time >= 0),
    PRIMARY KEY (
        from_stop_source_id,
        to_stop_source_id,
        from_trip_source_id,
        to_trip_source_id,
        from_route_source_id,
        to_route_source_id
    ),
    CHECK (transfer_type != 2 OR min_transfer_time IS NOT NULL),
    CHECK (transfer_type > 3 OR (from_stop_source_id != '' AND to_stop_source_id != '')),
    CHECK (transfer_type < 4 OR (from_trip_source_id != '' AND to_trip_source_id != ''))
) STRICT, WITHOUT ROWID;

CREATE INDEX transfers_by_to_stop
    ON transfers(to_stop_source_id, from_stop_source_id);

PRAGMA user_version = 4;
COMMIT;
