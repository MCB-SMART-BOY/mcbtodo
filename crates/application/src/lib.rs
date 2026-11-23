//! Application use cases shared by local transports and user interfaces.

mod completion_export;
pub mod graph_diff;
mod workflow;

pub use completion_export::CompletionExportEntry;
pub use workflow::WorkflowUseCases;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use chrono::{DateTime, FixedOffset, NaiveDate};
use chrono_tz::Tz;
use mcbtodo_domain::{
    Availability, CalendarEvent, EvidenceRef, Id, InboxEntry, Project, Task, TaskGraph, TimeBlock,
    WorkUnit, WorkflowRevisionHash,
    recurrence::{Reminder, ReminderSchedule},
    time::{DateSpan, DateTimeSpan, Deadline, Span, ZonedMoment},
};
pub use mcbtodo_domain::{
    CalendarItem, DependencyOperation, InboxSummary, OutlineItem, WorkUnitDependencyPreview,
    WorkUnitGraphNode,
};
use mcbtodo_vault::{
    AddWorkUnitGraph, BindWorkflow, CaptureInboxEntry, CaptureTask, CompleteWorkUnit, CreateEvent,
    CreateProject, CreateReminder, CreateTimeBlock, CreateWorkUnit, MaterializeTaskOccurrences,
    NewWorkUnit, PersistedDocument, Revision, SetDeadline, SetWorkUnitDependencies,
    TriageInboxEntry, Vault, VaultStore, WriteError,
};
use serde::Serialize;

pub use graph_diff::{Draft as WorkUnitGraphDraft, Edge as GraphEdge, GraphDiff, UnitDraft};

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Transport-neutral event data accepted by the atomic calendar import use case.
#[derive(Debug)]
pub struct CalendarEventDraft {
    pub event: CalendarEvent,
    pub title: String,
    pub body: String,
}

/// Dates selected in the Project or Deadline quick-create form.
#[derive(Clone, Copy, Debug)]
pub enum ProjectPlan {
    ActivePeriod(DateSpan),
    Deadline(NaiveDate),
}

impl ProjectPlan {
    fn draft(self, id: Id, title: String) -> Result<CreateProject, WriteError> {
        let (active_period, deadline) = match self {
            Self::ActivePeriod(span) => (Some(Span::DateSpan(span)), None),
            Self::Deadline(value) => (None, Some(Deadline::Date { value })),
        };
        validate_markdown(&title, "").map_err(WriteError::InvalidDocument)?;
        Ok(CreateProject {
            project: Project {
                id,
                status: Default::default(),
                active_period,
                deadline,
                tags: Vec::new(),
            },
            slug: None,
            title,
            body: String::new(),
        })
    }
}

fn all_day_event(id: Id, span: DateSpan) -> CalendarEvent {
    CalendarEvent {
        id,
        when: Span::DateSpan(span),
        recurrence: None,
        availability: Availability::Busy,
        tags: Vec::new(),
    }
}

/// Write-side application boundary for Vault-backed business commands.
pub struct VaultCommands {
    root: PathBuf,
    timezone: Tz,
    completion_origin: Option<Id>,
}

impl VaultCommands {
    pub fn new(root: impl AsRef<Path>, timezone: Tz) -> Self {
        Self {
            root: root.as_ref().to_owned(),
            timezone,
            completion_origin: None,
        }
    }

    /// Uses the host's installation identity when recording a new completion.
    pub fn with_completion_origin(mut self, origin_device_id: Id) -> Self {
        self.completion_origin = Some(origin_device_id);
        self
    }

    /// Creates a Project after applying the shared user-authored Markdown policy.
    pub fn create_project(
        &self,
        project: Project,
        slug: Option<String>,
        title: String,
        body: String,
    ) -> Result<PersistedDocument, WriteError> {
        validate_markdown(&title, &body).map_err(WriteError::InvalidDocument)?;
        VaultStore::new(&self.root, self.timezone).create_project(CreateProject {
            project,
            slug,
            title,
            body,
        })
    }

    /// Publishes the Project used to represent a new activity period or deadline.
    pub fn create_planned_project(
        &self,
        id: Id,
        plan: ProjectPlan,
        title: String,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).create_project(plan.draft(id, title)?)
    }

    /// Publishes the same Project without waiting for the writer lock.
    pub fn try_create_planned_project(
        &self,
        id: Id,
        plan: ProjectPlan,
        title: String,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).try_create_project(plan.draft(id, title)?)
    }

    /// Creates a CalendarEvent after applying the shared Markdown policy.
    pub fn create_event(
        &self,
        event: CalendarEvent,
        title: String,
        body: String,
    ) -> Result<PersistedDocument, WriteError> {
        validate_markdown(&title, &body).map_err(WriteError::InvalidDocument)?;
        VaultStore::new(&self.root, self.timezone).create_event(CreateEvent { event, title, body })
    }

    /// Creates the busy, non-recurring all-day event offered by the quick-create form.
    pub fn create_all_day_event(
        &self,
        id: Id,
        span: DateSpan,
        title: String,
    ) -> Result<PersistedDocument, WriteError> {
        self.create_event(all_day_event(id, span), title, String::new())
    }

    /// Creates the same quick event without waiting for the writer lock.
    pub fn try_create_all_day_event(
        &self,
        id: Id,
        span: DateSpan,
        title: String,
    ) -> Result<PersistedDocument, WriteError> {
        validate_markdown(&title, "").map_err(WriteError::InvalidDocument)?;
        VaultStore::new(&self.root, self.timezone).try_create_event(CreateEvent {
            event: all_day_event(id, span),
            title,
            body: String::new(),
        })
    }

    /// Atomically publishes imported CalendarEvents after applying shared text validation.
    ///
    /// Validation happens for the complete batch before Vault acquires its writer
    /// lock and performs the single directory publication.
    pub fn create_events_atomic(
        &self,
        drafts: Vec<CalendarEventDraft>,
    ) -> Result<usize, WriteError> {
        let mut requests = Vec::with_capacity(drafts.len());
        for draft in drafts {
            validate_markdown(&draft.title, &draft.body).map_err(WriteError::InvalidDocument)?;
            requests.push(CreateEvent {
                event: draft.event,
                title: draft.title,
                body: draft.body,
            });
        }
        VaultStore::new(&self.root, self.timezone).create_events_atomic(requests)
    }

    /// Materializes recurring Task dates using the caller's observed template revision.
    ///
    /// Vault owns locking, revision checks, stable occurrence identities, and atomic
    /// publication. Existing occurrence documents remain unchanged on retry.
    pub fn materialize_task_occurrences(
        &self,
        template_id: Id,
        window: DateSpan,
        expected_revision: Revision,
    ) -> Result<Vec<Id>, WriteError> {
        VaultStore::new(&self.root, self.timezone).materialize_task_occurrences(
            MaterializeTaskOccurrences {
                template_id,
                window,
                expected_revision,
            },
        )
    }

    /// Publishes an approved WorkUnit graph diff after revalidating the review payload.
    ///
    /// The caller owns proposal approval and idempotency state. Vault owns locking,
    /// revision checks, graph validation against the current snapshot, and atomic
    /// publication.
    pub fn apply_work_unit_graph_diff(
        &self,
        draft: &WorkUnitGraphDraft,
        diff: GraphDiff,
        expected_task_revision: Revision,
        expected_related_revisions: std::collections::BTreeMap<Id, Revision>,
    ) -> Result<Vec<Id>, WriteError> {
        draft.validate().map_err(WriteError::InvalidDocument)?;
        diff.validate_against(draft)
            .map_err(WriteError::InvalidDocument)?;
        for unit in &diff.added_nodes {
            validate_markdown(&unit.title, &unit.body).map_err(WriteError::InvalidDocument)?;
        }
        let required_revisions: std::collections::BTreeSet<_> = std::iter::once(diff.project_id)
            .chain(diff.existing_dependencies())
            .collect();
        if required_revisions != expected_related_revisions.keys().copied().collect() {
            return Err(WriteError::InvalidDocument(anyhow::anyhow!(
                "graph diff revisions must cover the project and each existing prerequisite"
            )));
        }

        let ids = diff.added_nodes.iter().map(|unit| unit.id).collect();
        let units = diff
            .added_nodes
            .iter()
            .map(|entry| NewWorkUnit {
                unit: entry.work_unit(diff.project_id, diff.task_id),
                title: entry.title.clone(),
                body: entry.body.clone(),
            })
            .collect();
        VaultStore::new(&self.root, self.timezone).add_work_unit_graph(AddWorkUnitGraph {
            project_id: diff.project_id,
            task_id: diff.task_id,
            expected_task: expected_task_revision,
            expected_related: expected_related_revisions,
            units,
        })?;
        Ok(ids)
    }

    /// Sets or clears a deadline using the revision observed by the caller.
    ///
    /// Vault owns locking, byte-exact revision checks, and atomic publication.
    pub fn set_deadline(
        &self,
        id: Id,
        deadline: Option<Deadline>,
        expected_revision: Revision,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).set_deadline(SetDeadline {
            id,
            deadline,
            expected_revision,
        })
    }

    /// Captures a Task under its Project using the caller's observed revision.
    pub fn capture_task(
        &self,
        task: Task,
        title: String,
        body: String,
        expected_revision: Revision,
    ) -> Result<PersistedDocument, WriteError> {
        validate_markdown(&title, &body).map_err(WriteError::InvalidDocument)?;
        VaultStore::new(&self.root, self.timezone).capture_task(CaptureTask {
            task,
            title,
            body,
            expected_revision,
        })
    }

    /// Captures a title-only Task from a remote snapshot without waiting for the writer lock.
    pub fn try_capture_task(
        &self,
        id: Id,
        project_id: Id,
        title: &str,
        project_revision: &str,
    ) -> Result<PersistedDocument, WriteError> {
        validate_markdown(title, "").map_err(WriteError::InvalidDocument)?;
        VaultStore::new(&self.root, self.timezone).try_capture_task(
            Task::new(id, project_id),
            title,
            "",
            project_revision,
        )
    }

    /// Captures an unclassified item after applying the shared Markdown policy.
    pub fn capture_inbox_entry(
        &self,
        entry: InboxEntry,
        title: String,
        body: String,
    ) -> Result<PersistedDocument, WriteError> {
        validate_markdown(&title, &body).map_err(WriteError::InvalidDocument)?;
        VaultStore::new(&self.root, self.timezone).capture_inbox_entry(CaptureInboxEntry {
            entry,
            title,
            body,
        })
    }

    /// Creates a WorkUnit after applying the shared Markdown policy.
    ///
    /// Vault owns parent/revision checks, candidate DAG validation, locking,
    /// and atomic publication.
    pub fn create_work_unit(
        &self,
        unit: WorkUnit,
        title: String,
        body: String,
        expected_revision: Revision,
    ) -> Result<PersistedDocument, WriteError> {
        validate_markdown(&title, &body).map_err(WriteError::InvalidDocument)?;
        VaultStore::new(&self.root, self.timezone).create_work_unit(CreateWorkUnit {
            unit,
            title,
            body,
            expected_revision,
        })
    }

    /// Completes a WorkUnit using context resolved from one Vault snapshot.
    ///
    /// Runtime evidence is validated here before the Vault performs its
    /// revision-checked atomic write.
    pub fn complete_work_unit(
        &self,
        context: WorkUnitCompletionContext,
        evidence: &[EvidenceRef],
        validate_evidence: impl FnOnce(
            Option<&WorkflowRevisionHash>,
            &[EvidenceRef],
        ) -> anyhow::Result<()>,
    ) -> Result<PersistedDocument, CompleteWorkUnitError> {
        if !evidence.is_empty() {
            validate_evidence(context.workflow_revision.as_ref(), evidence)
                .map_err(CompleteWorkUnitError::InvalidEvidence)?;
        }
        let mut store = VaultStore::new(&self.root, self.timezone);
        if let Some(origin) = self.completion_origin {
            store = store.with_completion_origin(origin);
        }
        store
            .complete_work_unit_with_evidence(
                CompleteWorkUnit {
                    id: context.id,
                    expected_revision: context.revision,
                },
                evidence,
            )
            .map_err(CompleteWorkUnitError::Write)
    }

    /// Binds a WorkUnit to an immutable workflow revision.
    ///
    /// Callers verify that the workflow revision has been published.
    pub fn bind_workflow(
        &self,
        work_unit_id: Id,
        workflow_revision: WorkflowRevisionHash,
        expected_revision: Revision,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).bind_workflow(BindWorkflow {
            work_unit_id,
            workflow_revision,
            expected_revision,
        })
    }

    /// Validates an offset-aware interval against the workspace timezone and
    /// publishes it as a WorkUnit time block.
    pub fn create_time_block(
        &self,
        work_unit_id: Id,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        availability: Availability,
        expected_revision: Revision,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).create_time_block(CreateTimeBlock {
            work_unit_id,
            block: self.time_block(start, end, availability)?,
            expected_revision,
        })
    }

    /// Validates and appends a busy block without waiting for another writer.
    pub fn try_create_busy_time_block(
        &self,
        work_unit_id: Id,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        work_unit_revision: &str,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).try_create_time_block(
            work_unit_id,
            self.time_block(start, end, Availability::Busy)?,
            work_unit_revision,
        )
    }

    fn time_block(
        &self,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        availability: Availability,
    ) -> Result<TimeBlock, WriteError> {
        let when = DateTimeSpan::new(start, end, self.timezone)
            .map_err(|error| WriteError::InvalidDocument(error.into()))?;
        Ok(TimeBlock { when, availability })
    }

    /// Creates a reminder after validating the user-authored text.
    ///
    /// Vault validates the optional target, locks the snapshot, and publishes
    /// the reminder atomically.
    pub fn create_reminder(
        &self,
        id: Id,
        target_id: Option<Id>,
        schedule: ReminderSchedule,
        title: String,
        body: String,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone)
            .create_reminder(reminder_draft(id, target_id, schedule, title, body)?)
    }

    /// Validates the submitted offset against the workspace timezone and
    /// publishes a one-time reminder without waiting for a writer.
    pub fn try_create_one_time_reminder(
        &self,
        id: Id,
        target_id: Option<Id>,
        at: DateTime<FixedOffset>,
        title: String,
    ) -> Result<PersistedDocument, WriteError> {
        let at = ZonedMoment::new(at, self.timezone)
            .map_err(|error| WriteError::InvalidDocument(error.into()))?;
        VaultStore::new(&self.root, self.timezone).try_create_reminder(reminder_draft(
            id,
            target_id,
            ReminderSchedule::Once { at },
            title,
            String::new(),
        )?)
    }

    /// Replaces one WorkUnit's prerequisites after validating the candidate DAG.
    ///
    /// Vault owns locking, revision checks, graph invariants, and atomic publication.
    pub fn set_work_unit_dependencies(
        &self,
        work_unit_id: Id,
        depends_on: Vec<Id>,
        expected_revision: Revision,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).set_work_unit_dependencies(
            SetWorkUnitDependencies {
                work_unit_id,
                depends_on,
                expected_revision,
            },
        )
    }

    /// Replaces prerequisites without waiting for a competing Vault writer.
    pub fn try_set_work_unit_dependencies(
        &self,
        work_unit_id: Id,
        depends_on: Vec<Id>,
        expected_digest: &str,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).try_set_work_unit_dependencies(
            work_unit_id,
            depends_on,
            expected_digest,
        )
    }

    /// Assigns a captured Inbox entry to an existing Project without losing its content.
    ///
    /// Vault validates the target and observed Inbox revision under its writer lock.
    pub fn triage_inbox_entry(
        &self,
        id: Id,
        project_id: Id,
        expected_revision: Revision,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).triage_inbox_entry(TriageInboxEntry {
            id,
            project_id,
            expected_revision,
        })
    }

    /// Assigns an Inbox entry using the observed IPC revision without waiting for the writer lock.
    pub fn try_triage_inbox_entry(
        &self,
        id: Id,
        project_id: Id,
        expected_digest: &str,
    ) -> Result<PersistedDocument, WriteError> {
        VaultStore::new(&self.root, self.timezone).try_triage_inbox_entry(
            id,
            project_id,
            expected_digest,
        )
    }
}

