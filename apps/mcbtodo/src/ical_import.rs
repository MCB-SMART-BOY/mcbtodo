use std::{fs, path::Path};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use icalendar::parser::{Component, Property, read_calendar};
use mcbtodo_application::CalendarEventDraft;
use mcbtodo_domain::{
    Availability, CalendarEvent, Id,
    time::{DateSpan, DateTimeSpan, Span},
};

const MAX_ICAL_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) fn parse_file(path: &Path) -> Result<Vec<CalendarEventDraft>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("reading iCalendar metadata from {}", path.display()))?;
    ensure!(
        metadata.len() <= MAX_ICAL_BYTES,
        "iCalendar input exceeds the 16 MiB limit"
    );
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= MAX_ICAL_BYTES,
        "iCalendar input exceeds the 16 MiB limit"
    );
    let source = std::str::from_utf8(&bytes).context("iCalendar input must be UTF-8")?;
    parse(source)
}

fn parse(source: &str) -> Result<Vec<CalendarEventDraft>> {
    let calendar = read_calendar(source)
        .map_err(|error| anyhow::anyhow!(error))
        .context("parsing iCalendar input")?;
    ensure!(
        calendar
            .properties
            .iter()
            .any(|property| property.name.as_str() == "VERSION" && property.val.as_str() == "2.0"),
        "iCalendar VERSION must be 2.0"
    );

    let mut events = Vec::new();
    let mut uids = std::collections::BTreeSet::new();
    for component in &calendar.components {
        match component.name.as_str() {
            "VEVENT" => {
                let request = parse_event(component)?;
                let uid = required_property(component, "UID")?.val.as_str();
                ensure!(
                    uids.insert(uid.to_owned()),
                    "duplicate iCalendar UID {uid:?}"
                );
                events.push(request);
            }
            "VTIMEZONE" => {
                bail!("embedded VTIMEZONE definitions are unsupported; use IANA TZID values")
            }
            name => bail!("unsupported iCalendar component {name}"),
        }
    }
    ensure!(
        !events.is_empty(),
        "iCalendar input contains no VEVENT components"
    );
    Ok(events)
}

fn parse_event(component: &Component<'_>) -> Result<CalendarEventDraft> {
    ensure!(
        component.components.is_empty(),
        "VEVENT subcomponents such as VALARM are unsupported"
    );
    let mut property_names = std::collections::BTreeSet::new();
    for property in &component.properties {
        ensure!(
            property_names.insert(property.name.as_str()),
            "VEVENT contains duplicate {} properties",
            property.name.as_str()
        );
    }
    for key in ["RRULE", "RDATE", "EXDATE", "RECURRENCE-ID", "DURATION"] {
        ensure!(
            !component
                .properties
                .iter()
                .any(|property| property.name.as_str() == key),
            "VEVENT property {key} is unsupported"
        );
    }

    const ALLOWED: &[&str] = &[
        "UID",
        "SUMMARY",
        "DTSTART",
        "DTEND",
        "DESCRIPTION",
        "DTSTAMP",
        "CREATED",
        "LAST-MODIFIED",
        "SEQUENCE",
        "STATUS",
        "CLASS",
        "TRANSP",
    ];
    for property in &component.properties {
        ensure!(
            ALLOWED.contains(&property.name.as_str())
                || matches!(property.name.as_str(), "X-MCBTODO-KIND" | "X-MCBTODO-ID"),
            "VEVENT property {} is unsupported",
            property.name.as_str()
        );
    }

    let uid = required_property(component, "UID")?.val.as_str();
    ensure!(!uid.trim().is_empty(), "VEVENT UID must not be empty");
    let title = unescape_text(required_property(component, "SUMMARY")?.val.as_str());
    ensure!(!title.trim().is_empty(), "VEVENT SUMMARY must not be empty");
    let start = required_property(component, "DTSTART")?;
    let end = component
        .properties
        .iter()
        .find(|property| property.name.as_str() == "DTEND");
    let when = match parse_temporal(start)? {
        Temporal::Date(start_date) => {
            let through = match end {
                Some(end) => match parse_temporal(end)? {
                    Temporal::Date(exclusive_end) => exclusive_end
                        .pred_opt()
                        .context("DTEND date is outside the supported range")?,
                    Temporal::DateTime { .. } => bail!("DTSTART and DTEND must both be DATE"),
                },
                None => start_date,
            };
            Span::DateSpan(DateSpan::new(start_date, through)?)
        }
        Temporal::DateTime {
            value: start_time,
            timezone: start_zone,
        } => {
            let end = end.context("DATE-TIME DTSTART requires DTEND")?;
            let Temporal::DateTime {
                value: end_time,
                timezone: end_zone,
            } = parse_temporal(end)?
            else {
                bail!("DTSTART and DTEND must both be DATE-TIME");
            };
            ensure!(
                start_zone == end_zone,
                "DTSTART and DTEND timezones must match"
            );
            Span::DateTimeSpan(DateTimeSpan::new(start_time, end_time, start_zone)?)
        }
    };

    let description = component
        .properties
        .iter()
        .find(|property| property.name.as_str() == "DESCRIPTION")
        .map(|property| unescape_text(property.val.as_str()))
        .unwrap_or_default();
    let id = Id::new_v5(&Id::NAMESPACE_URL, format!("mcbtodo:ical:{uid}").as_bytes());
    Ok(CalendarEventDraft {
        event: CalendarEvent {
            id,
            when,
            recurrence: None,
            availability: Availability::Busy,
            tags: Vec::new(),
        },
        title,
        body: if description.is_empty() {
            String::new()
        } else {
            format!("## Description\n\n{description}\n")
        },
    })
}

