# ROVAPI code review

Reviewed: 2026-09-28

Scope: standalone GTFS ingestion, SQLite storage, Axum API, operational
behavior, and tests. Findings are ordered by severity within each section.

## Processing status (2026-09-28)

Implemented and verified:

- **H1, M4:** four independent read-only SQLite workers now serve queries, and
  readiness is derived from the current pool's worker health.
- **H4, M14:** activation is an explicit, validated, idempotent command;
  `versions` and JSON `status` commands make installed and active schedules
  inspectable and support rollback.
- **H6, R1:** public date parsing is safe for arbitrary UTF-8 input, with
  regression tests, and runtime `data/` is ignored by Git.
- **M3:** nearby-search coordinates and radius are checked at the HTTP boundary
  and return the JSON 400 contract.
- **M6:** departure responses expose effective pickup, drop-off, and timepoint
  values; stop calls with explicit `pickup_type=1` are excluded from the
  boarding-oriented departure list.
- **M10:** a nonempty `frequencies.txt` is rejected explicitly, preventing a
  silently incomplete timetable.
- **L1:** the snapshot parent directory is synced after an atomic rename.

Substantially addressed, with follow-up still useful:

- **H2:** interrupted fetches resume from an existing snapshot and an existing
  completed database can be activated. Replacing a permanently invalid
  snapshot under the same immutable version label remains intentionally
  manual; operators should normally use a new publication label.
- **M5:** required agency, stop, route, and trip fields plus direction,
  pickup/drop-off, and timepoint enums are now validated. Full timing-point and
  every optional-file invariant remain open.
- **M13:** queued SQLite commands carry deadlines and abandoned responses are
  skipped; timeout and dead-worker errors have stable HTTP mappings. Router
  concurrency limits and interruption of a query already executing in SQLite
  remain open.
- **M16:** successful HTTP responses now log status/latency at info level, and
  imports log validation and persistence phase totals and elapsed time.
  Incremental row-rate reporting during each long phase remains open.
- **T2:** invalid geographic input and no-pickup behavior now have regression
  coverage, but the complete extractor/error-contract matrix is still open.
- **M18:** public wire types and query models now live in the independent
  `rovapi-models` crate and are shared by server and consumers. Agency, stop,
  route, trip, service, and shape IDs are distinct transparent newtypes, also
  used for HTTP path extraction. The current v1 fields still expose
  source-native identifiers and some numeric GTFS concepts; `location_type` is
  now a checked enum, while introducing namespaces, canonical IDs, and the
  remaining enums remains open (**M2**).

Out of scope by current product decision: **H5**, because private or
credential-bearing feed sources are not supported.

Still open: **H3**, **M1**, **M7-M9**, **M11-M12**, **M15**, **M17**,
**T1**, **T3**, and **L2**. The next architectural milestone is in-process
refresh with atomic reader replacement, followed by retention and provenance.

## Confirmed strengths

- Schedule construction happens in a private staging database and one SQLite
  transaction. A failed import cannot expose partially imported rows.
- Activation verifies the schema, runs a SQLite quick check, checkpoints the
  WAL, renames the immutable database, and atomically replaces `active.json`.
- The downloader bounds compressed bytes; the archive reader independently
  bounds file count and total advertised uncompressed bytes and rejects unsafe
  or nested paths.
- Blocking rusqlite work is isolated from Tokio's async worker threads.
- The production-sized OVapi feed imported successfully in release mode:
  18,354,720 stop times and 948,939 trips in approximately 1 minute 54 seconds.

## High-priority findings

### H1. One SQLite worker serializes every API query

`SqliteReader` owns one connection on one OS thread and sends every request
through one bounded channel. A large trip-shape response or slow departure
query therefore blocks unrelated stop, route, health metadata, and search
queries behind it. The channel provides backpressure but not read concurrency.
This also differs from the plan's stated small read pool.

Recommendation: create a small fixed pool of read-only workers and dispatch
requests across it. Keep each rusqlite connection confined to its owning
thread. Measure before choosing the pool size; 2-4 readers is a reasonable
starting point for a standalone service.

Relevant code: `src/storage/reader.rs:76-189`.

### H2. `fetch` cannot retry the same version after an interrupted/failed import

