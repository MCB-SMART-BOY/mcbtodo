//! Durable workflow queue and attempt operations used by the daemon loop.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mcbtodo_domain::WorkUnitCompleted;
use mcbtodo_workflow::{
    StepTrace,
    run::{ObservedOutcome, RunResolution, RunState, RunSummary},
    schedule::ScheduleSummary,
};
use serde::Serialize;
use uuid::Uuid;

use crate::notifications::NotificationStore;

/// Guarantees of the constructed backend for this workflow repository.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowStorageCapabilities {
    pub backend_id: &'static str,
    /// Multi-record changes made by one repository operation commit together.
    pub atomic_batches: bool,
    /// Completion delivery cursors survive restart with the associated runs
    /// and receipts.
    pub durable_cursor: bool,
}

/// One repository owns the shared workflow transaction and recovery scope.
/// The host must check its capabilities before serving workflow requests.
pub trait WorkflowRepository:
    WorkflowRunStore
    + WorkflowRunCatalog
    + WorkflowProgressStore
    + WorkflowHistoryStore
    + WorkflowQueueStore
    + WorkflowScheduleStore
    + ScriptAuthorizationStore
    + CompletionDeliveryStore
    + NotificationStore
{
    fn capabilities(&self) -> WorkflowStorageCapabilities;
}

/// The complete fence for releasing work and accepting its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowAttempt {
    pub id: Uuid,
    pub run_id: Uuid,
    pub revision: String,
    pub node: String,
    pub epoch: i64,
    pub expected_steps: u32,
}

pub struct ClaimAttempt<'a> {
    pub run_id: Uuid,
    pub revision: &'a str,
    pub node: &'a str,
    pub expected_steps: u32,
}

