use std::{fs, os::unix::fs::PermissionsExt};

use chrono::Utc;
use mcbtodo_protocol::workflow::{Missed, Overlap, ScheduleSummary, Trigger};

use super::*;
use crate::run_store::RunStore;

fn complete(store: &RunStore, id: Uuid, state: RunState) -> Result<()> {
    store.insert(id, "published-revision", None)?;
    store.transition(&store.get(id)?.unwrap(), state, None, None)
}

#[test]
fn notification_pages_follow_completion_order_and_exclude_newer_arrivals() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
    let ids: Vec<_> = (1..=14).map(Uuid::from_u128).collect();
    for id in &ids {
        store.insert(*id, "published-revision", None)?;
    }
    for id in ids.iter().rev() {
        store.transition(
            &store.get(*id)?.unwrap(),
            RunState::Failed,
            None,
            Some("x".repeat(4_000).as_str()),
        )?;
    }

    let inbox = store.notifications();
    let first = inbox.page(None)?;
    assert_eq!(entry_ids(&first), ids[..12]);
    assert_eq!(first.next_before, Some(ids[11]));
    assert_eq!(first.notifications[0].detail.as_ref().unwrap().len(), 512);
    complete(&store, Uuid::from_u128(15), RunState::Succeeded)?;
    let second = inbox.page(first.next_before)?;
    assert_eq!(entry_ids(&second), ids[12..]);
    assert_eq!(second.next_before, None);
    Ok(())
}

#[test]
fn notification_identifies_the_schedule_that_created_the_run() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
    let now = Utc::now();
    let schedule_id = Uuid::from_u128(1);
    store.insert_schedule(&ScheduleSummary {
        id: schedule_id,
        revision: "published-revision".into(),
        trigger: Trigger::Interval {
            anchor: now,
            seconds: 60,
        },
        missed: Missed::RunOnce,
        overlap: Overlap::Queue,
        next_at: Some(now),
        enabled: true,
        missed_occurrences: 0,
        overlap_skipped: 0,
        cascade_skipped: 0,
    })?;
    let after_due = now + chrono::TimeDelta::milliseconds(10);
    store.schedule_tick(after_due)?;
    let runs = store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    store.transition(&runs[0], RunState::Succeeded, None, None)?;

    let page = store.notifications().page(None)?;
    assert_eq!(entry_ids(&page), [runs[0].id]);
    assert_eq!(page.notifications[0].schedule_id, Some(schedule_id));
    assert_eq!(page.notifications[0].revision, "published-revision");
    Ok(())
}

#[test]
fn reminder_occurrences_are_atomic_and_deduplicated_across_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("workflow.sqlite3");
    let now = Utc::now();
    let due: Vec<_> = (1..=14)
        .map(|hour| DueReminder {
            reminder_id: Uuid::from_u128(1),
            target_id: Some(Uuid::from_u128(2)),
            title: format!("Reminder {hour}"),
            scheduled_at: now - chrono::TimeDelta::hours(hour),
        })
        .collect();
    let store = RunStore::open(&path)?;
    let inbox = store.notifications();
    inbox.0.execute_batch(
        "CREATE TRIGGER fail_test_reminder BEFORE INSERT ON reminder_notifications
         WHEN NEW.title='Reminder 2' BEGIN SELECT RAISE(ABORT, 'test'); END;",
    )?;
    assert!(inbox.publish_reminders(&due, now).is_err());
    assert!(inbox.reminder_page(None)?.notifications.is_empty());
    inbox.0.execute_batch("DROP TRIGGER fail_test_reminder")?;
    let published = inbox.publish_reminders(&due, now)?;
    assert_eq!(published.len(), 14);
    assert!(published.iter().all(|entry| entry.created_at == now));
    let queued: i64 = inbox.0.query_row(
        "SELECT count(*) FROM reminder_delivery_outbox WHERE state='pending'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(queued, 14);
    let first = inbox.reminder_page(None)?;
    assert_eq!(
        first.notifications,
        published[2..].iter().rev().cloned().collect::<Vec<_>>()
    );
    drop(store);

    let store = RunStore::open(&path)?;
    let inbox = store.notifications();
    assert!(inbox.publish_reminders(&due, now)?.is_empty());
    assert_eq!(inbox.reminder_page(None)?, first);
    let new = DueReminder {
        reminder_id: due[0].reminder_id,
        target_id: None,
        title: "New occurrence".into(),
        scheduled_at: now,
    };
    assert_eq!(inbox.publish_reminders(&[new], now)?.len(), 1);
    let second = inbox.reminder_page(first.next_before)?;
    assert_eq!(
        second.notifications,
        [published[1].clone(), published[0].clone()]
    );
    assert_eq!(second.next_before, None);
    Ok(())
}