The snapshot is renamed into its final path before import begins, while HTTP
validators are persisted only after import and activation succeed. If the
process stops during import, or validation rejects the feed, the final snapshot
remains and the validator state remains old or absent. A retry that receives
HTTP 200 fails with `DestinationExists` before it can reuse or replace that
snapshot. This occurred during the production-feed test; recovery required
calling `import` directly.

Recommendation: make `fetch` resumable as a state machine. If the destination
snapshot already exists but its version has no installed database, import that
snapshot first. Alternatively store download metadata beside the snapshot and
use content-addressed snapshot names. Add interruption/retry tests around each
state transition.

Relevant code: `src/main.rs:43-70`, `src/download.rs:56-125`.

### H3. A running server cannot refresh or switch schedules

The serving process keeps the exclusive data-directory lock, `AppState` holds
an immutable `Option<SqliteReader>`, and the importer is only exposed as a
separate CLI mode. Consequently an external import is rejected while the
server is running, and there is no in-process mechanism to download, import,
activate, and atomically swap readers. Static data can only be refreshed with
downtime/restart.

Recommendation: let the serving process own a background refresh task and a
swappable reader handle. Build the new immutable database using the existing
`DataDirectory`, open its reader, then atomically replace the active reader;
in-flight requests can retain clones of the old reader until completion.

Relevant code: `src/main.rs:82-100`, `src/app.rs:25-62`.

### H4. Import activation has an unrecoverable partial-success state

`install_and_activate` first renames and durably syncs the completed staging
database to its final versioned name, then writes `active.json`. If opening the
database or writing/syncing `active.json` fails, the valid final database
remains but the old schedule stays active. Re-running `import` rejects the
version because that database already exists, and the CLI has no `activate`
command. A transient metadata I/O failure therefore requires manual filesystem
intervention or custom code.

Recommendation: expose an idempotent recovery path. `import_and_activate`
should recognize a finalized, validated database for the requested version and
finish activation, or the CLI should provide an explicit validated `activate`
operation. Test failures immediately before and after the database rename and
the `active.json` rename.

Relevant code: `src/schedule.rs:188-270`.

### H5. Feed URLs can leak credentials into logs and local state

`fetch` logs the complete caller-supplied URL, and validator state persists that
complete URL in `state/static-feed.json`. If a production source uses URL
userinfo or signed query parameters, secrets appear in tracing output and in a
file created with ordinary process-umask permissions. Request errors may also
include the URL. The current public OVapi URL is unaffected, but this is unsafe
for credentialed NDOV sources.

Recommendation: configure a nonsecret source ID separately from transport
credentials. Put tokens in an authorization header/secret configuration,
redact URL userinfo and sensitive query values in logs, and key validator state
by source ID or a digest rather than the raw secret-bearing URL. Ensure secret
state files are created with restrictive permissions where applicable.

Relevant code: `src/main.rs:48-55`, `src/schedule.rs:25-29`,
`src/schedule.rs:136-185`.

User note: private sources are out of scope

### H6. A non-ASCII date query can panic during parsing

`GtfsDate::parse_iso` checks that the string is 10 bytes long and then slices
`value[4..5]` and `value[7..8]`. Those byte positions need not be UTF-8 character
boundaries. For example, five two-byte characters form a 10-byte string and can
make one of these slices panic. The function is reached directly from public
route-trip and departure query parameters, so malformed input can panic an
Axum request task instead of returning HTTP 400.

Recommendation: parse the byte array after requiring the exact ASCII
`YYYY-MM-DD` pattern, or use checked `get` operations; never byte-slice an
untrusted `str` before proving boundaries. Add non-ASCII and mixed-width
regression tests for both API endpoints and the date type.

Relevant code: `src/gtfs/date.rs:28-35`, `src/api/mod.rs:19-21`.

## Medium-priority findings

### M1. Import parses every GTFS CSV twice and maintains indexes row-by-row

`import_gtfs` first traverses the full archive for validation, then traverses it
again for insertion. The 1.09 GB `stop_times.txt` and 268 MB `shapes.txt` are
therefore decompressed and decoded twice. Secondary indexes already exist while
18 million stop-time rows are inserted, adding per-row B-tree maintenance.

Recommendation: combine validation and staging insertion where dependencies
permit, aborting the private transaction if the accumulated report is invalid.
Create nonessential secondary indexes after bulk loading. Benchmark each change
against the captured national feed before adding parsing parallelism.

