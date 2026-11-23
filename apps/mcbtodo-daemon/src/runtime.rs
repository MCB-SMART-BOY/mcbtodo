//! Owns durable workflow state on the daemon loop.

use std::{
    collections::VecDeque,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use mcbtodo_application::{ReminderSummary, VaultCommands, VaultQueries, project_reminders};
use mcbtodo_domain::Task;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_ports::process::{
    LaunchedWorker, ProcessController, ProcessLauncher, WorkerExit, WorkerInput,
};
use mcbtodo_ports::workflow::{
    AgentAttemptClaim, AgentBudgetDenied, AttemptResult, ClaimAttempt, InvalidDependency,
    WorkflowArtifact as Artifact, WorkflowAttempt as WorkerAttempt, WorkflowRepository,
    WorkflowRunStore, WorkflowStorageCapabilities,
};
use mcbtodo_protocol::{
    ErrorCode, Response, VaultWatchStatus, wire,
    worker::{
        AttemptIdentity, DaemonMessage, ScriptRunner as WorkerScriptRunner, WorkerArtifact,
        WorkerJob, WorkerMessage, WorkerOutcome, WorkerProtocolError,
    },
    workflow::{Overlap, ScheduleSummary, WorkflowRequest, WorkflowResponse},
};
#[cfg(test)]
use mcbtodo_vault::VaultStore;
use mcbtodo_vault::WriteError;
use mcbtodo_vault::{Vault, VaultWatcher};
use mcbtodo_workflow::{
    JobKind, NodeJob, NodeOutcome, NodeStatus, Progress, RevisionStore, Scalar, ScriptRunner,
    StepTrace, WorkflowRevision, WorkflowStatus, advance,
};
use serde_json::json;
use uuid::Uuid;

#[path = "completion_poller.rs"]
mod completion_poller;

use completion_poller::CompletionPoller;

use mcbtodo_ports::notifications::NotificationStore;
use mcbtodo_workflow::run::{RunState, RunSummary};
use mcbtodo_workflow::schedule::{Missed, Trigger};

// Poll the worker for completion without tying up the daemon's IPC or timer.
const WORKER_POLL: Duration = Duration::from_millis(50);
const WORKER_RESULT_DRAIN_GRACE: Duration = Duration::from_millis(100);
const WORKER_START_TIMEOUT: Duration = Duration::from_secs(5);
const WORKER_CONTROL_DELIVERY_TIMEOUT: Duration = Duration::from_millis(100);
// Give the sandbox a chance to report its own timeout and clean up.
const SCRIPT_WORKER_REPORT_GRACE: Duration = Duration::from_secs(2);
const WORKER_CANCEL_GRACE: Duration = Duration::from_secs(2);
// Recheck wall-clock deadlines periodically to account for clock steps and suspend.
const CLOCK_CHECK: Duration = Duration::from_secs(1);
const VAULT_WATCH_POLL: Duration = Duration::from_secs(1);

fn load_vault_snapshot(paths: &WorkspacePaths) -> Result<Vault> {
    let config = paths
        .load_config()
        .context("loading workspace configuration")?;
    let root = paths.vault_root();
    Vault::load(&root, config.timezone)
        .with_context(|| format!("loading Vault at {}", root.display()))
}

pub struct Runtime {
    store: Box<dyn WorkflowRepository + Send>,
    revisions: RevisionStore,
    paths: WorkspacePaths,
    worker_launcher: Box<dyn ProcessLauncher>,
    workers: VecDeque<WorkerSlot>,
    worker_limit: usize,
    completions: CompletionPoller,
    vault_watcher: Option<VaultWatcher>,
    vault_watch_version: Uuid,
    validated_vault: Option<Vault>,
    reminders: Vec<ReminderSummary>,
    vault_generation: u64,
    last_vault_error: Option<String>,
}

fn require_workflow_storage(capabilities: WorkflowStorageCapabilities) -> Result<()> {
    ensure!(
        capabilities.atomic_batches,
        "Workflow storage backend `{}` does not guarantee atomic batches",
        capabilities.backend_id
    );
    ensure!(
        capabilities.durable_cursor,
        "Workflow storage backend `{}` does not guarantee a durable completion cursor",
        capabilities.backend_id
    );
    Ok(())
}

enum WorkerSlot {
    Starting(StartingWorker),
    Active(ActiveWorker),
}

impl WorkerSlot {
    fn wait_timeout(&self, now: Instant) -> Duration {
        let deadline = match self {
            Self::Starting(worker) => worker.startup_deadline,
            Self::Active(worker) => match (worker.exit, worker.cancel_sent_at) {
                (Some(exit), _) => exit.drain_until.min(worker.deadline.at()),
                (_, Some(sent)) => sent + WORKER_CANCEL_GRACE,
                _ => worker.deadline.at(),
            },
        };
        deadline.saturating_duration_since(now).min(WORKER_POLL)
    }
}

struct StartingWorker {
    job: NodeJob,
    attempt: WorkerAttempt,
    child: Box<dyn ProcessController>,
    startup_deadline: Instant,
    execution_window: Duration,
    execution_deadline: fn(Instant) -> WorkerDeadline,
    handshake: Receiver<Result<ReadyWorker>>,
}

struct ReadyWorker {
    identity: AttemptIdentity,
    stdin: Box<dyn WorkerInput>,
    receiver: Receiver<Result<WorkerMessage>>,
}

struct ActiveWorker {
    run_id: Uuid,
    job: NodeJob,
    attempt: WorkerAttempt,
    deadline: WorkerDeadline,
    child: Box<dyn ProcessController>,
    stdin: Option<Box<dyn WorkerInput>>,
    receiver: Receiver<Result<WorkerMessage>>,
    result: Option<Box<AdmittedWorkerResult>>,
    cancel_sent_at: Option<Instant>,
    exit: Option<ObservedWorkerExit>,
}

struct AdmittedWorkerResult {
    outcome: WorkerOutcome,
    artifact: Option<WorkerArtifact>,
}

#[derive(Clone, Copy)]
struct ObservedWorkerExit {
    status: WorkerExit,
    drain_until: Instant,
}

enum WorkerDeadline {
    Agent(Instant),
    Script(Instant),
}

impl WorkerDeadline {
    fn at(&self) -> Instant {
        match *self {
            Self::Agent(at) | Self::Script(at) => at,
        }
    }

    fn elapsed_reason(&self, now: Instant) -> Option<&'static str> {
        match *self {
            Self::Agent(at) if now >= at => Some("Worker exceeded its Agent execution budget"),
            Self::Script(at) if now >= at => {
                Some("Worker exceeded its script supervision deadline")
            }
            _ => None,
        }
    }
}

struct JobResult {
    outcome: NodeOutcome,
    artifact: Option<Artifact>,
}

