use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
};

use crate::{
    AppState,
    error::ApiError,
    storage::{ScheduledStopCall, StoredShapePoint, StoredTrip},
};

pub(crate) async fn get_trip(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<StoredTrip>, ApiError> {
    state
        .schedule()?
        .trip(source_id)
        .await
        .map_err(ApiError::storage)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("trip does not exist"))
}

pub(crate) async fn trip_stops(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<Vec<ScheduledStopCall>>, ApiError> {
    let reader = state.schedule()?;
    if reader
        .trip(source_id.clone())
        .await
        .map_err(ApiError::storage)?
        .is_none()
    {
        return Err(ApiError::not_found("trip does not exist"));
    }
    let stops = reader
        .trip_stops(source_id)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(stops))
}

pub(crate) async fn trip_shape(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<Vec<StoredShapePoint>>, ApiError> {
    let reader = state.schedule()?;
    if reader
        .trip(source_id.clone())
        .await
        .map_err(ApiError::storage)?
        .is_none()
    {
        return Err(ApiError::not_found("trip does not exist"));
    }
    let shape = reader
        .trip_shape(source_id)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(shape))
}
