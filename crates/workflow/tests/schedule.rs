use std::num::NonZeroUsize;

use chrono::{DateTime, NaiveDate, TimeDelta, TimeZone, Utc};
use chrono_tz::{America::New_York, Asia::Kathmandu, Pacific::Apia, UTC};
use mcbtodo_workflow::schedule::{DstResolution, MissedPolicy, Schedule, ScheduleError};

fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .unwrap()
}

fn replay(limit: usize) -> MissedPolicy {
    MissedPolicy::ReplayLimited {
        limit: NonZeroUsize::new(limit).unwrap(),
    }
}

#[test]
fn once_occurs_only_inside_the_open_closed_window() {
    let at = utc(2026, 11, 20, 12, 0);
    let before = at - TimeDelta::nanoseconds(1);
    let schedule = Schedule::once(at);
    assert_eq!(schedule.next_after(before), Some(at));
    assert_eq!(schedule.next_after(at), None);
    let missed = schedule.missed(before, at, MissedPolicy::RunOnce);
    assert_eq!(missed.total, 1);
    assert_eq!(missed.occurrences, [at]);
    assert_eq!(schedule.missed(at, at, MissedPolicy::RunOnce).total, 0);
    assert_eq!(schedule.missed(at, before, MissedPolicy::RunOnce).total, 0);
}

#[test]
fn date_only_once_uses_midnight_in_its_iana_zone() {
    let date = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
    let schedule = Schedule::once_on(date, Kathmandu, DstResolution::Earlier).unwrap();
    assert_eq!(
        schedule.next_after(utc(2026, 11, 19, 0, 0)),
        Some(utc(2026, 11, 19, 18, 15))
    );
    let skipped = NaiveDate::from_ymd_opt(2011, 12, 30).unwrap();
    assert!(Schedule::once_on(skipped, Apia, DstResolution::Earlier).is_err());
}

#[test]
fn intervals_are_fixed_rate_and_include_the_anchor() {
    let anchor = utc(2026, 11, 20, 12, 0) + TimeDelta::milliseconds(200);
    let period = TimeDelta::milliseconds(500);
    let schedule = Schedule::interval(anchor, period).unwrap();
    assert_eq!(
        schedule.next_after(anchor - TimeDelta::nanoseconds(1)),
        Some(anchor)
    );
    assert_eq!(schedule.next_after(anchor), Some(anchor + period));
    assert_eq!(
        schedule.next_after(anchor + TimeDelta::milliseconds(800)),
        Some(anchor + period * 2)
    );
    let missed = schedule.missed(anchor, anchor + period * 4, replay(2));
    assert_eq!(missed.total, 4);
    assert_eq!(
        missed.occurrences,
        [anchor + period * 3, anchor + period * 4]
    );
}

#[test]
fn interval_needs_a_positive_period() {
    let anchor = utc(2026, 11, 20, 12, 0);
    for period in [TimeDelta::zero(), TimeDelta::seconds(-5)] {
        assert!(matches!(
            Schedule::interval(anchor, period),
            Err(ScheduleError::InvalidInterval)
        ));
    }
}

#[test]
fn interval_recovers_a_large_backlog_with_a_bounded_result() {
    let anchor = utc(2020, 1, 1, 0, 0);
    let period = TimeDelta::nanoseconds(1);
    let schedule = Schedule::interval(anchor, period).unwrap();
    let through = utc(2021, 1, 1, 0, 0);
    let missed = schedule.missed(anchor, through, MissedPolicy::Skip);
    assert_eq!(missed.total, 31_622_400_000_000_000);
    assert!(missed.occurrences.is_empty());
    let missed = schedule.missed(anchor, through, MissedPolicy::RunOnce);
    assert_eq!(missed.occurrences, [through]);
    let missed = schedule.missed(anchor, through, replay(2));
    assert_eq!(missed.occurrences, [through - period, through]);
}

#[test]
fn interval_handles_leap_second_cursors_on_its_posix_timeline() {
    let at = DateTime::from_timestamp(1_483_228_799, 1_500_000_000).unwrap();
    let after = utc(2017, 1, 1, 0, 0);
    let period = TimeDelta::milliseconds(100);
    let schedule = Schedule::interval(after - TimeDelta::seconds(1), period).unwrap();
    assert_eq!(schedule.next_after(at), Some(after));
    let missed = schedule.missed(at, after, MissedPolicy::RunOnce);
    assert_eq!(missed.total, 1);
    assert_eq!(missed.occurrences, [after]);
    assert!(Schedule::interval(at, period).is_err());
}

#[test]
fn cron_supports_ranges_lists_steps_names_and_day_field_or() {
    let schedule = Schedule::cron(
        "5-25/10,50 9 15 JAN,MAR MON-WED",
        UTC,
        DstResolution::Earlier,
    )
    .unwrap();
    let first = utc(2026, 1, 14, 9, 5); // Wednesday
    let second = utc(2026, 1, 15, 9, 5); // Thursday, the 15th
    assert_eq!(schedule.next_after(utc(2026, 1, 13, 10, 0)), Some(first));
    assert_eq!(schedule.next_after(first), Some(utc(2026, 1, 14, 9, 15)));
    assert_eq!(
        schedule.next_after(utc(2026, 1, 14, 9, 25)),
        Some(utc(2026, 1, 14, 9, 50))
    );
    assert_eq!(schedule.next_after(utc(2026, 1, 14, 10, 0)), Some(second));
}

