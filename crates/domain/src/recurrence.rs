//! Bounded recurrence for reminders, tasks, and calendar events.

use std::num::NonZeroU16;

use chrono::{
    DateTime, Days, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc,
};
use chrono_tz::{GapInfo, Tz};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Id,
    time::{DateSpan, DateTimeSpan, Span, ZonedMoment, date_window},
};

const MAX_CALENDAR_OCCURRENCES: usize = 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecurrenceError {
    #[error("recurrence end date precedes its start date")]
    DateOrder,
    #[error("occurrence window must be ordered and span no more than 366 days")]
    Window,
    #[error("too many occurrences intersect the calendar window (limit {limit})")]
    ProjectionLimit { limit: usize },
    #[error("recurring event end date is out of range")]
    OccurrenceEnd,
    #[error("recurrence start must match the event's start date")]
    EventStart,
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
            let occurrence = resolve_local(local, rule.timezone, rule.dst);
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

fn resolve_local(local: NaiveDateTime, zone: Tz, dst: DstPolicy) -> Option<DateTime<Tz>> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(instant) => Some(instant),
        LocalResult::Ambiguous(first, second) => Some(match dst.fold {
            FoldPolicy::Earlier => first.min(second),
            FoldPolicy::Later => first.max(second),
        }),
        LocalResult::None => match dst.gap {
            GapPolicy::Skip => None,
            GapPolicy::NextValid => GapInfo::new(&local, &zone)?.end,
        },
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

    pub fn validate_event(self, template: Span) -> Result<(), RecurrenceError> {
        let date = match template {
            Span::DateSpan(span) => span.start(),
            Span::DateTimeSpan(span) => template_local_start(span)?.date(),
        };
        if date != self.start() {
            return Err(RecurrenceError::EventStart);
        }
        Ok(())
    }

    /// Expand whole events intersecting the display window. Timed events
    /// repeat at the template's local start time and keep its elapsed duration.
    pub fn overlapping_events(
        self,
        template: Span,
        window: DateSpan,
        display_zone: Tz,
    ) -> Result<Vec<Span>, RecurrenceError> {
        self.validate_event(template)?;
        match template {
            Span::DateSpan(span) => self
                .overlapping_spans(span, window)
                .map(|spans| spans.into_iter().map(Span::DateSpan).collect()),
            Span::DateTimeSpan(span) => self.overlapping_timed_events(span, window, display_zone),
        }
    }

    /// Return occurrence anchor dates in an inclusive window no longer than 366 days.
    pub fn occurrence_dates(self, window: DateSpan) -> Result<Vec<NaiveDate>, RecurrenceError> {
        Self::check_window(window)?;
        Ok(self
            .dates_between(window.start(), window.through())
            .collect())
    }

    /// Project whole occurrences, including those that start before the query window.
    pub fn overlapping_spans(
        self,
        template: DateSpan,
        window: DateSpan,
    ) -> Result<Vec<DateSpan>, RecurrenceError> {
        Self::check_window(window)?;
        let duration = Days::new((template.through() - template.start()).num_days() as u64);
        let earliest = window
            .start()
            .checked_sub_days(duration)
            .unwrap_or(NaiveDate::MIN);
        let mut spans = Vec::new();
        for start in self.dates_between(earliest, window.through()) {
            check_occurrence_limit(spans.len())?;
            let through = start
                .checked_add_days(duration)
                .ok_or(RecurrenceError::OccurrenceEnd)?;
            spans.push(DateSpan::new(start, through).map_err(|_| RecurrenceError::OccurrenceEnd)?);
        }
        Ok(spans)
    }

    fn overlapping_timed_events(
        self,
        template: DateTimeSpan,
        window: DateSpan,
        display_zone: Tz,
    ) -> Result<Vec<Span>, RecurrenceError> {
        Self::check_window(window)?;
        let Some((window_start, window_end)) = date_window(window, display_zone) else {
            return Ok(Vec::new());
        };

        let duration = template.end() - template.start();
        let local_start = template_local_start(template)?.time();
        let zone = template.timezone();
        // Include adjacent anchor dates for zone offsets and a skipped local day.
        let earliest = window_start
            .checked_sub_signed(duration)
            .and_then(|time| time.date_naive().checked_sub_days(Days::new(2)))
            .unwrap_or(NaiveDate::MIN);
        let latest = window_end
            .date_naive()
            .checked_add_days(Days::new(2))
            .unwrap_or(NaiveDate::MAX);

        let mut spans = Vec::new();
        let mut last_start = None;
        for day in self.dates_between(earliest, latest) {
            let start = if day == self.start() {
                template.start()
            } else if let Some(start) = resolve_local(day.and_time(local_start), zone, self.0.dst) {
                start.fixed_offset()
            } else {
                continue;
            };
            if start >= window_end || last_start == Some(start) {
                continue;
            }
            let end = start
                .to_utc()
                .checked_add_signed(duration)
                .ok_or(RecurrenceError::OccurrenceEnd)?;
            if end <= window_start {
                continue;
            }
            check_occurrence_limit(spans.len())?;
            let span = DateTimeSpan::new(start, end.with_timezone(&zone).fixed_offset(), zone)
                .map_err(|_| RecurrenceError::OccurrenceEnd)?;
            spans.push(Span::DateTimeSpan(span));
            last_start = Some(start);
        }
        Ok(spans)
    }

    fn check_window(window: DateSpan) -> Result<(), RecurrenceError> {
        if (window.through() - window.start()).num_days() > 366 {
            return Err(RecurrenceError::Window);
        }
        Ok(())
    }

    fn dates_between(
        self,
        start: NaiveDate,
        through: NaiveDate,
    ) -> impl Iterator<Item = NaiveDate> {
        let rule = self.0;
        let stride = i64::from(rule.interval.get())
            * match rule.frequency {
                Frequency::Daily => 1,
                Frequency::Weekly => 7,
            };
        let first = start.max(rule.start);
        let offset = (first - rule.start).num_days();
        let steps = (offset + stride - 1) / stride;
        let stride_days = Days::new(stride as u64);
        let first = rule
            .start
            .checked_add_days(Days::new((steps * stride) as u64));
        let last = rule.through.map_or(through, |end| end.min(through));
        std::iter::successors(first, move |day| day.checked_add_days(stride_days))
            .take_while(move |&day| day <= last)
    }
}

