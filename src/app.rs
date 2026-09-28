use std::{
    sync::{Arc, PoisonError, RwLock},
    time::Instant,
};

use axum::{Router, http::Request, routing::get};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{
    api::routes::{get_route, route_trips, search_routes},
    api::schedule::get_schedule_metadata,
    api::stops::{get_stop, nearby_stops, scheduled_departures, search_stops, stops_in_rect},
    api::trips::{get_trip, trip_shape, trip_stops},
    error::{method_not_allowed, not_found},
    health::{live, ready},
    storage::SqliteReader,
};

#[derive(Clone, Debug)]
pub struct AppState {
    pub(crate) started_at: Instant,
    schedule: Arc<RwLock<Option<SqliteReader>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            schedule: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_schedule(schedule: SqliteReader) -> Self {
        Self {
            started_at: Instant::now(),
            schedule: Arc::new(RwLock::new(Some(schedule))),
        }
    }

    pub fn replace_schedule(&self, schedule: SqliteReader) -> Option<SqliteReader> {
        self.schedule
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(schedule)
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.schedule
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(SqliteReader::is_healthy)
    }

    pub(crate) fn schedule(&self) -> Result<SqliteReader, crate::error::ApiError> {
        self.schedule
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(crate::error::ApiError::schedule_unavailable)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/schedule", get(get_schedule_metadata))
        .route("/v1/stops", get(search_stops))
        .route("/v1/stops/nearby", get(nearby_stops))
        .route("/v1/stops/in-rect", get(stops_in_rect))
        .route("/v1/stops/:id/departures", get(scheduled_departures))
        .route("/v1/stops/:id", get(get_stop))
        .route("/v1/routes", get(search_routes))
        .route("/v1/routes/:id/trips", get(route_trips))
        .route("/v1/routes/:id", get(get_route))
        .route("/v1/trips/:id/stops", get(trip_stops))
        .route("/v1/trips/:id/shape", get(trip_shape))
        .route("/v1/trips/:id", get(get_trip))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(Arc::new(state))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("invalid");
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        uri = %request.uri(),
                        request_id,
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Cursor, Write},
        sync::atomic::{AtomicU64, Ordering},
    };

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::{
        gtfs::{GtfsArchive, ImportLimits},
        storage::{SqliteStore, StopInput},
    };

    static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn minimal_gtfs() -> GtfsArchive<Cursor<Vec<u8>>> {
        let files = [
            (
                "feed_info.txt",
                "feed_publisher_name,feed_publisher_url,feed_lang,feed_start_date,feed_end_date,feed_version\nExample Publisher,https://example.nl,nl,20260901,20260930,2026-09\n",
            ),
            (
                "agency.txt",
                "agency_id,agency_name,agency_url,agency_timezone\nNL,Example,https://example.nl,Europe/Amsterdam\n",
            ),
            (
                "stops.txt",
                "stop_id,stop_code,stop_name,stop_lat,stop_lon\nstop-1,UT,Utrecht Centraal,52.0907,5.1214\n",
            ),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_type\nroute-1,NL,8,3\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,trip_headsign,shape_id\nroute-1,weekday,trip-1,Science Park,shape-1\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\ntrip-1,25:10:00,25:11:00,stop-1,1\n",
            ),
            (
                "calendar.txt",
                "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekday,1,1,1,1,1,0,0,20260901,20260930\n",
            ),
            (
                "shapes.txt",
                "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence,shape_dist_traveled\nshape-1,52.0907,5.1214,1,0\nshape-1,52.1000,5.1300,2,1200.5\n",
            ),
        ];
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            for (name, contents) in files {
                writer
                    .start_file(name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(contents.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        output.set_position(0);
        GtfsArchive::open(output, ImportLimits::default()).unwrap()
    }

    #[tokio::test]
    async fn liveness_endpoint_reports_ok() {
        let response = router(AppState::new())
            .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["uptime_seconds"].is_number());
    }

    #[tokio::test]
    async fn readiness_endpoint_reports_missing_schedule() {
        let response = router(AppState::new())
            .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "not_ready");
        assert_eq!(
            json["checks"],
            serde_json::json!({"schedule": "not_loaded"})
        );
    }

    #[tokio::test]
    async fn unknown_route_has_json_error_and_request_id() {
        let response = router(AppState::new())
            .oneshot(Request::get("/missing").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().contains_key("x-request-id"));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn caller_request_id_is_preserved() {
        let response = router(AppState::new())
            .oneshot(
                Request::get("/health/live")
                    .header("x-request-id", "client-request-42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.headers()["x-request-id"], "client-request-42");
    }

    #[tokio::test]
    async fn unsupported_method_has_json_error() {
        let response = router(AppState::new())
            .oneshot(Request::post("/health/live").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn stop_query_reports_unavailable_without_schedule() {
        let response = router(AppState::new())
            .oneshot(
                Request::get("/v1/stops?query=utrecht")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "schedule_unavailable");
    }

    #[tokio::test]
    async fn nearby_stop_query_rejects_invalid_geography_before_schedule_access() {
        for uri in [
            "/v1/stops/nearby?lat=91&lon=5.12&radius=1000",
            "/v1/stops/nearby?lat=52.09&lon=181&radius=1000",
            "/v1/stops/nearby?lat=52.09&lon=5.12&radius=100001",
        ] {
            let response = router(AppState::new())
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "bad_request");
        }
    }

    #[tokio::test]
    async fn rectangle_stop_query_rejects_invalid_bounds_before_schedule_access() {
        for uri in [
            "/v1/stops/in-rect?min_lat=53&min_lon=4&max_lat=52&max_lon=5",
            "/v1/stops/in-rect?min_lat=52&min_lon=4&max_lat=53&max_lon=181",
            "/v1/stops/in-rect?min_lat=52&min_lon=4&max_lat=53&max_lon=5&limit=25001",
        ] {
            let response = router(AppState::new())
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "bad_request");
        }
    }

    #[tokio::test]
    async fn stop_search_uses_loaded_schedule() {
        let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rovapi-app-{}-{sequence}.sqlite",
            std::process::id()
        ));
        let mut store = SqliteStore::create(&path).unwrap();
        store
            .insert_stop(StopInput {
                source_id: "ut-centraal",
                code: Some("UT"),
                name: "Utrecht Centraal",
                latitude: Some(52.0893),
                longitude: Some(5.1103),
                location_type: Some(1),
                parent_source_id: None,
                platform_code: None,
            })
            .unwrap();
        store.prepare_for_activation().unwrap();
        let reader = SqliteReader::open(&path).await.unwrap();

        let app = router(AppState::with_schedule(reader));
        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/stops?query=utrecht")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json[0]["source_id"], "ut-centraal");

        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn scheduled_departure_endpoint_uses_service_date_and_gtfs_time() {
        let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rovapi-departures-{}-{sequence}.sqlite",
            std::process::id()
        ));
        let mut store = SqliteStore::create(&path).unwrap();
        store.import_gtfs(&mut minimal_gtfs()).unwrap();
        store
            .set_metadata("schedule_version", "test-version")
            .unwrap();
        store
            .set_metadata("imported_at_unix", "1234567890")
            .unwrap();
        store.prepare_for_activation().unwrap();
        let reader = SqliteReader::open(&path).await.unwrap();

        let app = router(AppState::with_schedule(reader));
        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/stops/stop-1/departures?date=2026-09-28&after=25:00:00&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json[0]["trip_id"], "trip-1");
        assert_eq!(json[0]["service_date"], "2026-09-28");
        assert_eq!(json[0]["scheduled_departure"], "25:11:00");

        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/routes/route-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(json["short_name"], "8");

        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/routes?query=8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(json[0]["source_id"], "route-1");

        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/routes/route-1/trips?date=2026-09-28")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(json[0]["source_id"], "trip-1");

        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/trips/trip-1/stops")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(json[0]["stop"]["source_id"], "stop-1");
        assert_eq!(json[0]["scheduled_departure"], "25:11:00");

        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/trips/trip-1/shape")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[1]["distance_traveled"], 1200.5);

        let response = app
            .oneshot(Request::get("/v1/schedule").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(json["schedule_version"], "test-version");
        assert_eq!(json["feed"]["source_version"], "2026-09");

        let _ = fs::remove_file(path);
    }
}
