use chrono_tz::Tz;
use mcbtodo_application::CalendarItem;
use mcbtodo_domain::{Occurrence, time::Span};

pub(crate) fn item_label(entry: &CalendarItem, timezone: Tz) -> String {
    let busy = if entry.busy { "busy" } else { "free" };
    let mut label = format!("{} · {busy} · {}", entry.kind.label(), entry.title);
    match entry.occurrence {
        Occurrence::Span(Span::DateTimeSpan(span)) => {
            let start = span.start().with_timezone(&timezone);
            let end = span.end().with_timezone(&timezone);
            label.push_str(&format!(
                " · {} → {}",
                start.format("%m-%d %H:%M %:z"),
                end.format("%m-%d %H:%M %:z")
            ));
        }
        Occurrence::Span(Span::DateSpan(span)) => {
            label.push_str(&format!(" · {}", span.start()));
            if span.start() != span.through() {
                label.push_str(&format!(" → {}", span.through()));
            }
        }
        Occurrence::Deadline(deadline) => {
            if let Some(at) = deadline.datetime(timezone) {
                label.push_str(&format!(" · {}", at.format("%Y-%m-%d %H:%M %:z")));
            } else if let Some(date) = deadline.local_date(timezone) {
                label.push_str(&format!(" · {date}"));
            }
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset, NaiveDate};
    use mcbtodo_domain::{
        CalendarKind, Id, Occurrence,
        time::{DateSpan, DateTimeSpan, Deadline, Span, ZonedMoment},
    };

    use super::item_label;
    use mcbtodo_application::CalendarItem;

    fn item(occurrence: Occurrence, busy: bool) -> CalendarItem {
        CalendarItem {
            id: Id::nil(),
            title: "Review".into(),
            kind: CalendarKind::Event,
            busy,
            occurrence,
        }
    }

    fn date(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }

    #[test]
    fn formats_single_and_multi_day_spans() {
        let timezone = chrono_tz::UTC;
        let single = item(
            Occurrence::Span(Span::DateSpan(
                DateSpan::new(date("2026-11-21"), date("2026-11-21")).unwrap(),
            )),
            false,
        );
        let multi = item(
            Occurrence::Span(Span::DateSpan(
                DateSpan::new(date("2026-11-21"), date("2026-11-23")).unwrap(),
            )),
            true,
        );

        assert_eq!(
            item_label(&single, timezone),
            "event · free · Review · 2026-11-21"
        );
        assert_eq!(
            item_label(&multi, timezone),
            "event · busy · Review · 2026-11-21 → 2026-11-23"
        );
    }

    #[test]
    fn formats_timed_spans_in_the_requested_timezone() {
        let start = "2026-11-21T09:00:00+00:00"
            .parse::<DateTime<FixedOffset>>()
            .unwrap();
        let end = "2026-11-21T10:30:00+00:00"
            .parse::<DateTime<FixedOffset>>()
            .unwrap();
        let span = DateTimeSpan::new(start, end, chrono_tz::UTC).unwrap();
        let item = item(Occurrence::Span(Span::DateTimeSpan(span)), true);

        assert_eq!(
            item_label(&item, chrono_tz::Asia::Shanghai),
            "event · busy · Review · 11-21 17:00 +08:00 → 11-21 18:30 +08:00"
        );
    }

    #[test]
    fn formats_date_and_timed_deadlines() {
        let date_item = item(
            Occurrence::Deadline(Deadline::Date {
                value: date("2026-11-21"),
            }),
            false,
        );
        let moment = ZonedMoment::new(
            "2026-11-21T09:00:00+00:00"
                .parse::<DateTime<FixedOffset>>()
                .unwrap(),
            chrono_tz::UTC,
        )
        .unwrap();
        let timed_item = item(Occurrence::Deadline(Deadline::DateTime(moment)), false);

        assert_eq!(
            item_label(&date_item, chrono_tz::UTC),
            "event · free · Review · 2026-11-21"
        );
        assert_eq!(
            item_label(&timed_item, chrono_tz::Asia::Shanghai),
            "event · free · Review · 2026-11-21 17:00 +08:00"
        );
    }
}
