# ovapi

An API for Dutch public-transport schedules, departures, disruptions, and live
vehicle positions.

OVAPI is designed as a standalone Rust service. Its schedule database is an
embedded, bundled SQLite build; no PostgreSQL, Redis, JVM, or other service is
required. FTS5 powers stop search and an R-tree narrows spatial queries before
exact distance filtering.

The project is in its initial scaffolding phase. See [PLAN.md](PLAN.md) for the
data-source research, architecture, API outline, and implementation milestones.

## Run locally

First import and activate a GTFS ZIP. The version is an operator-chosen ASCII
label and becomes the immutable schedule filename:

```sh
OVAPI_DATA_DIR=./data cargo run -- import ./gtfs.zip 2026-09-28
```

The import validates the archive, builds SQLite transactionally, checks the
finished database, and atomically updates `active.json`. Run imports while the
server is stopped; the data-directory lock prevents simultaneous writers.

Then start the server:

```sh
cargo run
```

The service binds to `127.0.0.1:3000` by default. Set `OVAPI_BIND_ADDRESS` to
change it, `OVAPI_DATA_DIR` to select the standalone data directory, and
`RUST_LOG` to configure tracing.

```sh
OVAPI_BIND_ADDRESS=0.0.0.0:8080 OVAPI_DATA_DIR=./data \
  RUST_LOG=ovapi=debug,tower_http=debug cargo run
```

Endpoints currently available:

- `GET /health/live`
- `GET /health/ready`
- `GET /v1/stops?query=utrecht&limit=20`
- `GET /v1/stops/nearby?lat=52.09&lon=5.12&radius=1000&limit=20`
- `GET /v1/stops/{source_id}`
- `GET /v1/stops/{source_id}/departures?date=2026-09-28&after=25:00:00&limit=20`
- `GET /v1/routes?query=8&limit=20`
- `GET /v1/routes/{source_id}`
- `GET /v1/routes/{source_id}/trips?date=2026-09-28&limit=20`
- `GET /v1/trips/{source_id}`
- `GET /v1/trips/{source_id}/stops`
- `GET /v1/trips/{source_id}/shape`

Departure queries use the GTFS service date and service-day time. Hours may
exceed 23, so a `25:11:00` departure belongs to the requested service date even
though it occurs at 01:11 on the following civil day. Calendar additions and
removals from `calendar_dates.txt` are applied.

Route-trip queries use the same service-date calendar rules. Trip stop calls are
returned in `stop_sequence` order with both readable GTFS times and their raw
seconds since the start of the service day.
When `shapes.txt` is present, trip geometry is returned as ordered latitude and
longitude points with the optional source distance along the shape.

Readiness returns HTTP 503 until a validated schedule database has been loaded;
liveness continues to report whether the process itself is running.

Schedule databases are validated and imported transactionally, then installed
and activated as immutable versioned files. The API therefore never exposes a
partially imported timetable.

The current importer intentionally supports the conventional fixed-stop GTFS
profile used for scheduled Dutch transit. It requires `agency.txt`, `stops.txt`,
`routes.txt`, `trips.txt`, `stop_times.txt`, and at least one calendar file.
Demand-responsive `location_group_id` and GeoJSON `location_id` stop times are
not supported yet and must not be silently treated as ordinary stops.
Optional `shapes.txt` and `transfers.txt` files are validated and imported,
including trip-specific and linked-trip transfer rules.

Run the checks with:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
