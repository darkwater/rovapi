# ROVAPI

An API for Dutch public-transport schedules, departures, disruptions, and live
vehicle positions.

ROVAPI is designed as a standalone Rust service. Its schedule database is an
embedded, bundled SQLite build; no PostgreSQL, Redis, JVM, or other service is
required. FTS5 powers stop search and an R-tree narrows spatial queries before
exact distance filtering.

ROVAPI is a separate project from OVapi.nl; that service can still be used as
an upstream source of Dutch GTFS data.

The project is in its initial scaffolding phase. See [PLAN.md](PLAN.md) for the
data-source research, architecture, API outline, and implementation milestones.

## Run locally

First fetch or locally import and activate a GTFS ZIP. The version is an
operator-chosen ASCII label and becomes the immutable schedule filename:

```sh
ROVAPI_DATA_DIR=./data cargo run -- fetch \
  https://example.nl/gtfs.zip 2026-09-28
```

The fetch command streams into a snapshot with a 1 GiB compressed-size limit,
then validates and activates it. Successful response validators are persisted;
subsequent fetches for the same URL send `If-None-Match` and
`If-Modified-Since`, avoiding another import after HTTP 304. Use a new version
label when checking for a new immutable feed publication. If a fetch was
interrupted during import, rerunning it with the same version resumes from the
completed local snapshot rather than downloading it again.

For a GTFS ZIP already on disk:

```sh
ROVAPI_DATA_DIR=./data cargo run -- import ./gtfs.zip 2026-09-28
```

The import validates the archive, builds SQLite transactionally, checks the
finished database, and atomically updates `active.json`. Run imports while the
server is stopped; the data-directory lock prevents simultaneous writers.

To roll back to an installed version, or finish activation after a metadata
write failure:

```sh
ROVAPI_DATA_DIR=./data cargo run -- activate 2026-09-28
```

Inspect the installed versions or print the active schedule and feed metadata:

```sh
ROVAPI_DATA_DIR=./data cargo run -- versions
ROVAPI_DATA_DIR=./data cargo run -- status
```

Then start the server:

```sh
cargo run
```

The service binds to `127.0.0.1:3000` by default. Set `ROVAPI_BIND_ADDRESS` to
change it, `ROVAPI_DATA_DIR` to select the standalone data directory, and
`RUST_LOG` to configure tracing.

```sh
ROVAPI_BIND_ADDRESS=0.0.0.0:8080 ROVAPI_DATA_DIR=./data \
  RUST_LOG=rovapi=debug,tower_http=debug cargo run
```

Endpoints currently available:

- `GET /health/live`
- `GET /health/ready`
- `GET /v1/schedule`
- `GET /v1/stops?query=utrecht&limit=20`
- `GET /v1/stops/nearby?lat=52.09&lon=5.12&radius=1000&limit=20`
- `GET /v1/stops/in-rect?min_lat=51.8&min_lon=4.8&max_lat=52.3&max_lon=5.8&limit=25000`
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

Rectangle stop queries use the spatial index directly, return stops in stable
ID order, default to 10,000 results, and accept at most 25,000 results. They are
intended for map viewports and province-scale synchronization; nearby queries
remain distance-sorted and capped at 100.

The schedule endpoint reports the locally activated version and import time. If
the feed supplies `feed_info.txt`, it also reports the publisher, source version,
language, and advertised date range.

Readiness returns HTTP 503 until a validated schedule database has been loaded;
liveness continues to report whether the process itself is running.

## Rust API models

The workspace contains the transport-independent `rovapi-models` crate. It is
the source of truth for the server's JSON response bodies and query parameters,
and provides both `Serialize` and `Deserialize` implementations without
depending on Axum, Tokio, or a particular HTTP client.
Geographic models expose `geo-types` points and shape `LineString` conversion;
as usual for geospatial Rust types, longitude is `x` and latitude is `y`.
Agency, stop, route, trip, service, and shape identifiers are distinct
string-backed newtypes, preventing IDs for different entities from being mixed
while preserving string values in JSON.
Stop location types use the `LocationType` enum while retaining GTFS numeric
values `0` through `4` in JSON.

```toml
[dependencies]
rovapi-models = { path = "../rovapi/crates/rovapi-models" }
```

```rust
let departures: Vec<rovapi_models::ScheduledDeparture> =
    serde_json::from_slice(response_body)?;
```

The server crate also re-exports it as `rovapi::models` for applications which
already depend on the full service package.

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
