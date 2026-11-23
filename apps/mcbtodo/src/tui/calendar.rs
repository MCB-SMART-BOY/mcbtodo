use std::cell::RefCell;

use chrono::{DateTime, Datelike, Days, LocalResult, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use mcbtodo_application::CalendarItem;
use mcbtodo_domain::{
    CalendarKind, Occurrence,
    time::{DateSpan, Span},
};
use mcbtodo_platform::WorkspacePaths;

use super::{View, timeline};

pub(super) fn window(view: View, day: NaiveDate, selection: (NaiveDate, NaiveDate)) -> DateSpan {
    let (start, through) = match view {
        View::Month => {
            let first = day.with_day(1).expect("valid day of month");
            let last = first
                .iter_days()
                .take_while(|date| date.month() == first.month())
                .last()
                .expect("month begins at the first day");
            (first, last)
        }
        View::Week => (monday_of(day), monday_of(day) + Days::new(6)),
        View::Day => (day, day),
        View::Agenda => selection,
        View::Timeline => {
            let start = timeline::window_start(day);
            (start, start + Days::new(timeline::WINDOW_DAYS - 1))
        }
        _ => unreachable!("only calendar views use a date window"),
    };
    DateSpan::new(
        start.max(NaiveDate::from_ymd_opt(1, 1, 1).unwrap()),
        through.min(NaiveDate::from_ymd_opt(9999, 12, 31).unwrap()),
    )
    .expect("calendar windows contain a day in the display years")
}

/// The most recently queried service window, including errors. Drawing and
/// input share it until the window changes or a reload invalidates it.
pub(super) struct DaemonCalendar {
    paths: WorkspacePaths,
    cached: RefCell<Option<WindowResult>>,
}

struct WindowResult {
    dates: DateSpan,
    timezone: Tz,
    items: Result<Vec<CalendarItem>, String>,
}

impl DaemonCalendar {
    pub(super) fn new(paths: WorkspacePaths) -> Self {
        Self {
            paths,
            cached: RefCell::new(None),
        }
    }

    pub(super) fn entries(
        &self,
        dates: DateSpan,
        timezone: Tz,
    ) -> anyhow::Result<Vec<CalendarItem>> {
        let mut cached = self.cached.borrow_mut();
        if let Some(result) = cached.as_ref()
            && result.dates == dates
            && result.timezone == timezone
        {
            return result.items.clone().map_err(anyhow::Error::msg);
        }
        let items = crate::daemon_calendar(&self.paths, dates, timezone)
            .map_err(|error| format!("{error:#}"));
        let result = items.clone().map_err(anyhow::Error::msg);
        *cached = Some(WindowResult {
            dates,
            timezone,
            items,
        });
        result
    }

    pub(super) fn invalidate(&mut self) {
        self.cached.get_mut().take();
    }
}

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

pub(super) fn sort_day(entries: &mut [CalendarItem], zone: Tz) {
    entries.sort_by_key(|entry| (lane(entry), start(entry, zone)));
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
    timezone: Tz,
    entries: Vec<CalendarItem>,
    columns: [Vec<usize>; 7],
}

pub(super) fn monday_of(date: NaiveDate) -> NaiveDate {
    date - Days::new(u64::from(date.weekday().num_days_from_monday()))
}

impl Week {
    pub(super) fn from_entries(date: NaiveDate, zone: Tz, mut entries: Vec<CalendarItem>) -> Self {
        let monday = monday_of(date);
        let days = std::array::from_fn(|offset| monday + Days::new(offset as u64));
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
            timezone: zone,
            entries,
            columns,
        }
    }

    pub(super) fn timezone(&self) -> Tz {
        self.timezone
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
