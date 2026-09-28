use std::sync::Arc;

use axum::{Json, extract::State};

use crate::{AppState, error::ApiError, storage::StoredScheduleMetadata};

pub(crate) async fn get_schedule_metadata(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StoredScheduleMetadata>, ApiError> {
    let metadata = state
        .schedule()?
        .schedule_metadata()
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(metadata))
}