fn template_local_start(template: DateTimeSpan) -> Result<NaiveDateTime, RecurrenceError> {
    let start = template.start();
    start
        .naive_utc()
        .checked_add_offset(*start.offset())
        .ok_or(RecurrenceError::EventStart)
}

fn check_occurrence_limit(count: usize) -> Result<(), RecurrenceError> {
    if count >= MAX_CALENDAR_OCCURRENCES {
        return Err(RecurrenceError::ProjectionLimit {
            limit: MAX_CALENDAR_OCCURRENCES,
        });
    }
    Ok(())
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

    #[test]
    fn calendar_continuations_are_limited_by_matching_anchors_not_duration() {
        let start = "2020-01-01".parse::<NaiveDate>().unwrap();
        let end = start + Days::new(1024);
        let template = DateSpan::new(start, end).unwrap();
        let policy = DstPolicy {
            gap: GapPolicy::Skip,
            fold: FoldPolicy::Earlier,
        };
        let daily = CalendarRecurrence::new(start, None, Frequency::Daily, NonZeroU16::MIN, policy)
            .unwrap();
        let before = end.pred_opt().unwrap();
        assert_eq!(
            daily
                .overlapping_spans(template, DateSpan::new(before, before).unwrap())
                .unwrap()
                .len(),
            1024
        );
        let window = DateSpan::new(end, end).unwrap();
        assert_eq!(
            daily.overlapping_spans(template, window).unwrap_err(),
            RecurrenceError::ProjectionLimit { limit: 1024 }
        );
        let weekly = CalendarRecurrence::new(
            start,
            Some(start),
            Frequency::Weekly,
            NonZeroU16::MIN,
            policy,
        )
        .unwrap();
        assert_eq!(
            weekly.overlapping_spans(template, window).unwrap(),
            [template]
        );
    }

    #[test]
    fn timed_continuations_share_the_calendar_occurrence_limit() {
        let start = instant("2020-01-01T00:00:00Z");
        let end = start + TimeDelta::days(1025);
        let zone = chrono_tz::UTC;
        let template = Span::DateTimeSpan(
            DateTimeSpan::new(start.fixed_offset(), end.fixed_offset(), zone).unwrap(),
        );
        let rule = CalendarRecurrence::new(
            start.date_naive(),
            None,
            Frequency::Daily,
            NonZeroU16::MIN,
            DstPolicy {
                gap: GapPolicy::Skip,
                fold: FoldPolicy::Earlier,
            },
        )
        .unwrap();
        let last = end.date_naive().pred_opt().unwrap();
        let previous = last.pred_opt().unwrap();
        let at_limit = rule
            .overlapping_events(template, DateSpan::new(previous, previous).unwrap(), zone)
            .unwrap();
        assert_eq!(at_limit.len(), 1024);
        assert_eq!(at_limit.first(), Some(&template));
        assert_eq!(
            rule.overlapping_events(template, DateSpan::new(last, last).unwrap(), zone),
            Err(RecurrenceError::ProjectionLimit { limit: 1024 })
        );
    }

    #[test]
    fn timed_recurrence_preserves_the_explicit_fold_offset_of_its_anchor() {
        let anchor: NaiveDate = "2026-11-01".parse().unwrap();
        let zone = chrono_tz::America::New_York;
        let template = Span::DateTimeSpan(
            DateTimeSpan::new(
                "2026-11-01T01:50:00-05:00".parse().unwrap(),
                "2026-11-01T02:10:00-05:00".parse().unwrap(),
                zone,
            )
            .unwrap(),
        );
        let rule = CalendarRecurrence::new(
            anchor,
            None,
            Frequency::Daily,
            NonZeroU16::MIN,
            DstPolicy {
                gap: GapPolicy::Skip,
                fold: FoldPolicy::Earlier,
            },
        )
        .unwrap();
        assert_eq!(
            rule.overlapping_events(template, DateSpan::new(anchor, anchor).unwrap(), zone)
                .unwrap(),
            [template]
        );
        let tomorrow = anchor.succ_opt().unwrap();
        let wrong_anchor = CalendarRecurrence::new(
            tomorrow,
            None,
            Frequency::Daily,
            NonZeroU16::MIN,
            rule.0.dst,
        )
        .unwrap();
        assert_eq!(
            wrong_anchor.validate_event(template),
            Err(RecurrenceError::EventStart)
        );
    }
}
