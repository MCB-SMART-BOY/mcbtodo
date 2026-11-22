//! Pure UTC occurrence calculation for fixed-rate and civil-time schedules.

use std::{cmp::Reverse, collections::BinaryHeap, num::NonZeroUsize};

use chrono::{DateTime, Datelike, LocalResult, NaiveDate, Offset, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

mod spec;

pub use spec::{ClockScheduleError, Fold, Missed, Overlap, ScheduleSummary, Trigger};

const NANOS_PER_SECOND: i128 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
const NANOS_PER_DAY: i128 = SECONDS_PER_DAY as i128 * NANOS_PER_SECOND;

/// Option for repeated local times. Cron skips nonexistent local times.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DstResolution {
    #[default]
    Earlier,
    Later,
    Both,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    Skip,
    #[default]
    RunOnce,
    ReplayLimited {
        limit: NonZeroUsize,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MissedOccurrences {
    /// Exact number of eligible occurrences, including those not selected for replay.
    pub total: u128,
    /// Selected UTC instants in chronological order.
    pub occurrences: Vec<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("schedule interval must be positive")]
    InvalidInterval,
    #[error("schedule interval anchor cannot be a leap second")]
    IntervalLeapSecond,
    #[error("expected five cron fields: minute hour day-of-month month day-of-week")]
    CronFieldCount,
    #[error("invalid cron {field} field `{value}`")]
    CronField { field: &'static str, value: String },
    #[error("cron has no matching calendar date")]
    ImpossibleCron,
    #[error("midnight on {date} in {timezone} does not resolve to a single instant")]
    UnresolvedDate { date: NaiveDate, timezone: Tz },
}

#[derive(Clone, Debug)]
pub struct Schedule(ClockTrigger);

#[derive(Clone, Debug)]
enum ClockTrigger {
    Once(DateTime<Utc>),
    Interval { anchor: DateTime<Utc>, period: i128 },
    Cron(Cron),
}

impl Schedule {
    pub fn once(at: DateTime<Utc>) -> Self {
        Self(ClockTrigger::Once(at))
    }

    /// Resolve a date-only once trigger to the first midnight of that civil date
    /// according to `dst`. A nonexistent midnight or a fold with `Both` is an error.
    pub fn once_on(
        date: NaiveDate,
        timezone: Tz,
        dst: DstResolution,
    ) -> Result<Self, ScheduleError> {
        let [Some(at), None] = resolve_local(timezone, dst, date, 0) else {
            return Err(ScheduleError::UnresolvedDate { date, timezone });
        };
        Ok(Self::once(at))
    }

    /// A fixed-rate series on the POSIX timeline whose first occurrence is `anchor`.
    pub fn interval(anchor: DateTime<Utc>, every: TimeDelta) -> Result<Self, ScheduleError> {
        let period =
            i128::from(every.num_seconds()) * NANOS_PER_SECOND + i128::from(every.subsec_nanos());
        if period <= 0 {
            return Err(ScheduleError::InvalidInterval);
        }
        if anchor.timestamp_subsec_nanos() >= NANOS_PER_SECOND as u32 {
            return Err(ScheduleError::IntervalLeapSecond);
        }
        Ok(Self(ClockTrigger::Interval { anchor, period }))
    }

    /// Five fields with wildcards, lists, inclusive ranges, optional steps on
    /// wildcards and ranges, and case-insensitive three-letter month and weekday
    /// names. Sunday is 0 or 7. If both day fields are restricted, either may match.
    pub fn cron(expression: &str, timezone: Tz, dst: DstResolution) -> Result<Self, ScheduleError> {
        Ok(Self(ClockTrigger::Cron(Cron::parse(
            expression, timezone, dst,
        )?)))
    }

    /// The first occurrence strictly after `after`.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match &self.0 {
            ClockTrigger::Once(at) => (*at > after).then_some(*at),
            ClockTrigger::Interval { anchor, period } => {
                let index = first_interval_index(*anchor, *period, after);
                interval_at(*anchor, *period, index)
            }
            ClockTrigger::Cron(cron) => cron.next_after(after),
        }
    }

    /// Count the occurrences in `(after, through]`. Run-once selects the latest
    /// occurrence. Limited replay selects the most recent `limit` occurrences
    /// and returns them oldest first.
    pub fn missed(
        &self,
        after: DateTime<Utc>,
        through: DateTime<Utc>,
        policy: MissedPolicy,
    ) -> MissedOccurrences {
        if after >= through {
            return MissedOccurrences::default();
        }
        let limit = match policy {
            MissedPolicy::Skip => 0,
            MissedPolicy::RunOnce => 1,
            MissedPolicy::ReplayLimited { limit } => limit.get(),
        };
        match &self.0 {
            ClockTrigger::Once(at) if *at > after && *at <= through => MissedOccurrences {
                total: 1,
                occurrences: if limit == 0 { vec![] } else { vec![*at] },
            },
            ClockTrigger::Once(_) => MissedOccurrences::default(),
            ClockTrigger::Interval { anchor, period } if through >= *anchor => {
                let first = first_interval_index(*anchor, *period, after);
                let end = (timestamp_nanos(through) - timestamp_nanos(*anchor)) / period + 1;
                let occurrences = (first.max(end - limit as i128)..end)
                    .filter_map(|index| interval_at(*anchor, *period, index))
                    .collect();
                MissedOccurrences {
                    total: (end - first).max(0) as u128,
                    occurrences,
                }
            }
            ClockTrigger::Interval { .. } => MissedOccurrences::default(),
            ClockTrigger::Cron(cron) => cron.missed(after, through, limit),
        }
    }
}

