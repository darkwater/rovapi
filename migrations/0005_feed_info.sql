BEGIN IMMEDIATE;

CREATE TABLE feed_info (
    singleton             INTEGER PRIMARY KEY CHECK (singleton = 1),
    publisher_name        TEXT NOT NULL,
    publisher_url         TEXT NOT NULL,
    feed_lang             TEXT NOT NULL,
    default_lang          TEXT,
    start_date            INTEGER,
    end_date              INTEGER,
    source_version        TEXT,
    contact_email         TEXT,
    contact_url           TEXT,
    CHECK (start_date IS NULL OR end_date IS NULL OR start_date <= end_date)
) STRICT;

PRAGMA user_version = 5;
COMMIT;
