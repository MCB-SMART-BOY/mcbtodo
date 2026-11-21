//! Advances one durable trigger at a time, atomically with any resulting runs.

use std::num::NonZeroUsize;

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, TimeDelta, Utc};
use mcbtodo_protocol::workflow::{Fold, Missed, Overlap, ScheduleSummary, Trigger};
use mcbtodo_workflow::schedule::{DstResolution, MissedPolicy, Schedule};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{RunStore, completion_store, dependency_store, due_millis, insert_run};

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
const MAX_REPLAY: u8 = 16;

pub(crate) fn calculator(trigger: &Trigger, missed: Missed) -> Result<(Schedule, MissedPolicy)> {
    let schedule = match trigger {
        Trigger::Interval { anchor, seconds } => {
            Schedule::interval(*anchor, TimeDelta::seconds(i64::from(*seconds)))?
        }
        Trigger::Cron {
            expression,
            timezone,
            fold,
        } => {
            ensure!(
                expression.len() <= 256 && timezone.len() <= 96,
                "Cron expression or timezone is too long"
            );
            let timezone = timezone
                .parse()
                .map_err(|_| anyhow::anyhow!("Unknown IANA timezone `{timezone}`"))?;
            let dst = match fold {
                Fold::Earlier => DstResolution::Earlier,
                Fold::Later => DstResolution::Later,
                Fold::Both => DstResolution::Both,
            };
            Schedule::cron(expression, timezone, dst)?
        }
        Trigger::Dependency { .. } => anyhow::bail!("Dependency triggers have no clock schedule"),
        Trigger::WorkUnitCompleted { .. } => {
            anyhow::bail!("WorkUnit completion triggers have no clock schedule")
        }
    };
    let policy = match missed {
        Missed::Skip => MissedPolicy::Skip,
        Missed::RunOnce => MissedPolicy::RunOnce,
        Missed::ReplayLimited { limit } => {
            ensure!(
                (1..=MAX_REPLAY).contains(&limit),
                "Replay limit must be between 1 and {MAX_REPLAY}"
            );
            MissedPolicy::ReplayLimited {
                limit: NonZeroUsize::new(limit.into()).expect("positive replay limit"),
            }
        }
    };
    Ok((schedule, policy))
}

impl RunStore {
    pub fn schedule(&self, id: Uuid) -> Result<Option<ScheduleSummary>> {
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

    pub fn insert_schedule(&self, summary: &ScheduleSummary) -> Result<()> {
        self.insert_schedule_at(summary, None)
    }

    pub fn insert_schedule_at(
        &self,
        summary: &ScheduleSummary,
        completion_cursor: Option<i64>,
    ) -> Result<()> {
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

    pub fn disable_schedule(&self, mut summary: ScheduleSummary) -> Result<ScheduleSummary> {
        summary.enabled = false;
        summary.next_at = None;
        let tx = self.write_transaction()?;
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
        Ok(summary)
    }

    pub fn next_schedule_due(&self) -> Result<Option<i64>> {
        Ok(self
            .0
            .query_row("SELECT min(next_ms) FROM workflow_schedules", [], |row| {
                row.get(0)
            })?)
    }

    pub fn schedule_tick(&mut self, now: DateTime<Utc>) -> Result<()> {
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
        let (calculator, policy) = calculator(&summary.trigger, summary.missed)?;

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
