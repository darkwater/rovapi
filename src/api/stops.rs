use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State, rejection::QueryRejection},
};
use ovapi_models::{DeparturesQuery, NearbyStopsQuery, StopSearchQuery};

use crate::{
    AppState,
    error::ApiError,
    gtfs::GtfsTime,
    storage::{NearbyStop, ScheduledDeparture, StoredStop},
};

use super::{parse_service_date, validate_limit};

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
    query: Result<Query<StopSearchQuery>, QueryRejection>,
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
    query: Result<Query<NearbyStopsQuery>, QueryRejection>,
) -> Result<Json<Vec<NearbyStop>>, ApiError> {
    let Query(query) = query.map_err(ApiError::query)?;
    validate_limit(query.limit)?;
    if !query.lat.is_finite()
        || !query.lon.is_finite()
        || !(-90.0..=90.0).contains(&query.lat)
        || !(-180.0..=180.0).contains(&query.lon)
    {
        return Err(ApiError::bad_request(
            "lat and lon must be finite WGS84 coordinates",
        ));
    }
    if !query.radius.is_finite() || !(0.0..=100_000.0).contains(&query.radius) {
        return Err(ApiError::bad_request(
            "radius must be between 0 and 100000 metres",
        ));
    }
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
    let date = parse_service_date(&query.date)?;
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
