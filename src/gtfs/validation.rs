use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};

use super::{GtfsArchive, GtfsError};

const MAX_REPORTED_ISSUES: usize = 100;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedCounts {
    pub agencies: u64,
    pub stops: u64,
    pub routes: u64,
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
    let mut agency_ids = HashSet::new();
    report.counts.agencies = archive.visit_agencies(|agency| {
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
    report.counts.stops = archive.visit_stops(|stop| {
        if !stop_ids.insert(stop.stop_id.clone()) {
            report.issue(
                "duplicate_stop",
                stop.stop_id.clone(),
                "stop_id occurs more than once".to_owned(),
            );
        }
        match (stop.stop_lat, stop.stop_lon) {
            (Some(latitude), Some(longitude))
                if !(-90.0..=90.0).contains(&latitude)
                    || !(-180.0..=180.0).contains(&longitude) =>
            {
                report.issue(
                    "invalid_coordinates",
                    stop.stop_id,
                    format!("coordinates are outside WGS84 bounds: {latitude}, {longitude}"),
                );
            }
            (Some(_), None) | (None, Some(_)) => report.issue(
                "incomplete_coordinates",
                stop.stop_id,
                "stop_lat and stop_lon must be supplied together".to_owned(),
            ),
            _ => {}
        }
        Ok::<_, GtfsError>(())
    })?;

    let mut route_ids = HashSet::new();
    report.counts.routes = archive.visit_routes(|route| {
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

    let mut trip_ids = HashSet::new();
    report.counts.trips = archive.visit_trips(|trip| {
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
                trip.trip_id,
                format!("route_id {:?} does not exist", trip.route_id),
            );
        }
        Ok::<_, GtfsError>(())
    })?;

    let mut last_sequence_by_trip = HashMap::new();
    report.counts.stop_times = archive.visit_stop_times(|stop_time| {
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
                "route_id,service_id,trip_id\nroute-1,weekday,trip-1\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\ntrip-1,25:10:00,25:11:00,stop-1,1\n",
            ),
            (
                "calendar.txt",
                "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekday,1,1,1,1,1,0,0,20260901,20260930\n",
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
        assert_eq!(report.counts.stop_times, 1);
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