fn first_interval_index(anchor: DateTime<Utc>, period: i128, after: DateTime<Utc>) -> i128 {
    if after < anchor {
        0
    } else {
        (timestamp_nanos(after) - timestamp_nanos(anchor)) / period + 1
    }
}

fn interval_at(anchor: DateTime<Utc>, period: i128, index: i128) -> Option<DateTime<Utc>> {
    if index == 0 {
        return Some(anchor);
    }
    let nanos = timestamp_nanos(anchor) + period * index;
    DateTime::from_timestamp(
        nanos.div_euclid(NANOS_PER_SECOND).try_into().ok()?,
        nanos.rem_euclid(NANOS_PER_SECOND).try_into().ok()?,
    )
}

fn timestamp_nanos(at: DateTime<Utc>) -> i128 {
    // In a leap second, all ordinary instants in the POSIX second precede
    // the cursor and the next second still follows it.
    let subsecond = i128::from(at.timestamp_subsec_nanos()).min(NANOS_PER_SECOND - 1);
    i128::from(at.timestamp()) * NANOS_PER_SECOND + subsecond
}

#[derive(Clone, Debug)]
struct Cron {
    timezone: Tz,
    dst: DstResolution,
    slots: Vec<u16>,
    days_of_month: Field,
    months: Field,
    days_of_week: Field,
}

impl Cron {
    fn parse(expression: &str, timezone: Tz, dst: DstResolution) -> Result<Self, ScheduleError> {
        let mut fields = expression.split_ascii_whitespace();
        let [
            Some(minute),
            Some(hour),
            Some(day),
            Some(month),
            Some(weekday),
            None,
        ] = std::array::from_fn(|_| fields.next())
        else {
            return Err(ScheduleError::CronFieldCount);
        };
        let minutes = Field::parse(minute, "minute", 0, 59, &[])?;
        let hours = Field::parse(hour, "hour", 0, 23, &[])?;
        let slots = (0..24)
            .filter(|&hour| hours.contains(hour))
            .flat_map(|hour| {
                (0..60)
                    .filter(move |&minute| minutes.contains(minute))
                    .map(move |minute| u16::from(hour) * 60 + u16::from(minute))
            })
            .collect();
        let cron = Self {
            timezone,
            dst,
            slots,
            days_of_month: Field::parse(day, "day-of-month", 1, 31, &[])?,
            months: Field::parse(
                month,
                "month",
                1,
                12,
                &[
                    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV",
                    "DEC",
                ],
            )?,
            days_of_week: Field::parse(
                weekday,
                "day-of-week",
                0,
                7,
                &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"],
            )?,
        };
        // The Gregorian day/weekday pattern repeats every 400 years.
        let first = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
        if !(0..146_097).any(|days| cron.matches_date(first + TimeDelta::days(days))) {
            return Err(ScheduleError::ImpossibleCron);
        }
        Ok(cron)
    }