Relevant code: `src/storage/sqlite.rs:266-517`,
`migrations/0001_schedule.sql:68-84`.

### M2. Public API identifiers are raw, unnamespaced GTFS identifiers

Responses and URL parameters expose `source_id` directly. The architecture plan
correctly notes that identifiers can collide between feeds and that GTFS trip
IDs may change on publication. Once clients and realtime matching depend on
these IDs, changing the contract will be expensive.

Recommendation: decide the public identity scheme before implementing
realtime. At minimum pair every source ID with a source/feed namespace; for
long-lived client references, define stable stop/route identities separately
from version-specific trip identities.

Relevant code: `PLAN.md:61-67`, `src/storage/sqlite.rs:30-126`.

## Repository hygiene

### R1. Runtime data is not ignored

`.gitignore` contains only `/target`, while the normal workflow creates
`data/`. The current untracked directory is roughly 1.5 GB and contains the
downloaded national feed and generated SQLite database. An indiscriminate add
could attempt to commit both.

Recommendation: add `/data/` to `.gitignore` and keep only small curated test
fixtures under `tests/fixtures/`.

Relevant code: `.gitignore:1`, `README.md:18-33`.

## Additional medium-priority findings

### M3. Invalid nearby-search input is reported as HTTP 500

The API validates only `limit`. Invalid latitude, longitude, NaN, infinity, or a
radius outside 0-100 km reaches the storage layer, which correctly returns
`InvalidCoordinate` or `InvalidRadius`; the handler then maps every storage
error to `internal_error`. These are client errors and should be HTTP 400. They
also produce misleading error-level logs about a schedule-query failure.

Recommendation: validate geographic query parameters at the HTTP boundary and
return stable field-specific 400 errors. Retain the storage validation as a
defense-in-depth invariant.

Relevant code: `src/api/stops.rs:69-80`, `src/storage/sqlite.rs:591-601`,
`src/error.rs:45-51`.

### M4. Readiness does not detect a dead or unusable SQLite worker

Readiness is a standalone atomic boolean initialized to true when a reader
opens. It is never linked to worker liveness or a database probe. If the reader
thread exits or panics later, `/health/ready` continues returning 200 while all
schedule endpoints return 500. This becomes more important once readers can be
replaced during refresh.

Recommendation: derive readiness from the currently published reader and its
health. A cheap periodic `SELECT 1`/metadata probe or explicit worker-lifecycle
state is sufficient; schedule freshness should later become another readiness
condition.

Relevant code: `src/app.rs:25-62`, `src/health.rs:27-39`,
`src/storage/reader.rs:82-188`.

### M5. Core GTFS validation omits several value and presence invariants

The importer validates important identities and references, but accepts values
that the GTFS model restricts. Required stop, route, trip, service, and shape IDs
can be empty strings. Other examples include `direction_id` outside 0/1,
`pickup_type` and `drop_off_type` outside 0-3, `timepoint` outside 0/1, routes
with both names empty, and stop-time timing-point requirements. The SQLite
schema also has no checks for these fields, so invalid values become public
data rather than causing an import failure. Empty IDs are especially awkward
because they cannot be addressed cleanly through the path-based API.

Recommendation: document the supported GTFS profile and add validation plus
SQLite checks for the enum/range invariants the API relies on. Add focused
negative fixtures. Timing interpolation can remain unsupported, but malformed
or semantically incomplete timing points should not be silently accepted.

Relevant code: `src/gtfs/records.rs:37-85`,
`src/gtfs/validation.rs:220-374`, `src/gtfs/validation.rs:511-538`,
`migrations/0001_schedule.sql:46-84`.

### M6. Departure results can present non-boardable stop calls as departures

The departure query returns every stop time with a departure value, regardless
of `pickup_type`, and the response omits pickup/drop-off restrictions. A
`pickup_type=1` call means passengers cannot board there, so a departure-board
client can present an unusable departure with no way to distinguish it. This is
material in the imported national feed: 1,010,808 calls have no pickup,
879,407 require contacting the agency, and 284 require coordination with the
driver.

Recommendation: either exclude no-pickup calls by default or expose pickup and
drop-off semantics in the response and provide an explicit query policy. The
latter is more flexible for vehicle-progress displays.

Relevant code: `src/storage/sqlite.rs:644-716`.

### M7. Trip-shape responses are unbounded and monopolize the sole reader

