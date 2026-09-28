use std::{
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use rovapi::{
    AppState,
    gtfs::ImportLimits,
    router,
    schedule::{DataDirectory, ScheduleVersion},
    storage::SqliteReader,
};
use rovapi_models::{
    ReadyResponse, ScheduleMetadata, ScheduledDeparture, ScheduledStopCall, ShapePoint, Stop,
};
use serde::de::DeserializeOwned;
use tower::ServiceExt;
use zip::{ZipWriter, write::SimpleFileOptions};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "rovapi-integration-{}-{sequence}",
            std::process::id()
        )))
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_zip(directory: &Path) -> Cursor<Vec<u8>> {
    let mut files: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();

    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        for path in files {
            let name = path.file_name().unwrap().to_str().unwrap();
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&fs::read(path).unwrap()).unwrap();
        }
        writer.finish().unwrap();
    }
    output.set_position(0);
    output
}

async fn json<T: DeserializeOwned>(app: axum::Router, uri: &str) -> T {
    let response = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn imports_activates_and_serves_the_representative_static_feed() {
    let temporary = TemporaryDirectory::new();
    let data = DataDirectory::open(&temporary.0).unwrap();
    let version = ScheduleVersion::parse("fixture-v1").unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/static_feed");

    let (active, summary) = data
        .import_and_activate(&version, fixture_zip(&fixture), ImportLimits::default())
        .unwrap();
    assert_eq!(summary.stops, 3);
    assert_eq!(summary.stop_times, 2);
    assert_eq!(summary.shape_points, 3);
    assert_eq!(summary.transfers, 1);
    assert_eq!(data.active().unwrap(), Some(active));

    let reader = SqliteReader::open(data.database_path(&version))
        .await
        .unwrap();
    let app = router(AppState::with_schedule(reader));

    let readiness: ReadyResponse = json(app.clone(), "/health/ready").await;
    assert_eq!(readiness.checks["schedule"], "ok");

    let schedule: ScheduleMetadata = json(app.clone(), "/v1/schedule").await;
    assert_eq!(schedule.schedule_version.as_deref(), Some("fixture-v1"));
    assert_eq!(
        schedule.feed.unwrap().source_version.as_deref(),
        Some("fixture-2026-09")
    );

    let rectangle_stops: Vec<Stop> = json(
        app.clone(),
        "/v1/stops/in-rect?min_lat=52.08&min_lon=5.10&max_lat=52.10&max_lon=5.12",
    )
    .await;
    assert_eq!(rectangle_stops.len(), 3);
    assert_eq!(rectangle_stops[0].source_id, "platform-a");
    assert_eq!(rectangle_stops[2].source_id, "station-ut");

    let removed: Vec<ScheduledDeparture> = json(
        app.clone(),
        "/v1/stops/platform-a/departures?date=2026-09-28&after=00:00:00",
    )
    .await;
    assert!(removed.is_empty());

    let added: Vec<ScheduledDeparture> = json(
        app.clone(),
        "/v1/stops/platform-a/departures?date=2026-10-03&after=25:00:00",
    )
    .await;
    assert_eq!(added[0].trip_id, "trip-late");
    assert_eq!(added[0].scheduled_departure, "25:11:00");

    let stops: Vec<ScheduledStopCall> = json(app.clone(), "/v1/trips/trip-late/stops").await;
    assert_eq!(stops[1].stop.platform_code.as_deref(), Some("B"));
    let shape: Vec<ShapePoint> = json(app, "/v1/trips/trip-late/shape").await;
    assert_eq!(shape.len(), 3);
}
