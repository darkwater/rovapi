# OVAPI implementation plan

Last updated: 2026-09-28

## Goal

Build a stable, documented API over Dutch public-transport data that is useful
for departure-board applications such as OVinfo and live vehicle maps such as
OVzoeker. The product provides schedules and operational realtime information;
door-to-door journey planning and routing are deliberately outside its scope.

The core service is designed to be self-contained: one executable, one local
data directory, and no required database server, cache server, message broker,
object-storage service, JVM, or sidecar process. The application and its public
API are implemented in Rust. A fresh installation should become useful after
being given feed URLs and write access to its data directory.

## Scope

The API covers:

- stops, stop areas, platforms, and nearby-stop search;
- operators, routes, scheduled trips, stop sequences, and shapes;
- scheduled, expected, and observed arrivals and departures;
- cancellations, skipped stops, service changes, and alerts;
- current vehicle positions and trip progress;
- railway departure information and positions where source data permits it;
- feed freshness, provenance, and operational health.

The API does not calculate door-to-door journeys, walking routes, transfer
recommendations, isochrones, or fares. Clients may combine this API with a
separate routing product, but this repository will not integrate or operate one.

## Data sources

Production should obtain the official feeds directly from the NDOV Loket. The
public OVapi GTFS and GTFS-Realtime conversions are useful for development and
bootstrapping, but relying on them alone creates an avoidable third-party
dependency.

| Source | Contents | Initial role |
| --- | --- | --- |
| GTFS or NeTEx | Operators, stops, routes, trips, calendars, times, shapes | Static network model |
| GTFS-Realtime | Trip updates, vehicles, alerts, and converted train updates | Fast MVP ingestion |
| KV6 | Vehicle positions and trip progress | Native realtime adapter |
| KV15 | Stop-related messages | Notices and alerts |
| KV17 | Cancellations and trip mutations | Realtime trip state |
| KV7/8 Turbo | Planned and expected passage times | Departure boards |
| DVS/DAS, RitInfo | Railway departures, arrivals, and progress | Railway adapter |
| NS train positions | Geographic train positions | Railway map |
| CHB, EPIAP, SIRI FM | Stop and facility accessibility | Accessibility phase |
| PPT and railway fares | Fare products and prices | Later phase |

Before operating a public service, confirm the current NDOV agreement for each
feed, including redistribution, retention, attribution, availability, and any
delivery charges. The main site describes source data as CC0, while older feed
agreements and community documentation contained additional operational terms.

## Architectural principles

1. Preserve source messages before normalization so parsing and matching errors
   can be replayed and corrected.
2. Expose a source-independent public model. Do not leak KV-specific concepts
   where a stable transit concept exists.
3. Namespace source identifiers and mint stable public identifiers. A raw GTFS
   `trip_id` may change with a new feed publication.
4. Track event time and ingestion time separately. Reject an older event when it
   would replace newer state.
5. Represent scheduled, estimated, observed, cancelled, stale, and unknown states
   explicitly. Absence of realtime information does not mean cancellation.
6. Treat a Dutch service day as a domain concept. GTFS times after midnight may
   exceed 24:00 and belong to the preceding service date.
7. Do not introduce routing-engine concepts into the schedule/realtime model.
8. Prefer embedded, durable components. External infrastructure is introduced
   only in response to measured scale or availability requirements.
9. Keep persistence behind repository interfaces so standalone operation does
   not prevent a later PostgreSQL implementation.

## Components

### Static importer

Download feeds conditionally using ETag/Last-Modified, validate their contents,
stage a complete version, and atomically activate it. Core entities are agency,
stop place, quay, route, service journey, dated journey, stop call, service
calendar, shape, and transfer.

SQLite is the primary store. Enable WAL mode for ordinary operation, use FTS5
for stop/name search, and use an R-tree index plus exact distance filtering for
nearby-stop and bounding-box queries. The importer builds and validates a new,
immutable database file away from the active reader, then atomically activates
that version. This avoids exposing half-imported timetables and avoids a long
write transaction competing with API reads.

Recent static database versions remain in the local data directory for rollback,
debugging, and matching realtime events around a timetable transition. Retention
is configurable and cleanup never removes the active version.

### Realtime collectors

Each collector owns transport, decompression, decoding, reconnect/backoff, and
source-specific metrics. It emits canonical events rather than writing public
read models directly. Start with GTFS-Realtime protobuf, then add native BISON
and railway adapters.

### Matcher and projector

Match realtime events to a dated journey using source namespace, operator,
service date, journey number, line, and timing points. Project accepted events
into current vehicle, trip, departure, and alert views. Keep confidence and
matching diagnostics; never silently attach an ambiguous event.

### Storage

- SQLite: static schedules, durable normalized records, import metadata, and
  optional realtime checkpoints.
- In-process indexed state: current vehicles, trip updates, departure estimates,
  alerts, and subscriber fan-out.
- Local files: compressed source snapshots and bounded replay data, organized by
  source and observation date.
- Tokio channels: communication between collectors, matcher/projector tasks, and
  streaming API subscribers. Bounded channels provide explicit backpressure.

The on-disk layout is intentionally portable and backup-friendly:

```text
data/
  active.json                 # active static version and source metadata
  schedules/
    <feed-version>.sqlite     # immutable, validated timetable databases
  snapshots/
    <source>/<date>/...       # compressed raw input, retention-limited
  state/
    realtime-checkpoint.*     # optional fast restart state
```

SQLite access uses a small read pool and a single controlled writer. High-rate
vehicle updates stay in memory rather than producing one database transaction
per message. A periodic checkpoint is optional: the service can always recover
by loading the current feed again after restart.

