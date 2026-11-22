//! Advances one durable trigger at a time, atomically with any resulting runs.

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, TimeDelta, Utc};
use mcbtodo_ports::workflow::{WorkflowQueueStore, WorkflowScheduleStore};
use mcbtodo_workflow::{
    run::RunSummary,
    schedule::{Overlap, ScheduleSummary, Trigger},
};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{
    RUN_COLUMNS, RunStore, completion_store, dependency_store, due_millis, insert_run, read_summary,
};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS workflow_schedules (
        id TEXT PRIMARY KEY NOT NULL,
        next_ms INTEGER,
        summary TEXT NOT NULL
    ) STRICT;
    CREATE INDEX IF NOT EXISTS workflow_schedule_due
        ON workflow_schedules(next_ms) WHERE next_ms IS NOT NULL;
";

const DELIVERY_WINDOW: TimeDelta = TimeDelta::seconds(1);

impl WorkflowScheduleStore for RunStore {
    fn schedule(&self, id: Uuid) -> Result<Option<ScheduleSummary>> {
        let row: Option<(String, i64)> = self
            .0
            .query_row(
                "SELECT summary, (SELECT count(*) FROM workflow_dependency_deliveries
                    WHERE schedule_id=?1 AND child_run_id IS NULL)
                 FROM workflow_schedules WHERE id=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(json, skipped)| {
            let mut summary: ScheduleSummary = serde_json::from_str(&json)?;
            summary.cascade_skipped = skipped.try_into()?;
            Ok(summary)
        })
        .transpose()
    }

    fn insert_schedule(
        &self,
        summary: &ScheduleSummary,
        completion_cursor: Option<i64>,
    ) -> Result<()> {
        ensure!(
            completion_cursor.is_some()
                == matches!(summary.trigger, Trigger::WorkUnitCompleted { .. }),
            "only completion rules require a cutoff"
        );
        let tx = self.write_transaction()?;
        tx.execute(
            "INSERT INTO workflow_schedules(id,next_ms,summary) VALUES(?1,?2,?3)",
            params![
                summary.id.to_string(),
                summary.next_at.map(due_millis),
                serde_json::to_string(summary)?
            ],
        )?;
        match &summary.trigger {
            Trigger::Dependency { upstream_revision } => {
                dependency_store::register(&tx, summary, upstream_revision)?;
            }
            Trigger::WorkUnitCompleted { work_unit_id } => {
                let cursor = completion_cursor.context("completion rule is missing its cutoff")?;
                completion_store::register(&tx, summary, *work_unit_id, cursor)?;
            }
            Trigger::Interval { .. } | Trigger::Cron { .. } => {}
        }
        tx.commit()?;
        Ok(())
    }

    fn disable_schedule(&self, id: Uuid) -> Result<Option<ScheduleSummary>> {
        let tx = self.write_transaction()?;
        let Some(mut summary) = self.schedule(id)? else {
            return Ok(None);
        };
        if !summary.enabled {
            return Ok(Some(summary));
        }
        summary.enabled = false;
        summary.next_at = None;
        match summary.trigger {
            Trigger::Dependency { .. } => {
                tx.execute(
                    "DELETE FROM workflow_dependency_rules WHERE schedule_id=?1",
                    [summary.id.to_string()],
                )?;
            }
            Trigger::WorkUnitCompleted { .. } => {
                tx.execute(
                    "DELETE FROM workflow_completion_rules WHERE schedule_id=?1",
                    [summary.id.to_string()],
                )?;
            }
            Trigger::Interval { .. } | Trigger::Cron { .. } => {}
        }
        update(&tx, &summary)?;
        tx.commit()?;
        Ok(Some(summary))
    }
}

