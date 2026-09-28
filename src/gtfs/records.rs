use serde::Deserialize;

use super::{
    GtfsDate,
    date::deserialize_optional as deserialize_optional_date,
    time::{GtfsTime, deserialize_optional as deserialize_optional_time},
};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Agency {
    #[serde(default)]
    pub agency_id: String,
    pub agency_name: String,
    pub agency_url: String,
    pub agency_timezone: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Stop {
    pub stop_id: String,
    #[serde(default)]
    pub stop_code: String,
    #[serde(default)]
    pub stop_name: String,
    #[serde(default)]
    pub stop_lat: Option<f64>,
    #[serde(default)]
    pub stop_lon: Option<f64>,
    #[serde(default)]
    pub location_type: Option<u8>,
    #[serde(default)]
    pub parent_station: String,
    #[serde(default)]
    pub platform_code: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Route {
    pub route_id: String,
    #[serde(default)]
    pub agency_id: String,
    #[serde(default)]
    pub route_short_name: String,
    #[serde(default)]
    pub route_long_name: String,
    pub route_type: u16,
    #[serde(default)]
    pub route_color: String,
    #[serde(default)]
    pub route_text_color: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Trip {
    pub route_id: String,
    pub service_id: String,
    pub trip_id: String,
    #[serde(default)]
    pub trip_headsign: String,
    #[serde(default)]
    pub trip_short_name: String,
    #[serde(default)]
    pub direction_id: Option<u8>,
    #[serde(default)]
    pub shape_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct StopTime {
    pub trip_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_time")]
    pub arrival_time: Option<GtfsTime>,
    #[serde(default, deserialize_with = "deserialize_optional_time")]
    pub departure_time: Option<GtfsTime>,
    pub stop_id: String,
    pub stop_sequence: u32,
    #[serde(default)]
    pub stop_headsign: String,
    #[serde(default)]
    pub pickup_type: Option<u8>,
    #[serde(default)]
    pub drop_off_type: Option<u8>,
    #[serde(default)]
    pub timepoint: Option<u8>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ShapePoint {
    pub shape_id: String,
    pub shape_pt_lat: f64,
    pub shape_pt_lon: f64,
    pub shape_pt_sequence: u32,
    #[serde(default)]
    pub shape_dist_traveled: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Transfer {
    #[serde(default)]
    pub from_stop_id: String,
    #[serde(default)]
    pub to_stop_id: String,
    #[serde(default)]
    pub from_route_id: String,
    #[serde(default)]
    pub to_route_id: String,
    #[serde(default)]
    pub from_trip_id: String,
    #[serde(default)]
    pub to_trip_id: String,
    #[serde(default)]
    pub transfer_type: u8,
    #[serde(default)]
    pub min_transfer_time: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct FeedInfo {
    pub feed_publisher_name: String,
    pub feed_publisher_url: String,
    pub feed_lang: String,
    #[serde(default)]
    pub default_lang: String,
    #[serde(default, deserialize_with = "deserialize_optional_date")]
    pub feed_start_date: Option<GtfsDate>,
    #[serde(default, deserialize_with = "deserialize_optional_date")]
    pub feed_end_date: Option<GtfsDate>,
    #[serde(default)]
    pub feed_version: String,
    #[serde(default)]
    pub feed_contact_email: String,
    #[serde(default)]
    pub feed_contact_url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Calendar {
    pub service_id: String,
    pub monday: u8,
    pub tuesday: u8,
    pub wednesday: u8,
    pub thursday: u8,
    pub friday: u8,
    pub saturday: u8,
    pub sunday: u8,
    pub start_date: GtfsDate,
    pub end_date: GtfsDate,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CalendarDate {
    pub service_id: String,
    pub date: GtfsDate,
    pub exception_type: u8,
}
