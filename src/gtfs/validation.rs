use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};

use super::{GtfsArchive, GtfsError};

const MAX_REPORTED_ISSUES: usize = 100;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedCounts {
    pub feed_info: u64,
    pub agencies: u64,
    pub stops: u64,
    pub routes: u64,
    pub calendars: u64,
    pub calendar_dates: u64,
    pub shape_points: u64,
    pub transfers: u64,
    pub trips: u64,
    pub stop_times: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationIssue {
    pub code: &'static str,
    pub record: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValidationReport {
    pub counts: FeedCounts,
    pub issues: Vec<ValidationIssue>,
    pub omitted_issue_count: u64,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.issues.is_empty() && self.omitted_issue_count == 0
    }

    fn issue(&mut self, code: &'static str, record: String, message: String) {
        if self.issues.len() < MAX_REPORTED_ISSUES {
            self.issues.push(ValidationIssue {
                code,
                record,
                message,
            });
        } else {
            self.omitted_issue_count += 1;
        }
    }
}

/// Validates identity and reference invariants across the core GTFS files.
///
/// This intentionally runs before persistence. It keeps identifiers and the
/// latest stop sequence per trip in memory, but streams the potentially very
/// large stop-times table one row at a time.
pub fn validate<R: Read + Seek>(
    archive: &mut GtfsArchive<R>,
) -> Result<ValidationReport, GtfsError> {
    let mut report = ValidationReport::default();
    report.counts.feed_info = archive.visit_feed_info(|feed| {
        if feed.feed_publisher_name.is_empty()
            || feed.feed_publisher_url.is_empty()
            || feed.feed_lang.is_empty()
        {
            report.issue(
                "incomplete_feed_info",
                "feed_info.txt".to_owned(),
                "feed_publisher_name, feed_publisher_url, and feed_lang are required".to_owned(),
            );
        }
        if feed
            .feed_start_date
            .zip(feed.feed_end_date)
            .is_some_and(|(start, end)| start > end)
        {
            report.issue(
                "invalid_feed_date_range",
                "feed_info.txt".to_owned(),
                "feed_start_date must not be after feed_end_date".to_owned(),
            );
        }
        Ok::<_, GtfsError>(())
    })?;
    if report.counts.feed_info > 1 {
        report.issue(
            "multiple_feed_info_records",
            "feed_info.txt".to_owned(),
            "feed_info.txt must contain exactly one record".to_owned(),
        );
    }

    let mut agency_ids = HashSet::new();
    report.counts.agencies = archive.visit_agencies(|agency| {
        if agency.agency_name.is_empty()
            || agency.agency_url.is_empty()
            || agency.agency_timezone.is_empty()
        {
            report.issue(
                "incomplete_agency",
                agency.agency_id.clone(),
                "agency_name, agency_url, and agency_timezone are required".to_owned(),
            );
        }
        if !agency_ids.insert(agency.agency_id.clone()) {
            report.issue(
                "duplicate_agency",
                agency.agency_id,
                "agency_id occurs more than once".to_owned(),
            );
        }
        Ok::<_, GtfsError>(())
    })?;
    if agency_ids.is_empty() {
        report.issue(
            "missing_agency",
            "agency.txt".to_owned(),
            "at least one agency is required".to_owned(),
        );
    } else if agency_ids.len() > 1 && agency_ids.contains("") {
        report.issue(
            "missing_agency_id",
            "agency.txt".to_owned(),
            "agency_id is required when the feed contains multiple agencies".to_owned(),
        );
    }

    let mut stop_ids = HashSet::new();
    let mut stop_hierarchy = HashMap::new();
    report.counts.stops = archive.visit_stops(|stop| {
        let stop_id = stop.stop_id.clone();
        if stop_id.is_empty() {
            report.issue(
                "missing_stop_id",
                "stops.txt".to_owned(),
                "stop_id is required".to_owned(),
            );
        }
        if !stop_ids.insert(stop_id.clone()) {
            report.issue(
                "duplicate_stop",
                stop_id.clone(),
                "stop_id occurs more than once".to_owned(),
            );
        }
        stop_hierarchy
            .entry(stop_id.clone())
            .or_insert_with(|| (stop.location_type.unwrap_or(0), stop.parent_station.clone()));
        match (stop.stop_lat, stop.stop_lon) {
            (Some(latitude), Some(longitude))
                if !(-90.0..=90.0).contains(&latitude)
                    || !(-180.0..=180.0).contains(&longitude) =>
            {
                report.issue(
                    "invalid_coordinates",
                    stop_id,
                    format!("coordinates are outside WGS84 bounds: {latitude}, {longitude}"),
                );
            }
            (Some(_), None) | (None, Some(_)) => report.issue(
                "incomplete_coordinates",
                stop_id,
                "stop_lat and stop_lon must be supplied together".to_owned(),
            ),
            _ => {}
        }
        Ok::<_, GtfsError>(())
    })?;

    for (stop_id, (location_type, parent_id)) in &stop_hierarchy {
        if *location_type > 4 {
            report.issue(
                "invalid_location_type",
                stop_id.clone(),
                format!("location_type {location_type} is not supported by GTFS"),
            );
            continue;
        }

        let required_parent_type = match location_type {
            0 => None,
            1 => {
                if !parent_id.is_empty() {
                    report.issue(
                        "unexpected_parent_station",
                        stop_id.clone(),
                        "a station must not have a parent_station".to_owned(),
                    );
                }
                continue;
            }
            2 | 3 => Some(1),
            4 => Some(0),
            _ => unreachable!(),
        };

        if parent_id.is_empty() {
            if required_parent_type.is_some() {
                report.issue(
                    "missing_parent_station",
                    stop_id.clone(),
                    "this location_type requires parent_station".to_owned(),
                );
            }
            continue;
        }
        if parent_id == stop_id {
            report.issue(
                "self_parent_station",
                stop_id.clone(),
                "a stop must not be its own parent_station".to_owned(),
            );
            continue;
        }
        let Some((parent_type, _)) = stop_hierarchy.get(parent_id) else {
            report.issue(
                "unknown_parent_station",
                stop_id.clone(),
                format!("parent_station {parent_id:?} does not exist"),
            );
            continue;
        };
        let expected_parent_type = required_parent_type.unwrap_or(1);
        if *parent_type != expected_parent_type {
            report.issue(
                "invalid_parent_location_type",
                stop_id.clone(),
                format!(
                    "parent_station {parent_id:?} has location_type {parent_type}; expected {expected_parent_type}"
                ),
            );
        }
    }

    let mut route_ids = HashSet::new();
    report.counts.routes = archive.visit_routes(|route| {
        if route.route_id.is_empty() {
            report.issue(
                "missing_route_id",
                "routes.txt".to_owned(),
                "route_id is required".to_owned(),
            );
        }
        if route.route_short_name.is_empty() && route.route_long_name.is_empty() {
            report.issue(
                "missing_route_name",
                route.route_id.clone(),
                "route_short_name or route_long_name is required".to_owned(),
            );
        }
        if !route_ids.insert(route.route_id.clone()) {
            report.issue(
                "duplicate_route",
                route.route_id.clone(),
                "route_id occurs more than once".to_owned(),
            );
        }
        if route.agency_id.is_empty() && agency_ids.len() != 1 {
            report.issue(
                "missing_route_agency",
                route.route_id,
                "agency_id is required when the feed contains multiple agencies".to_owned(),
            );
        } else if !route.agency_id.is_empty() && !agency_ids.contains(&route.agency_id) {
            report.issue(
                "unknown_agency",
                route.route_id,
                format!("agency_id {:?} does not exist", route.agency_id),
            );
        }
        Ok::<_, GtfsError>(())
    })?;

    let mut service_ids = HashSet::new();
    report.counts.calendars = archive.visit_calendars(|calendar| {
        if !service_ids.insert(calendar.service_id.clone()) {
            report.issue(
                "duplicate_calendar",
                calendar.service_id.clone(),
                "service_id occurs more than once in calendar.txt".to_owned(),
            );
        }
        if [
            calendar.monday,
            calendar.tuesday,
            calendar.wednesday,
            calendar.thursday,
            calendar.friday,
            calendar.saturday,
            calendar.sunday,
        ]
        .iter()
        .any(|&value| value > 1)
        {
            report.issue(
                "invalid_calendar_weekday",
                calendar.service_id.clone(),
                "weekday flags must be 0 or 1".to_owned(),
            );
        }
        if calendar.start_date > calendar.end_date {
            report.issue(
                "invalid_calendar_range",
                calendar.service_id,
                "start_date must not be after end_date".to_owned(),
            );
        }
        Ok::<_, GtfsError>(())
    })?;

    let mut calendar_date_keys = HashSet::new();
    report.counts.calendar_dates = archive.visit_calendar_dates(|exception| {
        service_ids.insert(exception.service_id.clone());
        if !calendar_date_keys.insert((exception.service_id.clone(), exception.date)) {
            report.issue(
                "duplicate_calendar_date",
                format!("{}:{}", exception.service_id, exception.date),
                "service_id and date occur more than once in calendar_dates.txt".to_owned(),
            );
        }
        if !matches!(exception.exception_type, 1 | 2) {
            report.issue(
                "invalid_exception_type",
                format!("{}:{}", exception.service_id, exception.date),
                "exception_type must be 1 (added) or 2 (removed)".to_owned(),
            );
        }
        Ok::<_, GtfsError>(())
    })?;

    let mut shape_ids = HashSet::new();
    let mut last_shape_sequence = HashMap::new();
    report.counts.shape_points = archive.visit_shape_points(|point| {
        shape_ids.insert(point.shape_id.clone());
        if !(-90.0..=90.0).contains(&point.shape_pt_lat)
            || !(-180.0..=180.0).contains(&point.shape_pt_lon)
        {
            report.issue(
                "invalid_shape_coordinates",
                format!("{}:{}", point.shape_id, point.shape_pt_sequence),
                format!(
                    "coordinates are outside WGS84 bounds: {}, {}",
                    point.shape_pt_lat, point.shape_pt_lon
                ),
            );
        }
        if let Some(previous) =
            last_shape_sequence.insert(point.shape_id.clone(), point.shape_pt_sequence)
            && point.shape_pt_sequence <= previous
        {
            report.issue(
                "non_increasing_shape_sequence",
                format!("{}:{}", point.shape_id, point.shape_pt_sequence),
                format!("shape_pt_sequence must be greater than {previous}"),
            );
        }
        if point
            .shape_dist_traveled
            .is_some_and(|distance| !distance.is_finite() || distance < 0.0)
        {
            report.issue(
                "invalid_shape_distance",
                format!("{}:{}", point.shape_id, point.shape_pt_sequence),
                "shape_dist_traveled must be finite and non-negative".to_owned(),
            );
        }
        Ok::<_, GtfsError>(())
    })?;

    let mut trip_ids = HashSet::new();
    let mut trip_routes = HashMap::new();
    report.counts.trips = archive.visit_trips(|trip| {
        if trip.trip_id.is_empty() || trip.route_id.is_empty() || trip.service_id.is_empty() {
            report.issue(
                "incomplete_trip",
                trip.trip_id.clone(),
                "trip_id, route_id, and service_id are required".to_owned(),
            );
        }
        if trip.direction_id.is_some_and(|value| value > 1) {
            report.issue(
                "invalid_direction_id",
                trip.trip_id.clone(),
                "direction_id must be 0 or 1".to_owned(),
            );
        }
        if !trip_ids.insert(trip.trip_id.clone()) {
            report.issue(
                "duplicate_trip",
                trip.trip_id.clone(),
                "trip_id occurs more than once".to_owned(),
            );
        }
        if !route_ids.contains(&trip.route_id) {
            report.issue(
                "unknown_route",
                trip.trip_id.clone(),
                format!("route_id {:?} does not exist", trip.route_id),
            );
        }
        if !service_ids.contains(&trip.service_id) {
            report.issue(
                "unknown_service",
                trip.trip_id.clone(),
                format!("service_id {:?} does not exist", trip.service_id),
            );
        }
        if !trip.shape_id.is_empty() && !shape_ids.contains(&trip.shape_id) {
            report.issue(
                "unknown_shape",
                trip.trip_id.clone(),
                format!("shape_id {:?} does not exist", trip.shape_id),
            );
        }
        trip_routes.entry(trip.trip_id).or_insert(trip.route_id);
        Ok::<_, GtfsError>(())
    })?;

    let mut transfer_keys = HashSet::new();
    report.counts.transfers = archive.visit_transfers(|transfer| {
        let record = format!(
            "{}:{}:{}:{}:{}:{}",
            transfer.from_stop_id,
            transfer.to_stop_id,
            transfer.from_trip_id,
            transfer.to_trip_id,
            transfer.from_route_id,
            transfer.to_route_id
        );
        let key = (
            transfer.from_stop_id.clone(),
            transfer.to_stop_id.clone(),
            transfer.from_trip_id.clone(),
            transfer.to_trip_id.clone(),
            transfer.from_route_id.clone(),
            transfer.to_route_id.clone(),
        );
        if !transfer_keys.insert(key) {
            report.issue(
                "duplicate_transfer",
                record.clone(),
                "transfer primary key occurs more than once".to_owned(),
            );
        }
        if transfer.transfer_type > 5 {
            report.issue(
                "invalid_transfer_type",
                record.clone(),
                "transfer_type must be between 0 and 5".to_owned(),
            );
        } else if transfer.transfer_type <= 3 {
            if transfer.from_stop_id.is_empty() || transfer.to_stop_id.is_empty() {
                report.issue(
                    "missing_transfer_stop",
                    record.clone(),
                    "from_stop_id and to_stop_id are required for transfer types 0 through 3"
                        .to_owned(),
                );
            }
        } else if transfer.from_trip_id.is_empty() || transfer.to_trip_id.is_empty() {
            report.issue(
                "missing_linked_trip",
                record.clone(),
                "from_trip_id and to_trip_id are required for transfer types 4 and 5".to_owned(),
            );
        }
        if transfer.transfer_type == 2 && transfer.min_transfer_time.is_none() {
            report.issue(
                "missing_transfer_time",
                record.clone(),
                "min_transfer_time is required for transfer_type 2".to_owned(),
            );
        }

        for (direction, stop_id) in [
            ("from", transfer.from_stop_id.as_str()),
            ("to", transfer.to_stop_id.as_str()),
        ] {
            if stop_id.is_empty() {
                continue;
            }
            let Some((location_type, _)) = stop_hierarchy.get(stop_id) else {
                report.issue(
                    "unknown_transfer_stop",
                    record.clone(),
                    format!("{direction}_stop_id {stop_id:?} does not exist"),
                );
                continue;
            };
            let valid_type = if matches!(transfer.transfer_type, 4 | 5) {
                *location_type == 0
            } else {
                matches!(location_type, 0 | 1)
            };
            if !valid_type {
                report.issue(
                    "invalid_transfer_stop_type",
                    record.clone(),
                    format!(
                        "{direction}_stop_id {stop_id:?} has invalid location_type {location_type}"
                    ),
                );
            }
        }

        for (direction, route_id) in [
            ("from", transfer.from_route_id.as_str()),
            ("to", transfer.to_route_id.as_str()),
        ] {
            if !route_id.is_empty() && !route_ids.contains(route_id) {
                report.issue(
                    "unknown_transfer_route",
                    record.clone(),
                    format!("{direction}_route_id {route_id:?} does not exist"),
                );
            }
        }
        for (direction, trip_id, route_id) in [
            (
                "from",
                transfer.from_trip_id.as_str(),
                transfer.from_route_id.as_str(),
            ),
            (
                "to",
                transfer.to_trip_id.as_str(),
                transfer.to_route_id.as_str(),
            ),
        ] {
            if trip_id.is_empty() {
                continue;
            }
            let Some(actual_route_id) = trip_routes.get(trip_id) else {
                report.issue(
                    "unknown_transfer_trip",
                    record.clone(),
                    format!("{direction}_trip_id {trip_id:?} does not exist"),
                );
                continue;
            };
            if !route_id.is_empty() && route_id != actual_route_id {
                report.issue(
                    "transfer_trip_route_mismatch",
                    record.clone(),
                    format!(
                        "{direction}_trip_id {trip_id:?} belongs to route {actual_route_id:?}, not {route_id:?}"
                    ),
                );
            }
        }
        Ok::<_, GtfsError>(())
    })?;

    let mut last_sequence_by_trip = HashMap::new();
    report.counts.stop_times = archive.visit_stop_times(|stop_time| {
        let record = format!("{}:{}", stop_time.trip_id, stop_time.stop_sequence);
        if stop_time.trip_id.is_empty() || stop_time.stop_id.is_empty() {
            report.issue(
                "incomplete_stop_time",
                record.clone(),
                "trip_id and stop_id are required".to_owned(),
            );
        }
        for (field, value, maximum) in [
            ("pickup_type", stop_time.pickup_type, 3),
            ("drop_off_type", stop_time.drop_off_type, 3),
            ("timepoint", stop_time.timepoint, 1),
        ] {
            if value.is_some_and(|value| value > maximum) {
                report.issue(
                    "invalid_stop_time_enum",
                    record.clone(),
                    format!("{field} must be between 0 and {maximum}"),
                );
            }
        }
        if !trip_ids.contains(&stop_time.trip_id) {
            report.issue(
                "unknown_trip",
                format!("{}:{}", stop_time.trip_id, stop_time.stop_sequence),
                "trip_id does not exist".to_owned(),
            );
        }
        if !stop_ids.contains(&stop_time.stop_id) {
            report.issue(
                "unknown_stop",
                format!("{}:{}", stop_time.trip_id, stop_time.stop_sequence),
                format!("stop_id {:?} does not exist", stop_time.stop_id),
            );
        }
        if let Some(previous) =
            last_sequence_by_trip.insert(stop_time.trip_id.clone(), stop_time.stop_sequence)
            && stop_time.stop_sequence <= previous
        {
            report.issue(
                "non_increasing_stop_sequence",
                format!("{}:{}", stop_time.trip_id, stop_time.stop_sequence),
                format!("stop_sequence must be greater than {previous}"),
            );
        }
        Ok::<_, GtfsError>(())
    })?;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::gtfs::ImportLimits;

    fn archive(overrides: &[(&str, &str)]) -> GtfsArchive<Cursor<Vec<u8>>> {
        let defaults = [
            (
                "feed_info.txt",
                "feed_publisher_name,feed_publisher_url,feed_lang,feed_start_date,feed_end_date,feed_version\nExample Publisher,https://example.nl,nl,20260901,20260930,2026-09\n",
            ),
            (
                "agency.txt",
                "agency_id,agency_name,agency_url,agency_timezone\nNL,Example,https://example.nl,Europe/Amsterdam\n",
            ),
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon\nstop-1,Centraal,52.0907,5.1214\n",
            ),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_type\nroute-1,NL,8,3\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,shape_id\nroute-1,weekday,trip-1,shape-1\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\ntrip-1,25:10:00,25:11:00,stop-1,1\n",
            ),
            (
                "calendar.txt",
                "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekday,1,1,1,1,1,0,0,20260901,20260930\n",
            ),
            ("calendar_dates.txt", "service_id,date,exception_type\n"),
            (
                "shapes.txt",
                "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence,shape_dist_traveled\nshape-1,52.0907,5.1214,1,0\nshape-1,52.1000,5.1300,2,1200.5\n",
            ),
            (
                "transfers.txt",
                "from_stop_id,to_stop_id,from_route_id,to_route_id,from_trip_id,to_trip_id,transfer_type,min_transfer_time\n",
            ),
        ];
        let mut output = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut output);
            for (name, default_contents) in defaults {
                let contents = overrides
                    .iter()
                    .find_map(|(override_name, contents)| {
                        (*override_name == name).then_some(*contents)
                    })
                    .unwrap_or(default_contents);
                zip.start_file(name, SimpleFileOptions::default()).unwrap();
                zip.write_all(contents.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        output.set_position(0);
        GtfsArchive::open(output, ImportLimits::default()).unwrap()
    }

    #[test]
    fn accepts_a_consistent_feed() {
        let report = validate(&mut archive(&[])).unwrap();
        assert!(report.is_valid());
        assert_eq!(report.counts.feed_info, 1);
        assert_eq!(report.counts.stop_times, 1);
        assert_eq!(report.counts.calendars, 1);
        assert_eq!(report.counts.shape_points, 2);
    }

    #[test]
    fn validates_calendars_and_service_references() {
        let mut feed = archive(&[
            (
                "calendar.txt",
                "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekday,2,1,1,1,1,0,0,20261001,20260901\n",
            ),
            (
                "calendar_dates.txt",
                "service_id,date,exception_type\nweekday,20260928,3\nweekday,20260928,1\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id\nroute-1,missing,trip-1\n",
            ),
        ]);

        let report = validate(&mut feed).unwrap();
        let codes: HashSet<_> = report.issues.iter().map(|issue| issue.code).collect();
        assert!(codes.contains("invalid_calendar_weekday"));
        assert!(codes.contains("invalid_calendar_range"));
        assert!(codes.contains("invalid_exception_type"));
        assert!(codes.contains("duplicate_calendar_date"));
        assert!(codes.contains("unknown_service"));
    }

    #[test]
    fn validates_feed_information() {
        let mut feed = archive(&[(
            "feed_info.txt",
            "feed_publisher_name,feed_publisher_url,feed_lang,feed_start_date,feed_end_date\n,https://example.nl,nl,20261001,20260901\nSecond,https://second.example,nl,,\n",
        )]);

        let report = validate(&mut feed).unwrap();
        let codes: HashSet<_> = report.issues.iter().map(|issue| issue.code).collect();
        assert!(codes.contains("incomplete_feed_info"));
        assert!(codes.contains("invalid_feed_date_range"));
        assert!(codes.contains("multiple_feed_info_records"));
    }

    #[test]
    fn validates_shapes_and_trip_references() {
        let mut feed = archive(&[
            (
                "shapes.txt",
                "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence,shape_dist_traveled\nshape-1,91.0,5.1,1,-1\nshape-1,52.1,5.2,1,10\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,shape_id\nroute-1,weekday,trip-1,missing-shape\n",
            ),
        ]);

        let report = validate(&mut feed).unwrap();
        let codes: HashSet<_> = report.issues.iter().map(|issue| issue.code).collect();
        assert!(codes.contains("invalid_shape_coordinates"));
        assert!(codes.contains("invalid_shape_distance"));
        assert!(codes.contains("non_increasing_shape_sequence"));
        assert!(codes.contains("unknown_shape"));
    }

    #[test]
    fn reports_cross_file_and_coordinate_problems() {
        let mut feed = archive(&[
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon\nstop-1,Centraal,152.0,5.1\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id\nmissing-route,weekday,trip-1\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\ntrip-1,10:00:00,10:00:00,missing-stop,1\ntrip-1,10:01:00,10:01:00,stop-1,1\n",
            ),
        ]);

        let report = validate(&mut feed).unwrap();
        let codes: HashSet<_> = report.issues.iter().map(|issue| issue.code).collect();
        assert!(codes.contains("invalid_coordinates"));
        assert!(codes.contains("unknown_route"));
        assert!(codes.contains("unknown_stop"));
        assert!(codes.contains("non_increasing_stop_sequence"));
    }

    #[test]
    fn validates_required_fields_and_enumerations() {
        let mut feed = archive(&[
            (
                "agency.txt",
                "agency_id,agency_name,agency_url,agency_timezone\nNL,,,\n",
            ),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_long_name,route_type\n,NL,,,3\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,direction_id\nroute-1,weekday,trip-1,2\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type,timepoint\ntrip-1,10:00:00,10:00:00,stop-1,1,4,4,2\n",
            ),
        ]);

        let report = validate(&mut feed).unwrap();
        let codes: HashSet<_> = report.issues.iter().map(|issue| issue.code).collect();
        assert!(codes.contains("incomplete_agency"));
        assert!(codes.contains("missing_route_id"));
        assert!(codes.contains("missing_route_name"));
        assert!(codes.contains("invalid_direction_id"));
        assert!(codes.contains("invalid_stop_time_enum"));
    }

    #[test]
    fn validates_stop_parent_hierarchy() {
        let mut feed = archive(&[(
            "stops.txt",
            "stop_id,stop_name,location_type,parent_station\nstation,Station,1,\nplatform,Platform,0,station\nentrance,Entrance,2,\nboarding,Boarding,4,station\norphan,Orphan,0,missing\nchild-station,Child station,1,station\nself,Self,0,self\ninvalid,Invalid,9,\n",
        )]);

        let report = validate(&mut feed).unwrap();
        let codes: HashSet<_> = report.issues.iter().map(|issue| issue.code).collect();
        assert!(codes.contains("missing_parent_station"));
        assert!(codes.contains("invalid_parent_location_type"));
        assert!(codes.contains("unknown_parent_station"));
        assert!(codes.contains("unexpected_parent_station"));
        assert!(codes.contains("self_parent_station"));
        assert!(codes.contains("invalid_location_type"));
    }

    #[test]
    fn validates_transfer_rules_and_references() {
        let mut feed = archive(&[(
            "transfers.txt",
            "from_stop_id,to_stop_id,from_route_id,to_route_id,from_trip_id,to_trip_id,transfer_type,min_transfer_time\nstop-1,missing-stop,route-1,missing-route,trip-1,missing-trip,2,\n,,,,trip-1,,4,\nstop-1,stop-1,missing-route,,trip-1,,9,\n",
        )]);

        let report = validate(&mut feed).unwrap();
        let codes: HashSet<_> = report.issues.iter().map(|issue| issue.code).collect();
        assert!(codes.contains("missing_transfer_time"));
        assert!(codes.contains("unknown_transfer_stop"));
        assert!(codes.contains("unknown_transfer_route"));
        assert!(codes.contains("unknown_transfer_trip"));
        assert!(codes.contains("missing_linked_trip"));
        assert!(codes.contains("invalid_transfer_type"));
    }

    #[test]
    fn validates_single_and_multiple_agency_rules() {
        let mut unknown = archive(&[(
            "routes.txt",
            "route_id,agency_id,route_short_name,route_type\nroute-1,unknown,8,3\n",
        )]);
        let unknown_report = validate(&mut unknown).unwrap();
        assert!(
            unknown_report
                .issues
                .iter()
                .any(|issue| issue.code == "unknown_agency")
        );

        let mut multiple = archive(&[
            (
                "agency.txt",
                "agency_id,agency_name,agency_url,agency_timezone\nA,One,https://one.example,Europe/Amsterdam\nB,Two,https://two.example,Europe/Amsterdam\n",
            ),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_type\nroute-1,,8,3\n",
            ),
        ]);
        let multiple_report = validate(&mut multiple).unwrap();
        assert!(
            multiple_report
                .issues
                .iter()
                .any(|issue| issue.code == "missing_route_agency")
        );
    }
}