impl WorkflowQueueStore for RunStore {
    fn next_wake_at(&self, can_advance_run: bool) -> Result<Option<DateTime<Utc>>> {
        let due_ms: Option<i64> = self.0.query_row(
            "SELECT min(due) FROM (
                SELECT min(next_ms) AS due FROM workflow_schedules WHERE next_ms IS NOT NULL
                UNION ALL
                SELECT min(due_ms) FROM workflow_runs WHERE ?1 AND state='pending'
             )",
            [can_advance_run],
            |row| row.get(0),
        )?;
        due_ms
            .map(|value| DateTime::from_timestamp_millis(value).context("invalid queue wake time"))
            .transpose()
    }

    fn next_pending_at(&self, now: DateTime<Utc>) -> Result<Option<RunSummary>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs
                 WHERE state='pending' AND due_ms <= ?1 ORDER BY due_ms, rowid LIMIT 1"
            ))?
            .query_row([now.timestamp_millis()], read_summary)
            .optional()?)
    }

    fn dispatch_due_schedule(&mut self, now: DateTime<Utc>) -> Result<()> {
        let tx = self.0.transaction()?;
        let summary: Option<String> = tx.query_row(
            "SELECT summary FROM workflow_schedules WHERE next_ms <= ?1 ORDER BY next_ms,rowid LIMIT 1",
            [now.timestamp_millis()],
            |row| row.get(0),
        ).optional()?;
        let Some(summary) = summary else {
            return Ok(());
        };
        let mut summary: ScheduleSummary = serde_json::from_str(&summary)?;
        let next = summary.next_at.context("due schedule has no occurrence")?;
        let cursor = next - TimeDelta::nanoseconds(1);
        let (calculator, policy) = summary.trigger.clock_schedule(summary.missed)?;

        let late_until = now - DELIVERY_WINDOW;
        let missed = calculator.missed(cursor, late_until, policy);
        summary.missed_occurrences = summary
            .missed_occurrences
            .saturating_add(u64::try_from(missed.total).unwrap_or(u64::MAX));
        let mut selected = missed.occurrences;
        let mut cursor = cursor.max(late_until);
        while let Some(timely) = calculator.next_after(cursor).filter(|next| *next <= now) {
            selected.push(timely);
            cursor = timely;
        }

        if !selected.is_empty() && summary.overlap == Overlap::Skip {
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE schedule_id=?1
                    AND state IN ('pending','waiting','running','cancelling','outcome_unknown'))",
                [summary.id.to_string()],
                |row| row.get(0),
            )?;
            let keep = usize::from(!active);
            let skipped = selected.len() - keep;
            summary.overlap_skipped = summary.overlap_skipped.saturating_add(skipped as u64);
            selected.drain(..skipped);
        }
        for at in selected {
            insert_run(
                &tx,
                Uuid::new_v4(),
                &summary.revision,
                Some(at),
                Some(summary.id),
            )?;
        }
        summary.next_at = calculator.next_after(now);
        update(&tx, &summary)?;
        tx.commit()?;
        Ok(())
    }
}

fn update(db: &Connection, summary: &ScheduleSummary) -> Result<()> {
    // Keep the full typed definition and counters with the query index in one row.
    db.execute(
        "UPDATE workflow_schedules SET next_ms=?1, summary=?2 WHERE id=?3",
        params![
            summary.next_at.map(due_millis),
            serde_json::to_string(summary)?,
            summary.id.to_string()
        ],
    )?;
    Ok(())
}

