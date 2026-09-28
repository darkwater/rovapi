BEGIN IMMEDIATE;

CREATE TABLE shape_points (
    shape_source_id       TEXT NOT NULL,
    sequence              INTEGER NOT NULL,
    latitude              REAL NOT NULL CHECK (latitude BETWEEN -90.0 AND 90.0),
    longitude             REAL NOT NULL CHECK (longitude BETWEEN -180.0 AND 180.0),
    distance_traveled     REAL CHECK (distance_traveled IS NULL OR distance_traveled >= 0.0),
    PRIMARY KEY (shape_source_id, sequence)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 3;
COMMIT;
