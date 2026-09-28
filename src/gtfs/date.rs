use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GtfsDate {
    year: u16,
    month: u8,
    day: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Weekday {
    Sunday = 0,
    Monday = 1,
    Tuesday = 2,
    Wednesday = 3,
    Thursday = 4,
    Friday = 5,
    Saturday = 6,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseGtfsDateError {
    value: String,
}

impl GtfsDate {
    pub fn parse_iso(value: &str) -> Result<Self, ParseGtfsDateError> {
        if value.len() != 10 || &value[4..5] != "-" || &value[7..8] != "-" {
            return Err(ParseGtfsDateError {
                value: value.to_owned(),
            });
        }
        Self::from_parts(&value[0..4], &value[5..7], &value[8..10], value)
    }

    fn from_parts(
        year: &str,
        month: &str,
        day: &str,
        original: &str,
    ) -> Result<Self, ParseGtfsDateError> {
        let invalid = || ParseGtfsDateError {
            value: original.to_owned(),
        };
        let year: u16 = year.parse().map_err(|_| invalid())?;
        let month: u8 = month.parse().map_err(|_| invalid())?;
        let day: u8 = day.parse().map_err(|_| invalid())?;
        let max_day = days_in_month(year, month).ok_or_else(invalid)?;
        if year < 1000 || day == 0 || day > max_day {
            return Err(invalid());
        }
        Ok(Self { year, month, day })
    }

    pub const fn compact(self) -> u32 {
        self.year as u32 * 10_000 + self.month as u32 * 100 + self.day as u32
    }

    pub fn weekday(self) -> Weekday {
        let mut year = i64::from(self.year);
        let month = i64::from(self.month);
        let day = i64::from(self.day);
        year -= i64::from(month <= 2);
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let adjusted_month = month + if month > 2 { -3 } else { 9 };
        let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days_since_epoch = era * 146_097 + day_of_era - 719_468;
        match (days_since_epoch + 4).rem_euclid(7) {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            6 => Weekday::Saturday,
            _ => unreachable!(),
        }
    }
}

const fn days_in_month(year: u16, month: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            Some(29)
        }
        2 => Some(28),
        _ => None,
    }
}

impl FromStr for GtfsDate {
    type Err = ParseGtfsDateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ParseGtfsDateError {
                value: value.to_owned(),
            });
        }
        Self::from_parts(&value[0..4], &value[4..6], &value[6..8], value)
    }
}

impl fmt::Display for GtfsDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

impl fmt::Display for ParseGtfsDateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid GTFS date {:?}", self.value)
    }
}

impl std::error::Error for ParseGtfsDateError {}

impl<'de> Deserialize<'de> for GtfsDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for GtfsDate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_leap_days_and_formats_iso() {
        let date: GtfsDate = "20240229".parse().unwrap();
        assert_eq!(date.to_string(), "2024-02-29");
        assert_eq!(date.compact(), 20_240_229);
        assert!("20230229".parse::<GtfsDate>().is_err());
    }

    #[test]
    fn calculates_weekdays_without_timezone_assumptions() {
        assert_eq!(
            GtfsDate::parse_iso("2026-09-28").unwrap().weekday(),
            Weekday::Monday
        );
        assert_eq!(
            GtfsDate::parse_iso("1970-01-01").unwrap().weekday(),
            Weekday::Thursday
        );
    }
}
