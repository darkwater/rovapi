use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer};

/// A time relative to the start of a GTFS service day.
///
/// Unlike a wall-clock time, this can exceed 24 hours. For example, `25:10:00`
/// denotes 01:10 on the calendar day after the service date.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GtfsTime(u32);

impl GtfsTime {
    pub const fn seconds_since_service_day_start(self) -> u32 {
        self.0
    }

    pub const fn day_offset(self) -> u32 {
        self.0 / 86_400
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseGtfsTimeError {
    value: String,
}

impl FromStr for GtfsTime {
    type Err = ParseGtfsTimeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseGtfsTimeError {
            value: value.to_owned(),
        };
        let mut parts = value.split(':');
        let hours: u32 = parts
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let minutes: u32 = parts
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let seconds: u32 = parts
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;

        if parts.next().is_some() || hours > 99 || minutes > 59 || seconds > 59 {
            return Err(invalid());
        }

        Ok(Self(hours * 3_600 + minutes * 60 + seconds))
    }
}

impl fmt::Display for GtfsTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hours = self.0 / 3_600;
        let minutes = self.0 % 3_600 / 60;
        let seconds = self.0 % 60;
        write!(formatter, "{hours:02}:{minutes:02}:{seconds:02}")
    }
}

impl fmt::Display for ParseGtfsTimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid GTFS time {:?}", self.value)
    }
}

impl std::error::Error for ParseGtfsTimeError {}

impl<'de> Deserialize<'de> for GtfsTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

pub(crate) fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<GtfsTime>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        Ok(None)
    } else {
        value.parse().map(Some).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_time_after_midnight_without_wrapping() {
        let time: GtfsTime = "25:10:30".parse().unwrap();
        assert_eq!(time.seconds_since_service_day_start(), 90_630);
        assert_eq!(time.day_offset(), 1);
        assert_eq!(time.to_string(), "25:10:30");
    }

    #[test]
    fn rejects_wall_clock_components_out_of_range() {
        assert!("12:60:00".parse::<GtfsTime>().is_err());
        assert!("12:00:60".parse::<GtfsTime>().is_err());
        assert!("100:00:00".parse::<GtfsTime>().is_err());
        assert!("12:00".parse::<GtfsTime>().is_err());
    }
}