fn reminder_draft(
    id: Id,
    target_id: Option<Id>,
    schedule: ReminderSchedule,
    title: String,
    body: String,
) -> Result<CreateReminder, WriteError> {
    validate_markdown(&title, &body).map_err(WriteError::InvalidDocument)?;
    Ok(CreateReminder {
        reminder: Reminder {
            id,
            target_id,
            schedule,
        },
        title,
        body,
    })
}

pub fn validate_markdown(title: &str, body: &str) -> Result<()> {
    anyhow::ensure!(
        !title.trim().is_empty()
            && title.chars().count() <= MAX_TITLE_CHARS
            && !title.chars().any(char::is_control),
        "title must contain 1–{MAX_TITLE_CHARS} characters and no control characters"
    );
    anyhow::ensure!(
        body.len() <= MAX_BODY_BYTES,
        "body exceeds {MAX_BODY_BYTES} bytes"
    );
    anyhow::ensure!(
        !body
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n')),
        "body contains unsupported control characters"
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectSummary {
    pub id: Id,
    pub title: String,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TaskSummary {
    pub id: Id,
    pub title: String,
    pub project_id: Id,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkUnitSummary {
    pub id: Id,
    pub title: String,
    pub project_id: Id,
    pub task_id: Id,
    pub blocked: bool,
    pub estimated_minutes: Option<u32>,
    pub revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Project,
    Task,
    WorkUnit,
    Inbox,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetSummary {
    pub id: Id,
    pub kind: TargetKind,
    pub title: String,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkUnitGraphReview {
    pub target: TargetSummary,
    pub diff: GraphDiff,
    pub expected_revisions: BTreeMap<Id, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedRevisionSet {
    pub target: Revision,
    pub related: BTreeMap<Id, Revision>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkUnitCompletionContext {
    id: Id,
    revision: Revision,
    workflow_revision: Option<WorkflowRevisionHash>,
}

impl WorkUnitCompletionContext {
    pub fn id(&self) -> Id {
        self.id
    }

    pub fn revision(&self) -> &Revision {
        &self.revision
    }

    pub fn workflow_revision(&self) -> Option<&WorkflowRevisionHash> {
        self.workflow_revision.as_ref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompleteWorkUnitError {
    #[error("workflow evidence is invalid: {0}")]
    InvalidEvidence(#[source] anyhow::Error),
    #[error(transparent)]
    Write(#[from] WriteError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadlineEditContext {
    pub revision: Revision,
    pub deadline: Option<Deadline>,
}

/// Business action allowed for a resolved Vault target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetAction {
    CaptureTask,
    TriageInbox { project_id: Id },
    CompleteWorkUnit,
    CreateTimeBlock,
    SetDeadline,
    CreateReminder,
    EditWorkUnitGraph,
}

/// Projects a supported Vault document into the shared owned target shape.
pub fn target_summary(vault: &Vault, id: Id) -> Option<TargetSummary> {
    if let Some(row) = vault.outline().into_iter().find(|row| row.id == id) {
        let kind = match row.depth {
            0 => TargetKind::Project,
            1 => TargetKind::Task,
            2 => TargetKind::WorkUnit,
            _ => return None,
        };
        return Some(TargetSummary {
            id,
            kind,
            title: row.title.to_owned(),
            revision: revision_digest(vault, id),
        });
    }
    vault
        .inbox_entries()
        .find(|entry| entry.data.id == id)
        .map(|entry| TargetSummary {
            id,
            kind: TargetKind::Inbox,
            title: entry.title.clone(),
            revision: revision_digest(vault, id),
        })
}

/// Resolves the revision only when the target is a Project in this snapshot.
pub fn project_revision_in_snapshot(vault: &Vault, project_id: Id) -> Option<Revision> {
    if !vault
        .outline()
        .iter()
        .any(|row| row.depth == 0 && row.id == project_id)
    {
        return None;
    }
    vault.revision(project_id).cloned()
}

/// Returns a document's opaque revision token from an already validated snapshot.
///
/// Adapters can pass the token unchanged to a write command without loading a
/// second Vault snapshot. Vault rechecks it under the writer lock.
pub fn document_revision_in_snapshot(vault: &Vault, id: Id) -> Option<Revision> {
    vault.revision(id).cloned()
}

/// Resolves the revision and workflow binding needed to complete a WorkUnit.
pub fn work_unit_completion_context(vault: &Vault, id: Id) -> Option<WorkUnitCompletionContext> {
    let work_unit = vault.work_unit(id)?;
    Some(WorkUnitCompletionContext {
        id,
        revision: vault.revision(id)?.clone(),
        workflow_revision: work_unit.data.workflow_revision.clone(),
    })
}

/// Resolves the current deadline and revision for a deadline-capable document.
///
/// The result is derived from one already validated snapshot so adapters can
/// preserve optimistic concurrency while distinguishing an unset deadline
/// from a document kind that does not support deadlines.
pub fn deadline_edit_context(vault: &Vault, id: Id) -> Option<DeadlineEditContext> {
    Some(DeadlineEditContext {
        revision: vault.revision(id)?.clone(),
        deadline: vault.deadline(id)?,
    })
}

/// Resolves a target and applies the shared business-action constraints against
/// an already validated Vault snapshot.
pub fn validate_target_in_snapshot(
    vault: &Vault,
    action: TargetAction,
    id: Id,
    expected_revision: Option<&str>,
) -> Result<TargetSummary> {
    let target =
        target_summary(vault, id).ok_or_else(|| anyhow::anyhow!("target {id} was not found"))?;

    if let TargetAction::TriageInbox { project_id } = action {
        let project = target_summary(vault, project_id);
        ensure!(
            project.is_some_and(|summary| summary.kind == TargetKind::Project),
            "triage Project {project_id} was not found"
        );
    }

    ensure!(
        target_allows_action(action, target.kind),
        "{}",
        invalid_target_message(action, id)
    );

    if expected_revision.is_some_and(|revision| revision != target.revision) {
        return Err(WriteError::StaleRevision { id }.into());
    }
    Ok(target)
}

/// Returns the business-action targets available in an already validated snapshot.
///
/// Candidate selection and final command validation share the same eligibility
/// rules. The caller remains responsible for presentation and revision-bearing writes.
pub fn project_action_targets(vault: &Vault, action: TargetAction) -> Vec<TargetSummary> {
    if matches!(action, TargetAction::TriageInbox { .. }) {
        return Vec::new();
    }
    vault
        .outline()
        .into_iter()
        .filter_map(|row| {
            let kind = match row.depth {
                0 => TargetKind::Project,
                1 => TargetKind::Task,
                2 => TargetKind::WorkUnit,
                _ => return None,
            };
            target_allows_action(action, kind).then(|| TargetSummary {
                id: row.id,
                kind,
                title: row.title.to_owned(),
                revision: revision_digest(vault, row.id),
            })
        })
        .collect()
}

fn target_allows_action(action: TargetAction, kind: TargetKind) -> bool {
    match action {
        TargetAction::CaptureTask => kind == TargetKind::Project,
        TargetAction::TriageInbox { .. } => kind == TargetKind::Inbox,
        TargetAction::CompleteWorkUnit | TargetAction::CreateTimeBlock => {
            kind == TargetKind::WorkUnit
        }
        TargetAction::SetDeadline => {
            matches!(
                kind,
                TargetKind::Project | TargetKind::Task | TargetKind::WorkUnit
            )
        }
        TargetAction::CreateReminder => matches!(kind, TargetKind::Task | TargetKind::WorkUnit),
        TargetAction::EditWorkUnitGraph => kind == TargetKind::Task,
    }
}

fn invalid_target_message(action: TargetAction, id: Id) -> String {
    match action {
        TargetAction::CaptureTask => format!("task capture target {id} must be a Project"),
        TargetAction::TriageInbox { .. } => format!("triage target {id} must be an Inbox entry"),
        TargetAction::CompleteWorkUnit => {
            format!("completion target {id} must be a WorkUnit")
        }
        TargetAction::CreateTimeBlock => {
            format!("time block target {id} must be a WorkUnit")
        }
        TargetAction::SetDeadline => {
            format!("deadline target {id} must be a Project, Task, or WorkUnit")
        }
        TargetAction::CreateReminder => {
            format!("reminder target {id} must be an existing Task or WorkUnit")
        }
        TargetAction::EditWorkUnitGraph => {
            format!("graph edit target {id} must be a Task")
        }
    }
}

/// Projects Inbox documents into the owned application representation.
pub fn project_inbox(vault: &Vault) -> Vec<InboxSummary> {
    vault
        .inbox_entries()
        .map(|entry| InboxSummary {
            id: entry.data.id,
            title: entry.title.clone(),
            captured_at: entry.data.captured_at,
            project_id: entry.data.project_id,
            tags: entry.data.tags.clone(),
            revision: revision_digest(vault, entry.data.id),
        })
        .collect()
}

/// Projects the validated Vault snapshot into the owned hierarchy used by adapters.
pub fn project_outline(vault: &Vault) -> Vec<OutlineItem> {
    let mut project_id = None;
    let mut task_id = None;
    vault
        .outline()
        .into_iter()
        .map(|row| {
            match row.depth {
                0 => {
                    project_id = Some(row.id);
                    task_id = None;
                }
                1 => task_id = Some(row.id),
                _ => {}
            }
            OutlineItem {
                id: row.id,
                depth: row.depth,
                title: row.title.to_owned(),
                source_path: row.path.to_string_lossy().into_owned(),
                project_id,
                task_id,
                blocked: row.blocked,
                revision: revision_digest(vault, row.id),
            }
        })
        .collect()
}

fn work_unit_summary(vault: &Vault, row: OutlineItem) -> WorkUnitSummary {
    WorkUnitSummary {
        id: row.id,
        title: row.title,
        project_id: row.project_id.expect("work units have a parent project"),
        task_id: row.task_id.expect("work units have a parent task"),
        blocked: row.blocked,
        estimated_minutes: vault
            .work_unit(row.id)
            .and_then(|document| document.data.estimated_minutes),
        revision: row.revision,
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct WorkUnitGraph {
    pub nodes: Vec<WorkUnitGraphNode>,
}

/// Project and WorkUnit metadata projected from one snapshot for focus reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusReportContext {
    pub projects: Vec<ProjectSummary>,
    pub work_units: Vec<WorkUnitSummary>,
}

/// Projects the supplied validated snapshot into owned calendar entries.
pub fn project_calendar(vault: &Vault, window: DateSpan) -> Result<Vec<CalendarItem>> {
    Ok(vault
        .calendar_between(window)?
        .into_iter()
        .map(CalendarItem::from)
        .collect())
}

/// Projects non-recurring entries without a date window.
/// Use [`project_calendar`] to include occurrences of recurring events.
pub fn project_calendar_all(vault: &Vault) -> Vec<CalendarItem> {
    vault
        .calendar()
        .into_iter()
        .map(CalendarItem::from)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderSummary {
    pub id: Id,
    pub target_id: Option<Id>,
    pub title: String,
    pub schedule: ReminderSchedule,
}

/// Projects reminder definitions from a validated Vault into owned daemon input.
pub fn project_reminders(vault: &Vault) -> Vec<ReminderSummary> {
    vault
        .reminders()
        .map(|reminder| ReminderSummary {
            id: reminder.data.id,
            target_id: reminder.data.target_id,
            title: reminder.title.clone(),
            schedule: reminder.data.schedule,
        })
        .collect()
}

/// Read-side application boundary for Vault-backed business queries.
///
/// Adapters receive use-case results, not the storage snapshot, so query
/// filtering and revision semantics remain consistent across transports.
pub struct VaultQueries {
    root: PathBuf,
    timezone: Tz,
}

impl VaultQueries {
    pub fn new(root: impl AsRef<Path>, timezone: Tz) -> Self {
        Self {
            root: root.as_ref().to_owned(),
            timezone,
        }
    }

    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    pub fn validate(&self) -> Result<()> {
        self.snapshot().map(|_| ())
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        Ok(self
            .outline()?
            .into_iter()
            .filter(|row| row.depth == 0)
            .map(|row| ProjectSummary {
                id: row.id,
                title: row.title,
                revision: row.revision,
            })
            .collect())
    }

    /// Returns report metadata from one Vault snapshot and validates its optional Project filter.
    pub fn focus_report_context(&self, project_filter: Option<Id>) -> Result<FocusReportContext> {
        let vault = self.snapshot()?;
        let outline = project_outline(&vault);
        let projects = outline
            .iter()
            .filter(|row| row.depth == 0)
            .map(|row| ProjectSummary {
                id: row.id,
                title: row.title.clone(),
                revision: row.revision.clone(),
            })
            .collect::<Vec<_>>();
        if let Some(id) = project_filter {
            ensure!(
                projects.iter().any(|project| project.id == id),
                "no project with ID {id} in the vault"
            );
        }
        let work_units = outline
            .into_iter()
            .filter(|row| row.depth == 2)
            .map(|row| work_unit_summary(&vault, row))
            .collect();
        Ok(FocusReportContext {
            projects,
            work_units,
        })
    }

    /// Returns the opaque revision token required to create a Task in a Project.
    ///
    /// The caller must pass this observed token unchanged to `VaultCommands`;
    /// Vault rechecks it under the writer lock before publishing the Task.
    pub fn project_revision(&self, project_id: Id) -> Result<Option<Revision>> {
        Ok(project_revision_in_snapshot(&self.snapshot()?, project_id))
    }

    pub fn list_tasks(&self, project_filter: Option<Id>) -> Result<Vec<TaskSummary>> {
        Ok(self
            .outline()?
            .into_iter()
            .filter(|row| {
                row.depth == 1 && project_filter.is_none_or(|id| row.project_id == Some(id))
            })
            .map(|row| TaskSummary {
                id: row.id,
                title: row.title,
                project_id: row.project_id.expect("tasks have a parent project"),
                revision: row.revision,
            })
            .collect())
    }

    pub fn list_work_units(
        &self,
        project_filter: Option<Id>,
        task_filter: Option<Id>,
    ) -> Result<Vec<WorkUnitSummary>> {
        let vault = self.snapshot()?;
        Ok(project_outline(&vault)
            .into_iter()
            .filter(|row| {
                row.depth == 2
                    && project_filter.is_none_or(|id| row.project_id == Some(id))
                    && task_filter.is_none_or(|id| row.task_id == Some(id))
            })
            .map(|row| work_unit_summary(&vault, row))
            .collect())
    }

    pub fn contains_work_unit(&self, id: Id) -> Result<bool> {
        Ok(self.snapshot()?.is_work_unit(id))
    }

    pub fn work_unit_completion_context(
        &self,
        id: Id,
    ) -> Result<Option<WorkUnitCompletionContext>> {
        Ok(work_unit_completion_context(&self.snapshot()?, id))
    }

    pub fn outline(&self) -> Result<Vec<OutlineItem>> {
        let vault = self.snapshot()?;
        Ok(project_outline(&vault))
    }

    pub fn list_inbox(&self) -> Result<Vec<InboxSummary>> {
        let vault = self.snapshot()?;
        Ok(project_inbox(&vault))
    }

    /// Resolves the requested business targets from one Vault snapshot.
    ///
    /// Missing IDs are omitted. Callers can validate target kinds without
    /// rebuilding outline and Inbox lookup rules in each adapter.
    pub fn targets(&self, ids: &[Id]) -> Result<std::collections::BTreeMap<Id, TargetSummary>> {
        let vault = self.snapshot()?;
        Ok(ids
            .iter()
            .filter_map(|&id| target_summary(&vault, id).map(|target| (id, target)))
            .collect())
    }

    /// Resolves and validates a target for a business action from one snapshot.
    ///
    /// If an observed revision is supplied, a mismatch is returned as the
    /// Vault's typed stale-revision error so callers can preserve conflict behavior.
    pub fn validate_target(
        &self,
        action: TargetAction,
        id: Id,
        expected_revision: Option<&str>,
    ) -> Result<TargetSummary> {
        let vault = self.snapshot()?;
        validate_target_in_snapshot(&vault, action, id, expected_revision)
    }

    /// Validates an action target and its related revision tokens from one snapshot.
    pub fn validate_revision_set(
        &self,
        action: TargetAction,
        id: Id,
        expected_revision: &str,
        related_expected: &BTreeMap<Id, String>,
    ) -> Result<ValidatedRevisionSet> {
        let vault = self.snapshot()?;
        validate_target_in_snapshot(&vault, action, id, Some(expected_revision))?;
        let target = matching_revision(&vault, id, expected_revision)?;
        let related = related_expected
            .iter()
            .map(|(&related_id, digest)| {
                matching_revision(&vault, related_id, digest).map(|revision| (related_id, revision))
            })
            .collect::<Result<_>>()?;
        Ok(ValidatedRevisionSet { target, related })
    }

    pub fn calendar(&self, window: DateSpan) -> Result<Vec<CalendarItem>> {
        project_calendar(&self.snapshot()?, window)
    }

    pub fn calendar_day(&self, date: NaiveDate) -> Result<Vec<CalendarItem>> {
        self.calendar(DateSpan::new(date, date)?)
    }

    pub fn work_unit_graph_mermaid(&self) -> Result<String> {
        Ok(self.snapshot()?.graph_mermaid())
    }

    pub fn work_unit_graph(&self) -> Result<WorkUnitGraph> {
        Ok(project_work_unit_graph(&self.snapshot()?))
    }

    pub fn preview_work_unit_dependency_change(
        &self,
        target: Id,
        sources: &BTreeSet<Id>,
        operation: DependencyOperation,
    ) -> Result<WorkUnitDependencyPreview> {
        preview_work_unit_dependency_change(&self.snapshot()?, target, sources, operation)
    }

    pub fn preview_work_unit_graph_diff(&self, draft: &WorkUnitGraphDraft) -> Result<GraphDiff> {
        draft.preview(&self.snapshot()?)
    }

    pub fn review_work_unit_graph_diff(
        &self,
        draft: &WorkUnitGraphDraft,
    ) -> Result<WorkUnitGraphReview> {
        let vault = self.snapshot()?;
        let target = validate_target_in_snapshot(
            &vault,
            TargetAction::EditWorkUnitGraph,
            draft.task_id,
            None,
        )?;
        let diff = draft.preview(&vault)?;
        let expected_revisions = std::iter::once(diff.project_id)
            .chain(diff.existing_dependencies())
            .map(|id| (id, revision_digest(&vault, id)))
            .collect();
        Ok(WorkUnitGraphReview {
            target,
            diff,
            expected_revisions,
        })
    }

    fn snapshot(&self) -> Result<Vault> {
        Vault::load(&self.root, self.timezone)
    }
}

/// Builds the owned application projection of the validated Vault DAG.
///
/// This is also used by the TUI when it already holds the current Vault
/// snapshot, avoiding a second disk load while keeping graph semantics shared.
pub fn project_work_unit_graph(vault: &Vault) -> WorkUnitGraph {
    let mut prerequisites = std::collections::BTreeMap::<Id, Vec<Id>>::new();
    let mut dependents = std::collections::BTreeMap::<Id, Vec<Id>>::new();
    for (prerequisite, dependent) in vault.task_graph().edges() {
        dependents.entry(prerequisite).or_default().push(dependent);
        prerequisites
            .entry(dependent)
            .or_default()
            .push(prerequisite);
    }

    WorkUnitGraph {
        nodes: vault
            .task_graph()
            .order()
            .iter()
            .map(|&id| {
                let document = vault.work_unit(id).expect("validated graph unit exists");
                WorkUnitGraphNode {
                    id,
                    title: document.title.clone(),
                    blocked: vault.task_graph().blocked(id),
                    prerequisites: prerequisites.remove(&id).unwrap_or_default(),
                    dependents: dependents.remove(&id).unwrap_or_default(),
                }
            })
            .collect(),
    }
}

/// Previews a dependency change against the caller's current validated snapshot.
///
/// The target revision digest binds the preview to the Markdown being edited.
pub fn preview_work_unit_dependency_change(
    vault: &Vault,
    target: Id,
    sources: &BTreeSet<Id>,
    operation: DependencyOperation,
) -> Result<WorkUnitDependencyPreview> {
    let document = vault
        .work_unit(target)
        .ok_or_else(|| anyhow::anyhow!("focused WorkUnit no longer exists"))?;
    let mut prerequisites = document.data.depends_on.clone();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for &source in sources {
        if source == target {
            continue;
        }
        match operation {
            DependencyOperation::Add if !prerequisites.contains(&source) => {
                prerequisites.push(source);
                added.push(source);
            }
            DependencyOperation::Remove if prerequisites.contains(&source) => {
                prerequisites.retain(|&id| id != source);
                removed.push(source);
            }
            DependencyOperation::Add | DependencyOperation::Remove => {}
        }
    }
    ensure!(
        !added.is_empty() || !removed.is_empty(),
        "the selected WorkUnits do not change this dependency set"
    );

    let graph = TaskGraph::build_with_dependencies(
        vault.work_units().map(|unit| &unit.data),
        target,
        &prerequisites,
    )?;
    let mut newly_blocked = Vec::new();
    let mut unblocked = Vec::new();
    for unit in vault.work_units() {
        let was_blocked = vault.task_graph().blocked(unit.data.id);
        let is_blocked = graph.blocked(unit.data.id);
        match (was_blocked, is_blocked) {
            (false, true) => newly_blocked.push(unit.data.id),
            (true, false) => unblocked.push(unit.data.id),
            _ => {}
        }
    }

    let titles = std::iter::once(&target)
        .chain(&added)
        .chain(&removed)
        .chain(&newly_blocked)
        .chain(&unblocked)
        .filter_map(|&id| vault.work_unit(id).map(|unit| (id, unit.title.clone())))
        .collect();
    Ok(WorkUnitDependencyPreview {
        target,
        prerequisites,
        added,
        removed,
        newly_blocked,
        unblocked,
        revision: revision_digest(vault, target),
        titles,
    })
}

fn revision_digest(vault: &Vault, id: Id) -> String {
    vault
        .revision(id)
        .expect("every projected document has a revision")
        .digest()
}

fn matching_revision(vault: &Vault, id: Id, digest: &str) -> Result<Revision> {
    vault
        .revision(id)
        .filter(|revision| revision.digest() == digest)
        .cloned()
        .ok_or_else(|| WriteError::StaleRevision { id }.into())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use mcbtodo_domain::{
        Availability, CalendarEvent, CalendarKind, EvidenceRef, InboxEntry, Occurrence, Task,
        WorkUnit, WorkUnitStatus,
        recurrence::{CalendarRecurrence, DstPolicy, FoldPolicy, Frequency, GapPolicy},
        time::{DateSpan, Span},
    };
    use mcbtodo_vault::{CaptureInboxEntry, CaptureTask, VaultStore};

    use super::*;

    const UNIT_ID: Id = Id::from_u128(0x3000_0000_0000_4000_8000_0000_0000_0002);
    const PROJECT_ID: Id = Id::from_u128(0x1000_0000_0000_4000_8000_0000_0000_0001);
    const TASK_ID: Id = Id::from_u128(0x2000_0000_0000_4000_8000_0000_0000_0001);
    const CAPTURED_TASK_ID: Id = Id::from_u128(0x2000_0000_0000_4000_8000_0000_0002);

    fn copy_example_vault(root: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        for relative in [
            "projects/rust-os/project.md",
            "projects/rust-os/tasks/memory.md",
            "projects/rust-os/units/read-spec.md",
            "projects/rust-os/units/mapping.md",
            "projects/rust-os/units/translation.md",
        ] {
            let destination = root.join(relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(source.join(relative), destination).unwrap();
        }
    }

    #[test]
    fn recurring_task_materialization_is_idempotent_and_revision_checked() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let expected_revision = vault.revision(PROJECT_ID).unwrap().clone();
        let task = Task {
            id: Id::new_v4(),
            project_id: PROJECT_ID,
            status: Default::default(),
            priority: Default::default(),
            active_period: None,
            deadline: None,
            recurrence: Some(
                CalendarRecurrence::new(
                    NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                    Some(NaiveDate::from_ymd_opt(2026, 11, 28).unwrap()),
                    Frequency::Weekly,
                    std::num::NonZeroU16::MIN,
                    DstPolicy {
                        gap: GapPolicy::Skip,
                        fold: FoldPolicy::Earlier,
                    },
                )
                .unwrap(),
            ),
            occurrence: None,
            tags: Vec::new(),
        };
        let template_id = task.id;
        VaultStore::new(&root, chrono_tz::Asia::Shanghai)
            .capture_task(CaptureTask {
                task,
                title: "Weekly review".into(),
                body: String::new(),
                expected_revision: expected_revision.clone(),
            })
            .unwrap();
        let template_revision = Vault::load(&root, chrono_tz::Asia::Shanghai)
            .unwrap()
            .revision(template_id)
            .unwrap()
            .clone();
        let window = DateSpan::new(
            NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
            NaiveDate::from_ymd_opt(2026, 11, 28).unwrap(),
        )
        .unwrap();
        let commands = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);

        let first = commands
            .materialize_task_occurrences(template_id, window, template_revision.clone())
            .unwrap();
        let second = commands
            .materialize_task_occurrences(template_id, window, template_revision.clone())
            .unwrap();

        assert_eq!(first.len(), 2);
        assert_eq!(second, first);
        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let entries = after
            .calendar_between(window)
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        assert!(first.iter().all(|id| entries.contains(id)));

        VaultStore::new(&root, chrono_tz::Asia::Shanghai)
            .set_deadline(SetDeadline {
                id: template_id,
                deadline: Some(Deadline::Date {
                    value: NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
                }),
                expected_revision: template_revision.clone(),
            })
            .unwrap();
        assert!(matches!(
            commands.materialize_task_occurrences(template_id, window, template_revision),
            Err(WriteError::StaleRevision { id }) if id == template_id
        ));
    }

    #[test]
    fn imported_event_batch_validates_all_text_before_atomic_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        let commands = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);
        let event = || CalendarEvent {
            id: Id::new_v4(),
            when: Span::DateSpan(
                DateSpan::new(
                    NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                    NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                )
                .unwrap(),
            ),
            recurrence: None,
            availability: Availability::Busy,
            tags: Vec::new(),
        };

        assert!(matches!(
            commands.create_events_atomic(vec![
                CalendarEventDraft {
                    event: event(),
                    title: "Valid event".into(),
                    body: String::new(),
                },
                CalendarEventDraft {
                    event: event(),
                    title: " \n".into(),
                    body: String::new(),
                },
            ]),
            Err(WriteError::InvalidDocument(_))
        ));
        assert_eq!(
            Vault::load(&root, chrono_tz::Asia::Shanghai)
                .unwrap()
                .calendar_between(
                    DateSpan::new(
                        NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                        NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                    )
                    .unwrap()
                )
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn task_capture_validates_text_before_vault_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let expected_revision = vault.revision(PROJECT_ID).unwrap().clone();
        let task = Task {
            id: Id::new_v4(),
            project_id: PROJECT_ID,
            status: Default::default(),
            priority: Default::default(),
            active_period: None,
            deadline: None,
            recurrence: None,
            occurrence: None,
            tags: Vec::new(),
        };

        let result = VaultCommands::new(&root, chrono_tz::Asia::Shanghai).capture_task(
            task,
            " \n".into(),
            "body".into(),
            expected_revision,
        );

        assert!(matches!(result, Err(WriteError::InvalidDocument(_))));
        assert_eq!(
            Vault::load(&root, chrono_tz::Asia::Shanghai)
                .unwrap()
                .outline()
                .iter()
                .filter(|row| row.depth == 1)
                .count(),
            1
        );
    }

    #[test]
    fn graph_diff_command_revalidates_reviewed_revisions_and_publishes_as_one_batch() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let unit_id = Id::new_v4();
        let draft = WorkUnitGraphDraft {
            task_id: TASK_ID,
            units: vec![UnitDraft {
                id: unit_id,
                title: "Application-owned graph unit".into(),
                body: "Review the resulting graph.".into(),
                depends_on: vec![UNIT_ID],
                estimated_minutes: Some(20),
            }],
        };
        let diff = draft.preview(&vault).unwrap();
        let related_revisions = diff
            .existing_dependencies()
            .into_iter()
            .chain([diff.project_id])
            .map(|id| (id, vault.revision(id).unwrap().clone()))
            .collect::<BTreeMap<_, _>>();

        let ids = VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .apply_work_unit_graph_diff(
                &draft,
                diff,
                vault.revision(TASK_ID).unwrap().clone(),
                related_revisions,
            )
            .unwrap();

        assert_eq!(ids, [unit_id]);
        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let saved = after.work_unit(unit_id).unwrap();
        assert_eq!(saved.title, "Application-owned graph unit");
        assert_eq!(saved.data.depends_on, [UNIT_ID]);
    }

    fn queries() -> VaultQueries {
        VaultQueries::new(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault"),
            chrono_tz::Asia::Shanghai,
        )
    }

    #[test]
    fn project_revision_returns_an_opaque_token_for_revision_checked_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let queries = VaultQueries::new(&root, chrono_tz::Asia::Shanghai);
        let revision = queries.project_revision(PROJECT_ID).unwrap().unwrap();
        let commands = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);

        let saved = commands
            .capture_task(
                Task {
                    id: Id::new_v4(),
                    project_id: PROJECT_ID,
                    status: Default::default(),
                    priority: Default::default(),
                    active_period: None,
                    deadline: None,
                    recurrence: None,
                    occurrence: None,
                    tags: Vec::new(),
                },
                "Created through shared query and command".into(),
                String::new(),
                revision,
            )
            .unwrap();

        assert!(saved.path.starts_with(root.join("projects/rust-os/tasks")));
        assert!(queries.project_revision(Id::new_v4()).unwrap().is_none());
        assert!(queries.project_revision(TASK_ID).unwrap().is_none());
    }

    #[test]
    fn work_unit_existence_query_distinguishes_units_from_other_objects() {
        let queries = queries();

        assert!(queries.contains_work_unit(UNIT_ID).unwrap());
        assert!(!queries.contains_work_unit(TASK_ID).unwrap());
        assert!(!queries.contains_work_unit(Id::new_v4()).unwrap());
    }

    #[test]
    fn calendar_creation_commands_share_markdown_validation_and_publish() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        let commands = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);
        let project_id = Id::new_v4();
        let project = Project {
            id: project_id,
            status: Default::default(),
            active_period: Some(mcbtodo_domain::time::Span::DateSpan(
                DateSpan::new(
                    NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
                    NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
                )
                .unwrap(),
            )),
            deadline: None,
            tags: Vec::new(),
        };
        let created = commands
            .create_project(
                project,
                Some("architecture-test".into()),
                "Project".into(),
                "Notes".into(),
            )
            .unwrap();
        assert_eq!(created.id, project_id);

        let event_id = Id::new_v4();
        let event = CalendarEvent {
            id: event_id,
            when: mcbtodo_domain::time::Span::DateSpan(
                DateSpan::new(
                    NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                    NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                )
                .unwrap(),
            ),
            recurrence: None,
            availability: Availability::Busy,
            tags: Vec::new(),
        };
        let created_event = commands
            .create_event(event, "Review".into(), String::new())
            .unwrap();
        assert_eq!(created_event.id, event_id);

        assert!(matches!(
            commands.create_project(
                Project {
                    id: Id::new_v4(),
                    status: Default::default(),
                    active_period: None,
                    deadline: None,
                    tags: Vec::new(),
                },
                None,
                " \n".into(),
                String::new(),
            ),
            Err(WriteError::InvalidDocument(_))
        ));
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(vault.outline().len(), 1);
        assert_eq!(vault.calendar().len(), 2);
    }

    #[test]
    fn deadline_command_publishes_and_returns_the_new_revision() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let before = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let revision = before.revision(UNIT_ID).unwrap().clone();
        let deadline = mcbtodo_domain::time::Deadline::Date {
            value: NaiveDate::from_ymd_opt(2026, 12, 10).unwrap(),
        };

        let written = VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .set_deadline(UNIT_ID, Some(deadline), revision)
            .unwrap();

        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(written.id, UNIT_ID);
        assert_eq!(
            written.revision.digest(),
            after.revision(UNIT_ID).unwrap().digest()
        );
        assert_eq!(
            after.work_unit(UNIT_ID).unwrap().data.deadline,
            Some(deadline)
        );

        let revision = after.revision(UNIT_ID).unwrap().clone();
        let existing_blocks = after.work_unit(UNIT_ID).unwrap().data.time_blocks.len();
        let parse = |value| DateTime::parse_from_rfc3339(value).unwrap();
        let block = VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .create_time_block(
                UNIT_ID,
                parse("2026-11-21T09:00:00+08:00"),
                parse("2026-11-21T10:00:00+08:00"),
                Availability::Busy,
                revision.clone(),
            )
            .unwrap();
        assert_eq!(block.id, UNIT_ID);
        assert!(matches!(
            VaultCommands::new(&root, chrono_tz::Asia::Shanghai).create_time_block(
                UNIT_ID,
                parse("2026-11-21T01:00:00+00:00"),
                parse("2026-11-21T02:00:00+00:00"),
                Availability::Busy,
                revision,
            ),
            Err(WriteError::InvalidDocument(_))
        ));
        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(
            after.work_unit(UNIT_ID).unwrap().data.time_blocks.len(),
            existing_blocks + 1
        );
    }

    #[test]
    fn capture_task_command_publishes_under_the_selected_project() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let before = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let expected_revision = before.revision(PROJECT_ID).unwrap().clone();
        let task = Task {
            id: CAPTURED_TASK_ID,
            project_id: PROJECT_ID,
            status: Default::default(),
            priority: Default::default(),
            active_period: None,
            deadline: None,
            recurrence: None,
            occurrence: None,
            tags: Vec::new(),
        };

        let written = VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .capture_task(
                task,
                "Captured through application".into(),
                "Notes".into(),
                expected_revision,
            )
            .unwrap();

        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(written.id, CAPTURED_TASK_ID);
        assert_eq!(
            written.revision.digest(),
            after.revision(CAPTURED_TASK_ID).unwrap().digest()
        );
        let captured = after
            .outline()
            .into_iter()
            .find(|item| item.id == CAPTURED_TASK_ID)
            .unwrap();
        assert_eq!(captured.depth, 1);
        assert_eq!(captured.title, "Captured through application");
        assert!(written.path.is_file());
        assert!(fs::read_to_string(written.path).unwrap().contains("Notes"));
    }

    #[test]
    fn inbox_capture_command_validates_text_and_publishes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        let commands = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);
        let id = Id::new_v4();
        let entry = InboxEntry {
            id,
            captured_at: chrono::Utc::now(),
            project_id: None,
            tags: vec!["idea".into()],
        };

        let written = commands
            .capture_inbox_entry(entry.clone(), "An idea".into(), "Keep this note.".into())
            .unwrap();
        assert_eq!(written.id, id);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let saved = vault.inbox_entries().next().unwrap();
        assert_eq!(saved.title, "An idea");
        assert_eq!(saved.data.tags, ["idea"]);
        assert!(
            fs::read_to_string(written.path)
                .unwrap()
                .contains("Keep this note.")
        );

        assert!(matches!(
            commands.capture_inbox_entry(entry, " \n".into(), String::new()),
            Err(WriteError::InvalidDocument(_))
        ));
        assert_eq!(
            Vault::load(&root, chrono_tz::Asia::Shanghai)
                .unwrap()
                .inbox_entries()
                .count(),
            1
        );
    }

    #[test]
    fn create_work_unit_command_rejects_stale_parent_without_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let stale_revision = vault.revision(TASK_ID).unwrap().clone();
        let store = VaultStore::new(&root, chrono_tz::Asia::Shanghai);
        store
            .set_deadline(SetDeadline {
                id: TASK_ID,
                deadline: Some(Deadline::Date {
                    value: NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
                }),
                expected_revision: stale_revision.clone(),
            })
            .unwrap();
        let unit_id = Id::new_v4();
        let unit = WorkUnit {
            id: unit_id,
            project_id: PROJECT_ID,
            parent_task_id: TASK_ID,
            status: WorkUnitStatus::Todo,
            priority: Default::default(),
            depends_on: Vec::new(),
            estimated_minutes: None,
            deadline: None,
            time_blocks: Vec::new(),
            workflow_revision: None,
            evidence: Vec::new(),
            tags: Vec::new(),
        };

        assert!(matches!(
            VaultCommands::new(&root, chrono_tz::Asia::Shanghai).create_work_unit(
                unit,
                "New unit".into(),
                String::new(),
                stale_revision
            ),
            Err(WriteError::StaleRevision { id }) if id == TASK_ID
        ));
        assert!(
            Vault::load(&root, chrono_tz::Asia::Shanghai)
                .unwrap()
                .work_unit(unit_id)
                .is_none()
        );
    }

    #[test]
    fn complete_work_unit_command_records_completion_event() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let context = work_unit_completion_context(&vault, UNIT_ID).unwrap();
        let evidence = [EvidenceRef {
            run_id: Id::new_v4(),
            step_index: 1,
        }];

        let written = VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .complete_work_unit(context, &evidence, |binding, references| {
                assert!(binding.is_none());
                assert_eq!(references, evidence);
                Ok(())
            })
            .unwrap();

        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(written.id, UNIT_ID);
        assert_eq!(
            after.work_unit(UNIT_ID).unwrap().data.status,
            WorkUnitStatus::Completed
        );
        let events = VaultStore::new(&root, chrono_tz::Asia::Shanghai)
            .work_unit_completed_events()
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].evidence, evidence);
    }

    #[test]
    fn invalid_completion_evidence_does_not_change_the_work_unit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let context = work_unit_completion_context(&vault, UNIT_ID).unwrap();
        let original_status = vault.work_unit(UNIT_ID).unwrap().data.status;

        let result = VaultCommands::new(&root, chrono_tz::Asia::Shanghai).complete_work_unit(
            context,
            &[EvidenceRef {
                run_id: Id::new_v4(),
                step_index: 0,
            }],
            |_, _| anyhow::bail!("run or step was not found"),
        );

        assert!(matches!(
            result,
            Err(CompleteWorkUnitError::InvalidEvidence(_))
        ));
        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(
            after.work_unit(UNIT_ID).unwrap().data.status,
            original_status
        );
    }

    #[test]
    fn work_unit_completion_context_returns_revision_and_workflow_binding() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let before = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let revision = before.revision(UNIT_ID).unwrap().clone();
        drop(before);
        let workflow_revision: WorkflowRevisionHash = "a".repeat(64).parse().unwrap();
        VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .bind_workflow(UNIT_ID, workflow_revision.clone(), revision)
            .unwrap();
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();

        let context = work_unit_completion_context(&vault, UNIT_ID).unwrap();
        assert_eq!(context.revision, *vault.revision(UNIT_ID).unwrap());
        assert_eq!(context.workflow_revision, Some(workflow_revision));
    }

    #[test]
    fn work_unit_completion_context_rejects_non_work_units_and_unknown_ids() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();

        assert!(work_unit_completion_context(&vault, PROJECT_ID).is_none());
        assert!(work_unit_completion_context(&vault, Id::new_v4()).is_none());
    }

    #[test]
    fn deadline_edit_context_distinguishes_unset_from_unsupported_deadlines() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let context = deadline_edit_context(&vault, TASK_ID).unwrap();

        assert_eq!(context.revision, *vault.revision(TASK_ID).unwrap());
        assert_eq!(context.deadline, None);
        assert!(deadline_edit_context(&vault, Id::new_v4()).is_none());
    }

    #[test]
    fn snapshot_revision_projections_distinguish_generic_documents_from_projects() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();

        assert_eq!(
            document_revision_in_snapshot(&vault, PROJECT_ID),
            vault.revision(PROJECT_ID).cloned()
        );
        assert_eq!(
            document_revision_in_snapshot(&vault, TASK_ID),
            vault.revision(TASK_ID).cloned()
        );
        assert!(document_revision_in_snapshot(&vault, Id::new_v4()).is_none());
        assert_eq!(
            project_revision_in_snapshot(&vault, PROJECT_ID),
            vault.revision(PROJECT_ID).cloned()
        );
        assert!(project_revision_in_snapshot(&vault, TASK_ID).is_none());
        assert!(project_revision_in_snapshot(&vault, Id::new_v4()).is_none());
    }

    #[test]
    fn create_reminder_command_publishes_a_valid_reminder() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        let id = Id::new_v4();
        let schedule = ReminderSchedule::Once {
            at: mcbtodo_domain::time::ZonedMoment::new(
                DateTime::parse_from_rfc3339("2026-11-25T09:30:00+08:00").unwrap(),
                chrono_tz::Asia::Shanghai,
            )
            .unwrap(),
        };

        let written = VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .create_reminder(
                id,
                None,
                schedule,
                "Review notes".into(),
                "Check the draft".into(),
            )
            .unwrap();

        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let reminder = vault.reminders().next().unwrap();
        assert_eq!(written.id, id);
        assert_eq!(
            written.revision.digest(),
            vault.revision(id).unwrap().digest()
        );
        assert_eq!(reminder.title, "Review notes");
        assert_eq!(reminder.data.schedule, schedule);
        assert!(
            fs::read_to_string(written.path)
                .unwrap()
                .contains("Check the draft")
        );
    }

    #[test]
    fn create_reminder_rejects_invalid_text_without_publishing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        let id = Id::new_v4();
        let schedule = ReminderSchedule::Once {
            at: mcbtodo_domain::time::ZonedMoment::new(
                DateTime::parse_from_rfc3339("2026-11-25T09:30:00+08:00").unwrap(),
                chrono_tz::Asia::Shanghai,
            )
            .unwrap(),
        };

        let result = VaultCommands::new(&root, chrono_tz::Asia::Shanghai).create_reminder(
            id,
            None,
            schedule,
            " \n".into(),
            String::new(),
        );

        assert!(matches!(result, Err(WriteError::InvalidDocument(_))));
        assert!(
            Vault::load(&root, chrono_tz::Asia::Shanghai)
                .unwrap()
                .reminders()
                .next()
                .is_none()
        );
    }

    #[test]
    fn create_reminder_use_case_publishes_without_owning_workspace_policy() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("external");
        fs::create_dir_all(&root).unwrap();
        let schedule = ReminderSchedule::Once {
            at: mcbtodo_domain::time::ZonedMoment::new(
                DateTime::parse_from_rfc3339("2026-11-25T09:30:00+08:00").unwrap(),
                chrono_tz::Asia::Shanghai,
            )
            .unwrap(),
        };
        let command = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);

        let external = command.create_reminder(
            Id::new_v4(),
            None,
            schedule,
            "Review notes".into(),
            String::new(),
        );
        let missing = command.create_reminder(
            Id::new_v4(),
            None,
            schedule,
            "Review notes".into(),
            String::new(),
        );

        assert!(external.is_ok());
        assert!(missing.is_ok());
        assert_eq!(
            Vault::load(&root, chrono_tz::Asia::Shanghai)
                .unwrap()
                .reminders()
                .count(),
            2
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_reminder_writes_through_a_symlinked_vault_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let alias = temp.path().join("vault-alias");
        fs::create_dir_all(&root).unwrap();
        symlink(&root, &alias).unwrap();
        let schedule = ReminderSchedule::Once {
            at: mcbtodo_domain::time::ZonedMoment::new(
                DateTime::parse_from_rfc3339("2026-11-25T09:30:00+08:00").unwrap(),
                chrono_tz::Asia::Shanghai,
            )
            .unwrap(),
        };

        VaultCommands::new(&alias, chrono_tz::Asia::Shanghai)
            .create_reminder(
                Id::new_v4(),
                None,
                schedule,
                "Review notes".into(),
                String::new(),
            )
            .unwrap();

        assert_eq!(
            Vault::load(&root, chrono_tz::Asia::Shanghai)
                .unwrap()
                .reminders()
                .count(),
            1
        );
    }

    #[test]
    fn dependency_command_publishes_valid_graph_edits_and_rejects_invalid_edits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let before = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let revision = before.revision(UNIT_ID).unwrap().clone();
        let commands = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);

        let written = commands
            .set_work_unit_dependencies(UNIT_ID, Vec::new(), revision.clone())
            .unwrap();
        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(written.id, UNIT_ID);
        assert!(after.work_unit(UNIT_ID).unwrap().data.depends_on.is_empty());
        assert_eq!(
            written.revision.digest(),
            after.revision(UNIT_ID).unwrap().digest()
        );

        let source = fs::read(&written.path).unwrap();
        let current_revision = after.revision(UNIT_ID).unwrap().clone();
        assert!(matches!(
            commands.set_work_unit_dependencies(UNIT_ID, vec![UNIT_ID], current_revision),
            Err(WriteError::InvalidDocument(_))
        ));
        assert_eq!(fs::read(&written.path).unwrap(), source);
        assert!(matches!(
            commands.set_work_unit_dependencies(UNIT_ID, Vec::new(), revision),
            Err(WriteError::StaleRevision { id: UNIT_ID })
        ));
        assert_eq!(fs::read(&written.path).unwrap(), source);
    }

    #[test]
    fn triage_command_preserves_content_and_rejects_invalid_target_or_stale_revision() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let inbox_id = Id::new_v4();
        let captured = VaultStore::new(&root, chrono_tz::Asia::Shanghai)
            .capture_inbox_entry(CaptureInboxEntry {
                entry: InboxEntry {
                    id: inbox_id,
                    captured_at: chrono::Utc::now(),
                    project_id: None,
                    tags: vec!["review".into()],
                },
                title: "Captured for triage".into(),
                body: "Keep these notes intact.".into(),
            })
            .unwrap();
        let before = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let revision = before.revision(inbox_id).unwrap().clone();
        let commands = VaultCommands::new(&root, chrono_tz::Asia::Shanghai);

        let written = commands
            .triage_inbox_entry(inbox_id, PROJECT_ID, revision.clone())
            .unwrap();
        let after = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let entry = after
            .inbox_entries()
            .find(|entry| entry.data.id == inbox_id)
            .unwrap();
        assert_eq!(entry.data.project_id, Some(PROJECT_ID));
        assert_eq!(entry.title, "Captured for triage");
        let task_id = entry.data.task_id();
        assert!(after.outline().iter().any(|row| row.id == task_id));
        assert_eq!(written.id, inbox_id);
        assert_eq!(
            written.revision.digest(),
            after.revision(inbox_id).unwrap().digest()
        );
        let source = fs::read(&captured.path).unwrap();
        assert!(matches!(
            commands.triage_inbox_entry(inbox_id, Id::new_v4(), written.revision.clone()),
            Err(WriteError::ProjectNotFound { .. })
        ));
        assert!(matches!(
            commands.triage_inbox_entry(inbox_id, PROJECT_ID, revision),
            Err(WriteError::StaleRevision { id }) if id == inbox_id
        ));
        assert_eq!(fs::read(&captured.path).unwrap(), source);
        assert!(
            fs::read_to_string(captured.path)
                .unwrap()
                .contains("Keep these notes intact.")
        );
    }

    #[test]
    fn outline_summaries_keep_parentage_revisions_and_blocked_state() {
        let queries = queries();
        let outline = queries.outline().unwrap();
        assert_eq!(outline.first().unwrap().id, PROJECT_ID);
        let blocked = outline
            .iter()
            .find(|item| item.depth == 2 && item.blocked)
            .unwrap();
        assert_eq!(blocked.project_id, Some(PROJECT_ID));
        assert_eq!(blocked.task_id, Some(TASK_ID));
        assert_eq!(blocked.revision.len(), 64);

        assert_eq!(queries.list_projects().unwrap()[0].id, PROJECT_ID);
        assert!(
            queries
                .list_tasks(Some(PROJECT_ID))
                .unwrap()
                .iter()
                .any(|task| task.id == TASK_ID)
        );
        assert_eq!(
            queries
                .list_work_units(Some(PROJECT_ID), Some(TASK_ID))
                .unwrap()
                .len(),
            3
        );

        let context = queries.focus_report_context(Some(PROJECT_ID)).unwrap();
        assert_eq!(context.projects.len(), 1);
        assert_eq!(context.projects[0].id, PROJECT_ID);
        assert_eq!(context.work_units.len(), 3);
        assert!(
            queries
                .focus_report_context(Some(Id::new_v4()))
                .unwrap_err()
                .to_string()
                .contains("no project with ID")
        );
    }

    #[test]
    fn target_queries_return_owned_kind_title_revision_and_omit_unknown_ids() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let inbox_id = Id::new_v4();
        VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .capture_inbox_entry(
                InboxEntry {
                    id: inbox_id,
                    captured_at: chrono::Utc::now(),
                    project_id: None,
                    tags: Vec::new(),
                },
                "Loose note".into(),
                "Captured for later.".into(),
            )
            .unwrap();

        let unknown_id = Id::new_v4();
        let targets = VaultQueries::new(&root, chrono_tz::Asia::Shanghai)
            .targets(&[PROJECT_ID, TASK_ID, UNIT_ID, inbox_id, unknown_id])
            .unwrap();
        assert_eq!(targets.len(), 4);
        assert_eq!(targets[&PROJECT_ID].kind, TargetKind::Project);
        assert_eq!(targets[&TASK_ID].kind, TargetKind::Task);
        assert_eq!(targets[&UNIT_ID].kind, TargetKind::WorkUnit);
        assert_eq!(targets[&inbox_id].kind, TargetKind::Inbox);
        assert_eq!(targets[&inbox_id].title, "Loose note");
        assert_eq!(targets[&inbox_id].revision.len(), 64);
        assert!(!targets.contains_key(&unknown_id));
    }

    #[test]
    fn action_target_validation_owns_kind_relationship_and_revision_rules() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let inbox_id = Id::new_v4();
        VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .capture_inbox_entry(
                InboxEntry {
                    id: inbox_id,
                    captured_at: chrono::Utc::now(),
                    project_id: None,
                    tags: Vec::new(),
                },
                "Loose note".into(),
                "Captured for later.".into(),
            )
            .unwrap();

        let queries = VaultQueries::new(&root, chrono_tz::Asia::Shanghai);
        let project = queries
            .validate_target(TargetAction::CaptureTask, PROJECT_ID, None)
            .unwrap();
        assert_eq!(project.kind, TargetKind::Project);
        assert!(
            queries
                .validate_target(TargetAction::CaptureTask, TASK_ID, None)
                .is_err()
        );

        let inbox_revision = queries.targets(&[inbox_id]).unwrap()[&inbox_id]
            .revision
            .clone();
        let inbox = queries
            .validate_target(
                TargetAction::TriageInbox {
                    project_id: PROJECT_ID,
                },
                inbox_id,
                Some(&inbox_revision),
            )
            .unwrap();
        assert_eq!(inbox.kind, TargetKind::Inbox);
        assert!(
            queries
                .validate_target(
                    TargetAction::TriageInbox {
                        project_id: TASK_ID,
                    },
                    inbox_id,
                    None,
                )
                .is_err()
        );

        let unit = queries
            .validate_target(TargetAction::CompleteWorkUnit, UNIT_ID, None)
            .unwrap();
        assert_eq!(unit.kind, TargetKind::WorkUnit);
        assert!(
            queries
                .validate_target(TargetAction::CreateReminder, PROJECT_ID, None)
                .is_err()
        );
        assert!(
            queries
                .validate_target(TargetAction::SetDeadline, inbox_id, None)
                .is_err()
        );
        assert!(
            queries
                .validate_target(TargetAction::EditWorkUnitGraph, UNIT_ID, None)
                .is_err()
        );

        let stale = queries
            .validate_target(
                TargetAction::CreateTimeBlock,
                UNIT_ID,
                Some("0".repeat(64).as_str()),
            )
            .unwrap_err();
        assert!(matches!(
            stale.downcast_ref::<WriteError>(),
            Some(WriteError::StaleRevision { id }) if *id == UNIT_ID
        ));
    }

    #[test]
    fn revision_set_validation_uses_action_rules_and_one_snapshot() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        let queries = VaultQueries::new(&root, chrono_tz::Asia::Shanghai);
        let targets = queries.targets(&[UNIT_ID, PROJECT_ID]).unwrap();
        let related_expected =
            BTreeMap::from([(PROJECT_ID, targets[&PROJECT_ID].revision.clone())]);

        let revisions = queries
            .validate_revision_set(
                TargetAction::CreateTimeBlock,
                UNIT_ID,
                &targets[&UNIT_ID].revision,
                &related_expected,
            )
            .unwrap();

        assert_eq!(revisions.target.digest(), targets[&UNIT_ID].revision);
        assert_eq!(
            revisions.related[&PROJECT_ID].digest(),
            targets[&PROJECT_ID].revision
        );

        let stale_related = BTreeMap::from([(PROJECT_ID, "0".repeat(64))]);
        let error = queries
            .validate_revision_set(
                TargetAction::CreateTimeBlock,
                UNIT_ID,
                &targets[&UNIT_ID].revision,
                &stale_related,
            )
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WriteError>(),
            Some(WriteError::StaleRevision { id }) if *id == PROJECT_ID
        ));

        let error = queries
            .validate_revision_set(
                TargetAction::CreateReminder,
                PROJECT_ID,
                &targets[&PROJECT_ID].revision,
                &BTreeMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("reminder target"));
    }

    #[test]
    fn outline_projection_is_owned_and_matches_the_query_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();

        let projected = project_outline(&vault);
        let queried = VaultQueries::new(&root, chrono_tz::Asia::Shanghai)
            .outline()
            .unwrap();
        assert_eq!(projected, queried);

        drop(vault);
        let work_unit = projected.iter().find(|item| item.depth == 2).unwrap();
        assert_eq!(work_unit.project_id, Some(PROJECT_ID));
        assert_eq!(work_unit.task_id, Some(TASK_ID));
        assert_eq!(work_unit.revision.len(), 64);
    }

    #[test]
    fn action_target_projection_matches_shared_action_constraints() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();

        let reminders = project_action_targets(&vault, TargetAction::CreateReminder);
        assert!(reminders.iter().any(|target| target.id == TASK_ID));
        assert!(reminders.iter().any(|target| target.id == UNIT_ID));
        assert!(reminders.iter().all(|target| {
            matches!(target.kind, TargetKind::Task | TargetKind::WorkUnit)
                && target.revision == vault.revision(target.id).unwrap().digest()
        }));
        assert!(!reminders.iter().any(|target| target.id == PROJECT_ID));

        let time_blocks = project_action_targets(&vault, TargetAction::CreateTimeBlock);
        assert!(!time_blocks.is_empty());
        assert!(time_blocks.iter().all(|target| {
            target.kind == TargetKind::WorkUnit
                && validate_target_in_snapshot(
                    &vault,
                    TargetAction::CreateTimeBlock,
                    target.id,
                    None,
                )
                .is_ok()
        }));
    }

    #[test]
    fn inbox_projection_is_owned_and_preserves_triage_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_example_vault(&root);
        let id = Id::new_v4();
        VaultCommands::new(&root, chrono_tz::Asia::Shanghai)
            .capture_inbox_entry(
                InboxEntry {
                    id,
                    captured_at: chrono::Utc::now(),
                    project_id: None,
                    tags: vec!["idea".into()],
                },
                "Unsorted".into(),
                "Preserve these notes.".into(),
            )
            .unwrap();
        let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();

        let projected = project_inbox(&vault);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].id, id);
        assert_eq!(projected[0].title, "Unsorted");
        assert_eq!(projected[0].project_id, None);
        assert_eq!(projected[0].tags, ["idea"]);
        assert_eq!(projected[0].revision.len(), 64);

        drop(vault);
        assert_eq!(projected[0].title, "Unsorted");
    }

    #[test]
    fn calendar_projection_is_owned_and_uses_display_timezone() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        let timezone = chrono_tz::Asia::Shanghai;
        let date = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let window = DateSpan::new(date, date).unwrap();
        let vault = Vault::load(&root, timezone).unwrap();

        let projected = project_calendar(&vault, window).unwrap();
        let all = project_calendar_all(&vault);
        let queried = VaultQueries::new(&root, timezone)
            .calendar_day(date)
            .unwrap();
        assert_eq!(
            projected
                .iter()
                .map(|item| (item.id, item.title.as_str(), item.kind, item.busy))
                .collect::<Vec<_>>(),
            queried
                .iter()
                .map(|item| (item.id, item.title.as_str(), item.kind, item.busy))
                .collect::<Vec<_>>()
        );
        assert!(!all.is_empty());
        assert!(projected.iter().any(|entry| {
            entry.kind == CalendarKind::Event && entry.title == "校园活动" && entry.busy
        }));
        assert!(projected.iter().any(|entry| {
            entry.kind == CalendarKind::Event && entry.title == "远程讨论" && !entry.busy
        }));

        drop(vault);
        let event = projected
            .iter()
            .find(|entry| entry.title == "校园活动")
            .unwrap();
        assert!(event.on_day(date, timezone));
    }

    #[test]
    fn serialized_vault_calendar_projects_business_objects_across_display_zones() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
        for relative in [
            "projects/rust-os/project.md",
            "projects/rust-os/tasks/memory.md",
            "projects/rust-os/units/read-spec.md",
            "projects/rust-os/units/mapping.md",
            "calendar/review.md",
            "calendar/university.md",
        ] {
            let destination = root.join(relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(examples.join(relative), destination).unwrap();
        }
        fs::write(
            root.join("projects/rust-os/tasks/deadline.md"),
            "---\nschema: mcbtodo.task/v1\n\
             id: 20000000-0000-4000-8000-000000000002\n\
             project_id: 10000000-0000-4000-8000-000000000001\n\
             status: todo\n\
             priority: normal\n\
             active_period: null\n\
             deadline: {kind: date, value: \"2026-11-25\"}\n\
             ---\n\n# Boundary task\n",
        )
        .unwrap();
        let timed_task_id = Id::parse_str("20000000-0000-4000-8000-000000000003").unwrap();
        fs::write(
            root.join("projects/rust-os/tasks/timed-deadline.md"),
            format!(
                "---\nschema: mcbtodo.task/v1\n\
                 id: {timed_task_id}\n\
                 project_id: {PROJECT_ID}\n\
                 status: todo\n\
                 priority: normal\n\
                 active_period: null\n\
                 deadline: {{kind: date_time, value: \"2026-11-25T09:15:00+09:00\", timezone: Asia/Tokyo}}\n\
                 ---\n\n# Timed boundary task\n"
            ),
        )
        .unwrap();
        for directory in ["reminders", "inbox"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }

        let shanghai = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
        let entries = project_calendar_all(&shanghai);
        let project_id = Id::parse_str("10000000-0000-4000-8000-000000000001").unwrap();
        let task_id = Id::parse_str("20000000-0000-4000-8000-000000000002").unwrap();
        let unit_id = Id::parse_str("30000000-0000-4000-8000-000000000002").unwrap();
        let event_id = Id::parse_str("40000000-0000-4000-8000-000000000002").unwrap();
        let all_day_event_id = Id::parse_str("40000000-0000-4000-8000-000000000001").unwrap();

        assert!(entries.iter().any(|entry| {
            entry.id == project_id
                && entry.kind == CalendarKind::ActivePeriod
                && entry.title == "Rust OS"
        }));
        assert!(
            entries
                .iter()
                .any(|entry| { entry.id == project_id && entry.kind == CalendarKind::Deadline })
        );
        assert!(
            entries
                .iter()
                .any(|entry| { entry.id == task_id && entry.kind == CalendarKind::Deadline })
        );
        assert!(
            entries
                .iter()
                .any(|entry| { entry.id == unit_id && entry.kind == CalendarKind::Deadline })
        );
        assert!(entries.iter().any(|entry| {
            entry.id == unit_id && entry.kind == CalendarKind::TimeBlock && entry.busy
        }));
        assert!(entries.iter().any(|entry| {
            entry.title == "校园活动" && entry.kind == CalendarKind::Event && entry.busy
        }));

        let shanghai_window = DateSpan::new(
            NaiveDate::from_ymd_opt(2026, 11, 25).unwrap(),
            NaiveDate::from_ymd_opt(2026, 11, 25).unwrap(),
        )
        .unwrap();
        let shanghai_day = project_calendar(&shanghai, shanghai_window).unwrap();
        let timed_deadline = shanghai_day
            .iter()
            .find(|entry| entry.id == timed_task_id)
            .unwrap();
        assert_eq!(timed_deadline.kind, CalendarKind::Deadline);
        let Occurrence::Deadline(Deadline::DateTime(moment)) = timed_deadline.occurrence else {
            panic!("expected a timed deadline");
        };
        assert_eq!(moment.instant().to_rfc3339(), "2026-11-25T00:15:00+00:00");
        assert!(shanghai_day.iter().any(|entry| {
            entry.id == event_id
                && entry.title == "远程讨论"
                && entry.on_day(
                    NaiveDate::from_ymd_opt(2026, 11, 25).unwrap(),
                    chrono_tz::Asia::Shanghai,
                )
        }));
        let los_angeles = Vault::load(&root, chrono_tz::America::Los_Angeles).unwrap();
        let los_angeles_day = project_calendar(&los_angeles, shanghai_window).unwrap();
        assert!(
            los_angeles_day
                .iter()
                .any(|entry| entry.id == all_day_event_id)
        );
        assert!(los_angeles_day.iter().any(|entry| entry.id == task_id));
        assert!(!los_angeles_day.iter().any(|entry| entry.id == event_id));
        assert!(
            !los_angeles_day
                .iter()
                .any(|entry| entry.id == timed_task_id)
        );
        let previous_day = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        let los_angeles_day = project_calendar(
            &los_angeles,
            DateSpan::new(previous_day, previous_day).unwrap(),
        )
        .unwrap();
        assert!(los_angeles_day.iter().any(|entry| {
            entry.id == event_id && entry.on_day(previous_day, chrono_tz::America::Los_Angeles)
        }));
        assert!(los_angeles_day.iter().any(|entry| {
            entry.id == timed_task_id && entry.occurrence == timed_deadline.occurrence
        }));
    }

    #[test]
    fn serialized_calendar_preserves_the_dst_fold_and_rejects_a_gap_endpoint() {
        let root = tempfile::tempdir().unwrap();
        let calendar = root.path().join("calendar");
        fs::create_dir(&calendar).unwrap();
        let path = calendar.join("transition.md");
        let write_event = |start: &str, end: &str| {
            fs::write(
                &path,
                format!(
                    "---\nschema: mcbtodo.calendar-event/v1\n\
                     id: 40000000-0000-4000-8000-000000000001\n\
                     when: {{kind: date_time_span, start: \"{start}\", end: \"{end}\", timezone: America/New_York}}\n\
                     availability: busy\n\
                     ---\n\n# DST transition\n"
                ),
            )
            .unwrap();
        };
        let day = "2026-11-01".parse().unwrap();
        let queries = VaultQueries::new(root.path(), chrono_tz::America::New_York);

        write_event("2026-11-01T01:50:00-04:00", "2026-11-01T01:10:00-05:00");
        let entries = queries.calendar_day(day).unwrap();
        let [event] = entries.as_slice() else {
            panic!("expected one event across the repeated hour");
        };
        let Occurrence::Span(Span::DateTimeSpan(span)) = event.occurrence else {
            panic!("expected a zoned event");
        };
        assert!(event.busy);
        assert_eq!(span.start().to_rfc3339(), "2026-11-01T01:50:00-04:00");
        assert_eq!(span.end().to_rfc3339(), "2026-11-01T01:10:00-05:00");
        assert_eq!((span.end() - span.start()).num_minutes(), 20);

        write_event("2026-03-08T02:30:00-05:00", "2026-03-08T04:00:00-04:00");
        let error = queries
            .calendar_day("2026-03-08".parse().unwrap())
            .unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("transition.md"), "{diagnostic}");
        assert!(
            diagnostic.contains("does not match America/New_York"),
            "{diagnostic}"
        );
    }

    #[test]
    fn serialized_timed_recurrence_uses_anchor_zone_and_elapsed_duration() {
        let root = tempfile::tempdir().unwrap();
        let calendar = root.path().join("calendar");
        fs::create_dir(&calendar).unwrap();
        let path = calendar.join("daily.md");
        let write_event = |gap: &str| {
            fs::write(
                &path,
                format!(
                    "---\nschema: mcbtodo.calendar-event/v1\n\
                     id: 40000000-0000-4000-8000-000000000001\n\
                     when: {{kind: date_time_span, start: '2026-03-07T02:30:00-05:00', end: '2026-03-07T04:00:00-05:00', timezone: America/New_York}}\n\
                     recurrence: {{start: 2026-03-07, through: 2026-03-08, frequency: daily, dst: {{gap: {gap}, fold: earlier}} }}\n\
                     ---\n\n# 日历 Daily\n"
                ),
            )
            .unwrap();
        };
        let queries = VaultQueries::new(root.path(), chrono_tz::America::Los_Angeles);
        let date = |value: &str| value.parse::<NaiveDate>().unwrap();
        let window = DateSpan::new(date("2026-03-08"), date("2026-03-08")).unwrap();

        write_event("skip");
        assert!(
            project_calendar(&queries.snapshot().unwrap(), window)
                .unwrap()
                .is_empty()
        );
        write_event("next_valid");
        let snapshot = queries.snapshot().unwrap();
        let entries = project_calendar(&snapshot, window).unwrap();
        let [event] = entries.as_slice() else {
            panic!("expected exactly one shifted occurrence, got {entries:?}");
        };
        let Occurrence::Span(Span::DateTimeSpan(span)) = event.occurrence else {
            panic!("expected a timed occurrence");
        };
        assert_eq!(span.start().to_rfc3339(), "2026-03-08T03:00:00-04:00");
        assert_eq!(span.end().to_rfc3339(), "2026-03-08T04:30:00-04:00");
        assert!(event.on_day(date("2026-03-07"), chrono_tz::America::Los_Angeles));
        assert!(event.on_day(date("2026-03-08"), chrono_tz::America::Los_Angeles));
        assert_eq!(snapshot.source(event.id), Some(path.as_path()));
    }

    #[test]
    fn serialized_timed_recurrence_projects_the_selected_fold_once() {
        let root = tempfile::tempdir().unwrap();
        let calendar = root.path().join("calendar");
        fs::create_dir(&calendar).unwrap();
        let path = calendar.join("weekly.md");
        let queries = VaultQueries::new(root.path(), chrono_tz::America::New_York);
        let day = "2026-11-01".parse().unwrap();
        for (fold, expected) in [
            ("earlier", "2026-11-01T01:50:00-04:00"),
            ("later", "2026-11-01T01:50:00-05:00"),
        ] {
            fs::write(
                &path,
                format!(
                    "---\nschema: mcbtodo.calendar-event/v1\n\
                     id: 40000000-0000-4000-8000-000000000001\n\
                     when: {{kind: date_time_span, start: '2026-10-25T01:50:00-04:00', end: '2026-10-25T02:10:00-04:00', timezone: America/New_York}}\n\
                     recurrence: {{start: 2026-10-25, through: 2026-11-01, frequency: weekly, dst: {{gap: skip, fold: {fold} }} }}\n\
                     ---\n\n# Weekly DST transition\n"
                ),
            )
            .unwrap();
            let entries = queries.calendar_day(day).unwrap();
            let [event] = entries.as_slice() else {
                panic!("expected one occurrence for {fold}, got {entries:?}");
            };
            let Occurrence::Span(Span::DateTimeSpan(span)) = event.occurrence else {
                panic!("expected a timed occurrence");
            };
            assert_eq!(span.start().to_rfc3339(), expected);
            assert_eq!((span.end() - span.start()).num_minutes(), 20);
        }
    }

    #[test]
    fn validation_and_empty_inbox_are_queries_without_transport_state() {
        let queries = queries();
        queries.validate().unwrap();
        assert!(queries.list_inbox().unwrap().is_empty());
    }

    #[test]
    fn work_unit_graph_query_returns_owned_topological_nodes_and_typed_edges() {
        let graph = queries().work_unit_graph().unwrap();
        let read_spec = Id::parse_str("30000000-0000-4000-8000-000000000001").unwrap();
        let mapping = Id::parse_str("30000000-0000-4000-8000-000000000002").unwrap();
        let translation = Id::parse_str("30000000-0000-4000-8000-000000000003").unwrap();
        let ids: Vec<_> = graph.nodes.iter().map(|node| node.id).collect();

        assert_eq!(ids, [read_spec, mapping, translation]);
        assert_eq!(graph.nodes[0].title, "阅读 Sv39 规范");
        assert!(!graph.nodes[1].blocked);
        assert!(graph.nodes[2].blocked);
        assert_eq!(graph.nodes[1].prerequisites, [read_spec]);
        assert_eq!(graph.nodes[0].dependents, [mapping]);
        assert_eq!(graph.nodes[2].prerequisites, [mapping]);
    }

    #[test]
    fn dependency_preview_reports_readiness_and_rejects_cycles_without_writes() {
        let queries = queries();
        let mapping = Id::parse_str("30000000-0000-4000-8000-000000000002").unwrap();
        let translation = Id::parse_str("30000000-0000-4000-8000-000000000003").unwrap();
        let preview = queries
            .preview_work_unit_dependency_change(
                translation,
                &BTreeSet::from([mapping]),
                DependencyOperation::Remove,
            )
            .unwrap();

        assert_eq!(preview.target, translation);
        assert_eq!(preview.removed, [mapping]);
        assert!(preview.added.is_empty());
        assert_eq!(preview.unblocked, [translation]);
        assert!(preview.newly_blocked.is_empty());
        assert!(preview.prerequisites.is_empty());
        assert_eq!(
            preview.revision,
            queries
                .snapshot()
                .unwrap()
                .revision(translation)
                .unwrap()
                .digest()
        );

        let read_spec = Id::parse_str("30000000-0000-4000-8000-000000000001").unwrap();
        assert!(
            queries
                .preview_work_unit_dependency_change(
                    read_spec,
                    &BTreeSet::from([translation]),
                    DependencyOperation::Add,
                )
                .is_err()
        );
    }

    #[test]
    fn graph_diff_uses_the_vault_dag_and_returns_reviewable_edges_and_ready_units() {
        let queries = queries();
        let vault = queries.snapshot().unwrap();
        let task_id = Id::parse_str("20000000-0000-4000-8000-000000000001").unwrap();
        let prerequisite = Id::parse_str("30000000-0000-4000-8000-000000000002").unwrap();
        let dependent = Id::parse_str("50000000-0000-4000-8000-000000000001").unwrap();
        let ready = Id::parse_str("50000000-0000-4000-8000-000000000002").unwrap();
        let draft = WorkUnitGraphDraft {
            task_id,
            units: vec![
                UnitDraft {
                    id: dependent,
                    title: "Finish dependent work".into(),
                    body: String::new(),
                    depends_on: vec![prerequisite],
                    estimated_minutes: Some(30),
                },
                UnitDraft {
                    id: ready,
                    title: "Independent work".into(),
                    body: String::new(),
                    depends_on: Vec::new(),
                    estimated_minutes: None,
                },
            ],
        };

        let diff = draft.preview(&vault).unwrap();
        diff.validate_against(&draft).unwrap();
        assert_eq!(diff.task_id, task_id);
        assert_eq!(diff.added_edges.len(), 1);
        assert_eq!(diff.added_edges[0].prerequisite, prerequisite);
        assert_eq!(diff.added_edges[0].dependent, dependent);
        assert_eq!(diff.ready, vec![ready]);
        assert_eq!(
            diff.existing_dependencies().into_iter().collect::<Vec<_>>(),
            vec![prerequisite]
        );
    }

    #[test]
    fn graph_diff_review_returns_target_diff_and_related_revisions_together() {
        let queries = queries();
        let task_id = Id::parse_str("20000000-0000-4000-8000-000000000001").unwrap();
        let prerequisite = Id::parse_str("30000000-0000-4000-8000-000000000002").unwrap();
        let added = Id::parse_str("50000000-0000-4000-8000-000000000009").unwrap();
        let draft = WorkUnitGraphDraft {
            task_id,
            units: vec![UnitDraft {
                id: added,
                title: "Reviewed work".into(),
                body: String::new(),
                depends_on: vec![prerequisite],
                estimated_minutes: None,
            }],
        };

        let review = queries.review_work_unit_graph_diff(&draft).unwrap();
        let vault = queries.snapshot().unwrap();

        assert_eq!(review.target.id, task_id);
        assert_eq!(review.target.kind, TargetKind::Task);
        assert_eq!(review.diff.task_id, task_id);
        assert_eq!(review.diff.added_nodes[0].id, added);
        assert_eq!(
            review
                .expected_revisions
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [review.diff.project_id, prerequisite]
        );
        for (id, revision) in &review.expected_revisions {
            assert_eq!(revision, &vault.revision(*id).unwrap().digest());
        }
        assert_eq!(
            review.target.revision,
            vault.revision(task_id).unwrap().digest()
        );
    }
}
