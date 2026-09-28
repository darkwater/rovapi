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

Readiness returns HTTP 503 until a validated schedule database has been loaded;
liveness continues to report whether the process itself is running.

The GTFS and SQLite modules are currently library-level building blocks. Public
transit endpoints will be enabled after atomic feed import and activation are in
place, so the API never exposes a partially imported timetable.

The current importer intentionally supports the conventional fixed-stop GTFS
profile used for scheduled Dutch transit. It requires `agency.txt`, `stops.txt`,
`routes.txt`, `trips.txt`, `stop_times.txt`, and at least one calendar file.
Demand-responsive `location_group_id` and GeoJSON `location_id` stop times are
not supported yet and must not be silently treated as ordinary stops.

Run the checks with:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
