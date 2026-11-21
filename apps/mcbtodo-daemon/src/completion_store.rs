//! Records deliveries and advances enabled WorkUnit rules in the workflow database.

use anyhow::Result;
use mcbtodo_domain::WorkUnitCompleted;
use mcbtodo_protocol::workflow::ScheduleSummary;
use rusqlite::{Connection, params};
use uuid::Uuid;

use super::{RunStore, insert_run};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS workflow_completion_rules (
        schedule_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_schedules(id),
        work_unit_id TEXT NOT NULL,
        revision TEXT NOT NULL,
        cursor INTEGER NOT NULL CHECK (cursor >= 0)
    ) STRICT;
    CREATE INDEX IF NOT EXISTS workflow_completion_cursor
        ON workflow_completion_rules(cursor);
    CREATE INDEX IF NOT EXISTS workflow_completion_target
        ON workflow_completion_rules(work_unit_id, cursor);
    CREATE TABLE IF NOT EXISTS workflow_completion_receipts (
        schedule_id TEXT NOT NULL REFERENCES workflow_schedules(id),
        event_id TEXT NOT NULL,
        run_id TEXT UNIQUE NOT NULL REFERENCES workflow_runs(id),
        PRIMARY KEY (schedule_id, event_id)
    ) STRICT;
";

pub(super) fn register(
    tx: &Connection,
    summary: &ScheduleSummary,
    work_unit_id: Uuid,
    cursor: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO workflow_completion_rules(schedule_id,work_unit_id,revision,cursor)
         VALUES(?1,?2,?3,?4)",
        params![
            summary.id.to_string(),
            work_unit_id.to_string(),
            summary.revision,
            cursor
        ],
    )?;
    Ok(())
}

impl RunStore {
    pub fn next_completion_cursor(&self) -> Result<Option<i64>> {
        Ok(self.0.query_row(
            "SELECT min(cursor) FROM workflow_completion_rules",
            [],
            |row| row.get(0),
        )?)
    }

    /// The page starts after the smallest active cursor at read time. This
    /// transaction rechecks the live rules because registration or disable
    /// may have run while the Vault reader was off-thread.
    pub fn deliver_completion_page(&self, page: &[(i64, WorkUnitCompleted)]) -> Result<()> {
        let Some((last, _)) = page.last() else {
            return Ok(());
        };
        let tx = self.write_transaction()?;
        {
            let mut targets = tx.prepare_cached(
                "SELECT schedule_id, revision FROM workflow_completion_rules r
                 WHERE work_unit_id=?1 AND cursor < ?2 AND NOT EXISTS (
                     SELECT 1 FROM workflow_completion_receipts d
                     WHERE d.schedule_id=r.schedule_id AND d.event_id=?3
                 ) ORDER BY schedule_id",
            )?;
            for (sequence, event) in page {
                let event_id = event.event_id.to_string();
                let matches = targets
                    .query_map(
                        params![event.work_unit_id.to_string(), sequence, event_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (schedule, revision) in matches {
                    let run_id = Uuid::new_v4();
                    insert_run(&tx, run_id, &revision, None, Some(schedule.parse()?))?;
                    tx.execute(
                        "INSERT INTO workflow_completion_receipts(schedule_id,event_id,run_id)
                         VALUES(?1,?2,?3)",
                        params![schedule, event_id, run_id.to_string()],
                    )?;
                }
            }
        }
        // Every scanned event, including events for other WorkUnits, is now
        // accounted for. Never move newer registration cutoffs backward.
        tx.execute(
            "UPDATE workflow_completion_rules SET cursor=?1 WHERE cursor < ?1",
            [last],
        )?;
        tx.commit()?;
        Ok(())
    }
}