`GET /v1/trips/{id}/shape` materializes and serializes every point. There is no
point limit, simplification, pagination, response compression, or cache header.
With the current single reader, one unusually large shape also blocks all other
database work. The archive's global 2 GiB limit is not an adequate per-request
resource bound.

Recommendation: add HTTP compression and immutable-version cache validators at
minimum. Consider a compact encoded polyline or GeoJSON endpoint and enforce a
per-shape import bound; pagination is usually awkward for geometry.

Relevant code: `src/api/trips.rs:47-64`, `src/storage/sqlite.rs:854-870`.

### M8. Retention is documented but not implemented

Every feed version can retain both a compressed snapshot and a roughly 1.3 GB
national SQLite database. The plan says retention is configurable and never
removes the active version, but there is no cleanup policy or command. Automatic
daily refresh would otherwise consume hundreds of gigabytes over time.

Recommendation: implement count- and/or age-based retention for snapshots and
schedule databases. Resolve and protect the active version, keep at least one
rollback version, and perform cleanup only after successful activation.

Relevant code: `PLAN.md:95-97`, `src/schedule.rs:94-299`.

### M9. Stored provenance is insufficient to reproduce an import

The schedule metadata stores the operator-chosen version, import time, and
optional `feed_info.txt`, but not the retrieval URL, ETag, Last-Modified value,
snapshot filename, byte size, or content digest. Download validators live in a
single separate mutable JSON file. The API therefore cannot prove which exact
download produced an active database, especially after URLs change or old
snapshots are cleaned up.

Recommendation: write immutable source provenance into each schedule database
during import. Include source name/namespace, retrieval URL, retrieval time,
validators, compressed size, and a SHA-256 digest. Avoid exposing credentials
if URLs can contain secrets.

Relevant code: `src/main.rs:43-70`, `src/schedule.rs:25-29`,
`src/schedule.rs:210-216`, `src/storage/sqlite.rs:873-914`.

### M10. Unsupported frequency-based service is silently misrepresented

`frequencies.txt` is accepted as an extra ZIP member but is never parsed,
validated, imported, or rejected. For a frequency-based trip, the API will
expose only the template stop times and omit the repeated departures. Returning
plausible but incomplete timetable data is worse than explicitly rejecting an
unsupported feed feature.

Recommendation: until frequency expansion is implemented, detect a nonempty
`frequencies.txt` and reject the feed with a clear unsupported-feature error.
Apply the same policy to any optional GTFS file whose presence changes the
meaning of otherwise imported records.

Relevant code: `src/gtfs/archive.rs:12-22`, `src/gtfs/records.rs`,
`src/storage/sqlite.rs:266-517`.

### M11. List endpoints do not yet provide stable pagination

The architecture promises cursor pagination, but current list endpoints only
accept a capped `limit`. Route trips return the lexicographically first source
IDs rather than trips ordered by service time, and there is no way to request
the next page. This makes the endpoint incomplete for routes with more than 100
trips and unsuitable for deterministic full-data traversal.

Recommendation: define stable sort keys and opaque cursors before clients
depend on the current behavior. For route trips, include a meaningful first
departure/service ordering rather than source-ID ordering alone.

Relevant code: `PLAN.md:169-172`, `src/api/mod.rs:8-17`,
`src/storage/sqlite.rs:752-805`.

### M12. Production import limits are hard-coded and nearing the feed size

The national feed is already about 1.44 GB uncompressed against a fixed 2 GiB
archive limit. Download size, file count, archive size, and the 15-minute HTTP
timeout are compiled constants/defaults with no configuration surface. Normal
feed growth can therefore turn a healthy deployment into a failed refresh even
though the machine has sufficient resources.

Recommendation: retain conservative defaults but make limits explicit
configuration, log them at import start, and report observed compressed and
uncompressed sizes. Alert before a feed approaches its configured ceiling.

Relevant code: `src/main.rs:49-60`, `src/download.rs:46-53`,
`src/gtfs/archive.rs:23-32`.

### M13. Queued SQLite work has no timeout or cancellation

Dropping an HTTP request drops its oneshot receiver, but an already queued
command remains in the SQLite worker channel and is still executed. There is
also no router timeout or concurrency limit. Slow or disconnected clients can
therefore leave expensive shape/spatial queries consuming the sole worker,
while subsequent handlers wait on the bounded channel.