    fn matches_date(&self, date: NaiveDate) -> bool {
        if !self.months.contains(date.month() as u8) {
            return false;
        }
        let weekday = date.weekday().num_days_from_sunday() as u8;
        let day_matches = self.days_of_month.contains(date.day() as u8);
        let weekday_matches =
            self.days_of_week.contains(weekday) || (weekday == 0 && self.days_of_week.contains(7));
        if self.days_of_month.wildcard || self.days_of_week.wildcard {
            day_matches && weekday_matches
        } else {
            day_matches || weekday_matches
        }
    }

    fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut date = after.date_naive().pred_opt().unwrap_or(NaiveDate::MIN);
        let mut earliest = None::<DateTime<Utc>>;
        loop {
            let midnight = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
            // A civil time maps to a UTC instant less than one day away.
            if earliest
                .is_some_and(|at| timestamp_nanos(at) <= timestamp_nanos(midnight) - NANOS_PER_DAY)
            {
                return earliest;
            }
            if self.matches_date(date) {
                let occurrences = self.day(
                    date,
                    timestamp_nanos(after),
                    timestamp_nanos(DateTime::<Utc>::MAX_UTC),
                );
                if let Some(at) = occurrences.get(0) {
                    earliest = Some(earliest.map_or(at, |previous| previous.min(at)));
                }
            }
            let Some(next) = date.succ_opt() else {
                return earliest;
            };
            date = next;
        }
    }

    fn missed(
        &self,
        after: DateTime<Utc>,
        through: DateTime<Utc>,
        limit: usize,
    ) -> MissedOccurrences {
        let after_nanos = timestamp_nanos(after);
        let through_nanos = timestamp_nanos(through);
        let first = after.date_naive().pred_opt().unwrap_or(NaiveDate::MIN);
        let mut date = through.date_naive().succ_opt().unwrap_or(NaiveDate::MAX);
        let mut total = 0;
        let mut tail: BinaryHeap<Reverse<DateTime<Utc>>> = BinaryHeap::new();
        loop {
            if self.matches_date(date) {
                let occurrences = self.day(date, after_nanos, through_nanos);
                total += occurrences.len() as u128;
                if limit != 0 {
                    for index in (0..occurrences.len()).rev() {
                        let at = occurrences.get(index).expect("validated occurrence");
                        if tail.len() < limit {
                            tail.push(Reverse(at));
                        } else if let Some(mut oldest) = tail.peek_mut() {
                            if at <= oldest.0 {
                                break;
                            }
                            *oldest = Reverse(at);
                        }
                    }
                }
            }
            if date == first {
                break;
            }
            date = date.pred_opt().expect("above first");
        }
        MissedOccurrences {
            total,
            occurrences: tail
                .into_sorted_vec()
                .into_iter()
                .rev()
                .map(|Reverse(at)| at)
                .collect(),
        }
    }

    fn day(&self, date: NaiveDate, after: i128, through: i128) -> DayOccurrences<'_> {
        let midnight = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        if let Some(offset) = self.uniform_offset(midnight) {
            let base = midnight - i64::from(offset);
            let instant = |slot: u16| i128::from(base + i64::from(slot) * 60) * NANOS_PER_SECOND;
            let from = self.slots.partition_point(|&slot| instant(slot) <= after);
            let to = self.slots.partition_point(|&slot| instant(slot) <= through);
            return DayOccurrences::Uniform {
                base,
                slots: &self.slots[from..to],
            };
        }
        let mut instants: Vec<_> = self
            .slots
            .iter()
            .flat_map(|&slot| resolve_local(self.timezone, self.dst, date, slot))
            .flatten()
            .filter(|&at| {
                let at = timestamp_nanos(at);
                at > after && at <= through
            })
            .collect();
        instants.sort_unstable();
        DayOccurrences::Adjusted(instants)
    }

    fn uniform_offset(&self, midnight: i64) -> Option<i32> {
        if self.timezone == chrono_tz::UTC {
            return Some(0);
        }
        // UTC instants for a civil day fit inside this three-day window. The
        // bundled chrono-tz 2025b data has no two transitions within three
        // days in one zone (the minimum separation is 597600 seconds).
        // If the database changes, resolve each slot until that invariant is
        // checked for the new version.
        if chrono_tz::IANA_TZDB_VERSION != "2025b" {
            return None;
        }
        let offset = |seconds| {
            let at = DateTime::from_timestamp(seconds, 0)?;
            Some(
                self.timezone
                    .offset_from_utc_datetime(&at.naive_utc())
                    .fix()
                    .local_minus_utc(),
            )
        };
        let before = offset(midnight - SECONDS_PER_DAY)?;
        (offset(midnight + 2 * SECONDS_PER_DAY)? == before).then_some(before)
    }
}

