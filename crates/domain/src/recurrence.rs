//! Bounded wall-clock recurrence for reminders.

use std::num::NonZeroU16;

use chrono::{DateTime, Days, LocalResult, NaiveDate, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono_tz::{GapInfo, Tz};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Id,
    time::{DateSpan, ZonedMoment},
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecurrenceError {
    #[error("recurrence end date precedes its start date")]
    DateOrder,
    #[error("occurrence window must be ordered and span no more than 366 days")]
    Window,
}

/// An exclusive start and inclusive end, with at most 366 days of lookback.
#[derive(Debug, Clone, Copy)]
pub struct OccurrenceWindow {
    after: DateTime<Utc>,
    through: DateTime<Utc>,
}

impl OccurrenceWindow {
    pub fn new(after: DateTime<Utc>, through: DateTime<Utc>) -> Result<Self, RecurrenceError> {
        if through < after || through - after > TimeDelta::days(366) {
            return Err(RecurrenceError::Window);
        }
        Ok(Self { after, through })
    }

    fn contains(self, instant: DateTime<Utc>) -> bool {
        self.after < instant && instant <= self.through
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Frequency {
    Daily,
    Weekly,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GapPolicy {
    Skip,
    NextValid,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FoldPolicy {
    Earlier,
    Later,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DstPolicy {
    pub gap: GapPolicy,
    pub fold: FoldPolicy,
}

/// Repeat on the anchored local date every `interval` days or weeks.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "RuleFields", into = "RuleFields")]
pub struct RecurrenceRule(RuleFields);

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuleFields {
    start: NaiveDate,
    through: Option<NaiveDate>,
    time: NaiveTime,
    timezone: Tz,
    frequency: Frequency,
    #[serde(default = "one")]
    interval: NonZeroU16,
    dst: DstPolicy,
}

fn one() -> NonZeroU16 {
    NonZeroU16::MIN
}

impl TryFrom<RuleFields> for RecurrenceRule {
    type Error = RecurrenceError;

    fn try_from(fields: RuleFields) -> Result<Self, RecurrenceError> {
        if fields.through.is_some_and(|through| through < fields.start) {
            return Err(RecurrenceError::DateOrder);
        }
        Ok(Self(fields))
    }
}

impl From<RecurrenceRule> for RuleFields {
    fn from(rule: RecurrenceRule) -> Self {
        rule.0
    }
}

impl RecurrenceRule {
    /// Expand only the requested window, jumping directly to the first candidate.
    pub fn occurrences(self, window: OccurrenceWindow) -> Vec<DateTime<Utc>> {
        let rule = self.0;
        let stride = i64::from(rule.interval.get())
            * match rule.frequency {
                Frequency::Daily => 1,
                Frequency::Weekly => 7,
            };
        // Include adjacent dates for zone offsets and for gaps that skip a local day.
        let first_date = window
            .after
            .date_naive()
            .checked_sub_days(Days::new(2))
            .unwrap_or(NaiveDate::MIN)
            .max(rule.start);
        let last_date = window
            .through
            .date_naive()
            .checked_add_days(Days::new(2))
            .unwrap_or(NaiveDate::MAX)
            .min(rule.through.unwrap_or(NaiveDate::MAX));
        let offset = (rule.start - first_date).num_days().rem_euclid(stride);
        let mut next = first_date.checked_add_days(Days::new(offset as u64));
        let mut occurrences = Vec::new();
        while let Some(day) = next.filter(|date| *date <= last_date) {
            let local = day.and_time(rule.time);
            let occurrence = match rule.timezone.from_local_datetime(&local) {
                LocalResult::Single(instant) => Some(instant),
                LocalResult::Ambiguous(first, second) => Some(match rule.dst.fold {
                    FoldPolicy::Earlier => first.min(second),
                    FoldPolicy::Later => first.max(second),
                }),
                LocalResult::None => match rule.dst.gap {
                    GapPolicy::Skip => None,
                    GapPolicy::NextValid => {
                        GapInfo::new(&local, &rule.timezone).and_then(|gap| gap.end)
                    }
                },
            };
            if let Some(instant) = occurrence.map(|instant| instant.to_utc())
                && window.contains(instant)
            {
                occurrences.push(instant);
            }
            next = day.checked_add_days(Days::new(stride as u64));
        }
        occurrences.sort_unstable();
        occurrences.dedup();
        occurrences
    }
}

/// A bounded daily or weekly date pattern for human calendar events.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "CalendarRuleFields", into = "CalendarRuleFields")]
pub struct CalendarRecurrence(CalendarRuleFields);

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CalendarRuleFields {
    start: NaiveDate,
    through: Option<NaiveDate>,
    frequency: Frequency,
    #[serde(default = "one")]
    interval: NonZeroU16,
    dst: DstPolicy,
}

impl CalendarRecurrence {
    pub fn new(
        start: NaiveDate,
        through: Option<NaiveDate>,
        frequency: Frequency,
        interval: NonZeroU16,
        dst: DstPolicy,
    ) -> Result<Self, RecurrenceError> {
        if through.is_some_and(|through| through < start) {
            return Err(RecurrenceError::DateOrder);
        }
        Ok(Self(CalendarRuleFields {
            start,
            through,
            frequency,
            interval,
            dst,
        }))
    }

    pub fn start(self) -> NaiveDate {
        self.0.start
    }

    /// Return occurrence anchor dates in an inclusive window no longer than 366 days.
    pub fn occurrence_dates(self, window: DateSpan) -> Result<Vec<NaiveDate>, RecurrenceError> {
        if (window.through() - window.start()).num_days() > 366 {
            return Err(RecurrenceError::Window);
        }
        let rule = self.0;
        let stride = i64::from(rule.interval.get())
            * match rule.frequency {
                Frequency::Daily => 1,
                Frequency::Weekly => 7,
            };
        let first = window.start().max(rule.start);
        let offset = (first - rule.start).num_days();
        let steps = (offset + stride - 1) / stride;
        let first = rule
            .start
            .checked_add_days(Days::new((steps * stride) as u64));
        let mut next = first;
        let mut dates = Vec::new();
        while let Some(day) = next.filter(|date| {
            *date <= window.through() && rule.through.is_none_or(|through| *date <= through)
        }) {
            dates.push(day);
            next = day.checked_add_days(Days::new(stride as u64));
        }
        Ok(dates)
    }
}

impl TryFrom<CalendarRuleFields> for CalendarRecurrence {
    type Error = RecurrenceError;

    fn try_from(fields: CalendarRuleFields) -> Result<Self, Self::Error> {
        Self::new(
            fields.start,
            fields.through,
            fields.frequency,
            fields.interval,
            fields.dst,
        )
    }
}

impl From<CalendarRecurrence> for CalendarRuleFields {
    fn from(recurrence: CalendarRecurrence) -> Self {
        recurrence.0
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReminderSchedule {
    Once { at: ZonedMoment },
    Recurring { rule: RecurrenceRule },
}

impl ReminderSchedule {
    pub fn occurrences(self, window: OccurrenceWindow) -> Vec<DateTime<Utc>> {
        match self {
            Self::Once { at } => Some(at.instant())
                .filter(|instant| window.contains(*instant))
                .into_iter()
                .collect(),
            Self::Recurring { rule } => rule.occurrences(window),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct Reminder {
    pub id: Id,
    /// Optional Task or WorkUnit this reminder concerns.
    pub target_id: Option<Id>,
    pub schedule: ReminderSchedule,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::DateSpan;

    fn rule(start: &str, time: &str, timezone: Tz) -> RuleFields {
        RuleFields {
            start: start.parse().unwrap(),
            through: None,
            time: time.parse().unwrap(),
            timezone,
            frequency: Frequency::Daily,
            interval: NonZeroU16::MIN,
            dst: DstPolicy {
                gap: GapPolicy::Skip,
                fold: FoldPolicy::Earlier,
            },
        }
    }

    fn instant(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    fn window(after: &str, through: &str) -> OccurrenceWindow {
        OccurrenceWindow::new(instant(after), instant(through)).unwrap()
    }

    #[test]
    fn weekly_interval_preserves_anchor_and_inclusive_end() {
        let mut fields = rule("1990-01-01", "00:30:00", chrono_tz::Asia::Tokyo);
        fields.frequency = Frequency::Weekly;
        fields.interval = NonZeroU16::new(2).unwrap();
        fields.through = Some("2026-02-02".parse().unwrap());
        let rule = RecurrenceRule::try_from(fields).unwrap();
        let due = rule.occurrences(window("2026-01-18T15:30:00Z", "2026-02-28T00:00:00Z"));
        assert_eq!(due, [instant("2026-01-25T15:30:00Z")]);
        assert_eq!(
            rule.occurrences(window("2026-01-25T15:29:59Z", "2026-01-25T15:30:00Z")),
            due
        );
    }

    #[test]
    fn daylight_saving_gap_and_fold_follow_declared_policies() {
        let mut fields = rule("2026-03-07", "02:30:00", chrono_tz::America::New_York);
        let spring = window("2026-03-08T00:00:00Z", "2026-03-09T00:00:00Z");
        assert!(
            RecurrenceRule::try_from(fields)
                .unwrap()
                .occurrences(spring)
                .is_empty()
        );
        fields.dst.gap = GapPolicy::NextValid;
        assert_eq!(
            RecurrenceRule::try_from(fields)
                .unwrap()
                .occurrences(spring),
            [instant("2026-03-08T07:00:00Z")]
        );
        fields.time = "01:30:00".parse().unwrap();
        let fall = window("2026-11-01T00:00:00Z", "2026-11-02T00:00:00Z");
        assert_eq!(
            RecurrenceRule::try_from(fields).unwrap().occurrences(fall),
            [instant("2026-11-01T05:30:00Z")]
        );
        fields.dst.fold = FoldPolicy::Later;
        assert_eq!(
            RecurrenceRule::try_from(fields).unwrap().occurrences(fall),
            [instant("2026-11-01T06:30:00Z")]
        );
    }

    #[test]
    fn date_order_and_unbounded_windows_are_rejected() {
        let mut fields = rule("2026-01-02", "12:00:00", chrono_tz::UTC);
        fields.through = Some("2026-01-01".parse().unwrap());
        assert_eq!(
            RecurrenceRule::try_from(fields).unwrap_err(),
            RecurrenceError::DateOrder
        );
        assert!(
            OccurrenceWindow::new(
                instant("2025-01-01T00:00:00Z"),
                instant("2026-01-03T00:00:00Z"),
            )
            .is_err()
        );
    }

    #[test]
    fn calendar_recurrence_expands_only_anchored_dates_in_a_bounded_window() {
        let rule = CalendarRecurrence::new(
            "2026-01-05".parse().unwrap(),
            Some("2026-02-28".parse().unwrap()),
            Frequency::Weekly,
            NonZeroU16::new(2).unwrap(),
            DstPolicy {
                gap: GapPolicy::Skip,
                fold: FoldPolicy::Earlier,
            },
        )
        .unwrap();
        let window =
            DateSpan::new("2026-01-20".parse().unwrap(), "2026-02-28".parse().unwrap()).unwrap();

        assert_eq!(
            rule.occurrence_dates(window).unwrap(),
            [
                "2026-02-02".parse::<NaiveDate>().unwrap(),
                "2026-02-16".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn calendar_recurrence_rejects_windows_over_one_year_and_reversed_rules() {
        let rule = CalendarRecurrence::new(
            "2026-01-05".parse().unwrap(),
            Some("2026-01-04".parse().unwrap()),
            Frequency::Daily,
            NonZeroU16::MIN,
            DstPolicy {
                gap: GapPolicy::Skip,
                fold: FoldPolicy::Earlier,
            },
        );
        assert_eq!(rule.unwrap_err(), RecurrenceError::DateOrder);

        let rule = CalendarRecurrence::new(
            "2026-01-05".parse().unwrap(),
            None,
            Frequency::Daily,
            NonZeroU16::MIN,
            DstPolicy {
                gap: GapPolicy::Skip,
                fold: FoldPolicy::Earlier,
            },
        )
        .unwrap();
        let window =
            DateSpan::new("2026-01-01".parse().unwrap(), "2027-01-03".parse().unwrap()).unwrap();
        assert_eq!(
            rule.occurrence_dates(window).unwrap_err(),
            RecurrenceError::Window
        );
    }
}
