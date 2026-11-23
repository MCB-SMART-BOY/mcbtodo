use chrono::{DateTime, Datelike, FixedOffset, LocalResult, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::{GapInfo, Tz};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DateParseError {
    #[error("expected a date in YYYY-MM-DD format")]
    Format,
    #[error("year must be between 0001 and 9999")]
    YearRange,
}

pub fn parse_date(value: &str) -> Result<NaiveDate, DateParseError> {
    if value.len() != 10
        || value.as_bytes().iter().enumerate().any(|(index, byte)| {
            if matches!(index, 4 | 7) {
                *byte != b'-'
            } else {
                !byte.is_ascii_digit()
            }
        })
    {
        return Err(DateParseError::Format);
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| DateParseError::Format)?;
    if !(1..=9999).contains(&date.year()) {
        return Err(DateParseError::YearRange);
    }
    Ok(date)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimeError {
    #[error("date range has invalid endpoints: {start} through {through}")]
    DateOrder {
        start: NaiveDate,
        through: NaiveDate,
    },
    #[error("time range must start before it ends")]
    TimeOrder,
    #[error("offset in {value} does not match {timezone} at that instant")]
    Offset {
        value: DateTime<FixedOffset>,
        timezone: Tz,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DateSpanFields")]
pub struct DateSpan {
    start: NaiveDate,
    through: NaiveDate,
}

#[derive(Deserialize)]
struct DateSpanFields {
    start: NaiveDate,
    through: NaiveDate,
}

impl TryFrom<DateSpanFields> for DateSpan {
    type Error = TimeError;

    fn try_from(value: DateSpanFields) -> Result<Self, Self::Error> {
        Self::new(value.start, value.through)
    }
}

impl DateSpan {
    pub fn new(start: NaiveDate, through: NaiveDate) -> Result<Self, TimeError> {
        if start > through || through.succ_opt().is_none() {
            return Err(TimeError::DateOrder { start, through });
        }
        Ok(Self { start, through })
    }

    pub fn start(self) -> NaiveDate {
        self.start
    }

    pub fn through(self) -> NaiveDate {
        self.through
    }

    pub fn contains(self, day: NaiveDate) -> bool {
        (self.start..self.through.succ_opt().expect("validated endpoint")).contains(&day)
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start <= other.through && other.start <= self.through
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DateTimeSpanFields")]
pub struct DateTimeSpan {
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    timezone: Tz,
}

#[derive(Deserialize)]
struct DateTimeSpanFields {
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    timezone: Tz,
}

impl TryFrom<DateTimeSpanFields> for DateTimeSpan {
    type Error = TimeError;

    fn try_from(value: DateTimeSpanFields) -> Result<Self, Self::Error> {
        Self::new(value.start, value.end, value.timezone)
    }
}

impl DateTimeSpan {
    pub fn new(
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        timezone: Tz,
    ) -> Result<Self, TimeError> {
        check_offset(start, timezone)?;
        check_offset(end, timezone)?;
        if start >= end {
            return Err(TimeError::TimeOrder);
        }
        Ok(Self {
            start,
            end,
            timezone,
        })
    }

    pub fn start(self) -> DateTime<FixedOffset> {
        self.start
    }

    pub fn end(self) -> DateTime<FixedOffset> {
        self.end
    }

    pub(crate) fn timezone(self) -> Tz {
        self.timezone
    }

    pub fn overlaps_day(self, day: NaiveDate, zone: Tz) -> bool {
        DateSpan::new(day, day).is_ok_and(|days| self.overlaps_dates(days, zone))
    }

    pub fn overlaps_dates(self, days: DateSpan, zone: Tz) -> bool {
        date_window(days, zone).is_some_and(|(start, end)| self.start < end && self.end > start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ZonedMomentFields")]
pub struct ZonedMoment {
    value: DateTime<FixedOffset>,
    timezone: Tz,
}

impl ZonedMoment {
    pub fn new(value: DateTime<FixedOffset>, timezone: Tz) -> Result<Self, TimeError> {
        check_offset(value, timezone)?;
        Ok(Self { value, timezone })
    }

    pub fn instant(self) -> DateTime<Utc> {
        self.value.to_utc()
    }

    pub fn fixed_offset(self) -> DateTime<FixedOffset> {
        self.value
    }
}

#[derive(Deserialize)]
struct ZonedMomentFields {
    value: DateTime<FixedOffset>,
    timezone: Tz,
}

impl TryFrom<ZonedMomentFields> for ZonedMoment {
    type Error = TimeError;

    fn try_from(fields: ZonedMomentFields) -> Result<Self, Self::Error> {
        Self::new(fields.value, fields.timezone)
    }
}

fn check_offset(value: DateTime<FixedOffset>, timezone: Tz) -> Result<(), TimeError> {
    if value.with_timezone(&timezone).offset().fix() != *value.offset() {
        return Err(TimeError::Offset { value, timezone });
    }
    Ok(())
}

pub(crate) fn date_window(days: DateSpan, zone: Tz) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = day_start(days.start(), zone)?;
    let end = day_start(days.through().succ_opt()?, zone)?;
    (start < end).then_some((start, end))
}

fn day_start(day: NaiveDate, timezone: Tz) -> Option<DateTime<Utc>> {
    let midnight = day.and_hms_opt(0, 0, 0)?;
    let instant = match timezone.from_local_datetime(&midnight) {
        LocalResult::Single(instant) => instant,
        LocalResult::Ambiguous(first, second) => first.min(second),
        LocalResult::None => GapInfo::new(&midnight, &timezone)?.end?,
    };
    Some(instant.to_utc())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Span {
    DateSpan(DateSpan),
    DateTimeSpan(DateTimeSpan),
}

impl Span {
    pub fn overlaps_day(self, day: NaiveDate, zone: Tz) -> bool {
        match self {
            Self::DateSpan(span) => span.contains(day),
            Self::DateTimeSpan(span) => span.overlaps_day(day, zone),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Deadline {
    Date { value: NaiveDate },
    DateTime(ZonedMoment),
}

impl Deadline {
    pub fn datetime(self, zone: Tz) -> Option<DateTime<Tz>> {
        match self {
            Self::Date { .. } => None,
            Self::DateTime(moment) => Some(moment.value.with_timezone(&zone)),
        }
    }

    pub fn on_day(self, day: NaiveDate, zone: Tz) -> bool {
        self.local_date(zone) == Some(day)
    }

    pub fn local_date(self, zone: Tz) -> Option<NaiveDate> {
        match self {
            Self::Date { value } => Some(value),
            Self::DateTime(moment) => {
                let local = moment.value.with_timezone(&zone);
                local
                    .naive_utc()
                    .checked_add_offset(local.offset().fix())
                    .map(|value| value.date())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }

    #[test]
    fn inclusive_dates_span_a_leap_day() {
        let span = DateSpan::new(date("2024-02-28"), date("2024-03-01")).unwrap();
        assert!(span.contains(date("2024-02-29")));
        assert!(span.contains(date("2024-03-01")));
        assert!(!span.contains(date("2024-03-02")));
        assert!(DateSpan::new(span.through(), span.start()).is_err());
    }

    #[test]
    fn date_parser_requires_canonical_calendar_dates_and_supported_years() {
        assert_eq!(parse_date("2024-02-29").unwrap(), date("2024-02-29"));
        assert_eq!(parse_date("0001-01-01").unwrap(), date("0001-01-01"));
        assert_eq!(parse_date("9999-12-31").unwrap(), date("9999-12-31"));
        assert_eq!(parse_date("0000-01-01"), Err(DateParseError::YearRange));
        assert_eq!(parse_date("2023-02-29"), Err(DateParseError::Format));
        assert_eq!(parse_date("2024-2-09"), Err(DateParseError::Format));
        assert_eq!(parse_date("+2024-02-09"), Err(DateParseError::Format));
    }

    #[test]
    fn midnight_endpoint_does_not_fill_the_next_day() {
        let zone = chrono_tz::Asia::Tokyo;
        let span = DateTimeSpan::new(
            "2026-11-25T22:00:00+09:00".parse().unwrap(),
            "2026-11-26T00:00:00+09:00".parse().unwrap(),
            zone,
        )
        .unwrap();
        assert!(span.overlaps_day(date("2026-11-25"), zone));
        assert!(!span.overlaps_day(date("2026-11-26"), zone));
        assert!(span.overlaps_day(date("2026-11-26"), chrono_tz::Australia::Sydney));
    }

    #[test]
    fn dst_fold_uses_explicit_offsets_and_gap_is_rejected() {
        let zone = chrono_tz::America::New_York;
        let fold = DateTimeSpan::new(
            "2026-11-01T01:50:00-04:00".parse().unwrap(),
            "2026-11-01T01:10:00-05:00".parse().unwrap(),
            zone,
        )
        .unwrap();
        assert_eq!((fold.end() - fold.start()).num_minutes(), 20);
        let gap = DateTimeSpan::new(
            "2026-03-08T02:30:00-05:00".parse().unwrap(),
            "2026-03-08T04:00:00-04:00".parse().unwrap(),
            zone,
        );
        assert!(matches!(gap, Err(TimeError::Offset { .. })));
    }

    #[test]
    fn zoned_moment_requires_the_explicit_offset_to_match_its_timezone() {
        let zone = chrono_tz::America::New_York;
        assert!(ZonedMoment::new("2026-11-01T01:30:00-04:00".parse().unwrap(), zone).is_ok());
        assert!(ZonedMoment::new("2026-11-01T01:30:00-06:00".parse().unwrap(), zone).is_err());
    }

    #[test]
    fn skipped_local_date_has_no_timed_occupancy() {
        let zone = chrono_tz::Pacific::Apia;
        let span = DateTimeSpan::new(
            "2011-12-29T21:00:00-10:00".parse().unwrap(),
            "2011-12-31T02:00:00+14:00".parse().unwrap(),
            zone,
        )
        .unwrap();
        assert!(!span.overlaps_day(date("2011-12-30"), zone));
        assert!(span.overlaps_day(date("2011-12-31"), zone));
    }
}
