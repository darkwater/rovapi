BEGIN IMMEDIATE;

CREATE TABLE stop_groups (
    id            INTEGER PRIMARY KEY,
    source_id     TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    latitude      REAL,
    longitude     REAL,
    min_latitude  REAL,
    min_longitude REAL,
    max_latitude  REAL,
    max_longitude REAL,
    member_count  INTEGER NOT NULL CHECK (member_count >= 1),
    CHECK ((latitude IS NULL) = (longitude IS NULL)),
    CHECK ((min_latitude IS NULL) = (min_longitude IS NULL)),
    CHECK ((min_latitude IS NULL) = (max_latitude IS NULL)),
    CHECK ((min_latitude IS NULL) = (max_longitude IS NULL))
) STRICT;

CREATE TABLE stop_group_members (
    group_id INTEGER NOT NULL REFERENCES stop_groups(id),
    stop_id  INTEGER NOT NULL UNIQUE REFERENCES stops(id),
    PRIMARY KEY (group_id, stop_id)
) STRICT, WITHOUT ROWID;

CREATE VIRTUAL TABLE stop_group_spatial USING rtree(
    group_id,
    min_latitude,
    max_latitude,
    min_longitude,
    max_longitude
);

PRAGMA user_version = 6;
COMMIT;
