//! Transport-independent request and response models for the ROVAPI HTTP API.
//!
//! These types describe the JSON bodies and query parameters exposed by the
//! server. The crate deliberately does not select an HTTP client or async
//! runtime, so applications can combine it with their preferred transport.

use std::{borrow::Borrow, collections::BTreeMap, fmt};

pub use geo_types::{LineString, Point};
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

macro_rules! id_type {
    ($name:ident, $entity:literal) => {
        #[doc = concat!("A source-native ", $entity, " identifier.")]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }
    };
}

id_type!(AgencyId, "agency");
id_type!(RouteId, "route");
id_type!(ServiceId, "service");
id_type!(ShapeId, "shape");
id_type!(StopId, "stop");
id_type!(TripId, "trip");

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
    pub source_id: StopId,
    pub code: Option<String>,
    pub name: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub location_type: Option<u8>,
    pub parent_source_id: Option<StopId>,
    pub platform_code: Option<String>,
}

impl Stop {
    /// Returns the WGS84 stop position with longitude as `x` and latitude as `y`.
    pub fn point(&self) -> Option<Point<f64>> {
        Some(Point::new(self.longitude?, self.latitude?))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NearbyStop {
    pub stop: Stop,
    pub distance_metres: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScheduledDeparture {
    pub trip_id: TripId,
    pub route_id: RouteId,
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
    pub source_id: RouteId,
    pub agency_source_id: Option<AgencyId>,
    pub short_name: Option<String>,
    pub long_name: Option<String>,
    pub route_type: u16,
    pub color: Option<String>,
    pub text_color: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Trip {
    pub source_id: TripId,
    pub route_id: RouteId,
    pub service_id: ServiceId,
    pub headsign: Option<String>,
    pub short_name: Option<String>,
    pub direction_id: Option<u8>,
    pub shape_id: Option<ShapeId>,
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

impl ShapePoint {
    /// Returns this WGS84 shape position with longitude as `x` and latitude as `y`.
    pub fn point(&self) -> Point<f64> {
        Point::new(self.longitude, self.latitude)
    }
}

/// Converts ordered API shape points into a `geo_types::LineString`.
pub fn shape_line_string(points: &[ShapePoint]) -> LineString<f64> {
    points.iter().map(ShapePoint::point).collect()
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

impl NearbyStopsQuery {
    /// Creates a nearby-stop query from a WGS84 point.
    pub fn from_point(point: Point<f64>, radius: f64, limit: usize) -> Self {
        Self {
            lat: point.y(),
            lon: point.x(),
            radius,
            limit,
        }
    }

    /// Returns the query position with longitude as `x` and latitude as `y`.
    pub fn point(&self) -> Point<f64> {
        Point::new(self.lon, self.lat)
    }
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
    fn identifiers_are_transparent_strings_on_the_wire() {
        let id = StopId::new("stop:ut-centraal");
        assert_eq!(id.as_str(), "stop:ut-centraal");

        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""stop:ut-centraal""#);
        assert_eq!(serde_json::from_str::<StopId>(&json).unwrap(), id);
        assert_eq!(String::from(id), "stop:ut-centraal");
    }

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

    #[test]
    fn geographic_models_use_longitude_as_x_and_latitude_as_y() {
        let stop = Stop {
            source_id: "ut".into(),
            code: None,
            name: "Utrecht Centraal".to_owned(),
            latitude: Some(52.0893),
            longitude: Some(5.1103),
            location_type: Some(0),
            parent_source_id: None,
            platform_code: None,
        };
        assert_eq!(stop.point(), Some(Point::new(5.1103, 52.0893)));

        let query = NearbyStopsQuery::from_point(Point::new(5.1103, 52.0893), 1_000.0, 20);
        assert_eq!(query.point(), Point::new(5.1103, 52.0893));
        assert_eq!((query.lon, query.lat), (5.1103, 52.0893));
    }

    #[test]
    fn shape_points_convert_to_a_line_string() {
        let points = [
            ShapePoint {
                sequence: 1,
                latitude: 52.0,
                longitude: 5.0,
                distance_traveled: Some(0.0),
            },
            ShapePoint {
                sequence: 2,
                latitude: 53.0,
                longitude: 6.0,
                distance_traveled: Some(100.0),
            },
        ];

        let line = shape_line_string(&points);
        assert_eq!(line, LineString::from(vec![(5.0, 52.0), (6.0, 53.0)]));
    }
}
