use crate::{error::ApiError, gtfs::GtfsDate};

pub(crate) mod routes;
pub(crate) mod stops;
pub(crate) mod trips;

pub(crate) const fn default_limit() -> usize {
    20
}

pub(crate) fn validate_limit(limit: usize) -> Result<(), ApiError> {
    if !(1..=100).contains(&limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 100"));
    }
    Ok(())
}

pub(crate) fn parse_service_date(value: &str) -> Result<GtfsDate, ApiError> {
    GtfsDate::parse_iso(value).map_err(|_| ApiError::bad_request("date must use YYYY-MM-DD"))
}