#[test]
fn cron_handles_sunday_aliases_wildcard_day_fields_and_leap_days() {
    let sunday = utc(2026, 11, 22, 0, 0);
    for token in ["0", "7", "sun"] {
        let schedule =
            Schedule::cron(&format!("0 0 * * {token}"), UTC, DstResolution::Earlier).unwrap();
        assert_eq!(schedule.next_after(utc(2026, 11, 20, 0, 0)), Some(sunday));
    }
    let leap = Schedule::cron("0 0 29 FEB *", UTC, DstResolution::Earlier).unwrap();
    assert_eq!(
        leap.next_after(utc(2096, 2, 29, 0, 0)),
        Some(utc(2104, 2, 29, 0, 0))
    );
}

#[test]
fn cron_rejects_invalid_or_impossible_fields() {
    for invalid in [
        "",
        "* * * *",
        "* * * * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * * 13 *",
        "* * * * 8",
        "*/0 * * * *",
        "9-3 * * * *",
        "* * * * MON,",
        "* * * * ? ",
        "0 0 31 FEB *",
        "0 0 * Spring *",
        "-1 * * * *",
        "+1 * * * *",
    ] {
        assert!(
            Schedule::cron(invalid, UTC, DstResolution::Earlier).is_err(),
            "accepted {invalid:?}"
        );
    }
}

#[test]
fn cron_skips_the_spring_gap() {
    let schedule = Schedule::cron("30 2 * * *", New_York, DstResolution::Both).unwrap();
    let after = utc(2026, 3, 7, 7, 30);
    let next = utc(2026, 3, 9, 6, 30);
    assert_eq!(schedule.next_after(after), Some(next));
    let missed = schedule.missed(after, next, replay(5));
    assert_eq!(missed.total, 1);
    assert_eq!(missed.occurrences, [next]);
}

#[test]
fn cron_fold_can_run_earlier_later_or_twice() {
    let after = utc(2026, 11, 1, 5, 0);
    let early = utc(2026, 11, 1, 5, 30);
    let late = utc(2026, 11, 1, 6, 30);
    for (policy, expected) in [
        (DstResolution::Earlier, vec![early]),
        (DstResolution::Later, vec![late]),
        (DstResolution::Both, vec![early, late]),
    ] {
        let schedule = Schedule::cron("30 1 * * *", New_York, policy).unwrap();
        assert_eq!(schedule.next_after(after), Some(expected[0]));
        let missed = schedule.missed(after, late, replay(2));
        assert_eq!(missed.total, expected.len() as u128);
        assert_eq!(missed.occurrences, expected);
    }
}

#[test]
fn cron_orders_the_second_fold_pass_by_utc() {
    let after = utc(2026, 11, 1, 5, 30);
    let through = utc(2026, 11, 1, 6, 0);
    let schedule = Schedule::cron("0,30 1 * * *", New_York, DstResolution::Both).unwrap();
    assert_eq!(schedule.next_after(after), Some(through));
    let missed = schedule.missed(after, through, MissedPolicy::RunOnce);
    assert_eq!(missed.total, 1);
    assert_eq!(missed.occurrences, [through]);
}

#[test]
fn cron_skips_a_whole_deleted_civil_date() {
    let schedule = Schedule::cron("0 0 * * *", Apia, DstResolution::Both).unwrap();
    let after = utc(2011, 12, 29, 10, 0); // Dec 29 midnight
    let next = utc(2011, 12, 30, 10, 0); // Dec 31 midnight
    assert_eq!(schedule.next_after(after), Some(next));
    assert_eq!(schedule.missed(after, next, MissedPolicy::Skip).total, 1);
}

#[test]
fn dense_cron_counts_decades_of_missed_minutes_and_keeps_a_short_tail() {
    let schedule = Schedule::cron("* * * * *", New_York, DstResolution::Both).unwrap();
    let after = utc(2000, 1, 1, 0, 0);
    let through = utc(2040, 1, 1, 0, 0);
    let missed = schedule.missed(after, through, replay(2));
    assert_eq!(missed.total, 21_038_400);
    assert_eq!(missed.occurrences, [utc(2039, 12, 31, 23, 59), through]);
}

#[test]
fn historical_subminute_offsets_preserve_the_local_cron_minute() {
    use chrono_tz::Europe::Paris;

    let schedule = Schedule::cron("0 12 * * *", Paris, DstResolution::Earlier).unwrap();
    let before = utc(1900, 1, 1, 11, 50);
    let next = utc(1900, 1, 1, 11, 50) + TimeDelta::seconds(39);
    assert_eq!(schedule.next_after(before), Some(next));
    assert_eq!(schedule.missed(before, next, replay(1)).occurrences, [next]);
}
