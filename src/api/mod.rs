use rovapi_models::MAX_LIMIT;

use crate::{error::ApiError, gtfs::GtfsDate};

pub(crate) mod routes;
pub(crate) mod schedule;
pub(crate) mod stops;
pub(crate) mod trips;

pub(crate) fn validate_limit(limit: usize) -> Result<(), ApiError> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    Ok(())
}

pub(crate) fn parse_service_date(value: &str) -> Result<GtfsDate, ApiError> {
    GtfsDate::parse_iso(value).map_err(|_| ApiError::bad_request("date must use YYYY-MM-DD"))
}
