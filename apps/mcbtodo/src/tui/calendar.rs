use chrono::{DateTime, Datelike, Days, LocalResult, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use mcbtodo_application::{CalendarItem, project_calendar};
use mcbtodo_domain::{
    CalendarKind, Occurrence,
    time::{DateSpan, Span},
};
use mcbtodo_vault::Vault;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Lane {
    AllDay,
    Timed,
    Deadline,
    Ongoing,
}

pub(super) fn lane(entry: &CalendarItem) -> Lane {
    match (entry.kind, entry.occurrence) {
        (CalendarKind::ActivePeriod, _) => Lane::Ongoing,
        (_, Occurrence::Deadline(_)) => Lane::Deadline,
        (_, Occurrence::Span(Span::DateSpan(_))) => Lane::AllDay,
        (_, Occurrence::Span(Span::DateTimeSpan(_))) => Lane::Timed,
    }
}

fn start(entry: &CalendarItem, zone: Tz) -> Option<DateTime<Tz>> {
    match entry.occurrence {
        Occurrence::Span(Span::DateTimeSpan(span)) => Some(span.start().with_timezone(&zone)),
        Occurrence::Deadline(deadline) => deadline.datetime(zone),
        _ => None,
    }
}

pub(super) fn day(vault: &Vault, date: NaiveDate) -> Vec<CalendarItem> {
    let zone = vault.timezone();
    let window = DateSpan::new(date, date).expect("a calendar date is a valid span");
    let mut entries =
        project_calendar(vault, window).expect("single-day calendar projection is bounded");
    entries.sort_by_key(|entry| (lane(entry), start(entry, zone)));
    entries
}

pub(super) fn day_label(entry: &CalendarItem, date: NaiveDate, zone: Tz) -> String {
    let availability = if entry.busy { "busy" } else { "free" };
    match entry.occurrence {
        Occurrence::Span(Span::DateTimeSpan(span)) => {
            let start = span.start().with_timezone(&zone);
            let end = span.end().with_timezone(&zone);
            let show_offset = start.offset() != end.offset();
            format!(
                "{}–{}  {} · {} · {}",
                clock(start, date, show_offset),
                clock(end, date, show_offset),
                entry.title,
                entry.kind.label(),
                availability,
            )
        }
        Occurrence::Span(Span::DateSpan(span)) if span.start() != span.through() => format!(
            "{} · {} · {} → {}",
            entry.title,
            availability,
            span.start(),
            span.through()
        ),
        Occurrence::Span(Span::DateSpan(_)) => format!("{} · {}", entry.title, availability),
        Occurrence::Deadline(deadline) => match deadline.datetime(zone) {
            Some(time) => format!("{}  {}", clock(time, date, false), entry.title),
            None => entry.title.to_owned(),
        },
    }
}

fn clock(time: DateTime<Tz>, date: NaiveDate, show_offset: bool) -> String {
    let mut value = if time.date_naive() == date {
        String::new()
    } else {
        time.format("%Y-%m-%d ").to_string()
    };
    let format = if time.nanosecond() != 0 {
        "%H:%M:%S%.f"
    } else if time.second() != 0 {
        "%H:%M:%S"
    } else {
        "%H:%M"
    };
    value.push_str(&time.format(format).to_string());
    if show_offset
        || matches!(
            time.timezone().from_local_datetime(&time.naive_local()),
            LocalResult::Ambiguous(_, _)
        )
    {
        value.push_str(&time.format(" %:z").to_string());
    }
    value
}

pub(super) struct WeekDay<'a> {
    pub(super) date: NaiveDate,
    entries: &'a [CalendarItem],
    indices: &'a [usize],
}

impl<'a> WeekDay<'a> {
    pub(super) fn in_lane(&self, kind: Lane) -> impl Iterator<Item = &'a CalendarItem> {
        self.indices
            .iter()
            .map(|&index| &self.entries[index])
            .filter(move |entry| lane(entry) == kind)
    }

    pub(super) fn at_hour(&self, hour: u32, zone: Tz) -> impl Iterator<Item = &'a CalendarItem> {
        let date = self.date;
        self.in_lane(Lane::Timed)
            .filter(move |entry| grid_hour(entry, date, zone) == hour)
    }
}

pub(super) struct Week {
    pub(super) monday: NaiveDate,
    pub(super) days: [NaiveDate; 7],
    pub(super) hours: Vec<u32>,
    entries: Vec<CalendarItem>,
    columns: [Vec<usize>; 7],
}

pub(super) fn monday_of(date: NaiveDate) -> NaiveDate {
    date - Days::new(u64::from(date.weekday().num_days_from_monday()))
}

impl Week {
    pub(super) fn new(vault: &Vault, date: NaiveDate) -> Self {
        let monday = monday_of(date);
        let days = std::array::from_fn(|offset| monday + Days::new(offset as u64));
        let range = DateSpan::new(monday, days[6]).expect("week in display years");
        let zone = vault.timezone();
        let mut entries =
            project_calendar(vault, range).expect("weekly calendar projection is bounded");
        entries.sort_by_key(|entry| (lane(entry), start(entry, zone)));
        let mut columns = std::array::from_fn(|_| Vec::new());
        let mut occupied = [false; 24];
        for (index, entry) in entries.iter().enumerate() {
            for (&date, column) in days.iter().zip(&mut columns) {
                if entry.on_day(date, zone) {
                    column.push(index);
                    if lane(entry) == Lane::Timed {
                        occupied[grid_hour(entry, date, zone) as usize] = true;
                    }
                }
            }
        }
        let hours = (0..24).filter(|&hour| occupied[hour as usize]).collect();
        Self {
            monday,
            days,
            hours,
            entries,
            columns,
        }
    }

    pub(super) fn column(&self, day_index: usize) -> WeekDay<'_> {
        WeekDay {
            date: self.days[day_index],
            entries: &self.entries,
            indices: &self.columns[day_index],
        }
    }

    pub(super) fn focused_timed(
        &self,
        date: NaiveDate,
        hour_index: usize,
        zone: Tz,
    ) -> Option<&CalendarItem> {
        let index = self.days.iter().position(|&day| day == date)?;
        let hour = *self
            .hours
            .get(hour_index.min(self.hours.len().saturating_sub(1)))?;
        self.column(index).at_hour(hour, zone).next()
    }
}

fn grid_hour(entry: &CalendarItem, date: NaiveDate, zone: Tz) -> u32 {
    let start = start(entry, zone).expect("timed lane entries have instants");
    if start.date_naive() < date {
        0
    } else {
        start.hour()
    }
}

pub(super) fn grid_time(entry: &CalendarItem, date: NaiveDate, zone: Tz) -> String {
    let start = start(entry, zone).expect("timed lane entries have instants");
    if start.date_naive() < date {
        "← cont.".into()
    } else {
        start.format("%H:%M").to_string()
    }
}