### Public API

Axum serves versioned REST endpoints. SSE is the first streaming mechanism
because it works through ordinary HTTP infrastructure; WebSockets can be added
when bidirectional subscriptions are justified.

Proposed endpoints:

```text
GET /v1/stops?query=...
GET /v1/stops/nearby?lat=...&lon=...&radius=...
GET /v1/stops/{id}
GET /v1/stops/{id}/departures
GET /v1/routes/{id}
GET /v1/routes/{id}/trips
GET /v1/routes/{id}/vehicles
GET /v1/trips/{id}
GET /v1/trips/{id}/stops
GET /v1/vehicles?bbox=west,south,east,north
GET /v1/vehicles/{id}
GET /v1/alerts
GET /v1/stream/vehicles?bbox=...
GET /v1/stream/departures?stop_id=...
GET /health/live
GET /health/ready
```

Responses containing realtime facts include `source`, `observed_at`,
`received_at`, `realtime`, and a freshness/staleness indication. Lists use
cursor pagination, bounded page sizes, ETags where useful, and documented cache
headers. Geographic vehicle queries require a bounded viewport.

## Operational requirements

- Structured `tracing` output with request IDs and source/feed dimensions.
- Metrics for feed age, reconnects, decode failures, unmatched events, SQLite
  latency, projection lag, request latency, and response status.
- Liveness only proves the process/event loop is alive. Readiness verifies the
  active SQLite schedule and that required feeds have produced acceptably fresh
  state.
- Graceful shutdown stops accepting traffic, drains collectors, and checkpoints
  durable ingestion positions.
- Rate limiting and API keys should be optional initially but designed into the
  edge; large unbounded map queries must never be allowed.
- Golden feed fixtures cover DST changes, after-midnight service, cancellations,
  short turns, skipped stops, duplicate/out-of-order events, and feed rollover.
- Startup takes an exclusive lock in the data directory to prevent two service
  processes from accidentally writing the same standalone store.
- Configuration works through environment variables and an optional local file;
  secrets and feed credentials are never written into `active.json` or snapshots.
- Backup consists of copying immutable schedule files plus configuration. WAL
  checkpointing is performed before copying a mutable state database.

## Milestones

### M0 — service foundation

- [x] Axum/Tokio executable and reusable router library
- [x] Environment configuration
- [x] Structured tracing and HTTP trace middleware
- [x] Liveness and state-backed readiness endpoints
- [x] Graceful Ctrl+C/SIGTERM shutdown
- [x] Endpoint tests and local run documentation
- [x] Request ID propagation in responses and trace spans
- [x] JSON envelopes for unknown routes and unsupported methods
- [ ] JSON envelopes for extractor and internal failures
- [ ] OpenAPI generation

### M1 — static GTFS

- [ ] Feed downloader with conditional requests and size limits
- [x] Offline GTFS import-and-activate command
- [x] Streaming ZIP/CSV reader with archive limits and core typed records
- [x] Cross-record validation report for core identity/reference invariants
- [x] Calendar and calendar-exception validation
- [ ] Shape, transfer, and parent-station validation
- [x] Core SQLite schema, including internal IDs, FTS5, and R-tree indexes
- [x] Calendar and calendar-exception schema
- [ ] Shape, transfer, and feed-version schema
- [x] Atomic core GTFS-to-SQLite import transaction
- [x] Mandatory validation before import and prepared bulk inserts
- [x] Integrity check, WAL checkpoint, and schema check before activation
- [x] Versioned database-file installation, validation, and atomic activation
- [x] Stop lookup, search, and nearby-stop API endpoints
- [x] Calendar-aware scheduled departure API endpoint
- [ ] Route and trip API endpoints
- [ ] Representative fixtures and integration tests

### M2 — realtime MVP

- [ ] GTFS-Realtime protobuf decoder
- [ ] Trip-update and vehicle-position matcher
- [ ] Freshness-aware in-process state behind repository traits
- [ ] Actual/expected/cancelled departure projection
- [ ] Vehicle bbox and live trip endpoints
- [ ] Raw snapshot capture and deterministic replay tests
- [ ] Optional periodic realtime checkpoint and restart recovery

### M3 — direct NDOV

- [ ] Confirm agreements and provision official access
- [ ] KV6, KV15, KV17, and KV7/8 collectors
- [ ] Reconnect, backpressure, deduplication, and out-of-order policy
- [ ] Source quality dashboards and alerts
- [ ] Compare native and converted feeds during a shadow period

### M4 — railway and accessibility

- [ ] Railway departure, disruption, and position adapters
- [ ] Accessibility data

## Near-term decisions

1. Specify the data-directory locking and atomic active-version swap protocol.
2. Decide the stable ID representation before publishing `/v1` data.
3. Obtain small, redistributable GTFS and GTFS-Realtime test fixtures.
4. Choose the realtime checkpoint interval and retention; correctness must not
   depend on a checkpoint being present after restart.
5. Set explicit freshness budgets per source rather than one global threshold.

## When standalone stops being enough

SQLite is the intended deployment, not a temporary toy database. Reconsider the
storage topology only when there is evidence for one of these requirements:

- several service instances must write shared state concurrently;
- API instances must run on different hosts with immediate shared visibility;
- retained vehicle history grows beyond practical local-disk management;
- complex geospatial analytics exceed R-tree plus application-side filtering;
- database replication, automatic failover, or independently managed backups
  become operational requirements.

At that point a PostgreSQL/PostGIS repository can replace the SQLite repository,
and an external event broker or cache can replace Tokio channels/in-process
state. Those components are not part of the default architecture.
