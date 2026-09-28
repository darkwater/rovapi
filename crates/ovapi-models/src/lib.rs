//! Transport-independent request and response models for the OVAPI HTTP API.
//!
//! These types describe the JSON bodies and query parameters exposed by the
//! server. The crate deliberately does not select an HTTP client or async
//! runtime, so applications can combine it with their preferred transport.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 100;

pub mod error_code {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const INTERNAL_ERROR: &str = "internal_error";
    pub const METHOD_NOT_ALLOWED: &str = "method_not_allowed";
    pub const NOT_FOUND: &str = "not_found";
    pub const SCHEDULE_TIMEOUT: &str = "schedule_timeout";
    pub const SCHEDULE_UNAVAILABLE: &str = "schedule_unavailable";
}

const fn default_limit() -> usize {
    DEFAULT_LIMIT
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LiveResponse {
    pub status: String,
    pub uptime_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadyResponse {
    pub status: String,
    pub checks: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub source_id: String,
    pub code: Option<String>,
    pub name: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub location_type: Option<u8>,
    pub parent_source_id: Option<String>,
    pub platform_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NearbyStop {
    pub stop: Stop,
    pub distance_metres: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScheduledDeparture {
    pub trip_id: String,
    pub route_id: String,
    pub route_short_name: Option<String>,
    pub route_long_name: Option<String>,
    pub headsign: Option<String>,
    pub stop_sequence: u32,
    /// ISO `YYYY-MM-DD` GTFS service date.
    pub service_date: String,
    /// GTFS service-day time. The hour can exceed 23.
    pub scheduled_departure: String,
    pub scheduled_departure_seconds: u32,
    pub pickup_type: u8,
    pub drop_off_type: u8,
    pub timepoint: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub source_id: String,
    pub agency_source_id: Option<String>,
    pub short_name: Option<String>,
    pub long_name: Option<String>,
    pub route_type: u16,
    pub color: Option<String>,
    pub text_color: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Trip {
    pub source_id: String,
    pub route_id: String,
    pub service_id: String,
    pub headsign: Option<String>,
    pub short_name: Option<String>,
    pub direction_id: Option<u8>,
    pub shape_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduledStopCall {
    pub stop: Stop,
    pub stop_sequence: u32,
    pub scheduled_arrival: Option<String>,
    pub scheduled_arrival_seconds: Option<u32>,
    pub scheduled_departure: Option<String>,
    pub scheduled_departure_seconds: Option<u32>,
    pub headsign: Option<String>,
    pub pickup_type: Option<u8>,
    pub drop_off_type: Option<u8>,
    pub timepoint: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapePoint {
    pub sequence: u32,
    pub latitude: f64,
    pub longitude: f64,
    pub distance_traveled: Option<f64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FeedInfo {
    pub publisher_name: String,
    pub publisher_url: String,
    pub feed_lang: String,
    pub default_lang: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub source_version: Option<String>,
    pub contact_email: Option<String>,
    pub contact_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScheduleMetadata {
    pub schedule_version: Option<String>,
    pub imported_at_unix: Option<u64>,
    pub feed: Option<FeedInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopSearchQuery {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NearbyStopsQuery {
    pub lat: f64,
    pub lon: f64,
    pub radius: f64,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeparturesQuery {
    pub date: String,
    pub after: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouteSearchQuery {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouteTripsQuery {
    pub date: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_models_apply_the_server_default_limit() {
        let query: StopSearchQuery = serde_json::from_str(r#"{"query":"centraal"}"#).unwrap();
        assert_eq!(query.limit, 20);
    }

    #[test]
    fn response_models_round_trip_json() {
        let response = ErrorResponse {
            error: ErrorDetail {
                code: "not_found".to_owned(),
                message: "stop does not exist".to_owned(),
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<ErrorResponse>(&json).unwrap(),
            response
        );
    }
}
