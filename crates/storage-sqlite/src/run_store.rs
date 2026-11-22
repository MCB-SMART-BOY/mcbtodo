use std::path::Path;

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use mcbtodo_ports::workflow::{
    AgentAttemptClaim, AgentBudgetDenied, ArtifactChunk, ArtifactMetadata, AttemptResult,
    ClaimAttempt, RecordedStep, ScriptAuthorizationStore, WorkflowArtifact, WorkflowAttempt,
    WorkflowHistoryStore, WorkflowProgressStore, WorkflowRepository, WorkflowRunCatalog,
    WorkflowRunStore, WorkflowStorageCapabilities,
};
use mcbtodo_workflow::{
    StepTrace,
    run::{ObservedOutcome, RunResolution, RunState, RunSummary},
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::schema_version::{WORKFLOW as SCHEMA_VERSION, check_workflow};

#[path = "agent_budget_store.rs"]
mod agent_budget_store;
#[path = "completion_store.rs"]
mod completion_store;
#[path = "dependency_store.rs"]
mod dependency_store;
#[path = "inbox.rs"]
mod inbox;
#[path = "schedule_store.rs"]
mod schedule_store;
pub struct RunStore(Connection);

pub(crate) use schedule_store::validate_schedules;

impl WorkflowRepository for RunStore {
    fn capabilities(&self) -> WorkflowStorageCapabilities {
        WorkflowStorageCapabilities {
            backend_id: "sqlite",
            atomic_batches: true,
            durable_cursor: true,
        }
    }
}

pub const RUN_PAGE_SIZE: usize = 12;
pub const ARTIFACT_CHUNK_SIZE: u32 = 8192;

const RUN_COLUMNS: &str = "id, revision, due_at, state, current_node, detail,
    (SELECT count(*) FROM workflow_steps WHERE run_id=workflow_runs.id), schedule_id,
    (SELECT source_run_id FROM workflow_dependency_deliveries WHERE child_run_id=workflow_runs.id),
    (SELECT event_id FROM workflow_completion_receipts WHERE run_id=workflow_runs.id),
    (SELECT outcome FROM workflow_run_resolutions WHERE run_id=workflow_runs.id),
    (SELECT note FROM workflow_run_resolutions WHERE run_id=workflow_runs.id),
    (SELECT recorded_at FROM workflow_run_resolutions WHERE run_id=workflow_runs.id)";

impl RunStore {
    pub fn open(path: &Path) -> Result<Self> {
        mcbtodo_platform::private_file::open_private_file(path)?;
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        let tx = db.transaction()?;
        let version = check_workflow(&tx)?;
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
        inbox::install(&tx)?;
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
        if version == 0 {
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        tx.commit()?;
        Ok(Self(db))
    }

    fn write_transaction(&self) -> Result<Transaction<'_>> {
        // Acquire the writer lock first so subsequent reads and mutations
        // use a single snapshot.
        Ok(Transaction::new_unchecked(
            &self.0,
            TransactionBehavior::Immediate,
        )?)
    }

    fn has_active_attempt(&self, run_id: Uuid) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_attempts
             WHERE run_id=?1 AND state IN ('claimed','started'))",
            [run_id.to_string()],
            |row| row.get(0),
        )?)
    }
}

