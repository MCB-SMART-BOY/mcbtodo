use chrono::NaiveDate;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::{
    Availability, Id,
    time::{DateSpan, Deadline, Span},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarKind {
    ActivePeriod,
    Event,
    TaskOccurrence,
    TimeBlock,
    Deadline,
}

impl CalendarKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ActivePeriod => "active",
            Self::Event => "event",
            Self::TaskOccurrence => "task",
            Self::TimeBlock => "block",
            Self::Deadline => "due",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Occurrence {
    Span(Span),
    Deadline(Deadline),
}

impl Occurrence {
    pub fn on_day(self, day: NaiveDate, zone: Tz) -> bool {
        match self {
            Self::Span(span) => span.overlaps_day(day, zone),
            Self::Deadline(deadline) => deadline.on_day(day, zone),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CalendarEntry<'a> {
    pub id: Id,
    pub title: &'a str,
    pub kind: CalendarKind,
    pub busy: bool,
    pub occurrence: Occurrence,
}

/// An owned calendar occurrence that can cross the client boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarItem {
    pub id: Id,
    pub title: String,
    pub kind: CalendarKind,
    pub busy: bool,
    pub occurrence: Occurrence,
}

impl CalendarItem {
    pub fn on_day(&self, day: NaiveDate, timezone: Tz) -> bool {
        self.occurrence.on_day(day, timezone)
    }
}

impl From<CalendarEntry<'_>> for CalendarItem {
    fn from(entry: CalendarEntry<'_>) -> Self {
        Self {
            id: entry.id,
            title: entry.title.to_owned(),
            kind: entry.kind,
            busy: entry.busy,
            occurrence: entry.occurrence,
        }
    }
}

impl<'a> CalendarEntry<'a> {
    pub fn task_occurrence(id: Id, title: &'a str, date: NaiveDate) -> Self {
        let span = DateSpan::new(date, date).expect("a date is a valid inclusive span");
        Self::span(
            id,
            title,
            CalendarKind::TaskOccurrence,
            Availability::Free,
            Span::DateSpan(span),
        )
    }

    pub fn active(id: Id, title: &'a str, span: Span) -> Self {
        Self::span(
            id,
            title,
            CalendarKind::ActivePeriod,
            Availability::Free,
            span,
        )
    }

    pub fn event(id: Id, title: &'a str, span: Span, availability: Availability) -> Self {
        Self::span(id, title, CalendarKind::Event, availability, span)
    }

    pub fn time_block(
        id: Id,
        title: &'a str,
        span: crate::time::DateTimeSpan,
        availability: Availability,
    ) -> Self {
        Self::span(
            id,
            title,
            CalendarKind::TimeBlock,
            availability,
            Span::DateTimeSpan(span),
        )
    }

    pub fn deadline(id: Id, title: &'a str, deadline: Deadline) -> Self {
        Self {
            id,
            title,
            kind: CalendarKind::Deadline,
            busy: false,
            occurrence: Occurrence::Deadline(deadline),
        }
    }

    fn span(
        id: Id,
        title: &'a str,
        kind: CalendarKind,
        availability: Availability,
        span: Span,
    ) -> Self {
        Self {
            id,
            title,
            kind,
            busy: availability == Availability::Busy,
            occurrence: Occurrence::Span(span),
        }
    }

    pub fn on_day(&self, day: NaiveDate, zone: Tz) -> bool {
        self.occurrence.on_day(day, zone)
    }

    pub fn in_range(&self, dates: DateSpan, zone: Tz) -> bool {
        match self.occurrence {
            Occurrence::Span(Span::DateSpan(span)) => span.overlaps(dates),
            Occurrence::Span(Span::DateTimeSpan(span)) => span.overlaps_dates(dates, zone),
            Occurrence::Deadline(deadline) => deadline
                .local_date(zone)
                .is_some_and(|day| dates.contains(day)),
        }
    }
}
