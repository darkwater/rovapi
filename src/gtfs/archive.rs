use std::{
    collections::HashSet,
    fmt,
    io::{Read, Seek},
};

use serde::de::DeserializeOwned;
use zip::ZipArchive;

use super::{Agency, Calendar, CalendarDate, Route, ShapePoint, Stop, StopTime, Trip};

const REQUIRED_FILES: &[&str] = &[
    "agency.txt",
    "routes.txt",
    "stops.txt",
    "stop_times.txt",
    "trips.txt",
];

#[derive(Clone, Copy, Debug)]
pub struct ImportLimits {
    pub max_files: usize,
    pub max_uncompressed_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_files: 64,
            max_uncompressed_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

pub struct GtfsArchive<R> {
    archive: ZipArchive<R>,
    files: HashSet<String>,
}

#[derive(Debug)]
pub enum GtfsError {
    Csv(csv::Error),
    DuplicateFile(String),
    InvalidPath(String),
    Io(std::io::Error),
    LimitExceeded(&'static str),
    MissingFile(&'static str),
    Zip(zip::result::ZipError),
}

impl<R: Read + Seek> GtfsArchive<R> {
    pub fn open(reader: R, limits: ImportLimits) -> Result<Self, GtfsError> {
        let mut archive = ZipArchive::new(reader)?;
        let files = validate_archive(&mut archive, limits)?;
        Ok(Self { archive, files })
    }

    pub fn visit_agencies<E>(
        &mut self,
        visitor: impl FnMut(Agency) -> Result<(), E>,
    ) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_file("agency.txt", visitor)
    }

    pub fn visit_stops<E>(&mut self, visitor: impl FnMut(Stop) -> Result<(), E>) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_file("stops.txt", visitor)
    }

    pub fn visit_routes<E>(&mut self, visitor: impl FnMut(Route) -> Result<(), E>) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_file("routes.txt", visitor)
    }

    pub fn visit_trips<E>(&mut self, visitor: impl FnMut(Trip) -> Result<(), E>) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_file("trips.txt", visitor)
    }

    pub fn visit_stop_times<E>(
        &mut self,
        visitor: impl FnMut(StopTime) -> Result<(), E>,
    ) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_file("stop_times.txt", visitor)
    }

    pub fn visit_calendars<E>(
        &mut self,
        visitor: impl FnMut(Calendar) -> Result<(), E>,
    ) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_optional_file("calendar.txt", visitor)
    }

    pub fn visit_calendar_dates<E>(
        &mut self,
        visitor: impl FnMut(CalendarDate) -> Result<(), E>,
    ) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_optional_file("calendar_dates.txt", visitor)
    }

    pub fn visit_shape_points<E>(
        &mut self,
        visitor: impl FnMut(ShapePoint) -> Result<(), E>,
    ) -> Result<u64, E>
    where
        E: From<GtfsError>,
    {
        self.visit_optional_file("shapes.txt", visitor)
    }

    fn visit_optional_file<T: DeserializeOwned, E: From<GtfsError>>(
        &mut self,
        name: &'static str,
        visitor: impl FnMut(T) -> Result<(), E>,
    ) -> Result<u64, E> {
        if self.files.contains(name) {
            self.visit_file(name, visitor)
        } else {
            Ok(0)
        }
    }

    fn visit_file<T: DeserializeOwned, E: From<GtfsError>>(
        &mut self,
        name: &'static str,
        mut visitor: impl FnMut(T) -> Result<(), E>,
    ) -> Result<u64, E> {
        let file = self
            .archive
            .by_name(name)
            .map_err(GtfsError::from)
            .map_err(E::from)?;
        let mut csv = csv::ReaderBuilder::new().flexible(true).from_reader(file);
        let mut count = 0;
        for row in csv.deserialize() {
            let record = row.map_err(GtfsError::from).map_err(E::from)?;
            visitor(record)?;
            count += 1;
        }
        Ok(count)
    }
}