#[test]
fn reminder_delivery_retries_after_restart_and_success_is_not_repeated() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("workflow.sqlite3");
    let helper = dir.path().join("notify-send");
    let log = helper.with_extension("log");
    fs::write(&helper, "#!/bin/sh\nprintf '%s\\n' \"$4\" >> \"$0.log\"\n")?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
    let reminder = DueReminder {
        reminder_id: Uuid::from_u128(21),
        target_id: None,
        title: "Durable reminder".into(),
        scheduled_at: Utc::now(),
    };

    let store = RunStore::open(&path)?;
    let inbox = store.notifications();
    inbox.publish_reminders(&[reminder], Utc::now())?;
    let mut desktop = DesktopNotifications::start(inbox, dir.path().join("missing-helper").into())?;
    desktop.dispatch_reminders(inbox)?;
    wait_for_delivery_state(inbox, "pending", 1, &mut desktop)?;
    assert_eq!(inbox.reminder_page(None)?.notifications.len(), 1);
    drop(desktop);
    drop(store);

    let store = RunStore::open(&path)?;
    let inbox = store.notifications();
    inbox.recover_reminder_deliveries()?;
    inbox
        .0
        .execute("UPDATE reminder_delivery_outbox SET next_attempt_ms=0", [])?;
    let mut desktop = DesktopNotifications::start(inbox, helper.into())?;
    desktop.dispatch_reminders(inbox)?;
    wait_for_delivery_state(inbox, "delivered", 2, &mut desktop)?;
    desktop.dispatch_reminders(inbox)?;
    thread::sleep(Duration::from_millis(50));
    desktop.dispatch_reminders(inbox)?;
    assert_eq!(fs::read_to_string(log)?, "Durable reminder\n");
    assert_eq!(inbox.reminder_page(None)?.notifications.len(), 1);
    Ok(())
}

#[test]
fn nonzero_helper_exit_is_retried_and_eventually_stops() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("workflow.sqlite3");
    let helper = dir.path().join("notify-send");
    fs::write(&helper, "#!/bin/sh\nexit 7\n")?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
    let store = RunStore::open(&path)?;
    let inbox = store.notifications();
    inbox.publish_reminders(
        &[DueReminder {
            reminder_id: Uuid::from_u128(31),
            target_id: None,
            title: "Nonzero".into(),
            scheduled_at: Utc::now(),
        }],
        Utc::now(),
    )?;
    let mut desktop = DesktopNotifications::start(inbox, helper.into())?;
    for attempt in 1..=REMINDER_MAX_ATTEMPTS {
        inbox.0.execute(
            "UPDATE reminder_delivery_outbox SET next_attempt_ms=0
             WHERE state='pending'",
            [],
        )?;
        desktop.dispatch_reminders(inbox)?;
        wait_for_delivery_state(
            inbox,
            if attempt == REMINDER_MAX_ATTEMPTS {
                "failed"
            } else {
                "pending"
            },
            attempt,
            &mut desktop,
        )?;
    }
    assert_eq!(inbox.reminder_page(None)?.notifications.len(), 1);
    Ok(())
}

