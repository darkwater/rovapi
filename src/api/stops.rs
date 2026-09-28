use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State, rejection::QueryRejection},
};
use serde::Deserialize;

use crate::{
    AppState,
    error::ApiError,
    gtfs::{GtfsDate, GtfsTime},
    storage::{NearbyStop, ScheduledDeparture, StoredStop},
};

#[derive(Debug, Deserialize)]
pub(crate) struct SearchQuery {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NearbyQuery {
    lat: f64,
    lon: f64,
    radius: f64,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeparturesQuery {
    date: String,
    after: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

const fn default_limit() -> usize {
    20
}

pub(crate) async fn get_stop(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<StoredStop>, ApiError> {
    let reader = state.schedule()?;
    reader
        .stop(source_id)
        .await
        .map_err(ApiError::storage)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("stop does not exist"))
}

pub(crate) async fn search_stops(
    State(state): State<Arc<AppState>>,
    query: Result<Query<SearchQuery>, QueryRejection>,
) -> Result<Json<Vec<StoredStop>>, ApiError> {
    let Query(query) = query.map_err(ApiError::query)?;
    validate_limit(query.limit)?;
    let reader = state.schedule()?;
    let stops = reader
        .search_stops(query.query, query.limit)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(stops))
}

pub(crate) async fn nearby_stops(
    State(state): State<Arc<AppState>>,
    query: Result<Query<NearbyQuery>, QueryRejection>,
) -> Result<Json<Vec<NearbyStop>>, ApiError> {
    let Query(query) = query.map_err(ApiError::query)?;
    validate_limit(query.limit)?;
    let reader = state.schedule()?;
    let stops = reader
        .nearby_stops(query.lat, query.lon, query.radius, query.limit)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(stops))
}

pub(crate) async fn scheduled_departures(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
    query: Result<Query<DeparturesQuery>, QueryRejection>,
) -> Result<Json<Vec<ScheduledDeparture>>, ApiError> {
    let Query(query) = query.map_err(ApiError::query)?;
    validate_limit(query.limit)?;
    let date = GtfsDate::parse_iso(&query.date)
        .map_err(|_| ApiError::bad_request("date must use YYYY-MM-DD"))?;
    let after: GtfsTime = query
        .after
        .parse()
        .map_err(|_| ApiError::bad_request("after must use HH:MM:SS (hours may exceed 23)"))?;

    let reader = state.schedule()?;
    if reader
        .stop(source_id.clone())
        .await
        .map_err(ApiError::storage)?
        .is_none()
    {
        return Err(ApiError::not_found("stop does not exist"));
    }
    let departures = reader
        .scheduled_departures(source_id, date, after, query.limit)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(departures))
}

fn validate_limit(limit: usize) -> Result<(), ApiError> {
    if !(1..=100).contains(&limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 100"));
    }
    Ok(())
}
