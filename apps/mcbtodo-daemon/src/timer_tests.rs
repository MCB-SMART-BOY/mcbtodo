use super::*;

#[test]
fn pause_resume_and_stop_preserve_elapsed_time_without_double_counting() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("focus.sqlite3");
    let t = Instant::now();
    let work_unit_id = Uuid::new_v4();
    let id = {
        let mut timer = Timer::open(&db)?;
        let running = timer
            .apply(
                Request::Start {
                    work_unit_id: Some(work_unit_id),
                },
                t,
            )?
            .unwrap();
        let id = running.session.unwrap().id;
        let paused = timer
            .apply(Request::Pause, t + Duration::from_secs(3))?
            .unwrap();
        assert_eq!(paused.state, TimerState::Paused);
        assert_eq!(paused.session.as_ref().unwrap().elapsed_seconds, 3);
        assert_eq!(
            timer.apply(Request::Pause, t + Duration::from_secs(13))?,
            Ok(paused)
        );
        id
    };

    let mut timer = Timer::open(&db)?;
    let paused = timer.status(t + Duration::from_secs(20));
    assert_eq!(paused.state, TimerState::Paused);
    assert_eq!(paused.session.as_ref().unwrap().id, id);
    assert_eq!(paused.session.unwrap().elapsed_seconds, 3);
    let resumed = timer
        .apply(
            Request::Start { work_unit_id: None },
            t + Duration::from_secs(20),
        )?
        .unwrap();
    assert_eq!(resumed.session.unwrap().work_unit_id, Some(work_unit_id));
    let stopped = timer
        .apply(Request::Stop, t + Duration::from_secs(22))?
        .unwrap();
    assert_eq!(stopped.state, TimerState::Idle);
    let session = stopped.session.as_ref().unwrap();
    assert_eq!(session.id, id);
    assert_eq!(session.elapsed_seconds, 5);
    assert!(session.stopped_at.is_some());
    assert_eq!(
        timer.apply(Request::Stop, t + Duration::from_secs(40))?,
        Ok(stopped.clone())
    );
    drop(timer);
    let mut timer = Timer::open(&db)?;
    assert_eq!(timer.status(t + Duration::from_secs(99)), stopped);
    let next = timer
        .apply(Request::Start { work_unit_id: None }, t)?
        .unwrap();
    assert_ne!(next.session.unwrap().id, id);
    let count: u32 =
        Connection::open(&db)?
            .query_row("SELECT count(*) FROM focus_sessions", [], |r| r.get(0))?;
    assert_eq!(count, 2);
    Ok(())
}

#[test]
fn interrupted_running_session_freezes_at_the_last_durable_checkpoint() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("focus.sqlite3");
    let t = Instant::now();
    let first_checkpoint = t + Duration::from_secs(5);
    let after_restart = t + Duration::from_secs(3_600);
    let id = {
        let mut timer = Timer::open(&db)?;
        let start = timer
            .apply(Request::Start { work_unit_id: None }, t)?
            .unwrap();
        assert_eq!(timer.checkpoint_due(), Some(first_checkpoint));
        timer.checkpoint(first_checkpoint)?;
        let live = timer.status(t + Duration::from_secs(7));
        assert_eq!(live.session.unwrap().elapsed_seconds, 7);
        start.session.unwrap().id
    };

    let mut timer = Timer::open(&db)?;
    let review = timer.status(after_restart);
    assert_eq!(review.state, TimerState::NeedsReview);
    assert_eq!(review.session.as_ref().unwrap().id, id);
    assert_eq!(review.session.as_ref().unwrap().elapsed_seconds, 5);
    assert!(
        timer
            .apply(Request::Start { work_unit_id: None }, after_restart)?
            .is_err()
    );
    assert_eq!(Timer::open(&db)?.status(after_restart), review);
    let stopped = timer.apply(Request::Stop, after_restart)?.unwrap();
    assert_eq!(stopped.state, TimerState::Idle);
    assert_eq!(stopped.session.unwrap().elapsed_seconds, 5);
    Ok(())
}

