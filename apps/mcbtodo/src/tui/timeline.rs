use chrono::{Days, NaiveDate};
use mcbtodo_application::{CalendarItem, project_calendar};
use mcbtodo_domain::{
    CalendarKind, Occurrence,
    time::{DateSpan, Span},
};
use mcbtodo_vault::Vault;

pub(super) const WINDOW_DAYS: u64 = 14;

pub(super) fn window_start(date: NaiveDate) -> NaiveDate {
    super::calendar::monday_of(date)
}

pub(super) fn entries(vault: &Vault, date: NaiveDate) -> Vec<CalendarItem> {
    let start = window_start(date);
    let through = start
        .checked_add_days(Days::new(WINDOW_DAYS - 1))
        .unwrap_or(start);
    project_calendar(
        vault,
        DateSpan::new(start, through).expect("ordered timeline window"),
    )
    .expect("timeline window is bounded")
}

pub(super) fn cell(entry: &CalendarItem, date: NaiveDate, vault: &Vault) -> &'static str {
    if !entry.on_day(date, vault.timezone()) {
        return "  ";
    }
    match (entry.kind, entry.occurrence) {
        (_, Occurrence::Deadline(_)) => "!!",
        (CalendarKind::ActivePeriod, _) => "..",
        (CalendarKind::TaskOccurrence, _) => "TT",
        (_, Occurrence::Span(Span::DateTimeSpan(span))) => {
            if span.start().with_timezone(&vault.timezone()).date_naive() == date {
                ">>"
            } else {
                "=="
            }
        }
        (_, Occurrence::Span(Span::DateSpan(_))) => "██",
    }
}