#[test]
fn timed_out_helper_is_not_marked_delivered() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("workflow.sqlite3");
    let helper = dir.path().join("notify-send");
    fs::write(&helper, "#!/bin/sh\nexec /bin/sleep 60\n")?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
    let store = RunStore::open(&path)?;
    let inbox = store.notifications();
    inbox.publish_reminders(
        &[DueReminder {
            reminder_id: Uuid::from_u128(41),
            target_id: None,
            title: "Timeout".into(),
            scheduled_at: Utc::now(),
        }],
        Utc::now(),
    )?;
    let mut desktop = DesktopNotifications::start(inbox, helper.into())?;
    desktop.dispatch_reminders(inbox)?;
    wait_for_delivery_state(inbox, "pending", 1, &mut desktop)?;
    let error: String = inbox.0.query_row(
        "SELECT last_error FROM reminder_delivery_outbox",
        [],
        |row| row.get(0),
    )?;
    assert!(error.contains("timed out"));
    assert_eq!(inbox.reminder_page(None)?.notifications.len(), 1);
    Ok(())
}

#[test]
fn a_hung_desktop_helper_leaves_the_inbox_intact_and_is_not_replayed() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("workflow.sqlite3");
    let helper = dir.path().join("notify-send");
    let deliveries = dir.path().join("notify-send.log");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s|%s\\n' \"$3\" \"$4\" >> \"$0.log\"\n\
         if [ \"$3\" = 'Workflow failed' ]; then exec /bin/sleep 60; fi\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
    let store = RunStore::open(&path)?;
    let mut desktop = DesktopNotifications::start(store.notifications(), helper.clone().into())?;
    let failed = Uuid::from_u128(1);
    let succeeded = Uuid::from_u128(2);
    complete(&store, failed, RunState::Failed)?;
    complete(&store, succeeded, RunState::Succeeded)?;
    desktop.dispatch(store.notifications())?;
    desktop.dispatch(store.notifications())?;
    assert_eq!(
        entry_ids(&store.notifications().page(None)?),
        [succeeded, failed],
    );
    wait_for_deliveries(&deliveries, 2)?;

    drop(desktop);
    drop(store);
    let store = RunStore::open(&path)?;
    let mut desktop = DesktopNotifications::start(store.notifications(), helper.into())?;
    let cancelled = Uuid::from_u128(3);
    complete(&store, cancelled, RunState::Cancelled)?;
    desktop.dispatch(store.notifications())?;
    assert_eq!(
        wait_for_deliveries(&deliveries, 3)?,
        format!(
            "Workflow failed|Run {failed}\nWorkflow succeeded|Run {succeeded}\n\
             Workflow cancelled|Run {cancelled}\n"
        ),
    );
    assert_eq!(
        entry_ids(&store.notifications().page(None)?),
        [cancelled, succeeded, failed],
    );
    Ok(())
}

fn entry_ids(page: &NotificationPage) -> Vec<Uuid> {
    page.notifications
        .iter()
        .map(|entry| entry.run_id)
        .collect()
}

fn wait_for_deliveries(path: &std::path::Path, count: usize) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let contents = fs::read_to_string(path).unwrap_or_default();
        if contents.bytes().filter(|byte| *byte == b'\n').count() >= count {
            return Ok(contents);
        }
        anyhow::ensure!(Instant::now() < deadline, "desktop delivery stalled");
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_delivery_state(
    inbox: Inbox<'_>,
    state: &str,
    attempts: u32,
    desktop: &mut DesktopNotifications,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        desktop.dispatch_reminders(inbox)?;
        let actual: (String, u32) = inbox.0.query_row(
            "SELECT state, attempt_count FROM reminder_delivery_outbox",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if actual == (state.to_owned(), attempts) {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "delivery state did not converge: {actual:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
