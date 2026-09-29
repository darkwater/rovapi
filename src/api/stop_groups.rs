use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
};
use rovapi_models::{MAX_RECT_STOPS_LIMIT, StopGroup, StopsInRectQuery};

use crate::{AppState, error::ApiError};

pub(crate) async fn stop_groups_in_rect(
    State(state): State<Arc<AppState>>,
    query: Result<Query<StopsInRectQuery>, QueryRejection>,
) -> Result<Json<Vec<StopGroup>>, ApiError> {
    let Query(query) = query.map_err(ApiError::query)?;
    if !(1..=MAX_RECT_STOPS_LIMIT).contains(&query.limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_RECT_STOPS_LIMIT}"
        )));
    }
    if !query.min_lat.is_finite()
        || !query.max_lat.is_finite()
        || !query.min_lon.is_finite()
        || !query.max_lon.is_finite()
        || !(-90.0..=90.0).contains(&query.min_lat)
        || !(-90.0..=90.0).contains(&query.max_lat)
        || !(-180.0..=180.0).contains(&query.min_lon)
        || !(-180.0..=180.0).contains(&query.max_lon)
    {
        return Err(ApiError::bad_request(
            "rectangle bounds must be finite WGS84 coordinates",
        ));
    }
    if query.min_lat > query.max_lat || query.min_lon > query.max_lon {
        return Err(ApiError::bad_request(
            "rectangle minimum bounds must not exceed maximum bounds",
        ));
    }
    Ok(Json(
        state
            .schedule()?
            .stop_groups_in_rect(
                query.min_lat,
                query.min_lon,
                query.max_lat,
                query.max_lon,
                query.limit,
            )
            .await
            .map_err(ApiError::storage)?,
    ))
}
