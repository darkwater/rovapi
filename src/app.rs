use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use axum::{Router, http::Request, routing::get};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::{
    api::stops::{get_stop, nearby_stops, search_stops},
    error::{method_not_allowed, not_found},
    health::{live, ready},
    storage::SqliteReader,
};

#[derive(Clone, Debug)]
pub struct AppState {
    pub(crate) started_at: Instant,
    pub(crate) ready: Arc<AtomicBool>,
    schedule: Option<SqliteReader>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            ready: Arc::new(AtomicBool::new(false)),
            schedule: None,
        }
    }

    pub fn with_schedule(schedule: SqliteReader) -> Self {
        Self {
            started_at: Instant::now(),
            ready: Arc::new(AtomicBool::new(true)),
            schedule: Some(schedule),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub(crate) fn schedule(&self) -> Result<&SqliteReader, crate::error::ApiError> {
        self.schedule
            .as_ref()
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
        .route("/v1/stops", get(search_stops))
        .route("/v1/stops/nearby", get(nearby_stops))
        .route("/v1/stops/:id", get(get_stop))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(Arc::new(state))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<_>| {
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
            }),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::storage::{SqliteStore, StopInput};

    static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    async fn readiness_can_be_enabled_after_schedule_load() {
        let state = AppState::new();
        state.set_ready(true);
        let response = router(state)
            .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
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
    async fn stop_search_uses_loaded_schedule() {
        let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ovapi-app-{}-{sequence}.sqlite",
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

        let response = router(AppState::with_schedule(reader))
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
}