enum Temporal {
    Date(NaiveDate),
    DateTime {
        value: DateTime<chrono::FixedOffset>,
        timezone: Tz,
    },
}

fn parse_temporal(property: &Property<'_>) -> Result<Temporal> {
    let value_type = property
        .params
        .iter()
        .find(|parameter| parameter.key.as_str() == "VALUE")
        .and_then(|parameter| parameter.val.as_ref())
        .map(|value| value.as_str());
    let tzid = property
        .params
        .iter()
        .find(|parameter| parameter.key.as_str() == "TZID")
        .and_then(|parameter| parameter.val.as_ref())
        .map(|value| value.as_str());
    if value_type == Some("DATE") {
        ensure!(tzid.is_none(), "DATE values cannot have a TZID");
        return NaiveDate::parse_from_str(property.val.as_str(), "%Y%m%d")
            .map(Temporal::Date)
            .context("invalid iCalendar DATE value");
    }
    ensure!(
        value_type.is_none() || value_type == Some("DATE-TIME"),
        "unsupported iCalendar temporal VALUE"
    );

    let raw = property.val.as_str();
    if let Some(utc) = raw.strip_suffix('Z') {
        ensure!(tzid.is_none(), "UTC DATE-TIME must not have TZID");
        let naive = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S")
            .context("invalid UTC DATE-TIME value")?;
        let instant = Utc.from_utc_datetime(&naive);
        return Ok(Temporal::DateTime {
            value: instant.fixed_offset(),
            timezone: chrono_tz::UTC,
        });
    }
    let tzid = tzid.context("floating DATE-TIME values are not supported")?;
    let timezone = tzid
        .parse::<Tz>()
        .with_context(|| format!("TZID {tzid:?} is not a recognized IANA timezone"))?;
    let local = NaiveDateTime::parse_from_str(raw, "%Y%m%dT%H%M%S")
        .context("invalid local DATE-TIME value")?;
    let value = match timezone.from_local_datetime(&local) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(_, _) => bail!("ambiguous local DATE-TIME {raw} in {tzid}"),
        LocalResult::None => bail!("nonexistent local DATE-TIME {raw} in {tzid}"),
    };
    Ok(Temporal::DateTime {
        value: value.fixed_offset(),
        timezone,
    })
}

