use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State, rejection::QueryRejection},
};
use serde::Deserialize;

use crate::{
    AppState,
    error::ApiError,
    storage::{StoredRoute, StoredTrip},
};

use super::{default_limit, parse_service_date, validate_limit};

#[derive(Debug, Deserialize)]
pub(crate) struct RouteTripsQuery {
    date: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RouteSearchQuery {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

pub(crate) async fn search_routes(
    State(state): State<Arc<AppState>>,
    query: Result<Query<RouteSearchQuery>, QueryRejection>,
) -> Result<Json<Vec<StoredRoute>>, ApiError> {
    let Query(query) = query.map_err(ApiError::query)?;
    validate_limit(query.limit)?;
    if query.query.trim().is_empty() {
        return Err(ApiError::bad_request("query must not be empty"));
    }
    let routes = state
        .schedule()?
        .search_routes(query.query, query.limit)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(routes))
}

pub(crate) async fn get_route(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<StoredRoute>, ApiError> {
    state
        .schedule()?
        .route(source_id)
        .await
        .map_err(ApiError::storage)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("route does not exist"))
}

pub(crate) async fn route_trips(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
    query: Result<Query<RouteTripsQuery>, QueryRejection>,
) -> Result<Json<Vec<StoredTrip>>, ApiError> {
    let Query(query) = query.map_err(ApiError::query)?;
    validate_limit(query.limit)?;
    let date = parse_service_date(&query.date)?;
    let reader = state.schedule()?;
    if reader
        .route(source_id.clone())
        .await
        .map_err(ApiError::storage)?
        .is_none()
    {
        return Err(ApiError::not_found("route does not exist"));
    }
    let trips = reader
        .route_trips(source_id, date, query.limit)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(trips))
}
