//! Index enabled revision edges and record each source occurrence in the run database.

use std::collections::HashSet;

use anyhow::Result;
use mcbtodo_ports::workflow::InvalidDependency;
use mcbtodo_workflow::{run::RunSummary, schedule::ScheduleSummary};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::insert_run;

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS workflow_dependency_rules (
        schedule_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_schedules(id),
        upstream_revision TEXT NOT NULL,
        downstream_revision TEXT NOT NULL,
        UNIQUE (upstream_revision, downstream_revision)
    ) STRICT;
    CREATE TABLE IF NOT EXISTS workflow_dependency_deliveries (
        schedule_id TEXT NOT NULL REFERENCES workflow_schedules(id),
        source_run_id TEXT NOT NULL REFERENCES workflow_runs(id),
        child_run_id TEXT UNIQUE REFERENCES workflow_runs(id),
        root_run_id TEXT NOT NULL REFERENCES workflow_runs(id),
        depth INTEGER NOT NULL CHECK (depth > 0),
        PRIMARY KEY (schedule_id, source_run_id)
    ) STRICT;
    CREATE INDEX IF NOT EXISTS workflow_dependency_root
        ON workflow_dependency_deliveries(root_run_id) WHERE child_run_id IS NOT NULL;
    CREATE INDEX IF NOT EXISTS workflow_dependency_skipped
        ON workflow_dependency_deliveries(schedule_id) WHERE child_run_id IS NULL;
";

const MAX_FAN_OUT: u32 = 16;
const MAX_DEPTH: u32 = 8;
const MAX_DESCENDANTS: u32 = 128;

pub(super) fn register(tx: &Connection, summary: &ScheduleSummary, upstream: &str) -> Result<()> {
    let downstream = &summary.revision;
    let (fan_out, duplicate): (u32, bool) = tx.query_row(
        "SELECT count(*), coalesce(max(downstream_revision=?2), 0)
         FROM workflow_dependency_rules WHERE upstream_revision=?1",
        params![upstream, downstream],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if duplicate {
        return Err(InvalidDependency::Duplicate.into());
    }
    if fan_out >= MAX_FAN_OUT {
        return Err(InvalidDependency::FanOut.into());
    }
    let cycle: bool = tx.query_row(
        "WITH RECURSIVE reachable(revision) AS (
             VALUES (?2)
             UNION
             SELECT r.downstream_revision FROM workflow_dependency_rules r
             JOIN reachable ON r.upstream_revision=reachable.revision
         )
         SELECT EXISTS(SELECT 1 FROM reachable WHERE revision=?1)",
        params![upstream, downstream],
        |row| row.get(0),
    )?;
    if cycle {
        return Err(InvalidDependency::Cycle.into());
    }
    tx.execute(
        "INSERT INTO workflow_dependency_rules(schedule_id,upstream_revision,downstream_revision)
         VALUES(?1,?2,?3)",
        params![summary.id.to_string(), upstream, downstream],
    )?;
    Ok(())
}

pub(super) fn deliver(tx: &Connection, source: &RunSummary) -> Result<()> {
    let source_id = source.id.to_string();
    let rules = tx
        .prepare_cached(
            "SELECT r.schedule_id, r.downstream_revision FROM workflow_dependency_rules r
             WHERE r.upstream_revision=?1 AND NOT EXISTS (
                 SELECT 1 FROM workflow_dependency_deliveries d
                 WHERE d.schedule_id=r.schedule_id AND d.source_run_id=?2
             ) ORDER BY r.downstream_revision",
        )?
        .query_map(params![source.revision, source_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rules.is_empty() {
        return Ok(());
    }

    let (root, source_depth): (String, u32) = tx
        .query_row(
            "SELECT root_run_id, depth FROM workflow_dependency_deliveries WHERE child_run_id=?1",
            [&source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or_else(|| (source_id.clone(), 0));
    let mut spawned: u32 = tx.query_row(
        "SELECT count(*) FROM workflow_dependency_deliveries
         WHERE root_run_id=?1 AND child_run_id IS NOT NULL",
        [&root],
        |row| row.get(0),
    )?;
    // Rules can be replaced while runs are queued. Check the actual lineage
    // as well as the enabled graph so a replacement cannot close an in-flight loop.
    let ancestors = tx
        .prepare_cached(
            "WITH RECURSIVE lineage(id) AS (
                 VALUES (?1)
                 UNION ALL
                 SELECT d.source_run_id FROM workflow_dependency_deliveries d
                 JOIN lineage ON d.child_run_id=lineage.id
             )
             SELECT r.revision FROM workflow_runs r JOIN lineage ON r.id=lineage.id",
        )?
        .query_map([&source_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    let depth = source_depth + 1;
    for (schedule, downstream) in rules {
        let child = if depth <= MAX_DEPTH
            && spawned < MAX_DESCENDANTS
            && !ancestors.contains(&downstream)
        {
            let child_id = Uuid::new_v4();
            insert_run(tx, child_id, &downstream, None, Some(schedule.parse()?))?;
            spawned += 1;
            Some(child_id.to_string())
        } else {
            None
        };
        tx.execute(
            "INSERT INTO workflow_dependency_deliveries
                (schedule_id,source_run_id,child_run_id,root_run_id,depth) VALUES(?1,?2,?3,?4,?5)",
            params![schedule, source_id, child, root, depth],
        )?;
    }
    Ok(())
}