fn required_property<'a>(component: &'a Component<'_>, key: &str) -> Result<&'a Property<'a>> {
    component
        .properties
        .iter()
        .find(|property| property.name.as_str() == key)
        .with_context(|| format!("VEVENT requires {key}"))
}

fn unescape_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => output.push('\n'),
                Some(next) => output.push(next),
                None => output.push('\\'),
            }
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcbtodo_domain::time::Span;

    fn event(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n")
    }

    #[test]
    fn imports_exclusive_all_day_end_and_defaults_missing_end_to_one_day() {
        let source = event(
            "BEGIN:VEVENT\r\nUID:one@example.test\r\nSUMMARY:Multi-day\r\nDTSTART;VALUE=DATE:20261120\r\nDTEND;VALUE=DATE:20261123\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:two@example.test\r\nSUMMARY:Single-day\r\nDTSTART;VALUE=DATE:20261125\r\nEND:VEVENT\r\n",
        );
        let events = parse(&source).unwrap();
        let Span::DateSpan(first) = events[0].event.when else {
            panic!("expected date span");
        };
        assert_eq!(first.through(), "2026-11-22".parse().unwrap());
        let Span::DateSpan(second) = events[1].event.when else {
            panic!("expected date span");
        };
        assert_eq!(second.start(), second.through());
    }

    #[test]
    fn imports_utc_and_iana_timezone_instants() {
        let source = event(
            "BEGIN:VEVENT\r\nUID:utc@example.test\r\nSUMMARY:UTC\r\nDTSTART:20261120T010000Z\r\nDTEND:20261120T020000Z\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:local@example.test\r\nSUMMARY:Local\r\nDTSTART;TZID=Asia/Tokyo:20261120T100000\r\nDTEND;TZID=Asia/Tokyo:20261120T110000\r\nEND:VEVENT\r\n",
        );
        let events = parse(&source).unwrap();
        let Span::DateTimeSpan(local) = events[1].event.when else {
            panic!("expected date-time span");
        };
        assert_eq!(local.start().offset().local_minus_utc(), 9 * 60 * 60);
    }

    #[test]
    fn rejects_recurrence_floating_ambiguous_and_duplicate_uids() {
        let recurring = event(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART;VALUE=DATE:20261120\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n",
        );
        assert!(parse(&recurring).is_err());
        let floating = event(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART:20261120T100000\r\nDTEND:20261120T110000\r\nEND:VEVENT\r\n",
        );
        assert!(parse(&floating).is_err());
        let ambiguous = event(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART;TZID=America/New_York:20261101T013000\r\nDTEND;TZID=America/New_York:20261101T023000\r\nEND:VEVENT\r\n",
        );
        assert!(parse(&ambiguous).is_err());
        let duplicate = event(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART;VALUE=DATE:20261120\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:B\r\nDTSTART;VALUE=DATE:20261121\r\nEND:VEVENT\r\n",
        );
        assert!(parse(&duplicate).is_err());
    }

    #[test]
    fn rejects_unrecognized_extension_properties() {
        let source = event(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART;VALUE=DATE:20261120\r\nX-OTHER:ignored-data\r\nEND:VEVENT\r\n",
        );
        assert!(parse(&source).is_err());
    }

    #[test]
    fn rejects_duplicate_optional_properties_and_embedded_timezone_definitions() {
        let duplicate = event(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART;VALUE=DATE:20261120\r\nDESCRIPTION:first\r\nDESCRIPTION:second\r\nEND:VEVENT\r\n",
        );
        assert!(parse(&duplicate).is_err());

        let timezone = event(
            "BEGIN:VTIMEZONE\r\nTZID:Asia/Tokyo\r\nEND:VTIMEZONE\r\n\
             BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\n\
             DTSTART;TZID=Asia/Tokyo:20261120T100000\r\n\
             DTEND;TZID=Asia/Tokyo:20261120T110000\r\nEND:VEVENT\r\n",
        );
        assert!(parse(&timezone).is_err());
    }
}