impl WorkflowProgressStore for RunStore {
    fn resolve_unknown(
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

    fn transition(
        &self,
        run: &RunSummary,
        state: RunState,
        node: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        let tx = self.write_transaction()?;
        if !(run.state == RunState::Running
            && state == RunState::Cancelling
            && node == run.current_node.as_deref())
        {
            ensure!(
                !self.has_active_attempt(run.id)?,
                "active Worker attempts require an Attempt transition"
            );
        }
        update_state(&tx, run, state, node, detail)?;
        if state == RunState::Succeeded && run.state != RunState::Succeeded {
            dependency_store::deliver(&tx, run)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn append_host_step(
        &self,
        run: &RunSummary,
        trace: &str,
        artifact: Option<&WorkflowArtifact>,
    ) -> Result<()> {
        let tx = self.write_transaction()?;
        ensure!(
            !self.has_active_attempt(run.id)?,
            "active Worker attempts require a fenced result"
        );
        append_step(&tx, run, trace, artifact)?;
        tx.commit()?;
        Ok(())
    }
}

impl ScriptAuthorizationStore for RunStore {
    fn set_authorization(&self, revision: &str, node: &str, granted: bool) -> Result<()> {
        let sql = if granted {
            "INSERT OR IGNORE INTO script_authorizations(revision,node) VALUES(?1,?2)"
        } else {
            "DELETE FROM script_authorizations WHERE revision=?1 AND node=?2"
        };
        self.0.execute(sql, params![revision, node])?;
        Ok(())
    }

    fn authorized(&self, revision: &str, node: &str) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM script_authorizations WHERE revision=?1 AND node=?2)",
            params![revision, node],
            |row| row.get(0),
        )?)
    }
}

impl WorkflowRunCatalog for RunStore {
    fn get(&self, id: Uuid) -> Result<Option<RunSummary>> {
        self.0
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id=?1"),
                [id.to_string()],
                read_summary,
            )
            .optional()
            .map_err(Into::into)
    }

    fn list(&self, before: Option<Uuid>) -> Result<(Vec<RunSummary>, Option<Uuid>)> {
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

    fn enqueue_if_absent(
        &self,
        id: Uuid,
        revision: &str,
        due_at: Option<DateTime<Utc>>,
    ) -> Result<RunSummary> {
        // Reads through this store participate in the transaction on its connection.
        let tx = self.write_transaction()?;
        let run = match self.get(id)? {
            Some(existing) => existing,
            None => {
                insert_run(&tx, id, revision, due_at, None)?;
                self.get(id)?.context("Inserted workflow run is missing")?
            }
        };
        tx.commit()?;
        Ok(run)
    }
}

impl WorkflowHistoryStore for RunStore {
    fn history(&self, run_id: Uuid) -> Result<Vec<StepTrace>> {
        let mut statement = self
            .0
            .prepare("SELECT trace FROM workflow_steps WHERE run_id=?1 ORDER BY position")?;
        let rows = statement.query_map([run_id.to_string()], |row| row.get::<_, String>(0))?;
        rows.enumerate()
            .map(|(index, row)| decode_trace(&row?, run_id, index))
            .collect()
    }

    fn recorded_step(&self, run_id: Uuid, index: u32) -> Result<Option<RecordedStep>> {
        let row = self
            .0
            .query_row(
                "SELECT trace, artifact_hash, artifact_type, length(data)
                 FROM workflow_steps LEFT JOIN workflow_artifacts ON artifact_hash=sha256
                 WHERE run_id=?1 AND position=?2",
                params![run_id.to_string(), index],
                |row| {
                    let artifact = match row.get(1)? {
                        Some(sha256) => Some(ArtifactMetadata {
                            sha256,
                            media_type: row.get(2)?,
                            size_bytes: row.get(3)?,
                        }),
                        None => None,
                    };
                    Ok((row.get::<_, String>(0)?, artifact))
                },
            )
            .optional()?;
        let Some((trace, artifact)) = row else {
            return Ok(None);
        };
        Ok(Some(RecordedStep {
            trace: decode_trace(&trace, run_id, index as usize)?,
            artifact,
        }))
    }