Recommendation: add request deadlines and concurrency limits at the HTTP layer,
then make database commands deadline-aware before execution. For long-running
queries, consider SQLite's progress handler/interruption support so cancellation
can stop work already in progress.

Relevant code: `src/app.rs:71-106`, `src/storage/reader.rs:76-189`.

### M14. Retained versions cannot be listed or rolled back through the CLI

The plan retains old schedules for rollback, and the library has an `activate`
method, but the executable exposes only `serve`, `import`, and `fetch`. An
operator cannot inspect installed versions or atomically roll back using the
standalone binary. This also removes the easiest recovery route for H4.

Recommendation: add read-only `versions`/`status` commands and a validated
`activate <version>` command. Report active version, source provenance, size,
and import time.

Relevant code: `PLAN.md:95-97`, `src/main.rs:105-148`,
`src/schedule.rs:243-299`.

### M15. Validation memory grows with several duplicated global ID maps

Validation streams CSV rows, but it simultaneously retains stop IDs and
hierarchy, route IDs, services, shape IDs and last sequences, trip IDs, a second
trip-to-route map, transfer keys, and per-trip stop sequences. With the current
949,000-trip feed, trip IDs alone are cloned into multiple collections. This is
bounded indirectly by the 2 GiB archive limit, but not by an explicit memory
budget, and adding parser parallelism would increase the peak further.

Recommendation: measure peak RSS on the national feed before parallelizing.
Reuse interned IDs or integer IDs, drop maps as soon as their last dependent
validation finishes, and consider using the private staging database for
uniqueness/reference checks rather than retaining duplicate strings.

Relevant code: `src/gtfs/validation.rs:92-540`.

### M16. Default tracing does not emit successful request completion records

The router creates an info-level request span, but tower-http's default request
and response events are debug-level. The default filter enables
`tower_http=info`, and the formatter does not emit span lifecycle events by
default. As a result, ordinary successful requests do not produce a completion
record with status and latency under the default configuration. Import work is
also silent between its start and final summary, which made the two-minute
national import indistinguishable from a stalled process.

Recommendation: configure explicit info-level response events containing
status, latency, and request ID, with failures at an appropriate higher level.
Add phase/count/rate progress events for long imports without logging per row.

Relevant code: `src/app.rs:89-105`, `src/main.rs:14-70`,
`src/main.rs:150-154`.

### M17. Static responses do not use cache validators or compression

All currently served schedule data is immutable for the lifetime of its
version, but responses have no ETag, Last-Modified, or explicit Cache-Control
policy. Response compression is also absent. Clients and intermediaries must
therefore repeatedly transfer and regenerate identical route, trip, stop, and
geometry responses, increasing pressure on the single SQLite reader.

Recommendation: derive strong or version-qualified weak ETags from the active
schedule version plus request target, honor conditional GET, and document cache
lifetimes. Enable compression for sufficiently large JSON responses while
avoiding wasted work on small health responses.

Relevant code: `PLAN.md:169-172`, `src/app.rs:71-106`.

### M18. Public DTOs expose GTFS-specific storage concepts

The plan calls for a source-independent public model, but responses expose
fields such as `source_id`, raw numeric `route_type`, `service_id`, `shape_id`,
`location_type`, and service-day seconds directly from GTFS-oriented storage
types. This makes a later NeTEx/native source or stable-ID layer an API-breaking
change rather than an internal adapter change.

Recommendation: separate persistence records from versioned public DTOs now.
Expose canonical enums and IDs while retaining source-native identifiers in a
clearly labeled provenance object. Keep raw GTFS values available where useful,
but do not make them the only public representation.

Relevant code: `PLAN.md:61-67`, `src/storage/sqlite.rs:21-127`,
`src/api/stops.rs:42-110`, `src/api/routes.rs:31-83`,
`src/api/trips.rs:14-64`.

## Test coverage findings

### T1. Downloader and activation failure transitions lack integration tests

Downloader tests cover conditional header construction and the byte-writing
limit, but not actual HTTP 200/304/error behavior, atomic rename, pre-existing
snapshots, or interrupted imports. Schedule tests cover normal activation and
ordinary validation failure, but not injected failures between final database
installation and active-metadata publication. Those untested transitions are
where H2 and H4 occur.

