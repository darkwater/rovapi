# Native database explorer

Status: stages 1 and 2 implemented as of 2026-10-13. Native egui application,
Walkers map, and egui_table views are in place. Stages 3 and 4 remain planned.
The route-to-trip workflow was checked against an imported GTFS fixture in a
virtual X display. National-feed performance remains to be measured.

## Purpose and interaction

Add `crates/rovapi-explorer`, a standalone native executable for inspecting
installed schedule databases. Combine raw stored fields and tables with transit
maps, stop sequences, calendars, and navigation through related records.

The initial workflow is: open a database, search routes, select a route, inspect
its variants on a map, select a trip, and inspect its ordered stops and times.

Use a persistent database/version header, entity navigation sidebar, paginated
record table, and resizable inspector/map area. Provide navigation history,
copyable identifiers, explicit null values, raw numeric values alongside enum
labels, and links for both internal IDs and source IDs. Keep selected rows and
map highlights synchronized. Preserve list filters and scroll position when
returning from a detail view.

## Technology and crate boundaries

- Use egui with eframe for the native application shell.
- Use Walkers for the map widget, drawing transit shapes and stop
  markers as overlays. Confirm selection/hit testing and fitting bounds in an
  early prototype. Use its OpenStreetMap tile source and display attribution;
  keep geometry visible when tiles are unavailable.
- Use `egui_table` for record lists, stop-time sequences, shape points, and raw
  table browsing. Use ordinary egui layouts for individual record fields and
  controls. Coordinate table selection with the inspector and map highlights.
- Resolve compatible eframe, egui, Walkers, and egui_table releases together.
  There is no Rust version constraint: current stable or latest nightly is
  acceptable. Update toolchain configuration and manifest declarations as needed,
  and document the toolchain used. No additional approval is needed to upgrade.
- Reuse `rovapi-models` for domain types and `rovapi` for existing read-only
  domain queries. Keep inspection-specific SQL and result types in the explorer.
  Initially use a separate read-only rusqlite connection for raw inspection.
  Both connections must point at the same selected immutable schedule file.
- Keep GUI dependencies confined to the new crate. Preserve ordinary server
  builds; document the explicit `cargo run -p rovapi-explorer` command.

Suggested modules: `app`, `navigation`, `db`, `worker`, `map`, and `views`.
Views own presentation; the database layer owns joins, pagination, and transit
interpretation. Revisit extracting shared storage only if actual duplication
justifies it.