#[test]
fn another_work_unit_cannot_replace_an_active_session() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let t = Instant::now();
    let mut timer = Timer::open(&dir.path().join("focus.sqlite3"))?;
    let first = Request::Start {
        work_unit_id: Some(Uuid::new_v4()),
    };
    let running = timer.apply(first.clone(), t)?.unwrap();
    assert_eq!(timer.apply(first, t)?, Ok(running.clone()));
    assert!(
        timer
            .apply(
                Request::Start {
                    work_unit_id: Some(Uuid::new_v4())
                },
                t
            )?
            .is_err()
    );
    assert_eq!(timer.status(t), running);
    Ok(())
}

#[test]
fn daemon_protocol_accepts_daily_focus_report_queries() {
    let request = serde_json::from_value::<Request>(serde_json::json!({
        "command": "focus_report",
        "date": "2026-11-20",
        "timezone": "Asia/Shanghai"
    }))
    .unwrap();
    assert_eq!(
        request,
        Request::FocusReport {
            date: "2026-11-20".parse().unwrap(),
            timezone: "Asia/Shanghai".into(),
        }
    );
}

#[test]
fn daily_report_groups_durable_history_by_work_unit_on_the_local_start_date() -> anyhow::Result<()>
{
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("focus.sqlite3");
    let time = Instant::now();
    let unit = Uuid::new_v4();
    {
        let mut timer = Timer::open(&db)?;
        for (work_unit_id, milliseconds) in [(Some(unit), 1_600), (Some(unit), 2_500), (None, 950)]
        {
            let start = timer.apply(Request::Start { work_unit_id }, time)?.unwrap();
            timer
                .apply(Request::Stop, time + Duration::from_millis(milliseconds))?
                .unwrap();
            // The session starts on Nov 19 UTC (Nov 20 in Shanghai) and stops
            // on Nov 21 in Shanghai.
            timer.db.execute(
                "UPDATE focus_sessions SET started_at = ?1, stopped_at = ?2 WHERE id = ?3",
                params![
                    "2026-11-19T16:59:59+00:00",
                    "2026-11-20T16:30:00+00:00",
                    start.session.unwrap().id.to_string()
                ],
            )?;
        }
    }
    let timer = Timer::open(&db)?;
    let day = "2026-11-20".parse()?;
    let report = timer.daily_report(day, chrono_tz::Asia::Shanghai, time)?;
    assert_eq!(report.session_count, 3);
    assert_eq!(report.total_elapsed_seconds, 5);
    assert_eq!(report.by_work_unit.len(), 2);
    let assigned = report
        .by_work_unit
        .iter()
        .find(|row| row.work_unit_id == Some(unit))
        .unwrap();
    assert_eq!(assigned.session_count, 2);
    assert_eq!(assigned.elapsed_seconds, 4);
    let unassigned = report
        .by_work_unit
        .iter()
        .find(|row| row.work_unit_id.is_none())
        .unwrap();
    assert_eq!(unassigned.session_count, 1);
    assert_eq!(unassigned.elapsed_seconds, 0);
    assert!(
        timer
            .daily_report(day, chrono_tz::UTC, time)?
            .by_work_unit
            .is_empty()
    );
    assert_eq!(
        timer
            .daily_report(day.succ_opt().unwrap(), chrono_tz::Asia::Shanghai, time)?
            .session_count,
        0
    );
    Ok(())
}

#[test]
fn daily_report_uses_live_elapsed_until_restart_then_only_the_checkpoint() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("focus.sqlite3");
    let time = Instant::now();
    let checkpoint = time + Duration::from_secs(5);
    let day = {
        let mut timer = Timer::open(&db)?;
        let start = timer
            .apply(Request::Start { work_unit_id: None }, time)?
            .unwrap();
        let day = start.session.unwrap().started_at.date_naive();
        timer.checkpoint(checkpoint)?;
        let live = timer.daily_report(day, chrono_tz::UTC, time + Duration::from_secs(8))?;
        assert_eq!(live.total_elapsed_seconds, 8);
        assert_eq!(live.needs_review_count, 0);
        day
    };
    let timer = Timer::open(&db)?;
    let recovered = timer.daily_report(day, chrono_tz::UTC, time + Duration::from_secs(3_600))?;
    assert_eq!(recovered.total_elapsed_seconds, 5);
    assert_eq!(recovered.needs_review_count, 1);
    assert_eq!(recovered.by_work_unit[0].needs_review_count, 1);
    Ok(())
}