Recommendation: factor the state transitions so filesystem and HTTP outcomes
can be deterministically injected. Add a table-driven recovery test for every
durable state: no snapshot, completed snapshot, staging DB, installed DB, and
active DB.

Relevant code: `src/download.rs:202-279`, `src/schedule.rs:404-589`.

### T2. API error-path coverage is incomplete

The tests cover unknown routes, unsupported methods, missing schedules, and a
few successful endpoints. They do not cover invalid coordinates/radii,
malformed path extraction, missing query fields, invalid UTF-8/percent escapes,
storage-worker failure, result limits, or no-pickup departures. Consequently
the HTTP 500 behavior in M3 was not detected.

Recommendation: add a compact error-contract test matrix asserting status,
content type, request ID, and JSON error code for every extractor and domain
validation failure.

Relevant code: `src/app.rs:183-430`, `tests/static_feed.rs:67-121`.

### T3. The declared minimum Rust version is not exercised in-repository

The manifest declares Rust 1.89, but there is no visible CI configuration or
toolchain file that tests that compiler. The available Rust 1.91 nightly and
1.95 stable toolchains compile the locked graph successfully; Rust 1.89 was not
installed in this environment, so the exact MSRV remains unverified.

Recommendation: add CI jobs for formatting, clippy, tests, and `cargo check`
with exactly Rust 1.89 (or raise the declared MSRV intentionally). Keep one job
on a current stable compiler as well.

Relevant code: `Cargo.toml:4-5`.

## Low-priority findings

### L1. Download snapshot rename is not directory-synced

The downloader flushes and syncs the temporary file and renames it, but does not
sync the parent directory. On filesystems where directory fsync is required for
rename durability, a power loss can lose the final snapshot directory entry.
Schedule/database activation already handles this correctly.

Recommendation: sync the snapshot directory after rename, using the same
platform-specific helper as schedule activation.

Relevant code: `src/download.rs:56-72`, `src/schedule.rs:338-345`.

### L2. Framework-generated extractor failures do not share the JSON contract

Query rejections are manually converted to `ApiError`, but path rejections and
other Axum-generated failures use framework responses. The plan already tracks
this as unfinished. Clients therefore cannot assume that every API error has
the documented `{ "error": ... }` envelope.

Recommendation: centralize rejection mapping or use typed extractors that
implement the service's error contract, and test content type as well as body.

Relevant code: `PLAN.md:206-208`, `src/api/stops.rs:42-110`,
`src/api/routes.rs:31-83`, `src/api/trips.rs:14-64`.

## Verification performed

- `cargo test --all-targets`: 45 unit tests and 1 integration test passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Locked dependency graph compiled with Rust 1.91 nightly and Rust 1.95 stable.
  Rust 1.89 was not installed, as noted in T3.
- A release import of the current national OVapi feed completed successfully in
  approximately 1 minute 54 seconds and produced a 1.3 GB SQLite database.
- SQLite `EXPLAIN QUERY PLAN` on the national database confirmed that departure
  lookup uses the unique stop-ID index and the covering
  `stop_times_by_stop(stop_id, departure_service_seconds)` index. Route search
  scans the routes table, which currently contains only 3,441 rows.
- The national-data checks found no empty stop/route/trip IDs, no nameless
  routes, and only valid 0/1 trip directions. These facts show that M5 does not
  invalidate the current snapshot; M5 concerns defenses for future feeds.
- Dependency vulnerability auditing was not performed because `cargo-audit`
  and `cargo-deny` are not installed in this environment.

## Recommended implementation order

1. Prevent accidental data commits (`R1`) and fix the UTF-8 date panic (`H6`).
2. Make download/import/activation resumable and idempotent (`H2`, `H4`).
3. Stop leaking secret-bearing feed URLs (`H5`).
4. Add a read pool, request bounds, and correct readiness (`H1`, `M4`, `M13`).
5. Implement in-process static refresh, reader swapping, rollback, and retention
   (`H3`, `M8`, `M14`).
6. Correct public transit semantics before realtime work: namespaced/canonical
   IDs and DTOs, boarding rules, validation, and unsupported-frequency handling
   (`M2`, `M5`, `M6`, `M10`, `M18`).
7. Optimize import only after recording CPU, memory, disk, and query benchmarks
   (`M1`, `M12`, `M15`).
8. Finish pagination, caching, observability, and the expanded failure tests.
