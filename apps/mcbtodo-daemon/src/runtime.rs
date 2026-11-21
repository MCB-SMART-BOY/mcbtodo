//! Owns durable workflow state on the daemon loop.

use std::{
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

#[cfg(test)]
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use mcbtodo_application::{ReminderSummary, VaultCommands, VaultQueries, project_reminders};
use mcbtodo_domain::Task;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{
    ErrorCode, Response, wire,
    worker::{
        AttemptIdentity, DaemonMessage, ScriptRunner as WorkerScriptRunner, WorkerArtifact,
        WorkerJob, WorkerMessage, WorkerOutcome,
    },
    workflow::{
        Missed, Overlap, RunState, RunSummary, ScheduleSummary, Trigger, WorkflowRequest,
        WorkflowResponse,
    },
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

use crate::notifications::Inbox;
use crate::run_store::{
    AgentBudgetDenied, Artifact, InvalidDependency, RunStore, WorkerAttempt, schedule_calculator,
};

// Poll the worker for completion without tying up the daemon's IPC or timer.
const WORKER_POLL: Duration = Duration::from_millis(50);
const WORKER_START_TIMEOUT: Duration = Duration::from_secs(5);
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
    store: RunStore,
    revisions: RevisionStore,
    paths: WorkspacePaths,
    worker: Option<ActiveWorker>,
    completions: CompletionPoller,
    vault_watcher: Option<VaultWatcher>,
    validated_vault: Option<Vault>,
    reminders: Vec<ReminderSummary>,
    vault_generation: u64,
    last_vault_error: Option<String>,
}

struct ActiveWorker {
    run_id: Uuid,
    job: NodeJob,
    attempt: WorkerAttempt,
    agent_deadline: Option<Instant>,
    child: Child,
    stdin: Option<ChildStdin>,
    receiver: Receiver<Result<WorkerMessage>>,
    result: Option<WorkerMessage>,
    cancel_sent_at: Option<Instant>,
}

struct JobResult {
    outcome: NodeOutcome,
    artifact: Option<Artifact>,
}

#[derive(Debug, thiserror::Error)]
#[error("Vault write was published before durability could be confirmed: {0}")]
struct UncertainWrite(#[source] WriteError);

impl Runtime {
    #[cfg(test)]
    pub(super) fn open_for_test(state_dir: &Path) -> Result<Self> {
        Self::open_with_paths(state_dir, WorkspacePaths::from_home(state_dir.to_owned()))
    }

    pub fn open(paths: WorkspacePaths) -> Result<Self> {
        let state_dir = paths.state_dir();
        Self::open_components(&state_dir, paths)
    }

    #[cfg(test)]
    pub(super) fn open_with_paths(state_dir: &Path, paths: WorkspacePaths) -> Result<Self> {
        Self::open_components(state_dir, paths)
    }

    fn open_components(state_dir: &Path, paths: WorkspacePaths) -> Result<Self> {
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
            store: RunStore::open(&if state_dir == paths.state_dir() {
                paths.workflow_database_path()
            } else {
                state_dir.join("workflow.sqlite3")
            })?,
            revisions: RevisionStore::new(state_dir),
            worker: None,
            completions: CompletionPoller::new(paths.clone()),
            vault_watcher,
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

    pub fn notifications(&self) -> Inbox<'_> {
        self.store.notifications()
    }

    pub fn wait_timeout(&self) -> Result<Option<Duration>> {
        let deadlines =
            self.store
                .next_schedule_due()?
                .into_iter()
                .chain(if self.worker.is_none() {
                    self.store.next_due()?
                } else {
                    None
                });
        let timed_wait = deadlines.min().map(|due| {
            let wait_ms = due.saturating_sub(Utc::now().timestamp_millis()).max(0) as u64;
            Duration::from_millis(wait_ms).min(CLOCK_CHECK)
        });
        Ok(timed_wait
            .into_iter()
            .chain(self.worker.as_ref().map(|_| WORKER_POLL))
            .chain(self.completions.wait_timeout(&self.store)?)
            .chain(self.vault_watcher.as_ref().map(|_| VAULT_WATCH_POLL))
            .min())
    }

    pub fn tick(&mut self) -> Result<()> {
        if let Some(worker) = self.worker.take()
            && let Some(completed) = self.poll_worker(worker)?
        {
            self.finish_worker(completed.0, completed.1)
                .context("committing supervised worker result")?;
        }
        self.store.schedule_tick(Utc::now())?;
        self.completions.tick(&self.store)?;
        self.refresh_vault_snapshot();
        if self.worker.is_none()
            && let Some(run) = self.store.next_pending()?
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
                if let Some(run) = self.store.get(run_id)? {
                    if run.revision != revision || run.due_at != due_at || run.schedule_id.is_some()
                    {
                        return Err(CommandError::new(
                            ErrorCode::Conflict,
                            "Run ID already belongs to different parameters",
                        ));
                    }
                    return Ok(WorkflowResponse::Run(run));
                }
                self.published(&revision)?;
                self.store.insert(run_id, &revision, due_at)?;
                Ok(WorkflowResponse::Run(self.run(run_id)?))
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
                let step = self.store.step(run_id, index)?.ok_or_else(|| {
                    CommandError::new(
                        ErrorCode::NotFound,
                        format!("No recorded step at index {index}"),
                    )
                })?;
                Ok(WorkflowResponse::Step { step })
            }
            Artifact {
                run_id,
                index,
                offset,
            } => {
                self.lookup(run_id)?;
                let (bytes, next_offset) =
                    self.store.artifact(run_id, index, offset)?.ok_or_else(|| {
                        CommandError::new(
                            ErrorCode::NotFound,
                            "No artifact at the requested position",
                        )
                    })?;
                Ok(WorkflowResponse::Artifact { bytes, next_offset })
            }
            Decide {
                run_id,
                node,
                approved,
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
                let outcome = if approved {
                    NodeOutcome::Approved
                } else {
                    NodeOutcome::Rejected
                };
                let step = job
                    .complete(outcome)
                    .context("Recording approval decision")?;
                self.record(&run, &step, None, Duration::ZERO)?;
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
                    RunState::Running => {
                        self.store.transition(
                            &run,
                            RunState::Cancelling,
                            run.current_node.as_deref(),
                            None,
                        )?;
                        let worker = self
                            .worker
                            .as_mut()
                            .filter(|worker| worker.run_id == run_id)
                            .context("Active workflow has no worker")?;
                        send_worker_cancel(worker)?;
                    }
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
                            if error.chain().any(|cause| {
                                cause
                                    .downcast_ref::<io::Error>()
                                    .is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock)
                            }) {
                                CommandError::new(
                                    ErrorCode::Conflict,
                                    "Default Vault is busy. Retry schedule registration.",
                                )
                            } else {
                                CommandError::new(ErrorCode::Conflict, format!("{error:#}"))
                            }
                        })?;
                        (None, Some(cursor))
                    }
                    Trigger::Interval { .. } | Trigger::Cron { .. } => {
                        let (calculator, _) = schedule_calculator(&trigger, missed)
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
                let inserted = if completion_cursor.is_some() {
                    self.store.insert_schedule_at(&summary, completion_cursor)
                } else {
                    self.store.insert_schedule(&summary)
                };
                inserted.map_err(|error| {
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
            InspectSchedule { id } => Ok(WorkflowResponse::Schedule(self.lookup_schedule(id)?)),
            DisableSchedule { id } => {
                let summary = self.lookup_schedule(id)?;
                Ok(WorkflowResponse::Schedule(
                    self.store.disable_schedule(summary)?,
                ))
            }
        }
    }

    fn lookup_schedule(&self, id: Uuid) -> Result<ScheduleSummary, CommandError> {
        self.store
            .schedule(id)?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, format!("Unknown schedule {id}")))
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
        Ok(advance(revision.plan(), &self.history(run.id)?)?)
    }

    fn history(&self, run_id: Uuid) -> Result<Vec<StepTrace>> {
        let history: Vec<StepTrace> = self
            .store
            .traces(run_id)?
            .iter()
            .map(|step| serde_json::from_str(step))
            .collect::<Result<_, _>>()?;
        Ok(history)
    }

    fn advance_run(&mut self, mut run: RunSummary) -> Result<()> {
        let revision = match self.revisions.load(&run.revision) {
            Ok(revision) => revision,
            Err(error) => {
                return self.fail(&run, &format!("Could not load published revision: {error}"));
            }
        };
        let (steps, next) = match self.progress(&run, &revision) {
            Ok(Progress::Complete(trace)) => {
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
            Ok(Progress::Next { job, steps }) => (steps, job),
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
                    Duration::ZERO,
                )
            }
            JobKind::Action {
                action: "task.create",
            } => self.run_task_create(&run, next),
            JobKind::Action { action } => self.fail(
                &run,
                &format!("Action `{action}` is unavailable for this run"),
            ),
            JobKind::Agent { .. } => self.start_agent(&run, next, &revision),
            JobKind::Script { .. } => self.start_script(&run, next, &revision),
        }
    }

    fn record_automatic<'a>(
        &mut self,
        run: &mut RunSummary,
        steps: impl IntoIterator<Item = &'a StepTrace>,
    ) -> Result<()> {
        for step in steps {
            if !self.finish_step(run, step, None, Duration::ZERO)? {
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
        revision: &WorkflowRevision,
    ) -> Result<()> {
        let window = match self.store.claim_agent(run, &job.node, || {
            Ok(self.history(run.id)?.iter().any(|step| {
                step.status != NodeStatus::Skipped && revision.plan().is_agent(&step.node)
            }))
        }) {
            Ok(window) => window,
            Err(error) if error.is::<AgentBudgetDenied>() => {
                return self.fail(run, &format!("Agent node `{}` denied: {error}", job.node));
            }
            Err(error) => return Err(error),
        };
        let deadline = Instant::now() + window;
        let JobKind::Agent { capability } = &job.kind else {
            unreachable!()
        };
        let timeout_seconds = u32::try_from(window.as_secs().max(1))
            .context("Agent execution timeout exceeds worker protocol limits")?;
        self.spawn_worker(
            run,
            job.clone(),
            WorkerJob::Agent {
                capability: capability.clone(),
                timeout_seconds,
                inputs: worker_inputs(&job)?,
            },
            Some(deadline),
        )
    }

    fn run_task_create(&mut self, run: &RunSummary, job: NodeJob) -> Result<()> {
        self.store
            .transition(run, RunState::Running, Some(&job.node), None)?;
        let result = execute_task_create(&self.paths, &job, &AtomicBool::new(false));
        let active = self.run(run.id)?;
        match result {
            Ok(Some(result)) => self.finish_job(&active, &job, result, Duration::ZERO),
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
            None,
        )
    }

    fn spawn_worker(
        &mut self,
        run: &RunSummary,
        job: NodeJob,
        worker_job: WorkerJob,
        agent_deadline: Option<Instant>,
    ) -> Result<()> {
        let binary = match worker_binary_path() {
            Ok(binary) => binary,
            Err(error) => {
                let current = self.run(run.id)?;
                return self.fail(
                    &current,
                    &format!("Could not locate mcbtodo-worker: {error}"),
                );
            }
        };
        self.spawn_worker_at(run, job, worker_job, agent_deadline, binary)
    }

    fn spawn_worker_at(
        &mut self,
        run: &RunSummary,
        job: NodeJob,
        worker_job: WorkerJob,
        agent_deadline: Option<Instant>,
        binary: PathBuf,
    ) -> Result<()> {
        let expected_steps = run
            .step_count
            .checked_add(1)
            .context("workflow step count overflow")?;
        let attempt = self
            .store
            .begin_worker_attempt(run, &job.node, expected_steps)?;
        let active_run = self.run(run.id)?;
        let identity = attempt_identity(&attempt)?;
        let mut command = Command::new(&binary);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear()
            .env("HOME", self.paths.home_dir());
        #[cfg(target_os = "linux")]
        {
            command.process_group(0);
            let parent = rustix::process::getpid();
            command.env("MCBTODO_PARENT_PID", parent.as_raw_pid().to_string());
        }
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("mcbtodo-worker supervision requires Linux");
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.store.fail_worker_attempt(
                    &active_run,
                    &attempt,
                    &format!("Could not start mcbtodo-worker: {error}"),
                )?;
                return Ok(());
            }
        };
        let mut stdin = child.stdin.take().context("worker stdin was not piped")?;
        let stdout = child.stdout.take().context("worker stdout was not piped")?;
        let (sender, receiver) = mpsc::sync_channel(2);
        if let Err(error) = thread::Builder::new()
            .name("mcbtodo-worker-reader".into())
            .spawn(move || read_worker_messages(stdout, sender))
        {
            self.store.fail_worker_attempt(
                &active_run,
                &attempt,
                &format!("Could not start worker protocol reader: {error}"),
            )?;
            terminate_worker(&mut child)?;
            return Ok(());
        }
        let prepare = DaemonMessage::Prepare {
            identity: identity.clone(),
            job: worker_job,
        };
        if let Err(error) =
            mcbtodo_protocol::worker::write(&mut BufWriter::new(&mut stdin), &prepare)
        {
            self.store.fail_worker_attempt(
                &active_run,
                &attempt,
                &format!("Could not prepare mcbtodo-worker: {error}"),
            )?;
            terminate_worker(&mut child)?;
            return Ok(());
        }
        let ready = receiver.recv_timeout(WORKER_START_TIMEOUT);
        match ready {
            Ok(Ok(WorkerMessage::Ready { identity: ready })) if ready == identity => {}
            result => {
                self.store.fail_worker_attempt(
                    &active_run,
                    &attempt,
                    &format!("Worker failed readiness handshake: {result:?}"),
                )?;
                terminate_worker(&mut child)?;
                return Ok(());
            }
        }
        if let Err(error) = self.store.mark_worker_started(&attempt) {
            self.store.fail_worker_attempt(
                &active_run,
                &attempt,
                &format!("Could not persist workflow worker start: {error}"),
            )?;
            terminate_worker(&mut child)?;
            return Ok(());
        }
        let start = DaemonMessage::Start {
            identity: identity.clone(),
        };
        if let Err(error) = mcbtodo_protocol::worker::write(&mut BufWriter::new(&mut stdin), &start)
        {
            terminate_worker(&mut child)?;
            let active = self.run(run.id)?;
            self.store.transition(
                &active,
                RunState::OutcomeUnknown,
                Some(&job.node),
                Some(&format!(
                    "Worker start was persisted but Start delivery failed: {error}"
                )),
            )?;
            return Ok(());
        }
        self.worker = Some(ActiveWorker {
            run_id: run.id,
            job,
            attempt,
            agent_deadline,
            child,
            stdin: Some(stdin),
            receiver,
            result: None,
            cancel_sent_at: None,
        });
        Ok(())
    }

    fn poll_worker(
        &mut self,
        mut worker: ActiveWorker,
    ) -> Result<Option<(ActiveWorker, WorkerMessage)>> {
        let run = self.run(worker.run_id)?;
        let timed_out = worker
            .agent_deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        if run.state == RunState::Cancelling || timed_out {
            if worker.cancel_sent_at.is_none()
                && let Err(error) = send_worker_cancel(&mut worker)
            {
                terminate_worker(&mut worker.child)?;
                self.mark_worker_unknown(
                    &run,
                    &format!("Worker cancellation could not be delivered: {error}"),
                )?;
                return Ok(None);
            }
            if worker
                .cancel_sent_at
                .is_some_and(|sent| sent.elapsed() >= WORKER_CANCEL_GRACE)
            {
                terminate_worker(&mut worker.child)?;
            }
        }
        match worker.receiver.try_recv() {
            Ok(Ok(message)) => {
                let expected = attempt_identity(&worker.attempt)?;
                if matches!(&message, WorkerMessage::Result { identity, .. } if identity == &expected)
                {
                    worker.result = Some(message);
                } else {
                    terminate_worker(&mut worker.child)?;
                    self.mark_worker_unknown(
                        &run,
                        "Worker returned a mismatched protocol message",
                    )?;
                    return Ok(None);
                }
            }
            Ok(Err(error)) => {
                terminate_worker(&mut worker.child)?;
                self.mark_worker_unknown(&run, &format!("Worker protocol failed: {error}"))?;
                return Ok(None);
            }
            Err(TryRecvError::Disconnected | TryRecvError::Empty) => {}
        }
        if let Some(status) = worker.child.try_wait()? {
            if worker.result.is_none()
                && let Ok(Ok(message)) = worker.receiver.recv_timeout(Duration::from_millis(100))
            {
                let expected = attempt_identity(&worker.attempt)?;
                if matches!(&message, WorkerMessage::Result { identity, .. } if identity == &expected)
                {
                    worker.result = Some(message);
                }
            }
            if !status.success()
                || run.state == RunState::Cancelling
                || timed_out
                || worker.result.is_none()
            {
                self.mark_worker_unknown(
                    &run,
                    if timed_out {
                        "Worker exceeded its Agent execution budget"
                    } else if run.state == RunState::Cancelling {
                        "Cancellation raced with active work; side effects may have occurred"
                    } else {
                        "Worker exited unsuccessfully or without a valid result"
                    },
                )?;
                return Ok(None);
            }
            let result = worker.result.take().context("worker result disappeared")?;
            return Ok(Some((worker, result)));
        }
        self.worker = Some(worker);
        Ok(None)
    }

    fn mark_worker_unknown(&self, run: &RunSummary, detail: &str) -> Result<()> {
        self.store.transition(
            run,
            RunState::OutcomeUnknown,
            run.current_node.as_deref(),
            Some(detail),
        )
    }

    fn finish_worker(&mut self, worker: ActiveWorker, message: WorkerMessage) -> Result<()> {
        let run = self.run(worker.run_id)?;
        let WorkerMessage::Result {
            identity,
            outcome,
            artifact,
        } = message
        else {
            anyhow::bail!("worker returned readiness after execution started");
        };
        ensure!(
            identity == attempt_identity(&worker.attempt)?,
            "worker result attempt identity mismatch"
        );
        if run.state == RunState::Cancelling {
            return self.mark_worker_unknown(
                &run,
                "Cancellation raced with active work; side effects may have occurred",
            );
        }
        let finished_at = Instant::now();
        let agent_unused = if let Some(deadline) = worker.agent_deadline {
            if finished_at >= deadline {
                return self.mark_worker_unknown(&run, "Agent execution exceeded its budget");
            }
            deadline.duration_since(finished_at)
        } else {
            Duration::ZERO
        };
        let result = match job_result(outcome, artifact) {
            Ok(result) => result,
            Err(error) => {
                return self.mark_worker_unknown(
                    &run,
                    &format!("Worker result failed validation: {error}"),
                );
            }
        };
        self.finish_worker_job(&run, &worker, result, agent_unused)
    }

    fn finish_worker_job(
        &mut self,
        run: &RunSummary,
        worker: &ActiveWorker,
        result: JobResult,
        agent_unused: Duration,
    ) -> Result<()> {
        let step = match worker.job.complete(result.outcome) {
            Ok(step) => step,
            Err(error) => worker.job.complete(NodeOutcome::Failed {
                message: error.to_string(),
            })?,
        };
        let trace = match encode_trace(&step) {
            Ok(trace) => trace,
            Err(_) => {
                let detail = format!("Node `{}` exceeded the trace size limit", step.node);
                self.store
                    .fail_worker_attempt(run, &worker.attempt, &detail)?;
                return Ok(());
            }
        };
        match self.store.append_worker_result(
            run,
            &worker.attempt,
            &trace,
            result.artifact.as_ref(),
            agent_unused,
        ) {
            Ok(()) => Ok(()),
            Err(error) => {
                let current = self.run(run.id)?;
                if current.state == RunState::Running {
                    self.store.transition(
                        &current,
                        RunState::OutcomeUnknown,
                        current.current_node.as_deref(),
                        Some("Worker result could not be durably committed; the external outcome is uncertain."),
                    )?;
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn finish_job(
        &mut self,
        run: &RunSummary,
        job: &NodeJob,
        result: JobResult,
        agent_unused: Duration,
    ) -> Result<()> {
        let step = match job.complete(result.outcome) {
            Ok(step) => step,
            Err(error) => job.complete(NodeOutcome::Failed {
                message: error.to_string(),
            })?,
        };
        self.finish_step(run, &step, result.artifact.as_ref(), agent_unused)?;
        Ok(())
    }

    fn finish_step(
        &mut self,
        run: &RunSummary,
        trace: &StepTrace,
        artifact: Option<&Artifact>,
        agent_unused: Duration,
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
        self.record_encoded(run, &encoded, artifact, agent_unused)?;
        Ok(true)
    }

    fn record(
        &mut self,
        run: &RunSummary,
        trace: &StepTrace,
        artifact: Option<&Artifact>,
        agent_unused: Duration,
    ) -> Result<()> {
        let encoded = encode_trace(trace)?;
        self.record_encoded(run, &encoded, artifact, agent_unused)
    }

    fn record_encoded(
        &mut self,
        run: &RunSummary,
        trace: &str,
        artifact: Option<&Artifact>,
        agent_unused: Duration,
    ) -> Result<()> {
        self.store.append(run, trace, artifact, agent_unused)
    }

    fn fail(&self, run: &RunSummary, message: &str) -> Result<()> {
        // Bound the inspect frame even when an invalid definition has a very long diagnostic.
        let detail: String = message.chars().take(512).collect();
        self.store
            .transition(run, RunState::Failed, None, Some(&detail))
    }
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
    let task = Task {
        id,
        project_id,
        status: Default::default(),
        priority: Default::default(),
        active_period: None,
        deadline: None,
        recurrence: None,
        occurrence: None,
        tags: Vec::new(),
    };
    let result = VaultCommands::new(&root, timezone).capture_task(
        task,
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

fn worker_inputs(job: &NodeJob) -> Result<serde_json::Map<String, serde_json::Value>> {
    job.inputs
        .iter()
        .map(|(name, value)| Ok((name.clone(), serde_json::to_value(value)?)))
        .collect()
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

fn send_worker_cancel(worker: &mut ActiveWorker) -> Result<()> {
    let identity = attempt_identity(&worker.attempt)?;
    if let Some(stdin) = worker.stdin.as_mut() {
        mcbtodo_protocol::worker::write(
            &mut BufWriter::new(stdin),
            &DaemonMessage::Cancel { identity },
        )
        .context("sending worker cancellation")?;
    }
    worker.cancel_sent_at = Some(Instant::now());
    Ok(())
}

fn terminate_worker(child: &mut Child) -> Result<()> {
    #[cfg(target_os = "linux")]
    let group_termination = {
        use rustix::{
            io::Errno,
            process::{Pid, Signal, kill_process_group},
        };
        match Pid::from_raw(child.id() as i32) {
            Some(pid) => kill_process_group(pid, Signal::KILL).or_else(|error| {
                if error == Errno::SRCH {
                    Ok(())
                } else {
                    Err(error)
                }
            }),
            None => Ok(()),
        }
    };
    #[cfg(target_os = "linux")]
    let group_termination = group_termination.map_err(anyhow::Error::from);
    let _ = child.kill();
    let child_wait = child
        .wait()
        .context("waiting for terminated mcbtodo-worker");
    #[cfg(target_os = "linux")]
    group_termination.context("terminating mcbtodo-worker process group")?;
    child_wait?;
    Ok(())
}

fn job_result(outcome: WorkerOutcome, artifact: Option<WorkerArtifact>) -> Result<JobResult> {
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
    Ok(JobResult { outcome, artifact })
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