    fn artifact_chunk(
        &self,
        run_id: Uuid,
        index: u32,
        offset: u32,
    ) -> Result<Option<ArtifactChunk>> {
        self.0
            .query_row(
                "SELECT substr(data, ?3 + 1, ?4), length(data)
                 FROM workflow_steps JOIN workflow_artifacts ON artifact_hash=sha256
                 WHERE run_id=?1 AND position=?2 AND ?3 <= length(data)",
                params![run_id.to_string(), index, offset, ARTIFACT_CHUNK_SIZE],
                |row| {
                    let bytes: Vec<u8> = row.get(0)?;
                    let size: u32 = row.get(1)?;
                    let end = offset + bytes.len() as u32;
                    Ok(ArtifactChunk {
                        bytes,
                        next_offset: (end < size).then_some(end),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

impl RunStore {
    fn insert_attempt(
        tx: &Transaction<'_>,
        run: RunSummary,
        claim: ClaimAttempt<'_>,
    ) -> Result<WorkflowAttempt> {
        ensure!(run.revision == claim.revision, "worker revision changed");
        ensure!(
            run.step_count.checked_add(1) == Some(claim.expected_steps),
            "worker claim step count mismatch"
        );
        match run.state {
            RunState::Pending => update_state(tx, &run, RunState::Running, Some(claim.node), None)?,
            RunState::Running => ensure!(
                run.current_node.as_deref() == Some(claim.node),
                "worker node does not match the active run"
            ),
            state => anyhow::bail!(
                "cannot claim worker attempt for run state `{}`",
                state_key(state)
            ),
        }
        let epoch: i64 = tx.query_row(
            "UPDATE workflow_attempt_clock SET epoch=epoch+1 WHERE singleton=1 RETURNING epoch",
            [],
            |row| row.get(0),
        )?;
        let attempt = WorkflowAttempt {
            id: Uuid::new_v4(),
            run_id: run.id,
            revision: run.revision,
            node: claim.node.to_owned(),
            epoch,
            expected_steps: claim.expected_steps,
        };
        tx.execute(
            "INSERT INTO workflow_attempts
                 (id,run_id,revision,node,epoch,expected_steps,state)
             VALUES(?1,?2,?3,?4,?5,?6,'claimed')",
            attempt_params(&attempt),
        )?;
        Ok(attempt)
    }
}

impl WorkflowRunStore for RunStore {
    fn claim_attempt(&self, claim: ClaimAttempt<'_>) -> Result<WorkflowAttempt> {
        let tx = self.write_transaction()?;
        let run = self
            .get(claim.run_id)?
            .context("workflow run was not found")?;
        let attempt = Self::insert_attempt(&tx, run, claim)?;
        tx.commit()?;
        Ok(attempt)
    }

    fn claim_agent(
        &self,
        claim: ClaimAttempt<'_>,
        has_prior_agent_step: bool,
    ) -> Result<AgentAttemptClaim> {
        self.reserve_and_claim_agent(claim, has_prior_agent_step)
    }

    fn mark_started(&self, attempt: &WorkflowAttempt) -> Result<()> {
        let tx = self.write_transaction()?;
        let run = self.attempt_run(attempt)?;
        ensure!(
            run.state == RunState::Running,
            "worker run is no longer running"
        );
        let changed = tx.execute(
            "UPDATE workflow_attempts
             SET state='started'
             WHERE id=?1 AND run_id=?2 AND revision=?3 AND node=?4 AND epoch=?5
               AND expected_steps=?6 AND state='claimed'",
            attempt_params(attempt),
        )?;
        ensure!(
            changed == 1,
            "worker attempt is stale or no longer claimable"
        );
        tx.commit()?;
        Ok(())
    }

    fn commit_result(&self, result: AttemptResult<'_>) -> Result<()> {
        let tx = self.write_transaction()?;
        let run = self.attempt_run(result.attempt)?;
        ensure!(
            run.state == RunState::Running,
            "worker run is no longer running"
        );
        let changed = tx.execute(
            "UPDATE workflow_attempts SET state='committed'
             WHERE id=?1 AND run_id=?2 AND revision=?3 AND node=?4 AND epoch=?5
               AND expected_steps=?6 AND state='started'",
            attempt_params(result.attempt),
        )?;
        ensure!(changed == 1, "worker result is stale or fenced");
        append_step(&tx, &run, result.trace, result.artifact)?;
        agent_budget_store::settle(&tx, &run, result.agent_unused)?;
        tx.commit()?;
        Ok(())
    }

    fn fail_attempt(&self, attempt: &WorkflowAttempt, detail: &str) -> Result<()> {
        self.end_attempt(attempt, RunState::Failed, detail)
    }

    fn cancel_claimed_attempt(&self, attempt: &WorkflowAttempt) -> Result<()> {
        self.end_attempt(
            attempt,
            RunState::Cancelled,
            "Cancelled before Worker Start",
        )
    }

    fn mark_unknown(&self, attempt: &WorkflowAttempt, detail: &str) -> Result<()> {
        self.end_attempt(attempt, RunState::OutcomeUnknown, detail)
    }
}

impl RunStore {
    /// Read under the caller's immediate writer transaction on this connection.
    fn attempt_run(&self, attempt: &WorkflowAttempt) -> Result<RunSummary> {
        let run = self
            .get(attempt.run_id)?
            .context("workflow run was not found")?;
        ensure!(run.revision == attempt.revision, "worker revision changed");
        ensure!(
            run.current_node.as_deref() == Some(&attempt.node),
            "worker attempt node mismatch"
        );
        ensure!(
            run.step_count.checked_add(1) == Some(attempt.expected_steps),
            "worker attempt step count mismatch"
        );
        Ok(run)
    }

    fn end_attempt(&self, attempt: &WorkflowAttempt, state: RunState, detail: &str) -> Result<()> {
        let tx = self.write_transaction()?;
        let run = self.attempt_run(attempt)?;
        let unknown = state == RunState::OutcomeUnknown;
        let cancelled = state == RunState::Cancelled;
        ensure!(
            match run.state {
                RunState::Running => !cancelled,
                RunState::Cancelling => unknown || cancelled,
                _ => false,
            },
            "workflow run is no longer active"
        );
        let changed = tx.execute(
            "UPDATE workflow_attempts SET state='invalidated'
             WHERE id=?1 AND run_id=?2 AND revision=?3 AND node=?4 AND epoch=?5
               AND expected_steps=?6 AND (state='claimed' OR (NOT ?7 AND state='started'))",
            params![
                attempt.id.to_string(),
                attempt.run_id.to_string(),
                attempt.revision,
                attempt.node,
                attempt.epoch,
                attempt.expected_steps,
                cancelled,
            ],
        )?;
        ensure!(changed == 1, "worker attempt is stale or already released");
        let node = unknown.then_some(attempt.node.as_str());
        update_state(&tx, &run, state, node, Some(detail))?;
        tx.commit()?;
        Ok(())
    }
}

fn attempt_params(attempt: &WorkflowAttempt) -> impl rusqlite::Params {
    (
        attempt.id.to_string(),
        attempt.run_id.to_string(),
        &attempt.revision,
        &attempt.node,
        attempt.epoch,
        attempt.expected_steps,
    )
}

fn append_step(
    tx: &Transaction<'_>,
    run: &RunSummary,
    trace: &str,
    artifact: Option<&WorkflowArtifact>,
) -> Result<()> {
    update_state(tx, run, RunState::Pending, None, None)?;
    let (hash, media_type) = match artifact {
        Some(artifact) => {
            let hash = hex::encode(Sha256::digest(&artifact.bytes));
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
    Ok(())
}

fn decode_trace(trace: &str, run_id: Uuid, index: usize) -> Result<StepTrace> {
    serde_json::from_str(trace)
        .with_context(|| format!("Invalid trace for run {run_id}, step {index}"))
}

pub(crate) fn validate_history(db: &Connection) -> Result<()> {
    let mut runs = db.prepare(&format!("SELECT {RUN_COLUMNS}, due_ms FROM workflow_runs"))?;
    let mut rows = runs.query([])?;
    while let Some(row) = rows.next()? {
        let id = row.get_ref(0)?.as_str()?;
        let run = read_summary(row).with_context(|| format!("decoding saved workflow run {id}"))?;
        let stored_due_ms: i64 = row.get(13)?;
        ensure!(
            run.due_at
                .is_none_or(|due| due_millis(due) == stored_due_ms),
            "inconsistent due time for workflow run {id}"
        );
    }

    let mut steps = db.prepare("SELECT run_id, position, trace FROM workflow_steps")?;
    let mut rows = steps.query([])?;
    while let Some(row) = rows.next()? {
        decode_trace(
            row.get_ref(2)?.as_str()?,
            row.get_ref(0)?.as_str()?.parse()?,
            row.get::<_, i64>(1)?.try_into()?,
        )?;
    }
    Ok(())
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
           AND (SELECT count(*) FROM workflow_steps WHERE run_id=?4)=?7
           AND revision=?8 AND detail IS ?9",
        params![
            state_key(state),
            node,
            detail,
            run.id.to_string(),
            state_key(run.state),
            run.current_node,
            run.step_count,
            run.revision,
            run.detail,
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
    use mcbtodo_ports::notifications::NotificationStore;
    use std::time::Duration;

    #[test]
    fn simultaneous_enqueue_requests_return_one_durable_run() -> Result<()> {
        use std::{sync::Barrier, thread};

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let id = Uuid::new_v4();
        let first = RunStore::open(&path)?;
        let second = RunStore::open(&path)?;
        let start = Barrier::new(2);
        let (a, b) = thread::scope(|scope| {
            let start = &start;
            let a = scope.spawn(move || {
                start.wait();
                first.enqueue_if_absent(id, "first-revision", None)
            });
            let b = scope.spawn(move || {
                start.wait();
                second.enqueue_if_absent(id, "second-revision", None)
            });
            (a.join().unwrap(), b.join().unwrap())
        });
        let run = a?;
        assert_eq!(run, b?);
        assert_eq!(run.id, id);
        assert_eq!(run.state, RunState::Pending);
        assert!(run.schedule_id.is_none());
        let reopened = RunStore::open(&path)?;
        assert_eq!(reopened.list(None)?, (vec![run], None));
        Ok(())
    }

    #[test]
    fn opening_unversioned_workflow_database_preserves_history_and_fences_future_schemas()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let id = Uuid::new_v4();
        let store = RunStore::open(&path)?;
        store.enqueue_if_absent(id, "published-revision", None)?;
        store.transition(
            &store.get(id)?.unwrap(),
            RunState::Failed,
            None,
            Some("failure"),
        )?;
        let original = store.get(id)?.unwrap();
        let page = store.workflow_page(None)?;
        let watermark = store.workflow_watermark()?;
        store.0.pragma_update(None, "user_version", 0)?;
        drop(store);

        let reopened = RunStore::open(&path)?;
        assert_eq!(reopened.get(id)?, Some(original));
        assert_eq!(reopened.workflow_page(None)?, page);
        assert_eq!(reopened.workflow_watermark()?, watermark);
        assert_eq!(
            reopened
                .0
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))?,
            SCHEMA_VERSION
        );
        reopened
            .0
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)?;
        drop(reopened);

        let error = RunStore::open(&path)
            .err()
            .context("future schema was accepted")?;
        assert!(
            error
                .to_string()
                .contains("unsupported workflow database schema version")
        );
        Ok(())
    }

    #[test]
    fn uncertain_run_resolution_is_audited_once_without_rewriting_run_state() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let id = Uuid::new_v4();
        store.enqueue_if_absent(id, &"a".repeat(64), None)?;
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
        store.enqueue_if_absent(id, &"b".repeat(64), None)?;
        assert!(
            store
                .resolve_unknown(id, ObservedOutcome::Failed, "The operation failed.")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn a_refreshed_review_fences_an_append_from_the_previous_snapshot() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let store = RunStore::open(&path)?;
        let id = Uuid::new_v4();
        store.enqueue_if_absent(id, "revision", None)?;
        let pending = store.get(id)?.context("missing pending run")?;
        store.transition(
            &pending,
            RunState::Waiting,
            Some("review"),
            Some("Previous review"),
        )?;
        let stale = store.get(id)?.context("missing waiting run")?;
        let writer = RunStore::open(&path)?;
        writer.transition(
            &stale,
            RunState::Waiting,
            Some("review"),
            Some("Updated review"),
        )?;

        let refreshed = store.get(id)?.context("missing refreshed run")?;
        assert_ne!(refreshed.approval_review(), stale.approval_review());
        assert!(
            store
                .append_host_step(&stale, r#"{"status":"succeeded"}"#, None)
                .is_err()
        );
        assert_eq!(store.get(id)?, Some(refreshed));
        assert!(store.history(id)?.is_empty());
        Ok(())
    }

    #[test]
    fn history_decodes_existing_traces_and_resumes_artifact_reads_after_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let store = RunStore::open(&path)?;
        let id = Uuid::new_v4();
        store.enqueue_if_absent(id, "published-revision", None)?;
        let trace = r#"{"node":"report","status":"succeeded","outcome":null,"inputs":{},"outputs":{},"reason":"完成"}"#;
        let artifact = WorkflowArtifact {
            bytes: (0..ARTIFACT_CHUNK_SIZE + 7)
                .map(|index| index as u8)
                .collect(),
            media_type: "application/octet-stream",
        };
        store.append_host_step(
            &store.get(id)?.context("missing run")?,
            trace,
            Some(&artifact),
        )?;

        let first = store.artifact_chunk(id, 0, 0)?.context("missing start")?;
        assert_eq!(first.bytes.len(), ARTIFACT_CHUNK_SIZE as usize);
        assert_eq!(first.bytes, artifact.bytes[..first.bytes.len()]);
        let cursor = first.next_offset.context("missing next offset")?;
        drop(store);

        let reopened = RunStore::open(&path)?;
        let step = reopened.recorded_step(id, 0)?.context("missing step")?;
        assert_eq!(
            serde_json::to_value(&step.trace)?,
            serde_json::from_str::<serde_json::Value>(trace)?
        );
        assert_eq!(reopened.history(id)?, vec![step.trace]);
        let metadata = step.artifact.context("missing metadata")?;
        assert_eq!(metadata.size_bytes as usize, artifact.bytes.len());
        assert_eq!(
            metadata.sha256,
            hex::encode(Sha256::digest(&artifact.bytes))
        );
        let rest = reopened
            .artifact_chunk(id, 0, cursor)?
            .context("missing final chunk")?;
        assert_eq!(rest.bytes, artifact.bytes[cursor as usize..]);
        assert_eq!(rest.next_offset, None);
        let end = reopened
            .artifact_chunk(id, 0, metadata.size_bytes)?
            .context("missing end")?;
        assert!(end.bytes.is_empty());
        assert_eq!(end.next_offset, None);
        assert!(
            reopened
                .artifact_chunk(id, 0, metadata.size_bytes + 1)?
                .is_none()
        );

        reopened
            .0
            .execute("UPDATE workflow_steps SET trace='{}'", [])?;
        assert!(reopened.history(id).is_err());
        assert!(reopened.recorded_step(id, 0).is_err());
        Ok(())
    }

    #[test]
    fn host_step_append_cannot_consume_an_active_worker_step() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let host = RunStore::open(&path)?;
        let worker = RunStore::open(&path)?;
        let id = Uuid::new_v4();
        host.enqueue_if_absent(id, "revision", None)?;
        let attempt = worker.claim_attempt(ClaimAttempt {
            run_id: id,
            revision: "revision",
            node: "script",
            expected_steps: 1,
        })?;
        worker.mark_started(&attempt)?;
        let active = host.get(id)?.context("missing running run")?;
        let trace = r#"{"node":"script","status":"succeeded","outcome":null,"inputs":{},"outputs":{},"reason":"completed"}"#;
        let progress: &dyn WorkflowProgressStore = &host;
        assert!(progress.append_host_step(&active, trace, None).is_err());
        assert!(
            progress
                .transition(&active, RunState::Failed, None, None)
                .is_err()
        );
        assert_eq!(host.get(id)?, Some(active));
        assert!(host.history(id)?.is_empty());

        worker.commit_result(AttemptResult {
            attempt: &attempt,
            trace,
            artifact: None,
            agent_unused: Duration::ZERO,
        })?;
        assert_eq!(host.history(id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn worker_attempts_are_started_once_and_fenced_by_their_full_binding() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let id = Uuid::new_v4();
        store.enqueue_if_absent(id, &"c".repeat(64), None)?;
        let pending = store.get(id)?.context("run was not inserted")?;
        let claim = || ClaimAttempt {
            run_id: pending.id,
            revision: &pending.revision,
            node: "script",
            expected_steps: 1,
        };
        let attempt = store.claim_attempt(claim())?;
        assert!(store.claim_attempt(claim()).is_err());
        let result = || AttemptResult {
            attempt: &attempt,
            trace: r#"{"status":"succeeded"}"#,
            artifact: None,
            agent_unused: Duration::ZERO,
        };
        assert!(store.commit_result(result()).is_err());

        let mut mismatched = attempt.clone();
        mismatched.node = "other-node".into();
        assert!(store.mark_started(&mismatched).is_err());
        store.mark_started(&attempt)?;
        assert!(store.mark_started(&attempt).is_err());

        let mut mismatched_result = attempt.clone();
        mismatched_result.epoch += 1;
        assert!(
            store
                .commit_result(AttemptResult {
                    attempt: &mismatched_result,
                    trace: r#"{"status":"succeeded"}"#,
                    artifact: None,
                    agent_unused: Duration::ZERO
                })
                .is_err()
        );
        store.commit_result(result())?;
        assert!(store.commit_result(result()).is_err());
        assert!(
            store
                .mark_unknown(&attempt, "late process failure")
                .is_err()
        );
        assert_eq!(store.get(id)?.context("run disappeared")?.step_count, 1);
        Ok(())
    }

    #[test]
    fn unstarted_worker_failure_atomically_fails_run_and_invalidates_attempt() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;

        for mark_started in [false, true] {
            let id = Uuid::new_v4();
            store.enqueue_if_absent(id, &"f".repeat(64), None)?;
            let pending = store.get(id)?.context("run was not inserted")?;
            let attempt = store.claim_attempt(ClaimAttempt {
                run_id: pending.id,
                revision: &pending.revision,
                node: "script",
                expected_steps: 1,
            })?;
            if mark_started {
                store.mark_started(&attempt)?;
            }

            let mut mismatched = attempt.clone();
            mismatched.epoch += 1;
            assert!(
                store
                    .fail_attempt(&mismatched, "worker failed before release",)
                    .is_err()
            );
            assert_eq!(
                store.get(id)?.context("run disappeared")?.state,
                RunState::Running
            );

            store.fail_attempt(&attempt, "worker failed before release")?;
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
            assert!(store.fail_attempt(&attempt, "duplicate failure",).is_err());
        }
        Ok(())
    }

    #[test]
    fn cancellation_before_start_requires_an_unreleased_claim() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        for started in [false, true] {
            let id = Uuid::new_v4();
            store.enqueue_if_absent(id, &"c".repeat(64), None)?;
            let pending = store.get(id)?.context("run was not inserted")?;
            let attempt = store.claim_attempt(ClaimAttempt {
                run_id: id,
                revision: &pending.revision,
                node: "script",
                expected_steps: 1,
            })?;
            assert!(store.cancel_claimed_attempt(&attempt).is_err());
            if started {
                store.mark_started(&attempt)?;
            }
            let run = store.get(id)?.context("run disappeared")?;
            store.transition(&run, RunState::Cancelling, Some("script"), None)?;
            let mut stale = attempt.clone();
            stale.epoch += 1;
            assert!(store.cancel_claimed_attempt(&stale).is_err());
            if started {
                assert!(store.cancel_claimed_attempt(&attempt).is_err());
                assert_eq!(
                    store.get(id)?.context("run disappeared")?.state,
                    RunState::Cancelling
                );
                store.mark_unknown(&attempt, "released before cancellation")?;
            } else {
                store.cancel_claimed_attempt(&attempt)?;
                assert_eq!(
                    store.get(id)?.context("run disappeared")?.state,
                    RunState::Cancelled
                );
            }
            assert!(store.cancel_claimed_attempt(&attempt).is_err());
        }
        Ok(())
    }

    #[test]
    fn worker_attempt_epochs_increase_and_restart_invalidates_uncertain_work() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("workflow.sqlite3");
        let id = Uuid::new_v4();
        let attempt = {
            let store = RunStore::open(&path)?;
            store.enqueue_if_absent(id, &"d".repeat(64), None)?;
            let pending = store.get(id)?.context("run was not inserted")?;
            let first = store.claim_attempt(ClaimAttempt {
                run_id: pending.id,
                revision: &pending.revision,
                node: "script",
                expected_steps: 1,
            })?;
            store.mark_started(&first)?;
            store.commit_result(AttemptResult {
                attempt: &first,
                trace: r#"{"status":"succeeded"}"#,
                artifact: None,
                agent_unused: Duration::ZERO,
            })?;
            let pending = store.get(id)?.context("run did not return to pending")?;
            let second = store.claim_attempt(ClaimAttempt {
                run_id: pending.id,
                revision: &pending.revision,
                node: "script",
                expected_steps: 2,
            })?;
            assert!(second.epoch > first.epoch);
            store.mark_started(&second)?;
            second
        };

        let store = RunStore::open(&path)?;
        let recovered = store.get(id)?.context("recovered run disappeared")?;
        assert_eq!(recovered.state, RunState::OutcomeUnknown);
        assert!(
            store
                .commit_result(AttemptResult {
                    attempt: &attempt,
                    trace: r#"{"status":"succeeded"}"#,
                    artifact: None,
                    agent_unused: Duration::ZERO
                })
                .is_err()
        );
        assert_eq!(recovered.step_count, 1);
        Ok(())
    }

    #[test]
    fn failed_worker_result_transaction_keeps_attempt_started_and_run_unchanged() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = RunStore::open(&dir.path().join("workflow.sqlite3"))?;
        let id = Uuid::new_v4();
        store.enqueue_if_absent(id, &"e".repeat(64), None)?;
        let pending = store.get(id)?.context("run was not inserted")?;
        let attempt = store.claim_attempt(ClaimAttempt {
            run_id: pending.id,
            revision: &pending.revision,
            node: "script",
            expected_steps: 1,
        })?;
        store.mark_started(&attempt)?;
        store.0.execute_batch(
            "CREATE TEMP TRIGGER reject_result BEFORE INSERT ON workflow_steps
             BEGIN SELECT RAISE(ABORT, 'test result persistence failure'); END;",
        )?;

        assert!(
            store
                .commit_result(AttemptResult {
                    attempt: &attempt,
                    trace: r#"{"status":"succeeded"}"#,
                    artifact: Some(&WorkflowArtifact {
                        media_type: "application/octet-stream",
                        bytes: vec![1, 2, 3],
                    }),
                    agent_unused: Duration::ZERO
                })
                .is_err()
        );
        let still_running = store.get(id)?.context("run disappeared")?;
        assert_eq!(still_running.state, RunState::Running);
        assert_eq!(still_running.step_count, 0);
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
