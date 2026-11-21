use std::{path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use mcbtodo_protocol::workflow::{ObservedOutcome, RunResolution, RunState, RunSummary};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::notifications::{self, Inbox};

#[path = "agent_budget_store.rs"]
mod agent_budget_store;
#[path = "completion_store.rs"]
mod completion_store;
#[path = "dependency_store.rs"]
mod dependency_store;
#[path = "schedule_store.rs"]
mod schedule_store;
pub(crate) use agent_budget_store::AgentBudgetDenied;
pub(crate) use dependency_store::InvalidDependency;
pub(crate) use schedule_store::calculator as schedule_calculator;

pub struct RunStore(Connection);

pub const RUN_PAGE_SIZE: usize = 12;
pub const ARTIFACT_CHUNK_SIZE: u32 = 8192;

pub struct Artifact {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerAttempt {
    pub(crate) id: Uuid,
    pub(crate) run_id: Uuid,
    pub(crate) revision: String,
    pub(crate) node: String,
    pub(crate) epoch: i64,
    pub(crate) expected_steps: u32,
}

const RUN_COLUMNS: &str = "id, revision, due_at, state, current_node, detail,
    (SELECT count(*) FROM workflow_steps WHERE run_id=workflow_runs.id), schedule_id,
    (SELECT source_run_id FROM workflow_dependency_deliveries WHERE child_run_id=workflow_runs.id),
    (SELECT event_id FROM workflow_completion_receipts WHERE run_id=workflow_runs.id),
    (SELECT outcome FROM workflow_run_resolutions WHERE run_id=workflow_runs.id),
    (SELECT note FROM workflow_run_resolutions WHERE run_id=workflow_runs.id),
    (SELECT recorded_at FROM workflow_run_resolutions WHERE run_id=workflow_runs.id)";

impl RunStore {
    pub fn open(path: &Path) -> Result<Self> {
        crate::endpoint::open_private_file(path)?;
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        let tx = db.transaction()?;
        tx.execute_batch(schedule_store::SCHEMA)?;
        tx.execute_batch(
            "
             CREATE TABLE IF NOT EXISTS workflow_runs (
                 id TEXT PRIMARY KEY NOT NULL,
                 revision TEXT NOT NULL,
                 due_at TEXT,
                 due_ms INTEGER NOT NULL,
                 schedule_id TEXT REFERENCES workflow_schedules(id),
                 state TEXT NOT NULL CHECK (state IN
                     ('pending','waiting','running','cancelling','cancelled',
                      'succeeded','failed','outcome_unknown')),
                 current_node TEXT,
                 detail TEXT,
                 CHECK ((state IN ('waiting','running','cancelling') AND current_node IS NOT NULL)
                      OR state IN ('cancelled','outcome_unknown')
                      OR (state IN ('pending','succeeded','failed') AND current_node IS NULL))
             ) STRICT;
             CREATE INDEX IF NOT EXISTS workflow_pending
                ON workflow_runs(due_ms) WHERE state='pending';
             CREATE UNIQUE INDEX IF NOT EXISTS workflow_schedule_occurrence
                ON workflow_runs(schedule_id, due_at) WHERE schedule_id IS NOT NULL;
             CREATE TABLE IF NOT EXISTS workflow_artifacts (
                 sha256 TEXT PRIMARY KEY NOT NULL,
                 data BLOB NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS workflow_steps (
                 run_id TEXT NOT NULL REFERENCES workflow_runs(id),
                 position INTEGER NOT NULL CHECK (position >= 0),
                 trace TEXT NOT NULL,
                 artifact_hash TEXT REFERENCES workflow_artifacts(sha256),
                 artifact_type TEXT,
                 CHECK ((artifact_hash IS NULL) = (artifact_type IS NULL)),
                 PRIMARY KEY (run_id, position)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS script_authorizations (
                 revision TEXT NOT NULL,
                 node TEXT NOT NULL,
                 PRIMARY KEY (revision, node)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS workflow_run_resolutions (
                 run_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_runs(id),
                 outcome TEXT NOT NULL CHECK (outcome IN ('completed','failed','not_started')),
                 note TEXT NOT NULL,
                 recorded_at TEXT NOT NULL
             ) STRICT;",
        )?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_attempt_clock (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 epoch INTEGER NOT NULL CHECK (epoch >= 0)
             ) STRICT;
             INSERT OR IGNORE INTO workflow_attempt_clock(singleton, epoch) VALUES(1, 0);
             CREATE TABLE IF NOT EXISTS workflow_attempts (
                 id TEXT PRIMARY KEY NOT NULL,
                 run_id TEXT NOT NULL REFERENCES workflow_runs(id),
                 revision TEXT NOT NULL,
                 node TEXT NOT NULL,
                 epoch INTEGER NOT NULL UNIQUE CHECK (epoch > 0),
                 expected_steps INTEGER NOT NULL CHECK (expected_steps > 0),
                 state TEXT NOT NULL CHECK (state IN
                     ('claimed','started','committed','invalidated'))
             ) STRICT;
             CREATE UNIQUE INDEX IF NOT EXISTS workflow_one_active_attempt_per_run
                 ON workflow_attempts(run_id)
                 WHERE state IN ('claimed','started');",
        )?;
        agent_budget_store::install(&tx)?;
        tx.execute_batch(dependency_store::SCHEMA)?;
        tx.execute_batch(completion_store::SCHEMA)?;
        notifications::install(&tx)?;
        tx.execute(
            "UPDATE workflow_runs
                 SET state='outcome_unknown',
                     detail='Interrupted while a worker could have executed this node'
                 WHERE state IN ('running','cancelling');",
            [],
        )?;
        tx.execute(
            "UPDATE workflow_attempts SET state='invalidated'
             WHERE state IN ('claimed','started');",
            [],
        )?;
        tx.commit()?;
        Ok(Self(db))
    }

    pub fn notifications(&self) -> Inbox<'_> {
        Inbox(&self.0)
    }

    pub fn get(&self, id: Uuid) -> Result<Option<RunSummary>> {
        self.0
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id=?1"),
                [id.to_string()],
                read_summary,
            )
            .optional()
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn worker_attempt_state(&self, id: Uuid) -> Result<Option<String>> {
        Ok(self
            .0
            .query_row(
                "SELECT state FROM workflow_attempts WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn resolve_unknown(
        &self,
        id: Uuid,
        outcome: ObservedOutcome,
        note: &str,
    ) -> Result<RunResolution> {
        let tx = self.write_transaction()?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM workflow_runs WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match state.as_deref() {
            None => anyhow::bail!("workflow run {id} does not exist"),
            Some("outcome_unknown") => (),
            Some(_) => anyhow::bail!("workflow run is not outcome_unknown"),
        }

        let recorded_at = Utc::now();
        tx.execute(
            "INSERT INTO workflow_run_resolutions(run_id,outcome,note,recorded_at)
             VALUES(?1,?2,?3,?4)",
            params![
                id.to_string(),
                outcome_key(&outcome),
                note,
                recorded_at.to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(RunResolution {
            outcome,
            note: note.to_owned(),
            recorded_at,
        })
    }

    pub fn list(&self, before: Option<Uuid>) -> Result<(Vec<RunSummary>, Option<Uuid>)> {
        let mut statement = self.0.prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM workflow_runs
             WHERE (?1 IS NULL OR rowid < (SELECT rowid FROM workflow_runs WHERE id=?1))
             ORDER BY rowid DESC LIMIT ?2"
        ))?;
        let mut rows = statement
            .query_map(
                params![before.map(|id| id.to_string()), (RUN_PAGE_SIZE + 1) as u32],
                read_summary,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut next_before = None;
        if rows.len() > RUN_PAGE_SIZE {
            rows.pop();
            next_before = rows.last().map(|row| row.id);
        }
        Ok((rows, next_before))
    }

    pub fn insert(&self, id: Uuid, revision: &str, due_at: Option<DateTime<Utc>>) -> Result<()> {
        insert_run(&self.0, id, revision, due_at, None)
    }

    pub fn next_due(&self) -> Result<Option<i64>> {
        Ok(self.0.query_row(
            "SELECT min(due_ms) FROM workflow_runs WHERE state='pending'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn next_pending(&self) -> Result<Option<RunSummary>> {
        let id: Option<String> = self
            .0
            .query_row(
                "SELECT id FROM workflow_runs WHERE state='pending' AND due_ms <= ?1
                 ORDER BY due_ms, rowid LIMIT 1",
                [Utc::now().timestamp_millis()],
                |row| row.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.get(id.parse()?),
            None => Ok(None),
        }
    }

    pub fn transition(
        &self,
        run: &RunSummary,
        state: RunState,
        node: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        if state == RunState::Succeeded && run.state != RunState::Succeeded {
            let tx = self.write_transaction()?;
            update_state(&tx, run, state, node, detail)?;
            dependency_store::deliver(&tx, run)?;
            tx.commit()?;
            Ok(())
        } else {
            update_state(&self.0, run, state, node, detail)
        }
    }

    pub(crate) fn begin_worker_attempt(
        &self,
        run: &RunSummary,
        node: &str,
        expected_steps: u32,
    ) -> Result<WorkerAttempt> {
        ensure!(
            expected_steps > 0,
            "worker attempt must include its result step"
        );
        let tx = self.write_transaction()?;
        let current: (String, String, Option<String>) = tx.query_row(
            "SELECT revision, state, current_node FROM workflow_runs WHERE id=?1",
            [run.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        ensure!(current.0 == run.revision, "worker revision changed");
        match current.1.as_str() {
            "pending" => update_state(&tx, run, RunState::Running, Some(node), None)?,
            "running" => ensure!(
                current.2.as_deref() == Some(node),
                "worker node does not match the active run"
            ),
            state => anyhow::bail!("cannot claim worker attempt for run state `{state}`"),
        }

        let epoch: i64 = tx.query_row(
            "UPDATE workflow_attempt_clock SET epoch=epoch+1 WHERE singleton=1
             RETURNING epoch",
            [],
            |row| row.get(0),
        )?;
        let attempt = WorkerAttempt {
            id: Uuid::new_v4(),
            run_id: run.id,
            revision: run.revision.clone(),
            node: node.to_owned(),
            epoch,
            expected_steps,
        };
        tx.execute(
            "INSERT INTO workflow_attempts
                 (id,run_id,revision,node,epoch,expected_steps,state)
             VALUES(?1,?2,?3,?4,?5,?6,'claimed')",
            params![
                attempt.id.to_string(),
                attempt.run_id.to_string(),
                attempt.revision,
                attempt.node,
                attempt.epoch,
                attempt.expected_steps,
            ],
        )?;
        tx.commit()?;
        Ok(attempt)
    }

    pub(crate) fn mark_worker_started(&self, attempt: &WorkerAttempt) -> Result<()> {
        let tx = self.write_transaction()?;
        let changed = tx.execute(
            "UPDATE workflow_attempts
             SET state='started'
             WHERE id=?1 AND run_id=?2 AND revision=?3 AND node=?4 AND epoch=?5
               AND expected_steps=?6 AND state='claimed'
               AND EXISTS (
                   SELECT 1 FROM workflow_runs
                   WHERE id=?2 AND revision=?3 AND current_node=?4 AND state='running'
               )",
            params![
                attempt.id.to_string(),
                attempt.run_id.to_string(),
                attempt.revision,
                attempt.node,
                attempt.epoch,
                attempt.expected_steps,
            ],
        )?;
        ensure!(
            changed == 1,
            "worker attempt is stale or no longer claimable"
        );
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn fail_worker_attempt(
        &self,
        run: &RunSummary,
        attempt: &WorkerAttempt,
        detail: &str,
    ) -> Result<()> {
        ensure!(run.id == attempt.run_id, "worker attempt run ID mismatch");
        ensure!(
            run.revision == attempt.revision,
            "worker attempt revision mismatch"
        );
        ensure!(
            run.current_node.as_deref() == Some(attempt.node.as_str()),
            "worker attempt node mismatch"
        );

        let tx = self.write_transaction()?;
        let changed = tx.execute(
            "UPDATE workflow_attempts SET state='invalidated'
             WHERE id=?1 AND run_id=?2 AND revision=?3 AND node=?4 AND epoch=?5
               AND expected_steps=?6 AND state IN ('claimed','started')
               AND EXISTS (
                   SELECT 1 FROM workflow_runs
                   WHERE id=?2 AND revision=?3 AND current_node=?4 AND state='running'
               )",
            params![
                attempt.id.to_string(),
                attempt.run_id.to_string(),
                attempt.revision,
                attempt.node,
                attempt.epoch,
                attempt.expected_steps,
            ],
        )?;
        ensure!(changed == 1, "worker attempt is stale or already released");
        update_state(&tx, run, RunState::Failed, None, Some(detail))?;
        tx.commit()?;
        Ok(())
    }

    fn write_transaction(&self) -> Result<Transaction<'_>> {
        // The daemon serializes store calls. Take the writer lock before any
        // registration or delivery reads so they share one consistent snapshot.
        Ok(Transaction::new_unchecked(
            &self.0,
            TransactionBehavior::Immediate,
        )?)
    }

    pub fn traces(&self, id: Uuid) -> Result<Vec<String>> {
        let mut statement = self
            .0
            .prepare("SELECT trace FROM workflow_steps WHERE run_id=?1 ORDER BY position")?;
        let traces = statement
            .query_map([id.to_string()], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(traces)
    }

    pub fn step(&self, id: Uuid, index: u32) -> Result<Option<serde_json::Value>> {
        let row = self
            .0
            .query_row(
                "SELECT trace, artifact_hash, artifact_type, length(data)
                 FROM workflow_steps LEFT JOIN workflow_artifacts ON artifact_hash=sha256
                 WHERE run_id=?1 AND position=?2",
                params![id.to_string(), index],
                |row| {
                    Ok(StoredStep {
                        trace: row.get(0)?,
                        hash: row.get(1)?,
                        media_type: row.get(2)?,
                        size: row.get(3)?,
                    })
                },
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some(StoredStep {
                trace,
                hash,
                media_type,
                size,
            }) => {
                let trace: serde_json::Value = serde_json::from_str(&trace)?;
                let artifact = hash.map(|hash| {
                    serde_json::json!({"sha256": hash, "media_type": media_type, "size_bytes": size})
                });
                Ok(Some(
                    serde_json::json!({ "trace": trace, "artifact": artifact }),
                ))
            }
        }
    }

    pub fn artifact(
        &self,
        id: Uuid,
        index: u32,
        offset: u32,
    ) -> Result<Option<(Vec<u8>, Option<u32>)>> {
        self.0
            .query_row(
                "SELECT substr(data, ?3 + 1, ?4), length(data)
                 FROM workflow_steps JOIN workflow_artifacts ON artifact_hash=sha256
                 WHERE run_id=?1 AND position=?2 AND ?3 <= length(data)",
                params![id.to_string(), index, offset, ARTIFACT_CHUNK_SIZE],
                |row| {
                    let bytes: Vec<u8> = row.get(0)?;
                    let size: u32 = row.get(1)?;
                    let end = offset + bytes.len() as u32;
                    Ok((bytes, (end < size).then_some(end)))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn append(
        &mut self,
        run: &RunSummary,
        trace: &str,
        artifact: Option<&Artifact>,
        agent_unused: Duration,
    ) -> Result<()> {
        let tx = self.0.transaction()?;
        update_state(&tx, run, RunState::Pending, None, None)?;
        agent_budget_store::settle(&tx, run, agent_unused)?;
        let (hash, media_type) = match artifact {
            Some(artifact) => {
                let hash = format!("{:x}", Sha256::digest(&artifact.bytes));
                tx.execute(
                    "INSERT INTO workflow_artifacts(sha256,data) VALUES(?1,?2)
                     ON CONFLICT(sha256) DO NOTHING",
                    params![hash, artifact.bytes],
                )?;
                (Some(hash), Some(artifact.media_type))
            }
            None => (None, None),
        };
        tx.execute(
            "INSERT INTO workflow_steps(run_id, position, trace, artifact_hash, artifact_type)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![run.id.to_string(), run.step_count, trace, hash, media_type],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn append_worker_result(
        &mut self,
        run: &RunSummary,
        attempt: &WorkerAttempt,
        trace: &str,
        artifact: Option<&Artifact>,
        agent_unused: Duration,
    ) -> Result<()> {
        ensure!(run.id == attempt.run_id, "worker result run ID mismatch");
        ensure!(
            run.revision == attempt.revision,
            "worker result revision mismatch"
        );
        ensure!(
            run.current_node.as_deref() == Some(attempt.node.as_str()),
            "worker result node mismatch"
        );
        ensure!(
            attempt.expected_steps == run.step_count.saturating_add(1),
            "worker result step count mismatch"
        );

        let tx = self.write_transaction()?;
        let changed = tx.execute(
            "UPDATE workflow_attempts SET state='committed'
             WHERE id=?1 AND run_id=?2 AND revision=?3 AND node=?4 AND epoch=?5
               AND expected_steps=?6 AND state='started'
               AND EXISTS (
                   SELECT 1 FROM workflow_runs
                   WHERE id=?2 AND revision=?3 AND current_node=?4 AND state='running'
               )",
            params![
                attempt.id.to_string(),
                attempt.run_id.to_string(),
                attempt.revision,
                attempt.node,
                attempt.epoch,
                attempt.expected_steps,
            ],
        )?;
        ensure!(changed == 1, "worker result is stale or fenced");

        update_state(&tx, run, RunState::Pending, None, None)?;
        agent_budget_store::settle(&tx, run, agent_unused)?;
        let (hash, media_type) = match artifact {
            Some(artifact) => {
                let hash = format!("{:x}", Sha256::digest(&artifact.bytes));
                tx.execute(
                    "INSERT INTO workflow_artifacts(sha256,data) VALUES(?1,?2)
                     ON CONFLICT(sha256) DO NOTHING",
                    params![hash, artifact.bytes],
                )?;
                (Some(hash), Some(artifact.media_type))
            }
            None => (None, None),
        };
        tx.execute(
            "INSERT INTO workflow_steps(run_id, position, trace, artifact_hash, artifact_type)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![run.id.to_string(), run.step_count, trace, hash, media_type],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_authorization(&self, revision: &str, node: &str, granted: bool) -> Result<()> {
        let sql = if granted {
            "INSERT OR IGNORE INTO script_authorizations(revision,node) VALUES(?1,?2)"
        } else {
            "DELETE FROM script_authorizations WHERE revision=?1 AND node=?2"
        };
        self.0.execute(sql, params![revision, node])?;
        Ok(())
    }

    pub fn authorized(&self, revision: &str, node: &str) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM script_authorizations WHERE revision=?1 AND node=?2)",
            params![revision, node],
            |row| row.get(0),
        )?)
    }
}

struct StoredStep {
    trace: String,
    hash: Option<String>,
    media_type: Option<String>,
    size: Option<u32>,
}

fn due_millis(due: DateTime<Utc>) -> i64 {
    due.timestamp_millis() + i64::from(!due.timestamp_subsec_nanos().is_multiple_of(1_000_000))
}

fn insert_run(
    db: &Connection,
    id: Uuid,
    revision: &str,
    due_at: Option<DateTime<Utc>>,
    schedule_id: Option<Uuid>,
) -> Result<()> {
    let due_ms = due_at.map_or_else(|| Utc::now().timestamp_millis(), due_millis);
    db.execute(
        "INSERT INTO workflow_runs(id, revision, due_at, due_ms, state, schedule_id)
         VALUES(?1, ?2, ?3, ?4, 'pending', ?5)",
        params![
            id.to_string(),
            revision,
            due_at.map(|at| at.to_rfc3339()),
            due_ms,
            schedule_id.map(|schedule| schedule.to_string()),
        ],
    )?;
    Ok(())
}

fn update_state(
    db: &Connection,
    run: &RunSummary,
    state: RunState,
    node: Option<&str>,
    detail: Option<&str>,
) -> Result<()> {
    let changed = db.execute(
        "UPDATE workflow_runs SET state=?1, current_node=?2, detail=?3
         WHERE id=?4 AND state=?5 AND current_node IS ?6
           AND (SELECT count(*) FROM workflow_steps WHERE run_id=?4)=?7",
        params![
            state_key(state),
            node,
            detail,
            run.id.to_string(),
            state_key(run.state),
            run.current_node,
            run.step_count,
        ],
    )?;
    if changed != 1 {
        let actual: Option<(String, Option<String>, i64)> = db
            .query_row(
                "SELECT state, current_node,
                    (SELECT count(*) FROM workflow_steps WHERE run_id=workflow_runs.id)
                 FROM workflow_runs WHERE id=?1",
                [run.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        anyhow::bail!(
            "workflow run changed since it was loaded (expected state={}, node={:?}, steps={}; actual={actual:?})",
            state_key(run.state),
            run.current_node,
            run.step_count
        );
    }
    Ok(())
}

fn read_summary(row: &Row<'_>) -> rusqlite::Result<RunSummary> {
    let id: String = row.get(0)?;
    let due_at: Option<String> = row.get(2)?;
    let state: String = row.get(3)?;
    let schedule_id: Option<String> = row.get(7)?;
    let source_run_id: Option<String> = row.get(8)?;
    let source_event_id: Option<String> = row.get(9)?;
    Ok(RunSummary {
        id: id.parse().map_err(|error| decode_error(0, error))?,
        revision: row.get(1)?,
        due_at: due_at
            .map(|due| due.parse())
            .transpose()
            .map_err(|error| decode_error(2, error))?,
        schedule_id: schedule_id
            .map(|id| id.parse())
            .transpose()
            .map_err(|error| decode_error(7, error))?,
        source_run_id: source_run_id
            .map(|id| id.parse())
            .transpose()
            .map_err(|error| decode_error(8, error))?,
        source_event_id: source_event_id
            .map(|id| id.parse())
            .transpose()
            .map_err(|error| decode_error(9, error))?,
        state: parse_state(&state).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(3, state, rusqlite::types::Type::Text)
        })?,
        current_node: row.get(4)?,
        detail: row.get(5)?,
        step_count: row.get(6).context("decoding step count").map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Integer,
                error.into(),
            )
        })?,
        resolution: {
            let outcome: Option<String> = row.get(10)?;
            let note: Option<String> = row.get(11)?;
            let recorded_at: Option<String> = row.get(12)?;
            match (outcome, note, recorded_at) {
                (None, None, None) => None,
                (Some(outcome), Some(note), Some(recorded_at)) => Some(RunResolution {
                    outcome: parse_outcome(&outcome).ok_or_else(|| {
                        rusqlite::Error::InvalidColumnType(10, outcome, rusqlite::types::Type::Text)
                    })?,
                    note,
                    recorded_at: recorded_at
                        .parse()
                        .map_err(|error| decode_error(12, error))?,
                }),
                _ => {
                    return Err(rusqlite::Error::InvalidColumnType(
                        10,
                        "incomplete resolution".to_owned(),
                        rusqlite::types::Type::Null,
                    ));
                }
            }
        },
    })
}

fn outcome_key(outcome: &ObservedOutcome) -> &'static str {
    match outcome {
        ObservedOutcome::Completed => "completed",
        ObservedOutcome::Failed => "failed",
        ObservedOutcome::NotStarted => "not_started",
    }
}

fn parse_outcome(outcome: &str) -> Option<ObservedOutcome> {
    match outcome {
        "completed" => Some(ObservedOutcome::Completed),
        "failed" => Some(ObservedOutcome::Failed),
        "not_started" => Some(ObservedOutcome::NotStarted),
        _ => None,
    }
}

pub(crate) fn decode_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn state_key(state: RunState) -> &'static str {
    match state {
        RunState::Pending => "pending",
        RunState::Waiting => "waiting",
        RunState::Running => "running",
        RunState::Cancelling => "cancelling",
        RunState::Cancelled => "cancelled",
        RunState::Succeeded => "succeeded",
        RunState::Failed => "failed",
        RunState::OutcomeUnknown => "outcome_unknown",
    }
}

pub(crate) fn parse_state(state: &str) -> Option<RunState> {
    [
        RunState::Pending,
        RunState::Waiting,
        RunState::Running,
        RunState::Cancelling,
        RunState::Cancelled,
        RunState::Succeeded,
        RunState::Failed,
        RunState::OutcomeUnknown,
    ]
    .into_iter()
    .find(|value| state_key(*value) == state)
}

#[cfg(test)]
mod resolution_tests {
    use super::*;

    #[test]
    fn uncertain_run_resolution_is_audited_once_without_rewriting_run_state() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let id = Uuid::new_v4();
        store.insert(id, &"a".repeat(64), None)?;
        let pending = store.get(id)?.context("run was not inserted")?;
        store.transition(
            &pending,
            RunState::OutcomeUnknown,
            Some("external"),
            Some("Interrupted before durable result"),
        )?;

        let resolution = store.resolve_unknown(
            id,
            ObservedOutcome::NotStarted,
            "Checked the external system; no effect exists.",
        )?;
        let run = store.get(id)?.context("resolved run disappeared")?;
        assert_eq!(run.state, RunState::OutcomeUnknown);
        assert_eq!(run.current_node.as_deref(), Some("external"));
        assert_eq!(run.resolution, Some(resolution.clone()));
        assert!(
            store
                .resolve_unknown(
                    id,
                    ObservedOutcome::Completed,
                    "conflicting second decision"
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn only_uncertain_runs_can_be_resolved() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let id = Uuid::new_v4();
        store.insert(id, &"b".repeat(64), None)?;
        assert!(
            store
                .resolve_unknown(id, ObservedOutcome::Failed, "The operation failed.")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn worker_attempts_are_started_once_and_fenced_by_their_full_binding() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let id = Uuid::new_v4();
        store.insert(id, &"c".repeat(64), None)?;
        let pending = store.get(id)?.context("run was not inserted")?;
        let attempt = store.begin_worker_attempt(&pending, "script", 1)?;

        let mut mismatched = attempt.clone();
        mismatched.node = "other-node".into();
        assert!(store.mark_worker_started(&mismatched).is_err());
        store.mark_worker_started(&attempt)?;
        assert!(store.mark_worker_started(&attempt).is_err());

        let running = store.get(id)?.context("running attempt disappeared")?;
        let mut mismatched_result = attempt.clone();
        mismatched_result.epoch += 1;
        assert!(
            store
                .append_worker_result(
                    &running,
                    &mismatched_result,
                    r#"{"status":"succeeded"}"#,
                    None,
                    Duration::ZERO,
                )
                .is_err()
        );
        store.append_worker_result(
            &running,
            &attempt,
            r#"{"status":"succeeded"}"#,
            None,
            Duration::ZERO,
        )?;
        assert_eq!(store.get(id)?.context("run disappeared")?.step_count, 1);
        Ok(())
    }

    #[test]
    fn unstarted_worker_failure_atomically_fails_run_and_invalidates_attempt() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;

        for mark_started in [false, true] {
            let id = Uuid::new_v4();
            store.insert(id, &"f".repeat(64), None)?;
            let pending = store.get(id)?.context("run was not inserted")?;
            let attempt = store.begin_worker_attempt(&pending, "script", 1)?;
            if mark_started {
                store.mark_worker_started(&attempt)?;
            }
            let running = store.get(id)?.context("run was not started")?;

            let mut mismatched = attempt.clone();
            mismatched.epoch += 1;
            assert!(
                store
                    .fail_worker_attempt(&running, &mismatched, "worker failed before release",)
                    .is_err()
            );
            assert_eq!(
                store.get(id)?.context("run disappeared")?.state,
                RunState::Running
            );

            store.fail_worker_attempt(&running, &attempt, "worker failed before release")?;
            let failed = store.get(id)?.context("failed run disappeared")?;
            assert_eq!(failed.state, RunState::Failed);
            assert_eq!(failed.current_node, None);
            assert_eq!(
                store.0.query_row(
                    "SELECT state FROM workflow_attempts WHERE id=?1",
                    [attempt.id.to_string()],
                    |row| row.get::<_, String>(0),
                )?,
                "invalidated"
            );
            assert!(
                store
                    .fail_worker_attempt(&running, &attempt, "duplicate failure",)
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn worker_attempt_epochs_increase_and_restart_invalidates_uncertain_work() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let id = Uuid::new_v4();
        let attempt = {
            let mut store = RunStore::open(&path)?;
            store.insert(id, &"d".repeat(64), None)?;
            let pending = store.get(id)?.context("run was not inserted")?;
            let first = store.begin_worker_attempt(&pending, "script", 1)?;
            store.mark_worker_started(&first)?;
            let running = store.get(id)?.context("run was not running")?;
            store.append_worker_result(
                &running,
                &first,
                r#"{"status":"succeeded"}"#,
                None,
                Duration::ZERO,
            )?;
            let pending = store.get(id)?.context("run did not return to pending")?;
            let second = store.begin_worker_attempt(&pending, "script", 2)?;
            assert!(second.epoch > first.epoch);
            store.mark_worker_started(&second)?;
            second
        };

        let mut store = RunStore::open(&path)?;
        let recovered = store.get(id)?.context("recovered run disappeared")?;
        assert_eq!(recovered.state, RunState::OutcomeUnknown);
        assert!(
            store
                .append_worker_result(
                    &recovered,
                    &attempt,
                    r#"{"status":"succeeded"}"#,
                    None,
                    Duration::ZERO,
                )
                .is_err()
        );
        assert_eq!(recovered.step_count, 1);
        Ok(())
    }

    #[test]
    fn failed_worker_result_transaction_keeps_attempt_started_and_run_unchanged() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let id = Uuid::new_v4();
        store.insert(id, &"e".repeat(64), None)?;
        let pending = store.get(id)?.context("run was not inserted")?;
        let attempt = store.begin_worker_attempt(&pending, "script", 1)?;
        store.mark_worker_started(&attempt)?;
        let running = store.get(id)?.context("run was not running")?;
        store.0.execute(
            "INSERT INTO workflow_steps(run_id,position,trace)
             VALUES(?1,?2,'{}')",
            params![id.to_string(), running.step_count],
        )?;

        assert!(
            store
                .append_worker_result(
                    &running,
                    &attempt,
                    r#"{"status":"succeeded"}"#,
                    Some(&Artifact {
                        media_type: "application/octet-stream",
                        bytes: vec![1, 2, 3],
                    }),
                    Duration::ZERO,
                )
                .is_err()
        );
        let still_running = store.get(id)?.context("run disappeared")?;
        assert_eq!(still_running.state, RunState::Running);
        assert_eq!(
            store.0.query_row(
                "SELECT state FROM workflow_attempts WHERE id=?1",
                [attempt.id.to_string()],
                |row| row.get::<_, String>(0),
            )?,
            "started"
        );
        assert_eq!(
            store
                .0
                .query_row("SELECT count(*) FROM workflow_artifacts", [], |row| row
                    .get::<_, u32>(0),)?,
            0
        );
        Ok(())
    }
}