pub(crate) fn validate_schedules(db: &Connection) -> Result<()> {
    let mut schedules = db.prepare("SELECT id, next_ms, summary FROM workflow_schedules")?;
    let mut rows = schedules.query([])?;
    while let Some(row) = rows.next()? {
        let id = row.get_ref(0)?.as_str()?;
        let stored_next_ms: Option<i64> = row.get(1)?;
        let summary: ScheduleSummary = serde_json::from_str(row.get_ref(2)?.as_str()?)
            .with_context(|| format!("decoding saved workflow schedule {id}"))?;
        ensure!(
            id == summary.id.to_string(),
            "inconsistent identity for workflow schedule {id}"
        );
        let is_clock_trigger = match summary.trigger {
            Trigger::Interval { .. } | Trigger::Cron { .. } => {
                summary
                    .trigger
                    .clock_schedule(summary.missed)
                    .with_context(|| format!("validating workflow schedule {id}"))?;
                true
            }
            Trigger::Dependency { .. } | Trigger::WorkUnitCompleted { .. } => false,
        };
        ensure!(
            stored_next_ms == summary.next_at.map(due_millis)
                && (summary.next_at.is_none() || summary.enabled && is_clock_trigger),
            "inconsistent next occurrence for workflow schedule {id}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcbtodo_ports::workflow::{WorkflowProgressStore, WorkflowRunCatalog};
    use mcbtodo_workflow::{run::RunState, schedule::Missed};

    fn interval(anchor: DateTime<Utc>) -> ScheduleSummary {
        ScheduleSummary {
            id: Uuid::from_u128(1),
            revision: "scheduled".into(),
            trigger: Trigger::Interval {
                anchor,
                seconds: 60,
            },
            missed: Missed::ReplayLimited { limit: 3 },
            overlap: Overlap::Queue,
            next_at: Some(anchor),
            enabled: true,
            missed_occurrences: 0,
            overlap_skipped: 0,
            cascade_skipped: 0,
        }
    }

    #[test]
    fn queue_wake_respects_worker_capacity_and_due_selection_does_not_claim() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let queue: &dyn WorkflowQueueStore = &store;
        let before = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let due = before + TimeDelta::nanoseconds(1);
        let due_tick = before + TimeDelta::milliseconds(1);
        let clock_at = before + TimeDelta::minutes(1);
        assert_eq!(queue.next_wake_at(false)?, None);
        assert_eq!(queue.next_wake_at(true)?, None);

        let first = Uuid::from_u128(3);
        let second = Uuid::from_u128(2);
        store.enqueue_if_absent(first, "first", Some(due))?;
        store.enqueue_if_absent(second, "second", Some(due))?;
        assert_eq!(queue.next_wake_at(false)?, None);
        assert_eq!(queue.next_wake_at(true)?, Some(due_tick));
        store.insert_schedule(&interval(clock_at), None)?;
        assert_eq!(queue.next_wake_at(false)?, Some(clock_at));
        assert_eq!(queue.next_wake_at(true)?, Some(due_tick));
        assert!(queue.next_pending_at(before)?.is_none());

        let candidate = queue.next_pending_at(due_tick)?.unwrap();
        assert_eq!(candidate.id, first);
        assert_eq!(candidate.state, RunState::Pending);
        assert_eq!(queue.next_pending_at(due_tick)?, Some(candidate.clone()));
        store.transition(&candidate, RunState::Cancelled, None, None)?;
        assert_eq!(queue.next_pending_at(due_tick)?.unwrap().id, second);
        Ok(())
    }

    #[test]
    fn clock_delivery_rolls_back_a_partial_batch_and_retries_after_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let anchor = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let now = anchor + TimeDelta::minutes(2);
        let initial = interval(anchor);
        let mut store = RunStore::open(&path)?;
        store.insert_schedule(&initial, None)?;
        store.0.execute_batch(
            "CREATE TEMP TRIGGER fail_second_occurrence BEFORE INSERT ON workflow_runs
             WHEN NEW.schedule_id IS NOT NULL AND EXISTS
               (SELECT 1 FROM workflow_runs WHERE schedule_id=NEW.schedule_id)
             BEGIN SELECT RAISE(ABORT, 'injected second occurrence failure'); END;",
        )?;
        let queue: &mut dyn WorkflowQueueStore = &mut store;
        let error = queue.dispatch_due_schedule(now).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected second occurrence failure")
        );
        assert!(store.list(None)?.0.is_empty());
        assert_eq!(store.schedule(initial.id)?, Some(initial.clone()));
        drop(store);

        let mut store = RunStore::open(&path)?;
        store.dispatch_due_schedule(now)?;
        store.dispatch_due_schedule(now)?;
        let mut runs = store.list(None)?.0;
        runs.sort_by_key(|run| run.due_at);
        assert_eq!(
            runs.iter().map(|run| run.due_at).collect::<Vec<_>>(),
            [
                Some(anchor),
                Some(anchor + TimeDelta::minutes(1)),
                Some(now)
            ]
        );
        assert!(runs.iter().all(|run| run.schedule_id == Some(initial.id)));
        let delivered = store.schedule(initial.id)?.unwrap();
        assert_eq!(delivered.missed_occurrences, 2);
        assert_eq!(delivered.next_at, Some(now + TimeDelta::minutes(1)));
        Ok(())
    }

    #[test]
    fn disabling_after_clock_delivery_keeps_durable_counters() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let store = RunStore::open(&path)?;
        let mut dispatcher = RunStore::open(&path)?;
        let anchor = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let initial = interval(anchor);
        let schedules: &dyn WorkflowScheduleStore = &store;
        schedules.insert_schedule(&initial, None)?;
        assert_eq!(schedules.schedule(initial.id)?, Some(initial.clone()));
        assert_eq!(schedules.disable_schedule(Uuid::nil())?, None);
        dispatcher.dispatch_due_schedule(anchor + TimeDelta::minutes(1))?;
        let delivered = dispatcher.schedule(initial.id)?.unwrap();
        assert_eq!(delivered.missed_occurrences, 1);

        let disabled = schedules.disable_schedule(initial.id)?.unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.next_at, None);
        assert_eq!(disabled.missed_occurrences, delivered.missed_occurrences);
        assert_eq!(
            schedules.disable_schedule(initial.id)?,
            Some(disabled.clone())
        );
        drop(store);
        drop(dispatcher);

        let mut reopened = RunStore::open(&path)?;
        assert_eq!(reopened.schedule(initial.id)?, Some(disabled));
        reopened.dispatch_due_schedule(anchor + TimeDelta::minutes(2))?;
        assert_eq!(reopened.list(None)?.0.len(), 2);
        Ok(())
    }
}