References: [eframe](https://docs.rs/eframe/latest/eframe/),
[Walkers](https://docs.rs/walkers/latest/walkers/),
[map widget](https://docs.rs/walkers/latest/walkers/struct.Map.html).

## Database access and responsiveness

- Open the active SQLite database named by `ROVAPI_DATA_DIR/active.json` by
  default. Accept an explicit CLI path and an in-app path field. Browsing all
  installed versions remains follow-up work.
- Open read-only, validate schema compatibility, and pin the session to one
  version. Switching databases explicitly clears selection and cached results.
- Avoid `DataDirectory::open`: it acquires the service writer lock. Read-only
  discovery must work while the server imports and activates another version.
- Run SQLite operations on a background worker, never in the egui frame loop.
  Use bounded request/result channels, request generations to discard obsolete
  results, and request repaint when results arrive. Coalesce search requests
  and interrupt or time-limit expensive abandoned work.
- Use deterministic pagination with complete primary-key tie breakers, including
  composite keys. Virtualize displayed rows, fetch geometry lazily, and deduplicate
  shapes. Show loading, errors, and truncation explicitly. Avoid scanning or
  loading the entire national stop-times table during ordinary navigation.
- Parameterize values and allowlist table/column names for generated queries.
  The initial scope is table browsing and domain inspection; a SQL console and
  database editing are deferred.

## Specialized views

| Entity | Inspection features |
| --- | --- |
| Route | Raw fields, agency link, paginated trips, distinct shapes, variant selection |
| Trip | Raw fields, route/service links, shape, numbered stop calls and ordered times |
| Stop | Position, parent/children, group, serving routes/trips, scheduled departures |
| Stop group | Representative point, bounds, all member locations and raw membership |
| Service | Weekday calendar, date range, added/removed dates, related trips |
| Shape | Geometry, paginated ordered points, referencing trips |
| Agency | Stored fields and routes |
| Transfer | Raw rule fields and links to referenced stops, routes, and trips |
| Database | Path, schema/import/feed metadata, domain table browsing and schema details |

Relationship metadata must cover source-ID references as well as declared SQL
foreign keys. Keep derived stop groups distinguishable from imported GTFS stops.
Raw views describe stored database values, not original GTFS fields discarded
or normalized during import. Hide FTS/R-tree implementation tables by default;
offer them in an advanced schema view.

## Route maps and service-day semantics

A route may have several shapes, directions, branches, and stop patterns. Show
distinct shapes in the route overview and load the selected trip's exact stop
sequence on demand. A shape alone is not a stop pattern: trips sharing geometry
can still serve different stops. Variant summaries should distinguish direction,
shape, and ordered stop sequence, with computation bounded or performed lazily.

Allow optional service-date filtering, with an explicit all-services mode as
the initial inspection default. Reuse calendar exception semantics. Do not choose
today silently when inspecting an archived schedule. Display service-day times
above 24:00 faithfully, alongside their stored seconds where requested.

When geometry is missing, retain valid stop markers. Any optional straight-line
connection must be labelled approximate. Preserve repeated stop calls and their
sequence numbers; exclude missing coordinates from map bounds while retaining
those rows in the inspector. Clicking a marker should expose overlapping calls.

## Delivery stages and acceptance

1. Native shell and database session: open a supported schedule, display its
   metadata, report open/schema errors, and browse/search paginated routes.
   Prove map overlays and dependency/compiler compatibility here.
2. Route-to-trip vertical slice: select a route, inspect its shapes, select a
   trip, and synchronize stop-table selection with map highlights. Keep the UI
   responsive during queries and rapid selection changes.
3. Connected exploration: stops, groups, agencies, services, transfers, history,
   optional date filters, and installed-version discovery.
4. Raw browsing and polish: all domain tables, schema details, allowlisted
   filters/sorts, row export, persisted navigation preferences, and bounded caches.

Verify queries with small fixtures containing multiple route variants, shared
shapes with different stop patterns, missing shapes/coordinates, repeated stops,
calendar exceptions, composite keys, and after-midnight times. Test stale-result
rejection and read-only behavior. Manually exercise a national feed for query
latency, memory bounds, map interaction, and concurrent service activation.

Run workspace formatting, relevant crate tests, and Clippy for the affected
targets. First milestone completion requires the full route-to-trip inspection
workflow on a real installed schedule, not only a window rendering fixture data.

## Implementation handoff: first milestone

Stages 1 and 2 were implemented together. Stages 3 and 4 are follow-up work.
The following defaults record the scope of the first milestone.

### Entry point and dependency gate

- Launch with `cargo run -p rovapi-explorer`, which opens the active database
  when present. `--database PATH` overrides it. Keep an in-app path field and
  Open button. No native file-dialog dependency is needed initially. Target
  the current Linux development environment first.
- Add the crate as a workspace member and keep root/models as default members,
  so ordinary root Cargo commands retain their existing scope. Use explicit
  `--workspace` or `-p rovapi-explorer` when validating the new application.
- First compile a minimal window containing an egui_table and Walkers overlay.
  Select mutually compatible dependency versions before building out views.
  Use a toolchain that supports the selected dependencies, including nightly
  if useful. For reproducibility, pin a dated nightly if nightly is selected.
- Start with a tile-free map, transit geometry, and stop markers. Network tile
  configuration can follow without blocking database exploration.

### Worker contract

Use one dedicated database thread owning `SqliteStore::open_read_only` for shared
domain methods and a separate read-only rusqlite connection for explorer queries.
Call the synchronous `ScheduleRepository` methods on that thread; do not layer
the asynchronous `SqliteReader` pool underneath it. The worker owns the database
session and no connection crosses into UI code.

Define typed requests/results for opening a database, route pages, route details,
trip pages, route shape summaries, shape geometry, and trip details with stop
calls. Tag results with session and view/request generations. Accept a result
only if both still match. Opening a different database invalidates all previous
requests. Keep the last successfully opened session if a replacement fails.

Use nonblocking UI submission and result polling with bounded queues. Retry the
latest pending request when a queue is full; avoid enqueuing searches each frame.
Implement query interruption or deadlines where necessary and explicitly bound
geometry payloads. Do not silently discard required results or block worker
shutdown on sending to an abandoned result queue.

### Concrete browsing behavior

- Routes: 100 rows per page, ordered by internal ID; literal substring search
  over source ID, short name, and long name, debounced by 200 ms. Escape SQL LIKE
  wildcards. Changing a filter resets pagination. Use next/previous navigation
  with page cursors and fetch one extra row to determine whether another page
  exists. Total counts and arbitrary column sorting can wait.
- Trips: route-scoped, 100 rows per page ordered by internal ID, showing source
  ID, headsign, direction, service ID, and shape ID. Include all services in this
  milestone; date filtering comes in stage 3.
- Route map: list distinct shape IDs with referencing trip counts separately
  from the paginated trip list. Initially enable the first shape, and let users
  toggle additional shapes with at most 20 enabled at once. Make this selection
  limit visible. Do not label shape groups as complete stop-pattern variants;
  full stop-pattern grouping is follow-up work.
- Selected trip: highlight its actual shape and numbered stop calls. Render
  stored times as service-day strings and expose raw seconds. Preserve nulls and
  repeated calls. In this milestone missing shapes show markers only.
- Fit map bounds when selecting a different route/trip or pressing Fit, never
  on every repaint. Selecting a stop row highlights and reveals its marker;
  selecting a marker selects/reveals its row. Overlapping calls offer a chooser.
- Raw fields for routes and trips must include internal IDs. Display related
  source IDs as copyable values until their destination inspectors exist; do
  not present inactive navigation as working links.
- Show explicit Open/Loading/Ready/Error and empty-result states. Do not leave
  geometry from a previously selected record displayed as the current record.

### Completion evidence

Provide the launch command, resolved dependency versions and compiler baseline,
automated test/check results, and a short manual walkthrough of opening a real
database and exploring a route/trip. Include evidence that selecting a new row
during an outstanding query cannot display the previous row's results, and that
opening the explorer does not acquire the service writer lock or modify the
schedule. If a real feed or graphical session is unavailable, explicitly report
that validation as outstanding rather than claiming the milestone fully verified.
