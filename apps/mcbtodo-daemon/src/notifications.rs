use std::{
    collections::HashSet,
    ffi::OsString,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use mcbtodo_protocol::{
    notifications::{
        NotificationPage, ReminderNotification, ReminderNotificationPage, WorkflowNotification,
    },
    workflow::RunState,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use uuid::Uuid;

use crate::run_store::{decode_error, parse_state};

pub mod reminders;

const PAGE_SIZE: u32 = 12;
const POPUP_QUEUE_SIZE: usize = 8;
const POPUP_TIMEOUT: Duration = Duration::from_secs(2);
const REMINDER_MAX_ATTEMPTS: u32 = 5;
const REMINDER_BATCH_SIZE: usize = 64;
const REMINDER_RETRY_BASE: Duration = Duration::from_secs(1);

// Store only inbox ordering and completion time. The run carries the outcome and detail.
// Install the trigger before recovery so interrupted runs enter the inbox atomically.
pub fn install(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflow_notifications (
                 sequence INTEGER PRIMARY KEY,
                 run_id TEXT NOT NULL UNIQUE REFERENCES workflow_runs(id),
                 finished_at TEXT NOT NULL
             ) STRICT;
             CREATE TRIGGER IF NOT EXISTS workflow_notify_terminal
                 AFTER UPDATE OF state ON workflow_runs
                 WHEN OLD.state NOT IN ('succeeded','failed','cancelled','outcome_unknown')
                  AND NEW.state IN ('succeeded','failed','cancelled','outcome_unknown')
                 BEGIN
                     INSERT INTO workflow_notifications(run_id, finished_at)
                     VALUES(NEW.id, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
                 END;
             CREATE TABLE IF NOT EXISTS reminder_notifications (
                 sequence INTEGER PRIMARY KEY,
                 id TEXT NOT NULL UNIQUE,
                 reminder_id TEXT NOT NULL,
                 target_id TEXT,
                 title TEXT NOT NULL,
                 scheduled_at TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(reminder_id, scheduled_at)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS reminder_delivery_outbox (
                 notification_id TEXT PRIMARY KEY NOT NULL
                     REFERENCES reminder_notifications(id) ON DELETE CASCADE,
                 state TEXT NOT NULL CHECK (state IN ('pending','in_flight','delivered','failed')),
                 attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
                 next_attempt_ms INTEGER NOT NULL DEFAULT 0,
                 lease_until_ms INTEGER,
                 last_error TEXT,
                 CHECK ((state='in_flight') = (lease_until_ms IS NOT NULL))
             ) STRICT;
             CREATE INDEX IF NOT EXISTS reminder_delivery_due
                 ON reminder_delivery_outbox(state, next_attempt_ms);",
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
pub struct Inbox<'db>(pub &'db Connection);

impl Inbox<'_> {
    pub fn publish_reminders(
        &self,
        due: &[DueReminder],
        now: DateTime<Utc>,
    ) -> Result<Vec<ReminderNotification>> {
        let tx = self.0.unchecked_transaction()?;
        let created_at = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
        let mut published = Vec::with_capacity(due.len());
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO reminder_notifications
                     (id, reminder_id, target_id, title, scheduled_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(reminder_id, scheduled_at) DO NOTHING",
            )?;
            for occurrence in due {
                let id = Uuid::new_v4();
                if insert.execute(params![
                    id.to_string(),
                    occurrence.reminder_id.to_string(),
                    occurrence.target_id.map(|id| id.to_string()),
                    occurrence.title,
                    occurrence
                        .scheduled_at
                        .to_rfc3339_opts(SecondsFormat::Nanos, true),
                    created_at,
                ])? != 0
                {
                    tx.execute(
                        "INSERT INTO reminder_delivery_outbox(notification_id, state)
                         VALUES (?1, 'pending')",
                        [id.to_string()],
                    )?;
                    published.push(ReminderNotification {
                        id,
                        reminder_id: occurrence.reminder_id,
                        target_id: occurrence.target_id,
                        title: occurrence.title.clone(),
                        scheduled_at: occurrence.scheduled_at,
                        created_at: now,
                    });
                }
            }
        }
        tx.commit()?;
        Ok(published)
    }

    pub fn recover_reminder_deliveries(&self) -> Result<()> {
        self.0.execute(
            "UPDATE reminder_delivery_outbox SET state='pending', lease_until_ms=NULL
             WHERE state='in_flight'",
            [],
        )?;
        Ok(())
    }

    fn claim_reminder_deliveries(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ReminderDelivery>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let tx = self.0.unchecked_transaction()?;
        let now_ms = now.timestamp_millis();
        let lease_until_ms = now_ms + POPUP_TIMEOUT.as_millis() as i64 + 5_000;
        let mut statement = tx.prepare_cached(
            "SELECT o.notification_id, n.title, o.attempt_count
             FROM reminder_delivery_outbox o
             JOIN reminder_notifications n ON n.id=o.notification_id
             WHERE o.state='pending' AND o.next_attempt_ms <= ?1
               AND o.attempt_count < ?2
             ORDER BY n.sequence LIMIT ?3",
        )?;
        let candidates = statement
            .query_map(
                params![now_ms, REMINDER_MAX_ATTEMPTS, limit as u32],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u32>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut claimed = Vec::with_capacity(candidates.len());
        for (id, title, attempt_count) in candidates {
            let attempt = attempt_count + 1;
            let changed = tx.execute(
                "UPDATE reminder_delivery_outbox
                 SET state='in_flight', attempt_count=?2, lease_until_ms=?3
                 WHERE notification_id=?1 AND state='pending'",
                params![id, attempt, lease_until_ms],
            )?;
            if changed != 0 {
                claimed.push(ReminderDelivery {
                    id: id.parse()?,
                    title,
                });
            }
        }
        tx.commit()?;
        Ok(claimed)
    }

    fn finish_reminder_delivery(
        &self,
        id: Uuid,
        result: std::result::Result<(), String>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let id = id.to_string();
        match result {
            Ok(()) => {
                self.0.execute(
                    "UPDATE reminder_delivery_outbox
                     SET state='delivered', lease_until_ms=NULL, last_error=NULL
                     WHERE notification_id=?1 AND state='in_flight'",
                    [id],
                )?;
            }
            Err(error) => {
                let attempt: Option<u32> = self
                    .0
                    .query_row(
                        "SELECT attempt_count FROM reminder_delivery_outbox
                     WHERE notification_id=?1 AND state='in_flight'",
                        [&id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(attempt) = attempt {
                    let terminal = attempt >= REMINDER_MAX_ATTEMPTS;
                    let exponent = attempt.saturating_sub(1).min(10);
                    let delay = REMINDER_RETRY_BASE.saturating_mul(1 << exponent);
                    self.0.execute(
                        "UPDATE reminder_delivery_outbox
                         SET state=?2, next_attempt_ms=?3, lease_until_ms=NULL, last_error=?4
                         WHERE notification_id=?1 AND state='in_flight'",
                        params![
                            id,
                            if terminal { "failed" } else { "pending" },
                            now.timestamp_millis() + delay.as_millis() as i64,
                            error.chars().take(512).collect::<String>(),
                        ],
                    )?;
                }
            }
        }
        Ok(())
    }

    fn release_reminder_delivery(&self, id: Uuid) -> Result<()> {
        self.0.execute(
            "UPDATE reminder_delivery_outbox
             SET state='pending', lease_until_ms=NULL, attempt_count=attempt_count-1
             WHERE notification_id=?1 AND state='in_flight'",
            [id.to_string()],
        )?;
        Ok(())
    }

    pub fn reminder_page(&self, before: Option<Uuid>) -> Result<ReminderNotificationPage> {
        let mut statement = self.0.prepare_cached(
            "SELECT id, reminder_id, target_id, title, scheduled_at, created_at
             FROM reminder_notifications
             WHERE sequence <= CASE WHEN ?1 IS NULL THEN
                 (SELECT max(sequence) FROM reminder_notifications)
             ELSE
                 (SELECT sequence - 1 FROM reminder_notifications WHERE id=?1)
             END
             ORDER BY sequence DESC LIMIT ?2",
        )?;
        let mut notifications = statement
            .query_map(
                params![before.map(|id| id.to_string()), PAGE_SIZE + 1],
                read_reminder,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_before = if notifications.len() > PAGE_SIZE as usize {
            notifications.pop();
            notifications.last().map(|entry| entry.id)
        } else {
            None
        };
        Ok(ReminderNotificationPage {
            notifications,
            next_before,
        })
    }

    pub fn page(&self, before: Option<Uuid>) -> Result<NotificationPage> {
        // Bound the worst-case escaped detail size as well as the number of entries.
        let mut statement = self.0.prepare_cached(
            "SELECT r.id, r.revision, r.schedule_id, r.state, n.finished_at,
                    substr(r.detail, 1, 512)
             FROM workflow_notifications n JOIN workflow_runs r ON r.id=n.run_id
             WHERE n.sequence <= CASE WHEN ?1 IS NULL THEN
                 (SELECT max(sequence) FROM workflow_notifications)
             ELSE
                 (SELECT sequence - 1 FROM workflow_notifications WHERE run_id=?1)
             END
             ORDER BY n.sequence DESC LIMIT ?2",
        )?;
        let mut notifications = statement
            .query_map(
                params![before.map(|id| id.to_string()), PAGE_SIZE + 1],
                read_notification,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_before = if notifications.len() > PAGE_SIZE as usize {
            notifications.pop();
            notifications.last().map(|entry| entry.run_id)
        } else {
            None
        };
        Ok(NotificationPage {
            notifications,
            next_before,
        })
    }
}

#[derive(Debug)]
pub struct DueReminder {
    pub reminder_id: Uuid,
    pub target_id: Option<Uuid>,
    pub title: String,
    pub scheduled_at: DateTime<Utc>,
}

fn read_reminder(row: &Row<'_>) -> rusqlite::Result<ReminderNotification> {
    fn parse<T: std::str::FromStr>(row: &Row<'_>, column: usize) -> rusqlite::Result<T>
    where
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        row.get::<_, String>(column)?
            .parse()
            .map_err(|error| decode_error(column, error))
    }

    Ok(ReminderNotification {
        id: parse(row, 0)?,
        reminder_id: parse(row, 1)?,
        target_id: row
            .get::<_, Option<String>>(2)?
            .map(|id| id.parse())
            .transpose()
            .map_err(|error| decode_error(2, error))?,
        title: row.get(3)?,
        scheduled_at: parse(row, 4)?,
        created_at: parse(row, 5)?,
    })
}

fn read_notification(row: &Row<'_>) -> rusqlite::Result<WorkflowNotification> {
    let run_id: String = row.get(0)?;
    let schedule_id: Option<String> = row.get(2)?;
    let state: String = row.get(3)?;
    let finished_at: String = row.get(4)?;
    Ok(WorkflowNotification {
        run_id: run_id.parse().map_err(|error| decode_error(0, error))?,
        revision: row.get(1)?,
        schedule_id: schedule_id
            .map(|id| id.parse())
            .transpose()
            .map_err(|error| decode_error(2, error))?,
        state: parse_state(&state).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(3, state, rusqlite::types::Type::Text)
        })?,
        finished_at: finished_at
            .parse()
            .map_err(|error| decode_error(4, error))?,
        detail: row.get(5)?,
    })
}

pub struct DesktopNotifications {
    last_sequence: i64,
    sender: SyncSender<Popup>,
    results: Receiver<ReminderDeliveryResult>,
    in_flight: HashSet<Uuid>,
}

struct Popup {
    title: &'static str,
    body: String,
    reminder_id: Option<Uuid>,
}

struct ReminderDelivery {
    id: Uuid,
    title: String,
}

struct ReminderDeliveryResult {
    id: Uuid,
    result: std::result::Result<(), String>,
}

impl DesktopNotifications {
    pub fn start(inbox: Inbox<'_>, helper: OsString) -> Result<Self> {
        // Keep existing and startup-recovered runs in the inbox without replaying popups.
        let last_sequence = inbox.0.query_row(
            "SELECT coalesce(max(sequence), 0) FROM workflow_notifications",
            [],
            |row| row.get(0),
        )?;
        let (sender, receiver) = mpsc::sync_channel(POPUP_QUEUE_SIZE);
        let (result_sender, results) = mpsc::channel();
        thread::Builder::new()
            .name("mcbtodo-notifications".into())
            .spawn(move || {
                for popup in receiver {
                    let result = send_popup(&helper, &popup);
                    if let Some(id) = popup.reminder_id
                        && result_sender
                            .send(ReminderDeliveryResult { id, result })
                            .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            last_sequence,
            sender,
            results,
            in_flight: HashSet::new(),
        })
    }

    pub fn dispatch(&mut self, inbox: Inbox<'_>) -> Result<()> {
        let mut statement = inbox.0.prepare_cached(
            "SELECT n.sequence, n.run_id, r.state
             FROM workflow_notifications n JOIN workflow_runs r ON r.id=n.run_id
             WHERE n.sequence > ?1 ORDER BY n.sequence DESC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![self.last_sequence, POPUP_QUEUE_SIZE as u32],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (sequence, run_id, state) in rows.into_iter().rev() {
            self.last_sequence = sequence;
            let title = match parse_state(&state) {
                Some(RunState::Succeeded) => "Workflow succeeded",
                Some(RunState::Failed) => "Workflow failed",
                Some(RunState::Cancelled) => "Workflow cancelled",
                Some(RunState::OutcomeUnknown) => "Workflow outcome unknown",
                _ => continue,
            };
            // A busy or unavailable desktop never delays daemon work.
            let body = format!("Run {run_id}");
            let _ = self.sender.try_send(Popup {
                title,
                body,
                reminder_id: None,
            });
        }
        Ok(())
    }

    pub fn dispatch_reminders(&mut self, inbox: Inbox<'_>) -> Result<()> {
        while let Ok(completion) = self.results.try_recv() {
            self.in_flight.remove(&completion.id);
            inbox.finish_reminder_delivery(completion.id, completion.result, Utc::now())?;
        }
        let available = POPUP_QUEUE_SIZE.saturating_sub(self.in_flight.len());
        let claimed =
            inbox.claim_reminder_deliveries(Utc::now(), available.min(REMINDER_BATCH_SIZE))?;
        for reminder in claimed {
            let body = reminder
                .title
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let popup = Popup {
                title: "Reminder",
                body,
                reminder_id: Some(reminder.id),
            };
            match self.sender.try_send(popup) {
                Ok(()) => {
                    self.in_flight.insert(reminder.id);
                }
                Err(_) => {
                    inbox.release_reminder_delivery(reminder.id)?;
                    break;
                }
            }
        }
        Ok(())
    }
}

fn send_popup(helper: &OsString, popup: &Popup) -> std::result::Result<(), String> {
    let mut child = Command::new(helper)
        .args(["--app-name=mcbtodo", "--", popup.title])
        .arg(&popup.body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start notification helper: {error}"))?;
    let deadline = Instant::now() + POPUP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("notification helper exited with {status}")),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("notification helper timed out".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not wait for notification helper: {error}"));
            }
        }
    }
}

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
