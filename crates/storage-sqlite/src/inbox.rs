use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use mcbtodo_ports::notifications::{
    DueReminder, NotificationPage, NotificationStore, REMINDER_MAX_ATTEMPTS, ReminderDelivery,
    ReminderNotification, ReminderNotificationPage, WorkflowNotification,
};
use mcbtodo_workflow::run::RunState;
use rusqlite::{OptionalExtension, Row, Transaction, params};
use uuid::Uuid;

use super::{RunStore, decode_error, parse_state};

const PAGE_SIZE: u32 = 12;
const REMINDER_RETRY_BASE: Duration = Duration::from_secs(1);

// Store only inbox ordering and completion time. The run carries the outcome and detail.
// Install the trigger before recovery so interrupted runs enter the inbox atomically.
pub(crate) fn install(tx: &Transaction<'_>) -> Result<()> {
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

impl NotificationStore for RunStore {
    fn workflow_watermark(&self) -> Result<i64> {
        Ok(self.0.query_row(
            "SELECT coalesce(max(sequence), 0) FROM workflow_notifications",
            [],
            |row| row.get(0),
        )?)
    }

    /// Return the newest bounded batch past the cursor in delivery order.
    fn recent_workflow_completions(
        &self,
        after: i64,
        limit: u32,
    ) -> Result<Vec<(i64, Uuid, RunState)>> {
        let mut statement = self.0.prepare_cached(
            "SELECT n.sequence, n.run_id, r.state
             FROM workflow_notifications n JOIN workflow_runs r ON r.id=n.run_id
             WHERE n.sequence > ?1 ORDER BY n.sequence DESC LIMIT ?2",
        )?;
        let mut completions = statement
            .query_map(params![after, limit], |row| {
                let id: String = row.get(1)?;
                let state: String = row.get(2)?;
                Ok((
                    row.get(0)?,
                    id.parse().map_err(|error| decode_error(1, error))?,
                    parse_state(&state).ok_or_else(|| {
                        rusqlite::Error::InvalidColumnType(2, state, rusqlite::types::Type::Text)
                    })?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        completions.reverse();
        Ok(completions)
    }

    fn publish_reminders(
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

    fn recover_reminder_deliveries(&self) -> Result<()> {
        self.0.execute(
            "UPDATE reminder_delivery_outbox
             SET state=CASE WHEN attempt_count >= ?1 THEN 'failed' ELSE 'pending' END,
                 lease_until_ms=NULL,
                 last_error=CASE WHEN attempt_count >= ?1
                     THEN 'Interrupted before notification delivery could be confirmed'
                     ELSE last_error END
             WHERE state='in_flight' OR (state='pending' AND attempt_count >= ?1)",
            [REMINDER_MAX_ATTEMPTS],
        )?;
        Ok(())
    }

    fn claim_reminder_deliveries(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        lease_until: DateTime<Utc>,
    ) -> Result<Vec<ReminderDelivery>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let tx = self.0.unchecked_transaction()?;
        let now_ms = now.timestamp_millis();
        let lease_until_ms = lease_until.timestamp_millis();
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
        result: std::result::Result<(), &str>,
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

    fn reminder_page(&self, before: Option<Uuid>) -> Result<ReminderNotificationPage> {
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

    fn workflow_page(&self, before: Option<Uuid>) -> Result<NotificationPage> {
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