#[derive(Debug, thiserror::Error)]
#[error("Vault write was published before durability could be confirmed: {0}")]
struct UncertainWrite(#[source] WriteError);

impl Runtime {
    pub(super) fn paths(&self) -> &WorkspacePaths {
        &self.paths
    }

    #[cfg(test)]
    pub(super) fn open_for_test(state_dir: &Path) -> Result<Self> {
        Self::open_with_paths(state_dir, WorkspacePaths::from_home(state_dir.to_owned()))
    }

    #[cfg(test)]
    pub(super) fn open_with_paths(state_dir: &Path, paths: WorkspacePaths) -> Result<Self> {
        let store =
            mcbtodo_storage_sqlite::run_store::RunStore::open(&state_dir.join("workflow.sqlite3"))?;
        Self::new(
            state_dir,
            paths,
            store,
            crate::worker_process::LinuxWorkerLauncher,
            crate::DEFAULT_WORKER_SLOTS,
        )
    }

    pub(crate) fn new(
        state_dir: &Path,
        paths: WorkspacePaths,
        store: impl WorkflowRepository + Send + 'static,
        worker_launcher: impl ProcessLauncher + 'static,
        worker_slots: u8,
    ) -> Result<Self> {
        require_workflow_storage(store.capabilities())?;
        let vault_watcher = match VaultWatcher::new(&paths.vault_root()) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                eprintln!("mcbtodo-daemon: Vault auto-validation unavailable: {error:#}");
                None
            }
        };
        let (validated_vault, reminders) = match load_vault_snapshot(&paths) {
            Ok(vault) => {
                let reminders = project_reminders(&vault);
                (Some(vault), reminders)
            }
            Err(error) => {
                eprintln!("mcbtodo-daemon: validating Vault: {error:#}");
                (None, Vec::new())
            }
        };
        Ok(Self {
            store: Box::new(store),
            revisions: RevisionStore::new(state_dir),
            worker_launcher: Box::new(worker_launcher),
            workers: VecDeque::with_capacity(usize::from(worker_slots)),
            worker_limit: usize::from(worker_slots),
            completions: CompletionPoller::new(paths.clone()),
            vault_watcher,
            vault_watch_version: Uuid::new_v4(),
            validated_vault,
            reminders,
            vault_generation: 0,
            last_vault_error: None,
            paths,
        })
    }

    pub fn reminder_snapshot(&self) -> (u64, &[ReminderSummary]) {
        (self.vault_generation, &self.reminders)
    }

    pub fn vault_watch_status(&self) -> VaultWatchStatus {
        VaultWatchStatus {
            version: self.vault_watch_version,
            watching: self.vault_watcher.is_some(),
        }
    }

    pub fn notifications(&self) -> &dyn NotificationStore {
        self.store.as_ref()
    }

    fn has_worker_capacity(&self) -> bool {
        self.workers.len() < self.worker_limit
    }

    pub fn wait_timeout(&self) -> Result<Option<Duration>> {
        let timed_wait = self
            .store
            .next_wake_at(self.has_worker_capacity())?
            .map(|due| {
                (due - Utc::now())
                    .to_std()
                    .unwrap_or_default()
                    .min(CLOCK_CHECK)
            });
        let now = Instant::now();
        let worker_wait = self.workers.iter().map(|worker| worker.wait_timeout(now));
        Ok(timed_wait
            .into_iter()
            .chain(worker_wait)
            .chain(self.completions.wait_timeout(self.store.as_ref())?)
            .chain(self.vault_watcher.as_ref().map(|_| VAULT_WATCH_POLL))
            .min())
    }

    pub fn tick(&mut self) -> Result<()> {
        // Poll each occupied slot once. Live Workers go back at the end.
        for _ in 0..self.workers.len() {
            let worker = self.workers.pop_front().expect("occupied Worker slot");
            let worker = match worker {
                WorkerSlot::Starting(worker) => self.poll_worker_start(worker)?,
                WorkerSlot::Active(worker) => self.poll_worker(worker)?,
            };
            if let Some(worker) = worker {
                self.workers.push_back(worker);
            }
        }
        self.store.dispatch_due_schedule(Utc::now())?;
        self.completions.tick(self.store.as_ref())?;
        self.refresh_vault_snapshot();
        // Advance one run per tick so timers and IPC get a turn between admissions.
        if self.has_worker_capacity()
            && let Some(run) = self.store.next_pending_at(Utc::now())?
        {
            self.advance_run(run)
                .context("advancing pending workflow run")?;
        }
        Ok(())
    }

    fn refresh_vault_snapshot(&mut self) {
        let changed = match self.vault_watcher.as_mut() {
            Some(watcher) => match watcher.changed() {
                Ok(changed) => changed,
                Err(error) => {
                    eprintln!("mcbtodo-daemon: Vault watcher stopped: {error:#}");
                    self.vault_watcher = None;
                    false
                }
            },
            None => false,
        };
        if !changed {
            return;
        }
        // Projections also need to see a failed validation and the subsequent repair.
        self.vault_watch_version = Uuid::new_v4();
        match load_vault_snapshot(&self.paths) {
            Ok(vault) => {
                self.reminders = project_reminders(&vault);
                self.validated_vault = Some(vault);
                self.vault_generation = self.vault_generation.wrapping_add(1);
                self.last_vault_error = None;
            }
            Err(error) => {
                let message = format!("{error:#}");
                if self.last_vault_error.as_ref() != Some(&message) {
                    eprintln!(
                        "mcbtodo-daemon: Vault reload rejected; retaining last valid snapshot: {message}"
                    );
                    self.last_vault_error = Some(message);
                }
            }
        }
    }

    pub fn handle(&mut self, request: WorkflowRequest) -> Response {
        match self.command(request) {
            Ok(response) => Response::Workflow(response),
            Err(error) => {
                if error.code == ErrorCode::Internal {
                    eprintln!("mcbtodo-daemon: workflow request: {:#}", error.source);
                }
                Response::Error {
                    code: error.code,
                    message: if error.code == ErrorCode::Internal {
                        "Could not process workflow request".into()
                    } else {
                        error.source.to_string()
                    },
                }
            }
        }
    }

    fn command(&mut self, request: WorkflowRequest) -> Result<WorkflowResponse, CommandError> {
        use WorkflowRequest::*;
        match request {
            Enqueue {
                run_id,
                revision,
                due_at,
            } => {
                let run = match self.store.get(run_id)? {
                    Some(existing) => existing,
                    None => {
                        self.published(&revision)?;
                        self.store.enqueue_if_absent(run_id, &revision, due_at)?
                    }
                };
                if run.revision != revision || run.due_at != due_at || run.schedule_id.is_some() {
                    return Err(CommandError::new(
                        ErrorCode::Conflict,
                        "Run ID already belongs to different parameters",
                    ));
                }
                Ok(WorkflowResponse::Run(run))
            }
            Inspect { run_id } => Ok(WorkflowResponse::Run(self.lookup(run_id)?)),
            List { before } => {
                if let Some(id) = before {
                    self.lookup(id)?;
                }
                let (runs, next_before) = self.store.list(before)?;
                Ok(WorkflowResponse::Runs { runs, next_before })
            }
            Step { run_id, index } => {
                self.lookup(run_id)?;
                let step = self.store.recorded_step(run_id, index)?.ok_or_else(|| {
                    CommandError::new(
                        ErrorCode::NotFound,
                        format!("No recorded step at index {index}"),
                    )
                })?;
                Ok(WorkflowResponse::Step {
                    step: serde_json::to_value(step).context("Encoding recorded workflow step")?,
                })
            }
            Artifact {
                run_id,
                index,
                offset,
            } => {
                self.lookup(run_id)?;
                let chunk = self
                    .store
                    .artifact_chunk(run_id, index, offset)?
                    .ok_or_else(|| {
                        CommandError::new(
                            ErrorCode::NotFound,
                            "No artifact at the requested position",
                        )
                    })?;
                Ok(WorkflowResponse::Artifact {
                    bytes: chunk.bytes,
                    next_offset: chunk.next_offset,
                })
            }
            Decide {
                run_id,
                node,
                approved,
                review,
            } => {
                let run = self.lookup(run_id)?;
                if run.state != RunState::Waiting || run.current_node.as_deref() != Some(&node) {
                    return Err(CommandError::new(
                        ErrorCode::Conflict,
                        "Run is not waiting for this approval",
                    ));
                }
                let revision = self.published(&run.revision)?;
                let Progress::Next { job, steps } = self.progress(&run, &revision)? else {
                    return Err(anyhow::anyhow!("Approval is absent from the pending plan").into());
                };
                let JobKind::Approval { prompt } = &job.kind else {
                    return Err(anyhow::anyhow!("Pending node is not an approval").into());
                };
                if !steps.is_empty() || job.node != node {
                    return Err(anyhow::anyhow!(
                        "Pending plan does not match the recorded approval"
                    )
                    .into());
                }
                if approved && run.detail.as_deref() != Some(prompt) {
                    self.store
                        .transition(&run, RunState::Waiting, Some(&node), Some(prompt))?;
                    return Err(CommandError::new(
                        ErrorCode::Conflict,
                        "Approval review changed. Inspect the run before approving again.",
                    ));
                }
                if approved && (review.is_none() || review != run.approval_review()) {
                    return Err(CommandError::new(
                        ErrorCode::Conflict,
                        "Approval requires the current review. Inspect the run before approving.",
                    ));
                }
                let outcome = if approved {
                    NodeOutcome::Approved
                } else {
                    NodeOutcome::Rejected
                };
                let step = job
                    .complete(outcome)
                    .context("Recording approval decision")?;
                self.record(&run, &step)?;
                Ok(WorkflowResponse::Run(self.run(run_id)?))
            }
            Cancel { run_id } => {
                let run = self.lookup(run_id)?;
                match run.state {
                    RunState::Pending | RunState::Waiting => self.store.transition(
                        &run,
                        RunState::Cancelled,
                        run.current_node.as_deref(),
                        Some("Cancelled by user"),
                    )?,
                    // The supervisor delivers Cancel and handles delivery failure.
                    RunState::Running => self.store.transition(
                        &run,
                        RunState::Cancelling,
                        run.current_node.as_deref(),
                        None,
                    )?,
                    RunState::Cancelling | RunState::Cancelled => (),
                    _ => {
                        return Err(CommandError::new(
                            ErrorCode::Conflict,
                            "Run already finished",
                        ));
                    }
                }
                Ok(WorkflowResponse::Run(self.run(run_id)?))
            }
            ResolveUnknown {
                run_id,
                outcome,
                note,
            } => {
                let run = self.lookup(run_id)?;
                if run.state != RunState::OutcomeUnknown {
                    return Err(CommandError::new(
                        ErrorCode::Conflict,
                        "Only an outcome_unknown run can be resolved",
                    ));
                }
                if run.resolution.is_some() {
                    return Err(CommandError::new(
                        ErrorCode::Conflict,
                        "This run already has an operator resolution",
                    ));
                }
                if note.trim().is_empty()
                    || note.len() > 4096
                    || note
                        .chars()
                        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
                {
                    return Err(CommandError::new(
                        ErrorCode::InvalidRequest,
                        "Resolution note must contain 1-4096 bytes and no control characters",
                    ));
                }
                let resolution = self.store.resolve_unknown(run_id, outcome, &note)?;
                Ok(WorkflowResponse::Resolution(resolution))
            }
            AuthorizeScript {
                revision,
                node,
                granted,
            } => {
                let published = self.published(&revision)?;
                if !published.plan().is_script(&node) {
                    return Err(CommandError::new(
                        ErrorCode::InvalidRequest,
                        "Published node is not a script",
                    ));
                }
                self.store.set_authorization(&revision, &node, granted)?;
                Ok(WorkflowResponse::ScriptAuthorization {
                    revision,
                    node,
                    granted,
                })
            }
            AddSchedule {
                id,
                revision,
                trigger,
                missed,
                overlap,
            } => {
                if let Some(existing) = self.store.schedule(id)? {
                    if existing.revision != revision
                        || existing.trigger != trigger
                        || existing.missed != missed
                        || existing.overlap != overlap
                    {
                        return Err(CommandError::new(
                            ErrorCode::Conflict,
                            "Schedule ID already belongs to different parameters",
                        ));
                    }
                    return Ok(WorkflowResponse::Schedule(existing));
                }
                self.published(&revision)?;
                let (next_at, completion_cursor) = match &trigger {
                    Trigger::Dependency { upstream_revision } => {
                        self.published(upstream_revision)?;
                        validate_event_policy(missed, overlap)?;
                        (None, None)
                    }
                    Trigger::WorkUnitCompleted { .. } => {
                        validate_event_policy(missed, overlap)?;
                        let cursor = self.completions.watermark().map_err(|error| {
                            let message = match error {
                                mcbtodo_ports::CompletionReadError::Busy => {
                                    "Default Vault is busy. Retry schedule registration.".into()
                                }
                                error => error.to_string(),
                            };
                            CommandError::new(ErrorCode::Conflict, message)
                        })?;
                        (None, Some(cursor))
                    }
                    Trigger::Interval { .. } | Trigger::Cron { .. } => {
                        let (calculator, _) = trigger
                            .clock_schedule(missed)
                            .map_err(|error| CommandError::new(ErrorCode::InvalidRequest, error))?;
                        let next_at = calculator.next_after(Utc::now()).ok_or_else(|| {
                            CommandError::new(
                                ErrorCode::InvalidRequest,
                                "Schedule has no future occurrence",
                            )
                        })?;
                        (Some(next_at), None)
                    }
                };
                let summary = ScheduleSummary {
                    id,
                    revision,
                    trigger,
                    missed,
                    overlap,
                    next_at,
                    enabled: true,
                    missed_occurrences: 0,
                    overlap_skipped: 0,
                    cascade_skipped: 0,
                };
                self.store
                    .insert_schedule(&summary, completion_cursor)
                    .map_err(|error| {
                        if error.is::<InvalidDependency>() {
                            CommandError {
                                code: ErrorCode::InvalidRequest,
                                source: error,
                            }
                        } else {
                            error.into()
                        }
                    })?;
                if completion_cursor.is_some() {
                    self.completions.wake();
                }
                Ok(WorkflowResponse::Schedule(summary))
            }
            InspectSchedule { id } | DisableSchedule { id } => {
                let schedule = if matches!(request, DisableSchedule { .. }) {
                    self.store.disable_schedule(id)?
                } else {
                    self.store.schedule(id)?
                };
                let schedule = schedule.ok_or_else(|| {
                    CommandError::new(ErrorCode::NotFound, format!("Unknown schedule {id}"))
                })?;
                Ok(WorkflowResponse::Schedule(schedule))
            }
        }
    }

    fn lookup(&self, id: Uuid) -> Result<RunSummary, CommandError> {
        self.store
            .get(id)?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, format!("Unknown run {id}")))
    }

    fn run(&self, id: Uuid) -> Result<RunSummary> {
        self.store
            .get(id)?
            .context("Known workflow run missing from database")
    }

    fn published(&self, hash: &str) -> Result<WorkflowRevision, CommandError> {
        self.revisions
            .load(hash)
            .map_err(|error| CommandError::new(ErrorCode::InvalidRequest, error))
    }

    fn progress(&self, run: &RunSummary, revision: &WorkflowRevision) -> Result<Progress> {
        Ok(advance(revision.plan(), &self.store.history(run.id)?)?)
    }

    fn advance_run(&mut self, mut run: RunSummary) -> Result<()> {
        let revision = match self.revisions.load(&run.revision) {
            Ok(revision) => revision,
            Err(error) => {
                return self.fail(&run, &format!("Could not load published revision: {error}"));
            }
        };
        let progress = self.store.history(run.id).and_then(|history| {
            advance(revision.plan(), &history)
                .map(|progress| (progress, history))
                .map_err(Into::into)
        });
        let (steps, next, history) = match progress {
            Ok((Progress::Complete(trace), _)) => {
                let state = match trace.status {
                    WorkflowStatus::Succeeded => RunState::Succeeded,
                    WorkflowStatus::Failed => RunState::Failed,
                };
                let remaining = &trace.steps[run.step_count as usize..];
                self.record_automatic(&mut run, remaining)?;
                if run.state == RunState::Failed {
                    return Ok(());
                }
                return self.store.transition(&run, state, None, None);
            }
            Ok((Progress::Next { job, steps }, history)) => (steps, job, history),
            Err(error) => return self.fail(&run, &format!("Could not evaluate workflow: {error}")),
        };
        self.record_automatic(&mut run, &steps)?;
        if run.state == RunState::Failed {
            return Ok(());
        }
        match &next.kind {
            JobKind::Approval { prompt } => {
                self.store
                    .transition(&run, RunState::Waiting, Some(&next.node), Some(prompt))
            }
            JobKind::Action {
                action: "report.create",
            } => {
                let content = &next.inputs["summary"];
                let Scalar::String(summary) = content else {
                    return self.fail(&run, "Invalid report summary");
                };
                self.finish_job(
                    &run,
                    &next,
                    JobResult {
                        outcome: NodeOutcome::Succeeded {
                            outputs: Default::default(),
                        },
                        artifact: Some(Artifact {
                            media_type: "text/markdown",
                            bytes: summary.as_bytes().to_vec(),
                        }),
                    },
                )
            }
            JobKind::Action {
                action: "task.create",
            } => self.run_task_create(&run, next),
            JobKind::Action { action } => self.fail(
                &run,
                &format!("Action `{action}` is unavailable for this run"),
            ),
            JobKind::Agent { .. } => {
                let has_prior_agent_step = history.iter().any(|step| {
                    step.status != NodeStatus::Skipped && revision.plan().is_agent(&step.node)
                });
                self.start_agent(&run, next, has_prior_agent_step)
            }
            JobKind::Script { .. } => self.start_script(&run, next, &revision),
        }
    }

    fn record_automatic<'a>(
        &mut self,
        run: &mut RunSummary,
        steps: impl IntoIterator<Item = &'a StepTrace>,
    ) -> Result<()> {
        for step in steps {
            if !self.finish_step(run, step, None)? {
                run.state = RunState::Failed;
                break;
            }
            run.step_count += 1;
        }
        Ok(())
    }

    fn start_agent(
        &mut self,
        run: &RunSummary,
        job: NodeJob,
        has_prior_agent_step: bool,
    ) -> Result<()> {
        let JobKind::Agent { capability } = &job.kind else {
            unreachable!()
        };
        let capability = capability.clone();
        let inputs = worker_inputs(&job)?;
        let AgentAttemptClaim {
            attempt,
            execution_window: window,
        } = match self
            .store
            .claim_agent(worker_claim(run, &job.node)?, has_prior_agent_step)
        {
            Ok(reservation) => reservation,
            Err(error) if error.is::<AgentBudgetDenied>() => {
                return self.fail(run, &format!("Agent node `{}` denied: {error}", job.node));
            }
            Err(error) => return Err(error),
        };
        let timeout_seconds = agent_worker_timeout_seconds(window)?;
        self.launch_claimed_worker(
            attempt,
            job,
            WorkerJob::Agent {
                capability,
                timeout_seconds,
                inputs,
            },
            Some(window),
            worker_binary_path(),
        )
    }

    fn run_task_create(&mut self, run: &RunSummary, job: NodeJob) -> Result<()> {
        self.store
            .transition(run, RunState::Running, Some(&job.node), None)?;
        let result = execute_task_create(&self.paths, &job, &AtomicBool::new(false));
        let active = self.run(run.id)?;
        match result {
            Ok(Some(result)) => self.finish_job(&active, &job, result),
            Ok(None) => self.store.transition(
                &active,
                RunState::OutcomeUnknown,
                active.current_node.as_deref(),
                Some("Daemon stopped before the Action outcome was committed"),
            ),
            Err(error) if error.is::<UncertainWrite>() => self.store.transition(
                &active,
                RunState::OutcomeUnknown,
                active.current_node.as_deref(),
                Some("Vault publication may have completed but durability is uncertain"),
            ),
            Err(error) => self.fail(&active, &format!("Action `{}` failed: {error}", job.node)),
        }
    }

    fn start_script(
        &mut self,
        run: &RunSummary,
        job: NodeJob,
        revision: &WorkflowRevision,
    ) -> Result<()> {
        if !self.store.authorized(&run.revision, &job.node)? {
            return self.fail(
                run,
                &format!(
                    "Script `{}` requires authorization for revision {}",
                    job.node, run.revision
                ),
            );
        }
        let JobKind::Script {
            source,
            runner,
            timeout_seconds,
            capabilities,
        } = &job.kind
        else {
            unreachable!()
        };
        let executable = match capabilities.as_slice() {
            [] => None,
            [capability] if capability.kind == "process.exec" => {
                let Some(executable) = revision.script_sources().get(&capability.scope) else {
                    return self.fail(run, "Published executable bytes are missing");
                };
                Some(executable.clone())
            }
            _ => return self.fail(run, "Sandbox cannot enforce the requested capabilities"),
        };
        let Some(bytes) = revision.script_sources().get(source).cloned() else {
            return self.fail(run, "Published script bytes are missing");
        };
        let runner = match runner {
            ScriptRunner::Bash => WorkerScriptRunner::Bash,
            ScriptRunner::Nushell => WorkerScriptRunner::Nushell,
            ScriptRunner::Python => {
                return self.fail(
                    run,
                    "Python runner is not available in the isolation profile",
                );
            }
        };
        self.spawn_worker(
            run,
            job.clone(),
            WorkerJob::Script {
                source: bytes,
                runner,
                timeout_seconds: *timeout_seconds,
                inputs: worker_inputs(&job)?,
                executable,
            },
        )
    }

    fn spawn_worker(
        &mut self,
        run: &RunSummary,
        job: NodeJob,
        worker_job: WorkerJob,
    ) -> Result<()> {
        let binary = match worker_binary_path() {
            Ok(binary) => binary,
            Err(error) => {
                return self.fail(run, &format!("Could not locate mcbtodo-worker: {error}"));
            }
        };
        self.spawn_worker_at(run, job, worker_job, binary)
    }

    fn spawn_worker_at(
        &mut self,
        run: &RunSummary,
        job: NodeJob,
        worker_job: WorkerJob,
        binary: PathBuf,
    ) -> Result<()> {
        let attempt = self.store.claim_attempt(worker_claim(run, &job.node)?)?;
        self.launch_claimed_worker(attempt, job, worker_job, None, Ok(binary))
    }

    fn launch_claimed_worker(
        &mut self,
        attempt: WorkerAttempt,
        job: NodeJob,
        worker_job: WorkerJob,
        agent_budget: Option<Duration>,
        binary: Result<PathBuf>,
    ) -> Result<()> {
        let binary = match binary {
            Ok(binary) => binary,
            Err(error) => {
                self.store.fail_attempt(
                    &attempt,
                    &format!("Could not locate mcbtodo-worker: {error}"),
                )?;
                return Ok(());
            }
        };
        let identity = attempt_identity(&attempt)?;
        let (window, deadline_kind): (Duration, fn(Instant) -> WorkerDeadline) =
            match (&worker_job, agent_budget) {
                (
                    WorkerJob::Script {
                        timeout_seconds, ..
                    },
                    None,
                ) => (
                    Duration::from_secs(u64::from(*timeout_seconds)) + SCRIPT_WORKER_REPORT_GRACE,
                    WorkerDeadline::Script,
                ),
                (WorkerJob::Agent { .. }, Some(window)) => (window, WorkerDeadline::Agent),
                _ => {
                    self.store
                        .fail_attempt(&attempt, "Worker job and execution budget do not match")?;
                    return Ok(());
                }
            };
        let LaunchedWorker {
            process: child,
            stdin,
            stdout,
        } = match self
            .worker_launcher
            .launch_worker(&binary, self.paths.home_dir())
        {
            Ok(worker) => worker,
            Err(error) => {
                self.store.fail_attempt(
                    &attempt,
                    &format!("Could not start mcbtodo-worker: {error}"),
                )?;
                return Ok(());
            }
        };
        let (sender, receiver) = mpsc::sync_channel(2);
        if let Err(error) = thread::Builder::new()
            .name("mcbtodo-worker-reader".into())
            .spawn(move || read_worker_messages(stdout, sender))
        {
            self.store.fail_attempt(
                &attempt,
                &format!("Could not start worker protocol reader: {error}"),
            )?;
            return Ok(());
        }
        let startup_deadline = Instant::now() + WORKER_START_TIMEOUT;
        let (ready_sender, handshake) = mpsc::sync_channel(1);
        if let Err(error) = thread::Builder::new()
            .name("mcbtodo-worker-prepare".into())
            .spawn(move || {
                let _ = ready_sender.send(prepare_worker(
                    stdin,
                    receiver,
                    identity,
                    worker_job,
                    startup_deadline,
                ));
            })
        {
            self.store.fail_attempt(
                &attempt,
                &format!("Could not start Worker handshake: {error}"),
            )?;
            return Ok(());
        }
        self.workers.push_back(WorkerSlot::Starting(StartingWorker {
            job,
            attempt,
            child,
            startup_deadline,
            execution_window: window,
            execution_deadline: deadline_kind,
            handshake,
        }));
        Ok(())
    }

    fn poll_worker_start(&self, mut worker: StartingWorker) -> Result<Option<WorkerSlot>> {
        if self.run(worker.attempt.run_id)?.state == RunState::Cancelling {
            self.store.cancel_claimed_attempt(&worker.attempt)?;
            return Ok(None);
        }
        let now = Instant::now();
        if now >= worker.startup_deadline {
            self.store.fail_attempt(
                &worker.attempt,
                "Worker startup deadline elapsed before Start",
            )?;
            return Ok(None);
        }
        let ready = match worker.handshake.try_recv() {
            Ok(Ok(ready)) => Some(ready),
            Ok(Err(error)) => {
                self.store
                    .fail_attempt(&worker.attempt, &error.to_string())?;
                return Ok(None);
            }
            Err(TryRecvError::Disconnected) => {
                self.store.fail_attempt(
                    &worker.attempt,
                    "Worker handshake thread exited without reporting readiness",
                )?;
                return Ok(None);
            }
            Err(TryRecvError::Empty) => None,
        };
        match worker.child.try_wait() {
            Ok(Some(_)) => {
                self.store
                    .fail_attempt(&worker.attempt, "Worker exited before Start")?;
                return Ok(None);
            }
            Err(error) => {
                self.store.fail_attempt(
                    &worker.attempt,
                    &format!("Worker liveness could not be confirmed before Start: {error}"),
                )?;
                return Ok(None);
            }
            Ok(None) => {}
        }
        let Some(ReadyWorker {
            identity,
            mut stdin,
            receiver,
        }) = ready
        else {
            return Ok(Some(WorkerSlot::Starting(worker)));
        };
        let Some(released_at) = release_worker(
            self.store.as_ref(),
            &worker.attempt,
            identity,
            stdin.as_mut(),
            worker.startup_deadline,
            worker.execution_window,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(WorkerSlot::Active(ActiveWorker {
            run_id: worker.attempt.run_id,
            job: worker.job,
            attempt: worker.attempt,
            deadline: (worker.execution_deadline)(released_at + worker.execution_window),
            child: worker.child,
            stdin: Some(stdin),
            receiver,
            result: None,
            cancel_sent_at: None,
            exit: None,
        })))
    }

    fn poll_worker(&mut self, mut worker: ActiveWorker) -> Result<Option<WorkerSlot>> {
        let run = self.run(worker.run_id)?;
        if let Some(exit) = worker.exit {
            return self.poll_exited_worker(worker, exit, run.state);
        }
        if run.state == RunState::Cancelling
            || worker.deadline.elapsed_reason(Instant::now()).is_some()
        {
            if worker.cancel_sent_at.is_none()
                && let Err(error) = send_worker_cancel(&mut worker)
            {
                let termination = worker.child.terminate();
                let detail = if let Some(reason) = worker.deadline.elapsed_reason(Instant::now()) {
                    format!("{reason}; cancellation could not be delivered: {error}")
                } else {
                    format!("Worker cancellation could not be delivered: {error}")
                };
                self.store.mark_unknown(&worker.attempt, &detail)?;
                termination.context("terminating Worker after failed cancellation")?;
                return Ok(None);
            }
            if worker
                .cancel_sent_at
                .is_some_and(|sent| sent.elapsed() >= WORKER_CANCEL_GRACE)
                && let Err(error) = worker.child.terminate()
            {
                let reason = worker
                    .deadline
                    .elapsed_reason(Instant::now())
                    .unwrap_or("Worker cancellation did not complete");
                self.store.mark_unknown(
                    &worker.attempt,
                    &format!("{reason}: Worker could not be terminated: {error}"),
                )?;
                return Err(error).context("terminating Worker after cancellation grace period");
            }
        }
        if let Ok(message) = worker.receiver.try_recv()
            && !self.accept_worker_message(&mut worker, message)?
        {
            return Ok(None);
        }
        let status = match worker.child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let termination = worker.child.terminate();
                self.store.mark_unknown(
                    &worker.attempt,
                    &format!("Worker exit could not be confirmed: {error}"),
                )?;
                termination.context("terminating Worker after exit check failed")?;
                return Err(error).context("checking Worker exit");
            }
        };
        if let Some(status) = status {
            let exit = ObservedWorkerExit {
                status,
                drain_until: Instant::now() + WORKER_RESULT_DRAIN_GRACE,
            };
            return self.poll_exited_worker(worker, exit, run.state);
        }
        Ok(Some(WorkerSlot::Active(worker)))
    }

    fn poll_exited_worker(
        &mut self,
        mut worker: ActiveWorker,
        exit: ObservedWorkerExit,
        state: RunState,
    ) -> Result<Option<WorkerSlot>> {
        let now = Instant::now();
        let deadline_exceeded = worker.deadline.elapsed_reason(now);
        let cancelling = state == RunState::Cancelling;
        // The reader can publish the final frame after the process is reaped.
        // Keep the slot occupied while draining, and let the next tick poll it.
        if worker.result.is_none() && now < exit.drain_until {
            match worker.receiver.try_recv() {
                Ok(message) => {
                    if !self.accept_worker_message(&mut worker, message)? {
                        return Ok(None);
                    }
                }
                Err(TryRecvError::Empty) if !cancelling && deadline_exceeded.is_none() => {
                    worker.exit = Some(exit);
                    return Ok(Some(WorkerSlot::Active(worker)));
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
            }
        }
        if exit.status == WorkerExit::Success
            && !cancelling
            && deadline_exceeded.is_none()
            && let Some(result) = worker.result.take()
        {
            self.finish_worker(worker, *result)
                .context("committing supervised worker result")?;
            return Ok(None);
        }
        self.store.mark_unknown(
            &worker.attempt,
            if let Some(reason) = deadline_exceeded {
                reason
            } else if cancelling {
                "Cancellation raced with active work; side effects may have occurred"
            } else {
                "Worker exited unsuccessfully or without a valid result"
            },
        )?;
        Ok(None)
    }

    // True allows polling to continue. False means the attempt was fenced as unknown.
    fn accept_worker_message(
        &self,
        worker: &mut ActiveWorker,
        message: Result<WorkerMessage>,
    ) -> Result<bool> {
        match message {
            Ok(WorkerMessage::Result {
                identity,
                outcome,
                artifact,
            }) if identity == attempt_identity(&worker.attempt)? => {
                worker.result = Some(Box::new(AdmittedWorkerResult { outcome, artifact }));
            }
            Ok(_) => {
                let termination = worker.child.terminate();
                self.store.mark_unknown(
                    &worker.attempt,
                    "Worker returned a mismatched protocol message",
                )?;
                termination.context("terminating Worker after mismatched result")?;
                return Ok(false);
            }
            Err(error) => {
                let termination = worker.child.terminate();
                if let Some(reason) = worker.deadline.elapsed_reason(Instant::now()) {
                    self.store.mark_unknown(
                        &worker.attempt,
                        &format!("{reason}; protocol failed: {error}"),
                    )?;
                } else {
                    self.store.mark_unknown(
                        &worker.attempt,
                        &format!("Worker protocol failed: {error}"),
                    )?;
                }
                termination.context("terminating Worker after protocol failure")?;
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn finish_worker(&mut self, worker: ActiveWorker, result: AdmittedWorkerResult) -> Result<()> {
        let run = self.run(worker.run_id)?;
        if run.state == RunState::Cancelling {
            return self.store.mark_unknown(
                &worker.attempt,
                "Cancellation raced with active work; side effects may have occurred",
            );
        }
        let finished_at = Instant::now();
        if let Some(reason) = worker.deadline.elapsed_reason(finished_at) {
            return self.store.mark_unknown(&worker.attempt, reason);
        }
        let result = match decode_worker_result(result.outcome, result.artifact) {
            Ok(DecodedWorkerResult::Finished(result)) => result,
            Ok(DecodedWorkerResult::AgentOutcomeUnknown { provider_id }) => {
                if !matches!(worker.job.kind, JobKind::Agent { .. }) {
                    return self.store.mark_unknown(
                        &worker.attempt,
                        "Worker reported an Agent outcome for a non-Agent job",
                    );
                }
                self.store.mark_unknown(
                    &worker.attempt,
                    &format!("Agent provider {provider_id} did not yield a trustworthy result"),
                )?;
                eprintln!(
                    "mcbtodo-daemon: run_id={} attempt_id={} provider_id={provider_id} outcome=outcome_unknown",
                    worker.run_id, worker.attempt.id
                );
                return Ok(());
            }
            Err(error) => {
                return self.store.mark_unknown(
                    &worker.attempt,
                    &format!("Worker result failed validation: {error}"),
                );
            }
        };
        let agent_unused = if let WorkerDeadline::Agent(deadline) = worker.deadline {
            deadline.duration_since(finished_at)
        } else {
            Duration::ZERO
        };
        self.finish_worker_job(&worker, result, agent_unused)
    }

    fn finish_worker_job(
        &mut self,
        worker: &ActiveWorker,
        result: JobResult,
        agent_unused: Duration,
    ) -> Result<()> {
        let step = match worker.job.complete(result.outcome) {
            Ok(step) => step,
            Err(error) if matches!(worker.job.kind, JobKind::Agent { .. }) => {
                return self.store.mark_unknown(
                    &worker.attempt,
                    &format!("Agent output failed validation: {error}"),
                );
            }
            Err(error) => worker.job.complete(NodeOutcome::Failed {
                message: error.to_string(),
            })?,
        };
        let trace = match encode_trace(&step) {
            Ok(trace) => trace,
            Err(_) => {
                let detail = format!("Node `{}` exceeded the trace size limit", step.node);
                self.store.fail_attempt(&worker.attempt, &detail)?;
                return Ok(());
            }
        };
        match self.store.commit_result(AttemptResult {
            attempt: &worker.attempt,
            trace: &trace,
            artifact: result.artifact.as_ref(),
            agent_unused,
        }) {
            Ok(()) => Ok(()),
            Err(error) => {
                let current = self.run(worker.run_id)?;
                if current.state == RunState::Running {
                    self.store.mark_unknown(
                        &worker.attempt,
                        "Worker result could not be durably committed; the external outcome is uncertain.",
                    )?;
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn finish_job(&mut self, run: &RunSummary, job: &NodeJob, result: JobResult) -> Result<()> {
        let step = match job.complete(result.outcome) {
            Ok(step) => step,
            Err(error) => job.complete(NodeOutcome::Failed {
                message: error.to_string(),
            })?,
        };
        self.finish_step(run, &step, result.artifact.as_ref())?;
        Ok(())
    }

    fn finish_step(
        &mut self,
        run: &RunSummary,
        trace: &StepTrace,
        artifact: Option<&Artifact>,
    ) -> Result<bool> {
        // Leave room in the frame for the trace envelope and artifact metadata.
        let encoded = match encode_trace(trace) {
            Ok(encoded) => encoded,
            Err(_) => {
                self.fail(
                    run,
                    &format!("Node `{}` exceeded the trace size limit", trace.node),
                )?;
                return Ok(false);
            }
        };
        self.store.append_host_step(run, &encoded, artifact)?;
        Ok(true)
    }

    fn record(&self, run: &RunSummary, trace: &StepTrace) -> Result<()> {
        let encoded = encode_trace(trace)?;
        self.store.append_host_step(run, &encoded, None)
    }

    fn fail(&self, run: &RunSummary, message: &str) -> Result<()> {
        // Bound the inspect frame even when an invalid definition has a very long diagnostic.
        let detail: String = message.chars().take(512).collect();
        self.store
            .transition(run, RunState::Failed, None, Some(&detail))
    }
}

/// Persist Started and return the instant immediately before Start delivery.
/// From the first write onward, an I/O error cannot establish that the child
/// did not execute.
fn release_worker(
    store: &dyn WorkflowRunStore,
    attempt: &WorkerAttempt,
    identity: AttemptIdentity,
    stdin: &mut dyn WorkerInput,
    startup_deadline: Instant,
    execution_window: Duration,
) -> Result<Option<Instant>> {
    if let Err(error) = store.mark_started(attempt) {
        store.fail_attempt(
            attempt,
            &format!("Could not persist workflow worker start: {error}"),
        )?;
        return Ok(None);
    }
    let start = DaemonMessage::Start { identity };
    let released_at = Instant::now();
    // Start delivery consumes the same execution window as the running job.
    // The handshake and control limits still bound the write independently.
    stdin.set_deadline(
        startup_deadline.min(released_at + execution_window.min(WORKER_CONTROL_DELIVERY_TIMEOUT)),
    );
    let mut writer = BufWriter::new(stdin);
    let delivery = mcbtodo_protocol::worker::write(&mut writer, &start);
    // On failure, discard buffered bytes so Drop cannot retry Start delivery.
    let _ = writer.into_parts();
    if let Err(error) = delivery {
        store.mark_unknown(
            attempt,
            &format!("Worker start was persisted but Start delivery failed: {error}"),
        )?;
        return Ok(None);
    }
    Ok(Some(released_at))
}

fn encode_trace(trace: &StepTrace) -> Result<String> {
    let encoded = serde_json::to_string(trace)?;
    ensure!(
        encoded.len() <= wire::MAX_FRAME_BYTES as usize - 512,
        "step trace exceeds the size limit"
    );
    Ok(encoded)
}

fn execute_task_create(
    paths: &WorkspacePaths,
    job: &NodeJob,
    cancel: &AtomicBool,
) -> Result<Option<JobResult>> {
    let (Some(Scalar::String(project_id)), Some(Scalar::String(title))) =
        (job.inputs.get("project_id"), job.inputs.get("title"))
    else {
        anyhow::bail!("Task action requires a project UUID and a title");
    };
    let project_id = project_id.parse().context("Invalid project UUID")?;
    let timezone = paths.load_config()?.timezone;
    let root = paths.vault_root();
    let expected_revision = VaultQueries::new(&root, timezone)
        .project_revision(project_id)?
        .context("Task action project is missing")?;
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let id = Uuid::new_v4();
    let result = VaultCommands::new(&root, timezone).capture_task(
        Task::new(id, project_id),
        title.clone(),
        String::new(),
        expected_revision,
    );
    match result {
        Ok(_) => Ok(Some(JobResult {
            outcome: NodeOutcome::Succeeded {
                outputs: Default::default(),
            },
            artifact: Some(Artifact {
                media_type: "application/json",
                bytes: serde_json::to_vec(&json!({"task_id": id, "project_id": project_id}))?,
            }),
        })),
        Err(error @ WriteError::PublishedButNotSynced { .. }) => Err(UncertainWrite(error).into()),
        Err(error) => Ok(Some(JobResult {
            outcome: NodeOutcome::Failed {
                message: error.to_string().chars().take(1024).collect(),
            },
            artifact: None,
        })),
    }
}

fn agent_worker_timeout_seconds(window: Duration) -> Result<u32> {
    let seconds = window
        .as_secs()
        .saturating_add(u64::from(window.subsec_nanos() != 0))
        .max(1);
    u32::try_from(seconds).context("Agent execution timeout exceeds worker protocol limits")
}

fn worker_inputs(job: &NodeJob) -> Result<serde_json::Map<String, serde_json::Value>> {
    job.inputs
        .iter()
        .map(|(name, value)| Ok((name.clone(), serde_json::to_value(value)?)))
        .collect()
}

fn worker_claim<'a>(run: &'a RunSummary, node: &'a str) -> Result<ClaimAttempt<'a>> {
    Ok(ClaimAttempt {
        run_id: run.id,
        revision: &run.revision,
        node,
        expected_steps: run
            .step_count
            .checked_add(1)
            .context("workflow step count overflow")?,
    })
}

fn attempt_identity(attempt: &WorkerAttempt) -> Result<AttemptIdentity> {
    Ok(AttemptIdentity {
        run_id: attempt.run_id,
        revision: attempt.revision.clone(),
        node: attempt.node.clone(),
        attempt_id: attempt.id,
        epoch: u64::try_from(attempt.epoch).context("worker epoch is negative")?,
        expected_step: attempt.expected_steps,
    })
}

fn worker_binary_path() -> Result<PathBuf> {
    let current = std::env::current_exe().context("locating daemon executable")?;
    let mut parent = current
        .parent()
        .context("daemon executable has no parent")?;
    for _ in 0..8 {
        let candidate = parent.join("mcbtodo-worker");
        if candidate.is_file() {
            return Ok(candidate);
        }
        parent = parent
            .parent()
            .context("could not locate worker binary directory")?;
    }
    anyhow::bail!("mcbtodo-worker is missing beside {}", current.display())
}

fn read_worker_messages(
    reader: impl std::io::Read,
    sender: mpsc::SyncSender<Result<WorkerMessage>>,
) {
    let mut reader = BufReader::new(reader);
    loop {
        match mcbtodo_protocol::worker::read::<WorkerMessage>(&mut reader) {
            Ok(message) => {
                let terminal = matches!(message, WorkerMessage::Result { .. });
                if sender.send(Ok(message)).is_err() || terminal {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error.into()));
                return;
            }
        }
    }
}

// This thread owns only protocol I/O before Start. The daemon retains the
// process controller and the authority to persist and release the attempt.
fn prepare_worker(
    mut stdin: Box<dyn WorkerInput>,
    receiver: Receiver<Result<WorkerMessage>>,
    identity: AttemptIdentity,
    job: WorkerJob,
    deadline: Instant,
) -> Result<ReadyWorker> {
    stdin.set_deadline(deadline);
    let prepare = DaemonMessage::Prepare {
        identity: identity.clone(),
        job,
    };
    match mcbtodo_protocol::worker::write(&mut stdin, &prepare) {
        Ok(()) => {}
        Err(WorkerProtocolError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut => {
            anyhow::bail!("Worker startup deadline elapsed while delivering Prepare");
        }
        Err(error) => anyhow::bail!("Could not prepare mcbtodo-worker: {error}"),
    }
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(WorkerMessage::Ready { identity: ready })) if ready == identity => Ok(ReadyWorker {
            identity,
            stdin,
            receiver,
        }),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            anyhow::bail!("Worker startup deadline elapsed before Start")
        }
        result => anyhow::bail!("Worker failed readiness handshake: {result:?}"),
    }
}

fn send_worker_cancel(worker: &mut ActiveWorker) -> Result<()> {
    let identity = attempt_identity(&worker.attempt)?;
    if let Some(stdin) = worker.stdin.as_mut() {
        stdin.set_deadline(Instant::now() + WORKER_CONTROL_DELIVERY_TIMEOUT);
        mcbtodo_protocol::worker::write(stdin, &DaemonMessage::Cancel { identity })
            .context("sending worker cancellation")?;
    }
    worker.cancel_sent_at = Some(Instant::now());
    Ok(())
}

enum DecodedWorkerResult {
    Finished(JobResult),
    AgentOutcomeUnknown { provider_id: String },
}

fn decode_worker_result(
    outcome: WorkerOutcome,
    artifact: Option<WorkerArtifact>,
) -> Result<DecodedWorkerResult> {
    let outcome = match outcome {
        WorkerOutcome::Succeeded { outputs } => NodeOutcome::Succeeded {
            outputs: outputs
                .into_iter()
                .map(|(name, value)| {
                    serde_json::from_value(value)
                        .map(|value| (name.clone(), value))
                        .with_context(|| format!("invalid Worker output `{name}`"))
                })
                .collect::<Result<_>>()?,
        },
        WorkerOutcome::Failed { message } => NodeOutcome::Failed {
            message: message.chars().take(1024).collect(),
        },
        WorkerOutcome::AgentOutcomeUnknown { provider_id } => {
            ensure!(
                artifact.is_none(),
                "uncertain Agent outcome included an artifact"
            );
            return Ok(DecodedWorkerResult::AgentOutcomeUnknown { provider_id });
        }
    };
    let artifact = artifact
        .map(|artifact| {
            ensure!(
                artifact.media_type == "application/json",
                "unsupported Worker artifact media type"
            );
            Ok(Artifact {
                media_type: "application/json",
                bytes: artifact.bytes,
            })
        })
        .transpose()?;
    Ok(DecodedWorkerResult::Finished(JobResult {
        outcome,
        artifact,
    }))
}

fn validate_event_policy(missed: Missed, overlap: Overlap) -> Result<(), CommandError> {
    if missed != Missed::Skip || overlap != Overlap::Queue {
        return Err(CommandError::new(
            ErrorCode::InvalidRequest,
            "Event triggers require missed=skip and overlap=queue",
        ));
    }
    Ok(())
}

struct CommandError {
    code: ErrorCode,
    source: anyhow::Error,
}

impl CommandError {
    fn new(
        code: ErrorCode,
        message: impl std::fmt::Display + std::fmt::Debug + Send + Sync + 'static,
    ) -> Self {
        Self {
            code,
            source: anyhow::Error::msg(message),
        }
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(source: anyhow::Error) -> Self {
        Self {
            code: ErrorCode::Internal,
            source,
        }
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent_budget_tests.rs"]
mod agent_budget_tests;

#[cfg(test)]
#[path = "dependency_tests.rs"]
mod dependency_tests;

#[cfg(test)]
#[path = "completion_trigger_tests.rs"]
mod completion_trigger_tests;
