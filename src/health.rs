use std::{collections::BTreeMap, sync::Arc};

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::AppState;

#[derive(Debug, Serialize)]
pub(crate) struct LiveResponse {
    status: &'static str,
    uptime_seconds: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReadyResponse {
    status: &'static str,
    checks: BTreeMap<&'static str, &'static str>,
}

pub(crate) async fn live(State(state): State<Arc<AppState>>) -> Json<LiveResponse> {
    Json(LiveResponse {
        status: "ok",
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
            status,
            checks: BTreeMap::from([("schedule", schedule)]),
        }),
    )
}