pub struct AgentAttemptClaim {
    pub attempt: WorkflowAttempt,
    pub execution_window: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentBudgetDenied {
    #[error("per-run Agent call budget exhausted (limit: {limit})")]
    Calls { limit: u32 },
    #[error("per-run Agent wall-time budget exhausted")]
    WallTime,
    #[error("per-run Agent budget is unavailable for a run with unaccounted Agent history")]
    Untracked,
}

pub struct WorkflowArtifact {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

pub struct AttemptResult<'a> {
    pub attempt: &'a WorkflowAttempt,
    /// The bounded, serialized workflow trace.
    pub trace: &'a str,
    pub artifact: Option<&'a WorkflowArtifact>,
    pub agent_unused: Duration,
}

/// A synchronous transactional store. The host serializes calls and authorizes
/// launches and results. Implementations must guard against stale attempts
/// and atomically publish each mutation or leave the prior state intact.
pub trait WorkflowRunStore {
    /// Claims sole execution rights for the identified run and step.
    fn claim_attempt(&self, claim: ClaimAttempt<'_>) -> Result<WorkflowAttempt>;

    /// Atomically reserves the Agent budget, transitions the run, and claims
    /// the attempt. The host derives `has_prior_agent_step` from the published
    /// workflow and history for this revision and step count. If there is no
    /// budget account and prior Agent work exists, the store must fail closed.
    /// A budget denial returns [`AgentBudgetDenied`] without a claim.
    fn claim_agent(
        &self,
        claim: ClaimAttempt<'_>,
        has_prior_agent_step: bool,
    ) -> Result<AgentAttemptClaim>;

    /// Persists the release fence before the host may send Start.
    fn mark_started(&self, attempt: &WorkflowAttempt) -> Result<()>;

    /// Advances the run, stores the trace and artifact, and settles the budget
    /// in the same transaction. Replayed results must not advance the run.
    fn commit_result(&self, result: AttemptResult<'_>) -> Result<()>;

    /// Publishes a known failure and invalidates the active attempt together.
    /// Only call when the host has evidence for a terminal failure.
    fn fail_attempt(&self, attempt: &WorkflowAttempt, detail: &str) -> Result<()>;

    /// Publishes cancellation and invalidates a claim that has not been released.
    /// Must reject an attempt whose Started fence is already durable.
    fn cancel_claimed_attempt(&self, attempt: &WorkflowAttempt) -> Result<()>;

    /// Invalidates an attempt that might have executed and records an unknown
    /// outcome without scheduling any follow-up work or refunding its budget.
    fn mark_unknown(&self, attempt: &WorkflowAttempt, detail: &str) -> Result<()>;
}

/// Synchronous persistence of host-driven workflow progress. The host evaluates
/// the published plan and approves transitions. Mutations compare the supplied
/// run snapshot with the durable revision, state, node, detail and step count.
pub trait WorkflowProgressStore {
    /// Commits a host-authorized transition. Success publishes dependency
    /// deliveries atomically. For an active Worker, only entry into Cancelling
    /// is allowed here. The Attempt owns release and terminal state changes.
    fn transition(
        &self,
        run: &RunSummary,
        state: RunState,
        node: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()>;

    /// Atomically advances an in-process step and records its bounded trace
    /// and optional artifact. An active Worker Attempt precludes this write.
    fn append_host_step(
        &self,
        run: &RunSummary,
        trace: &str,
        artifact: Option<&WorkflowArtifact>,
    ) -> Result<()>;

    /// Adds a one-time operator observation without advancing or requeuing
    /// the uncertain run.
    fn resolve_unknown(
        &self,
        id: Uuid,
        outcome: ObservedOutcome,
        note: &str,
    ) -> Result<RunResolution>;
}

/// Synchronous persistence of exact revision/node Script grants. A grant may
/// only be written after the host verifies that the published node is a Script.
pub trait ScriptAuthorizationStore {
    fn set_authorization(&self, revision: &str, node: &str, granted: bool) -> Result<()>;

    fn authorized(&self, revision: &str, node: &str) -> Result<bool>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedStep {
    pub trace: StepTrace,
    pub artifact: Option<ArtifactMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactMetadata {
    pub sha256: String,
    pub media_type: String,
    pub size_bytes: u32,
}

pub struct ArtifactChunk {
    pub bytes: Vec<u8>,
    pub next_offset: Option<u32>,
}

/// Read-only workflow evidence. Steps and artifacts are immutable once committed.
pub trait WorkflowHistoryStore {
    /// Returns the committed steps in traversal order. A malformed trace is an
    /// error, not a missing step or an empty history.
    fn history(&self, run_id: Uuid) -> Result<Vec<StepTrace>>;

    fn recorded_step(&self, run_id: Uuid, index: u32) -> Result<Option<RecordedStep>>;

    /// Returns at most 8 KiB from the byte offset. The end of a present
    /// artifact returns an empty final chunk. A missing artifact or offset
    /// beyond its end returns `None`.
    fn artifact_chunk(
        &self,
        run_id: Uuid,
        index: u32,
        offset: u32,
    ) -> Result<Option<ArtifactChunk>>;
}

/// Durable run identities and their summaries. Publication validation belongs
/// to the caller. Creation grants no execution rights.
pub trait WorkflowRunCatalog {
    fn get(&self, id: Uuid) -> Result<Option<RunSummary>>;

    /// Returns a bounded page from newest to oldest by insertion order.
    /// Resume strictly before the returned run UUID.
    fn list(&self, before: Option<Uuid>) -> Result<(Vec<RunSummary>, Option<Uuid>)>;

    /// Atomically creates a manual pending run or returns the existing run
    /// with this UUID. The caller distinguishes idempotent replay from a
    /// collision using the returned revision, due time and source.
    fn enqueue_if_absent(
        &self,
        id: Uuid,
        revision: &str,
        due_at: Option<DateTime<Utc>>,
    ) -> Result<RunSummary>;
}

/// Synchronous queue operations for the serialized workflow loop. The host
/// supplies wall time and owns the worker slot and execution authorization.
pub trait WorkflowQueueStore {
    /// Returns the earliest clock rule or, when a run can advance, the
    /// earliest pending-run eligibility time in the same read snapshot.
    fn next_wake_at(&self, can_advance_run: bool) -> Result<Option<DateTime<Utc>>>;

    /// Returns a coherent snapshot of the first due pending run, ordered by
    /// eligibility time and then insertion order. This grants no execution
    /// rights. State transitions and attempt claims must still fence the run.
    fn next_pending_at(&self, now: DateTime<Utc>) -> Result<Option<RunSummary>>;

    /// Advances at most one due clock rule. Occurrence runs, cursor and
    /// counters commit together. Retrying with the same time after a failure
    /// or restart must not duplicate an occurrence.
    fn dispatch_due_schedule(&mut self, now: DateTime<Utc>) -> Result<()>;
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidDependency {
    #[error("An enabled dependency already links these revisions")]
    Duplicate,
    #[error("An upstream revision supports at most 16 dependency rules")]
    FanOut,
    #[error("Dependency would create a revision cycle")]
    Cycle,
}

/// Synchronous registration and administration of durable workflow triggers.
/// The host validates publications and obtains a watermark for completion
/// rules. The store owns transactional rule activation and deactivation.
pub trait WorkflowScheduleStore {
    fn schedule(&self, id: Uuid) -> Result<Option<ScheduleSummary>>;

    /// Inserts a new schedule and its trigger rule atomically. Only a
    /// completion rule takes a cutoff. Duplicate IDs fail without changes.
    /// A rejected revision dependency returns [`InvalidDependency`].
    fn insert_schedule(
        &self,
        summary: &ScheduleSummary,
        completion_cursor: Option<i64>,
    ) -> Result<()>;

    /// Disables future delivery in one transaction, keeping existing runs,
    /// receipts and current counters. Missing schedules return `None`.
    fn disable_schedule(&self, id: Uuid) -> Result<Option<ScheduleSummary>>;
}

/// Persists completion-trigger delivery on the host's serialized loop.
pub trait CompletionDeliveryStore {
    /// Returns the smallest active rule cursor, or `None` without active rules.
    fn next_completion_cursor(&self) -> Result<Option<i64>>;

    /// Delivers an ordered page from the same journal. Recheck current rules
    /// and their cutoffs because registration and disable may have happened
    /// while the page was being read. Atomically persist triggered runs,
    /// deduplication receipts keyed by rule and event UUID, and cursor advances
    /// (including for unmatched events). An empty page has no effect. After an
    /// error, re-reading from the durable cursor must be safe.
    fn deliver_completion_page(&self, page: &[(i64, WorkUnitCompleted)]) -> Result<()>;
}
