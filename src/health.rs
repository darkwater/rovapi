use std::{collections::BTreeMap, sync::Arc};

use axum::{Json, extract::State, http::StatusCode};
use ovapi_models::{LiveResponse, ReadyResponse};

use crate::AppState;

pub(crate) async fn live(State(state): State<Arc<AppState>>) -> Json<LiveResponse> {
    Json(LiveResponse {
        status: "ok".to_owned(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
    })
}

pub(crate) async fn ready(State(state): State<Arc<AppState>>) -> (StatusCode, Json<ReadyResponse>) {
    let (status_code, status, schedule) = if state.is_ready() {
        (StatusCode::OK, "ready", "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not_ready", "not_loaded")
    };
    (
        status_code,
        Json(ReadyResponse {
            status: status.to_owned(),
            checks: BTreeMap::from([("schedule".to_owned(), schedule.to_owned())]),
        }),
    )
}