enum DayOccurrences<'a> {
    Uniform { base: i64, slots: &'a [u16] },
    Adjusted(Vec<DateTime<Utc>>),
}

impl DayOccurrences<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Uniform { slots, .. } => slots.len(),
            Self::Adjusted(instants) => instants.len(),
        }
    }

    fn get(&self, index: usize) -> Option<DateTime<Utc>> {
        match self {
            Self::Uniform { base, slots } => {
                DateTime::from_timestamp(base + i64::from(*slots.get(index)?) * 60, 0)
            }
            Self::Adjusted(instants) => instants.get(index).copied(),
        }
    }
}

fn resolve_local(
    timezone: Tz,
    dst: DstResolution,
    date: NaiveDate,
    slot: u16,
) -> [Option<DateTime<Utc>>; 2] {
    let slot = u32::from(slot);
    let time = date.and_hms_opt(slot / 60, slot % 60, 0).unwrap();
    match timezone.from_local_datetime(&time) {
        LocalResult::Single(at) => [Some(at.to_utc()), None],
        LocalResult::None => [None, None],
        LocalResult::Ambiguous(earlier, later) => match dst {
            DstResolution::Earlier => [Some(earlier.to_utc()), None],
            DstResolution::Later => [Some(later.to_utc()), None],
            DstResolution::Both => [Some(earlier.to_utc()), Some(later.to_utc())],
        },
    }
}

#[derive(Clone, Copy, Debug)]
struct Field {
    bits: u64,
    wildcard: bool,
}

impl Field {
    fn parse(
        value: &str,
        field: &'static str,
        min: u8,
        max: u8,
        names: &[&str],
    ) -> Result<Self, ScheduleError> {
        let invalid = || ScheduleError::CronField {
            field,
            value: value.to_owned(),
        };
        let number = |token: &str| {
            if token.bytes().all(|c| c.is_ascii_digit()) {
                token.parse::<u8>().ok().filter(|n| (min..=max).contains(n))
            } else {
                names
                    .iter()
                    .position(|name| name.eq_ignore_ascii_case(token))
                    .map(|index| min + index as u8)
            }
        };
        let mut bits = 0;
        for part in value.split(',') {
            let (range, step) = if let Some((range, step)) = part.split_once('/') {
                let step = step
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0 && step.bytes().all(|c| c.is_ascii_digit()))
                    .ok_or_else(invalid)?;
                (range, Some(step))
            } else {
                (part, None)
            };
            let (start, end) = if range == "*" {
                (min, max)
            } else if let Some((start, end)) = range.split_once('-') {
                (
                    number(start).ok_or_else(invalid)?,
                    number(end).ok_or_else(invalid)?,
                )
            } else if step.is_none() {
                let single = number(range).ok_or_else(invalid)?;
                (single, single)
            } else {
                return Err(invalid());
            };
            if start > end {
                return Err(invalid());
            }
            for n in (start..=end).step_by(step.unwrap_or(1)) {
                bits |= 1 << n;
            }
        }
        Ok(Self {
            bits,
            wildcard: value.starts_with('*'),
        })
    }

    fn contains(self, value: u8) -> bool {
        self.bits & (1 << value) != 0
    }
}