fn validate_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: ImportLimits,
) -> Result<HashSet<String>, GtfsError> {
    if archive.len() > limits.max_files {
        return Err(GtfsError::LimitExceeded("file count"));
    }

    let mut names = HashSet::with_capacity(archive.len());
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let file = archive.by_index_raw(index)?;
        let name = file.name().to_owned();
        let enclosed = file
            .enclosed_name()
            .ok_or_else(|| GtfsError::InvalidPath(name.clone()))?;
        if enclosed.components().count() != 1 {
            return Err(GtfsError::InvalidPath(name));
        }
        if !names.insert(name.clone()) {
            return Err(GtfsError::DuplicateFile(name));
        }
        total_size = total_size
            .checked_add(file.size())
            .ok_or(GtfsError::LimitExceeded("uncompressed size"))?;
        if total_size > limits.max_uncompressed_bytes {
            return Err(GtfsError::LimitExceeded("uncompressed size"));
        }
    }

    for &required in REQUIRED_FILES {
        if !names.contains(required) {
            return Err(GtfsError::MissingFile(required));
        }
    }
    if !names.contains("calendar.txt") && !names.contains("calendar_dates.txt") {
        return Err(GtfsError::MissingFile("calendar.txt or calendar_dates.txt"));
    }
    Ok(names)
}

impl fmt::Display for GtfsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Csv(error) => write!(formatter, "invalid GTFS CSV: {error}"),
            Self::DuplicateFile(name) => write!(formatter, "duplicate GTFS file {name:?}"),
            Self::InvalidPath(path) => write!(formatter, "invalid GTFS archive path {path:?}"),
            Self::Io(error) => write!(formatter, "GTFS I/O error: {error}"),
            Self::LimitExceeded(limit) => write!(formatter, "GTFS archive exceeds {limit} limit"),
            Self::MissingFile(name) => write!(formatter, "missing required GTFS file {name}"),
            Self::Zip(error) => write!(formatter, "invalid GTFS ZIP: {error}"),
        }
    }
}

impl std::error::Error for GtfsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Csv(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Zip(error) => Some(error),
            _ => None,
        }
    }
}

impl From<csv::Error> for GtfsError {
    fn from(error: csv::Error) -> Self {
        Self::Csv(error)
    }
}

impl From<std::io::Error> for GtfsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<zip::result::ZipError> for GtfsError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    const MINIMAL_FILES: &[(&str, &str)] = &[
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
            "route_id,service_id,trip_id,trip_headsign\nroute-1,weekday,trip-1,Centraal\n",
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

    fn archive(files: &[(&str, &str)]) -> Cursor<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut output);
            for (name, contents) in files {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(contents.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        output.set_position(0);
        output
    }

    #[test]
    fn streams_typed_records_and_preserves_service_day_time() {
        let mut gtfs = GtfsArchive::open(archive(MINIMAL_FILES), ImportLimits::default()).unwrap();
        let mut records = Vec::new();

        let count = gtfs
            .visit_stop_times(|record| {
                records.push(record);
                Ok::<_, GtfsError>(())
            })
            .unwrap();

        assert_eq!(count, 1);
        assert_eq!(records[0].trip_id, "trip-1");
        assert_eq!(records[0].arrival_time.unwrap().day_offset(), 1);
    }

    #[test]
    fn rejects_a_feed_without_a_required_file() {
        let files: Vec<_> = MINIMAL_FILES
            .iter()
            .copied()
            .filter(|(name, _)| *name != "stops.txt")
            .collect();

        let result = GtfsArchive::open(archive(&files), ImportLimits::default());
        assert!(matches!(result, Err(GtfsError::MissingFile("stops.txt"))));
    }

    #[test]
    fn rejects_nested_paths() {
        let mut files = MINIMAL_FILES.to_vec();
        files[0].0 = "feed/agency.txt";

        let result = GtfsArchive::open(archive(&files), ImportLimits::default());
        assert!(matches!(result, Err(GtfsError::InvalidPath(_))));
    }

    #[test]
    fn enforces_uncompressed_size_limit_before_parsing() {
        let result = GtfsArchive::open(
            archive(MINIMAL_FILES),
            ImportLimits {
                max_uncompressed_bytes: 10,
                ..ImportLimits::default()
            },
        );
        assert!(matches!(
            result,
            Err(GtfsError::LimitExceeded("uncompressed size"))
        ));
    }
}
