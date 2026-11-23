use chrono::NaiveDate;
use mcbtodo_domain::{CalendarItem, CalendarKind, Occurrence, time::Span};

pub(super) const WINDOW_DAYS: u64 = 14;

pub(super) fn window_start(date: NaiveDate) -> NaiveDate {
    super::calendar::monday_of(date)
}

pub(super) fn cell(entry: &CalendarItem, date: NaiveDate, timezone: chrono_tz::Tz) -> &'static str {
    if !entry.on_day(date, timezone) {
        return "  ";
    }
    match (entry.kind, entry.occurrence) {
        (_, Occurrence::Deadline(_)) => "!!",
        (CalendarKind::ActivePeriod, _) => "..",
        (CalendarKind::TaskOccurrence, _) => "TT",
        (_, Occurrence::Span(Span::DateTimeSpan(span))) => {
            if span.start().with_timezone(&timezone).date_naive() == date {
                ">>"
            } else {
                "=="
            }
        }
        (_, Occurrence::Span(Span::DateSpan(_))) => "██",
    }
}
