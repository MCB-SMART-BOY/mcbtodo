//! Private proposals from MCP with review-bound commits.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::{
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, FixedOffset, NaiveDate};
use chrono_tz::Tz;
use mcbtodo_application::{
    TargetAction, TargetSummary, VaultCommands, VaultQueries, WorkUnitGraphReview,
    WorkflowUseCases, graph_diff, validate_markdown,
};
use mcbtodo_domain::{
    Availability, Id, Task,
    recurrence::ReminderSchedule,
    time::{DateTimeSpan, Deadline},
};
use mcbtodo_platform::WorkspacePaths;
#[cfg(all(test, unix))]
use mcbtodo_vault::Vault;
#[cfg(all(test, unix))]
use mcbtodo_vault::VaultStore;
use mcbtodo_vault::{Revision, WriteError};
#[cfg(all(test, unix))]
use mcbtodo_workflow::RevisionStore;
use mcbtodo_workflow::WorkflowDraft;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub const QUEUE_SUPPORTED: bool = cfg!(unix);
const VERSION: u8 = 1;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    CaptureTask {
        project_id: Id,
        title: String,
        #[serde(default)]
        body: String,
    },
    TriageInbox {
        inbox_id: Id,
        project_id: Id,
        expected_revision: String,
    },
    CompleteWorkUnit {
        work_unit_id: Id,
    },
    CreateTimeBlock {
        work_unit_id: Id,
        expected_revision: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        #[serde(default)]
        availability: Availability,
    },
    SetDeadline {
        target_id: Id,
        expected_revision: String,
        date: NaiveDate,
    },
    ClearDeadline {
        target_id: Id,
        expected_revision: String,
    },
    CreateReminder {
        target_id: Id,
        expected_revision: String,
        schedule: ReminderSchedule,
        title: String,
        #[serde(default)]
        body: String,
    },
    WorkUnitGraphDiff(graph_diff::Draft),
    PublishWorkflow {
        draft: WorkflowDraft,
    },
}

impl Operation {
    fn target(&self) -> Id {
        match self {
            Self::CaptureTask { project_id, .. } => *project_id,
            Self::TriageInbox { inbox_id, .. } => *inbox_id,
            Self::CompleteWorkUnit { work_unit_id }
            | Self::CreateTimeBlock { work_unit_id, .. } => *work_unit_id,
            Self::SetDeadline { target_id, .. }
            | Self::ClearDeadline { target_id, .. }
            | Self::CreateReminder { target_id, .. } => *target_id,
            Self::WorkUnitGraphDiff(draft) => draft.task_id,
            Self::PublishWorkflow { .. } => Id::nil(),
        }
    }

    fn expected_revision(&self) -> Option<&str> {
        match self {
            Self::TriageInbox {
                expected_revision, ..
            }
            | Self::CreateTimeBlock {
                expected_revision, ..
            }
            | Self::SetDeadline {
                expected_revision, ..
            }
            | Self::ClearDeadline {
                expected_revision, ..
            }
            | Self::CreateReminder {
                expected_revision, ..
            } => Some(expected_revision),
            Self::CaptureTask { .. }
            | Self::CompleteWorkUnit { .. }
            | Self::WorkUnitGraphDiff(_)
            | Self::PublishWorkflow { .. } => None,
        }
    }

    fn target_action(&self) -> Option<TargetAction> {
        match self {
            Self::CaptureTask { .. } => Some(TargetAction::CaptureTask),
            Self::TriageInbox { project_id, .. } => Some(TargetAction::TriageInbox {
                project_id: *project_id,
            }),
            Self::CompleteWorkUnit { .. } => Some(TargetAction::CompleteWorkUnit),
            Self::CreateTimeBlock { .. } => Some(TargetAction::CreateTimeBlock),
            Self::SetDeadline { .. } | Self::ClearDeadline { .. } => {
                Some(TargetAction::SetDeadline)
            }
            Self::CreateReminder { .. } => Some(TargetAction::CreateReminder),
            Self::WorkUnitGraphDiff(_) => Some(TargetAction::EditWorkUnitGraph),
            Self::PublishWorkflow { .. } => None,
        }
    }

    fn validate(&self, timezone: Tz) -> Result<()> {
        match self {
            Self::CaptureTask { title, body, .. } => validate_markdown(title, body),
            Self::TriageInbox {
                expected_revision, ..
            } => {
                ensure!(valid_digest(expected_revision), "invalid revision digest");
                Ok(())
            }
            Self::CompleteWorkUnit { .. } => Ok(()),
            Self::CreateTimeBlock {
                expected_revision,
                start,
                end,
                ..
            } => {
                ensure!(valid_digest(expected_revision), "invalid revision digest");
                DateTimeSpan::new(*start, *end, timezone)?;
                Ok(())
            }
            Self::SetDeadline {
                expected_revision, ..
            }
            | Self::ClearDeadline {
                expected_revision, ..
            }
            | Self::CreateReminder {
                expected_revision, ..
            } => {
                ensure!(valid_digest(expected_revision), "invalid revision digest");
                Ok(())
            }
            Self::WorkUnitGraphDiff(draft) => draft.validate(),
            Self::PublishWorkflow { draft } => draft.validate().map_err(Into::into),
        }?;
        if let Self::CreateReminder { title, body, .. } = self {
            validate_markdown(title, body)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Applying,
    Applied,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Binding {
    Default,
    External { path: PathBuf },
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    version: u8,
    pub id: Id,
    vault: Binding,
    timezone: Tz,
    pub target_title: String,
    expected_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<GraphReview>,
    pub operation: Operation,
    pub status: Status,
}

impl Proposal {
    pub fn graph_diff(&self) -> Option<&graph_diff::GraphDiff> {
        self.graph.as_ref().map(|graph| &graph.diff)
    }

    fn result_ids(&self) -> Vec<Id> {
        if let Some(graph) = &self.graph {
            graph.diff.added_nodes.iter().map(|node| node.id).collect()
        } else {
            self.result_id.into_iter().collect()
        }
    }

    pub fn workflow_revision(&self) -> Option<&str> {
        self.workflow_revision.as_deref()
    }

    /// Whether applying this record needs a fresh approval from the caller.
    pub fn requires_approval(&self) -> Result<bool> {
        match (&self.operation, self.status) {
            (_, Status::Applied) => Ok(false),
            (Operation::PublishWorkflow { .. }, Status::Applying) => Ok(true),
            (_, status) => {
                ensure_pending(status)?;
                Ok(true)
            }
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphReview {
    diff: graph_diff::GraphDiff,
    expected_revisions: BTreeMap<Id, String>,
}

impl GraphReview {
    fn from_application(review: WorkUnitGraphReview) -> (TargetSummary, Self) {
        (
            review.target,
            Self {
                diff: review.diff,
                expected_revisions: review.expected_revisions,
            },
        )
    }

    fn related_ids(diff: &graph_diff::GraphDiff) -> impl Iterator<Item = Id> {
        std::iter::once(diff.project_id).chain(diff.existing_dependencies())
    }

    fn validate(&self, draft: &graph_diff::Draft) -> Result<()> {
        self.diff.validate_against(draft)?;
        ensure!(
            self.expected_revisions.len() == Self::related_ids(&self.diff).count()
                && Self::related_ids(&self.diff)
                    .all(|id| self.expected_revisions.contains_key(&id))
                && self
                    .expected_revisions
                    .values()
                    .all(|revision| valid_digest(revision)),
            "graph diff has inconsistent revision expectations"
        );
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Submission {
    pub proposal_id: Id,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_diff: Option<graph_diff::GraphDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_revision: Option<String>,
}

impl From<Proposal> for Submission {
    fn from(record: Proposal) -> Self {
        Self {
            proposal_id: record.id,
            status: "queued",
            graph_diff: record.graph.map(|graph| graph.diff),
            workflow_revision: record.workflow_revision,
        }
    }
}

/// Queue a typed operation. A repeated request ID with the same operation keeps
/// the original revision and document UUID even after the Vault has changed.
#[cfg(all(test, unix))]
fn enqueue(
    root: &Path,
    timezone: Tz,
    paths: &WorkspacePaths,
    operation: Operation,
    request_id: Option<Id>,
) -> Result<Id> {
    Ok(submit(root, timezone, paths, operation, request_id)?.proposal_id)
}

pub fn submit(
    root: &Path,
    timezone: Tz,
    paths: &WorkspacePaths,
    operation: Operation,
    request_id: Option<Id>,
) -> Result<Submission> {
    operation.validate(timezone)?;
    let queue = Queue::open(root, timezone, paths)?;
    queue.enqueue(operation, request_id)
}

pub struct Queue {
    root: PathBuf,
    paths: WorkspacePaths,
    binding: Binding,
    timezone: Tz,
    directory: PathBuf,
}

impl Queue {
    pub fn open(root: &Path, timezone: Tz, paths: &WorkspacePaths) -> Result<Self> {
        ensure!(
            QUEUE_SUPPORTED,
            "the private proposal queue is unavailable on this platform"
        );
        let root = root
            .canonicalize()
            .context("opening the configured Vault")?;
        ensure!(root.is_dir(), "the configured Vault must be a directory");
        let paths = canonical_workspace(paths)?;
        let directory = paths.proposals_dir();
        ensure!(
            !directory.starts_with(&root) && !root.starts_with(&directory),
            "the proposal queue and Vault must be separate directories"
        );
        let default_root = match paths.vault_root().canonicalize() {
            Ok(root) => Some(root),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).context("resolving the configured default Vault");
            }
        };
        let binding = if default_root.as_ref() == Some(&root) {
            Binding::Default
        } else {
            Binding::External { path: root.clone() }
        };
        prepare_queue_directory(&directory)?;
        Ok(Self {
            root,
            paths,
            binding,
            timezone,
            directory,
        })
    }

    fn enqueue(&self, operation: Operation, request_id: Option<Id>) -> Result<Submission> {
        ensure!(
            !matches!(&operation, Operation::CreateReminder { .. })
                || self.binding == Binding::Default,
            "background reminders are only supported for the default Vault"
        );
        let _lock = lock_queue(&self.directory)?;
        let id = request_id.unwrap_or_else(Id::new_v4);
        if let Some(existing) = self.read(id)? {
            ensure!(
                request_id.is_some() && self.matches(&existing) && existing.operation == operation,
                "request ID already belongs to a different proposal"
            );
            sync_dir(&self.directory)?;
            return Ok(existing.into());
        }
        if let Operation::PublishWorkflow { draft } = &operation {
            let workflow_revision = draft.hash()?;
            let record = Proposal {
                version: VERSION,
                id,
                vault: self.binding.clone(),
                timezone: self.timezone,
                target_title: format!("Workflow · {}", &workflow_revision[..12]),
                expected_revision: workflow_revision.clone(),
                workflow_revision: Some(workflow_revision),
                result_id: None,
                graph: None,
                operation,
                status: Status::Pending,
            };
            self.persist(&record, false)?;
            return Ok(record.into());
        }
        let target = operation.target();
        let (target_info, graph) = if let Operation::WorkUnitGraphDiff(draft) = &operation {
            let review =
                VaultQueries::new(&self.root, self.timezone).review_work_unit_graph_diff(draft)?;
            let (target_info, graph) = GraphReview::from_application(review);
            (target_info, Some(graph))
        } else {
            let action = operation
                .target_action()
                .expect("handled operations have a Vault target action");
            (
                VaultQueries::new(&self.root, self.timezone).validate_target(
                    action,
                    target,
                    operation.expected_revision(),
                )?,
                None,
            )
        };
        let target_title = target_info.title;
        let revision = target_info.revision;
        let record = Proposal {
            version: VERSION,
            id,
            vault: self.binding.clone(),
            timezone: self.timezone,
            target_title,
            expected_revision: revision,
            workflow_revision: None,
            result_id: match operation {
                Operation::CaptureTask { .. } => Some(Id::new_v4()),
                Operation::TriageInbox { inbox_id, .. } => Some(inbox_id),
                Operation::CompleteWorkUnit { work_unit_id }
                | Operation::CreateTimeBlock { work_unit_id, .. } => Some(work_unit_id),
                Operation::SetDeadline { target_id, .. }
                | Operation::ClearDeadline { target_id, .. } => Some(target_id),
                Operation::CreateReminder { .. } => Some(Id::new_v4()),
                Operation::WorkUnitGraphDiff(_) | Operation::PublishWorkflow { .. } => None,
            },
            graph,
            operation,
            status: Status::Pending,
        };
        self.persist(&record, false)?;
        Ok(record.into())
    }

    pub fn list(&self) -> Result<Vec<Proposal>> {
        let mut proposals = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if let Some(id) = record_id(&path)
                && let Some(proposal) = self.read(id)?
                && self.matches(&proposal)
            {
                proposals.push(proposal);
            }
        }
        proposals.sort_unstable_by_key(|proposal| proposal.id);
        Ok(proposals)
    }

    pub fn review(&self, id: Id) -> Result<Proposal> {
        self.read(id)?
            .filter(|proposal| self.matches(proposal))
            .with_context(|| {
                format!("proposal {id} was not found for the configured Vault and timezone")
            })
    }

    fn matches(&self, proposal: &Proposal) -> bool {
        self.binding == proposal.vault && self.timezone == proposal.timezone
    }

    pub fn reject(&self, id: Id) -> Result<()> {
        let _lock = lock_queue(&self.directory)?;
        let mut record = self.review(id)?;
        if record.status == Status::Rejected {
            return sync_dir(&self.directory);
        }
        ensure_pending(record.status)?;
        record.status = Status::Rejected;
        self.persist(&record, true)
    }

    /// Commit the exact record approved by the caller.
    /// The caller acquires approval before calling this method.
    pub fn apply(&self, reviewed: &Proposal) -> Result<Vec<Id>> {
        let _lock = lock_queue(&self.directory)?;
        let mut record = self.review(reviewed.id)?;
        if !record.requires_approval()? {
            sync_dir(&self.directory)?;
            return Ok(record.result_ids());
        }
        ensure!(&record == reviewed, "proposal changed during review");
        record.operation.validate(record.timezone)?;

        if let Operation::PublishWorkflow { draft } = &record.operation {
            record.status = Status::Applying;
            self.persist(&record, true)?;
            let state_dir = self
                .directory
                .parent()
                .context("locating workflow revision store")?;
            let revision = WorkflowUseCases::new(state_dir)
                .publish_snapshot(draft)
                .context("publishing the reviewed workflow snapshot")?;
            ensure!(
                Some(revision.hash()) == record.workflow_revision.as_deref(),
                "published workflow hash differs from the reviewed snapshot"
            );
            record.status = Status::Applied;
            self.persist(&record, true)
                .context("workflow published but saving its proposal receipt failed")?;
            return Ok(Vec::new());
        }

        let target = record.operation.target();
        let related_expected = record
            .graph
            .as_ref()
            .map(|graph| &graph.expected_revisions)
            .cloned()
            .unwrap_or_default();
        let revisions = VaultQueries::new(&self.root, self.timezone).validate_revision_set(
            record
                .operation
                .target_action()
                .expect("Vault proposals have a target action"),
            target,
            &record.expected_revision,
            &related_expected,
        )?;
        let completion_context = match &record.operation {
            Operation::CompleteWorkUnit { work_unit_id } => Some(
                VaultQueries::new(&self.root, self.timezone)
                    .work_unit_completion_context(*work_unit_id)?
                    .context("completion target is no longer a WorkUnit")?,
            ),
            _ => None,
        };
        if matches!(&record.operation, Operation::CreateReminder { .. }) {
            ensure!(
                self.paths.is_default_vault(&self.root)?,
                "background reminders are only supported for the default Vault"
            );
        }

        let mut commands = VaultCommands::new(&self.root, self.timezone);
        if completion_context.is_some() {
            commands =
                commands.with_completion_origin(self.paths.load_or_create_device_identity()?);
        }

        // An interruption can leave a committed Vault write with no queue receipt.
        // Persist intent before calling Vault and never replay an uncertain write.
        record.status = Status::Applying;
        self.persist(&record, true)?;
        match apply_operation(
            &commands,
            &record,
            revisions.target,
            revisions.related,
            completion_context,
        ) {
            Ok(()) => {
                record.status = Status::Applied;
                self.persist(&record, true)
                    .context("Vault apply succeeded but saving its receipt failed")?;
                Ok(record.result_ids())
            }
            Err(
                error @ (WriteError::PublishedButNotSynced { .. }
                | WriteError::GraphPublicationUncertain { .. }),
            ) => Err(error).context(
                "proposal outcome is uncertain. Inspect the Vault before submitting a replacement",
            ),
            Err(error) => {
                // Vault guarantees that these errors precede publication.
                record.status = Status::Pending;
                self.persist(&record, true).with_context(|| {
                    format!("{error:#}. Could not return the proposal to pending")
                })?;
                Err(error.into())
            }
        }
    }

    fn path(&self, id: Id) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }

    fn read(&self, id: Id) -> Result<Option<Proposal>> {
        let path = self.path(id);
        match fs::symlink_metadata(&path) {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", path.display()));
            }
        }
        Ok(Some(read_record(&path, id)?.0))
    }

    fn persist(&self, record: &Proposal, replace: bool) -> Result<()> {
        let mut staged = NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(&mut staged, record)?;
        staged.as_file().sync_all()?;
        let path = self.path(record.id);
        if replace {
            staged.persist(path)?;
        } else {
            staged.persist_noclobber(path)?;
        }
        sync_dir(&self.directory)
    }
}

/// Acquire this before the Vault's writer lock, the order used by proposal
/// apply. Hold both until the Vault and proposal records have been captured.
#[cfg(unix)]
pub fn lock_for_snapshot(paths: &WorkspacePaths) -> Result<ProposalSnapshot> {
    let directory = canonical_workspace(paths)?.proposals_dir();
    prepare_queue_directory(&directory)?;
    let lock = lock_queue(&directory)?;
    Ok(ProposalSnapshot {
        directory,
        _lock: lock,
    })
}

#[cfg(unix)]
pub struct ProposalSnapshot {
    directory: PathBuf,
    _lock: File,
}

#[cfg(unix)]
impl ProposalSnapshot {
    /// Copy the exact bytes of portable proposals into a private staging home.
    /// The caller supplies the Vault writer lock for the same source workspace.
    pub fn copy_to(&self, destination: &WorkspacePaths) -> Result<()> {
        let target = destination.proposals_dir();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            let Some(id) = record_id(&path) else {
                continue;
            };
            let (record, bytes) = read_record(&path, id)?;
            if record.vault == Binding::Default {
                prepare_queue_directory(&target)?;
                let mut staged = NamedTempFile::new_in(&target)?;
                staged.write_all(&bytes)?;
                staged.as_file().sync_all()?;
                staged.persist_noclobber(target.join(format!("{id}.json")))?;
                sync_dir(&target)?;
            }
        }
        Ok(())
    }
}

/// Validate the staged or restored proposal subset without creating state.
#[cfg(unix)]
pub fn validate_snapshot(paths: &WorkspacePaths) -> Result<()> {
    let directory = paths.proposals_dir();
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("checking proposal snapshot directory"),
    };
    ensure!(
        metadata.is_dir(),
        "proposal snapshot must be a real directory"
    );
    verify_private(&directory, &metadata)?;
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        let id = record_id(&path)
            .with_context(|| format!("invalid proposal snapshot filename {}", path.display()))?;
        let (record, _) = read_record(&path, id)?;
        ensure!(
            record.vault == Binding::Default,
            "proposal {id} is not bound to the default Vault"
        );
    }
    Ok(())
}

fn canonical_workspace(paths: &WorkspacePaths) -> Result<WorkspacePaths> {
    let state = paths.state_dir();
    let workspace = state.parent().context("locating the workspace directory")?;
    let home = workspace.parent().context("locating the home directory")?;
    Ok(WorkspacePaths::from_home(home.canonicalize()?))
}

fn prepare_queue_directory(directory: &Path) -> Result<()> {
    let state = directory.parent().context("locating the state directory")?;
    let workspace = state.parent().context("locating the workspace directory")?;
    for path in [workspace, state, directory] {
        ensure_private_dir(path)?;
    }
    Ok(())
}

fn record_id(path: &Path) -> Option<Id> {
    let name = path.file_name()?.to_str()?;
    let id = Id::parse_str(name.strip_suffix(".json")?).ok()?;
    (name == format!("{id}.json")).then_some(id)
}

fn read_record(path: &Path, id: Id) -> Result<(Proposal, Vec<u8>)> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file(),
        "expected a regular proposal file at {}",
        path.display()
    );
    verify_private(path, &metadata)?;
    let bytes = fs::read(path)?;
    let record: Proposal =
        serde_json::from_slice(&bytes).with_context(|| format!("reading proposal {id}"))?;
    ensure!(
        record.version == VERSION && record.id == id,
        "invalid proposal identity or version for {id}"
    );
    ensure!(
        valid_digest(&record.expected_revision),
        "invalid revision digest for proposal {id}"
    );
    match (&record.operation, &record.workflow_revision) {
        (Operation::PublishWorkflow { draft }, Some(hash)) => {
            draft.validate()?;
            ensure!(
                valid_digest(hash) && hash == &draft.hash()?,
                "invalid workflow snapshot hash for proposal {id}"
            );
            ensure!(
                record.expected_revision == *hash
                    && record.result_id.is_none()
                    && record.graph.is_none(),
                "invalid workflow proposal metadata for {id}"
            );
        }
        (Operation::PublishWorkflow { .. }, None) | (_, Some(_)) => {
            bail!("invalid workflow revision metadata for proposal {id}")
        }
        _ => {}
    }
    if let Binding::External { path } = &record.vault {
        ensure!(path.is_absolute(), "invalid Vault path for proposal {id}");
    }
    record.operation.validate(record.timezone)?;
    match (&record.operation, record.result_id, &record.graph) {
        (Operation::CaptureTask { .. }, Some(_), None) => {}
        (
            Operation::TriageInbox {
                inbox_id,
                expected_revision,
                ..
            },
            Some(result_id),
            None,
        ) if *inbox_id == result_id && *expected_revision == record.expected_revision => {}
        (Operation::CompleteWorkUnit { work_unit_id }, Some(result_id), None)
            if *work_unit_id == result_id => {}
        (
            Operation::CreateTimeBlock {
                work_unit_id,
                expected_revision,
                ..
            },
            Some(result_id),
            None,
        ) if *work_unit_id == result_id && *expected_revision == record.expected_revision => {}
        (
            Operation::SetDeadline {
                target_id,
                expected_revision,
                ..
            }
            | Operation::ClearDeadline {
                target_id,
                expected_revision,
            },
            Some(result_id),
            None,
        ) if *target_id == result_id && *expected_revision == record.expected_revision => {}
        (
            Operation::CreateReminder {
                expected_revision, ..
            },
            Some(_),
            None,
        ) if *expected_revision == record.expected_revision => {}
        (Operation::WorkUnitGraphDiff(draft), None, Some(graph)) => graph.validate(draft)?,
        (Operation::PublishWorkflow { .. }, None, None) => {}
        _ => bail!("invalid result for proposal {id}"),
    }
    Ok((record, bytes))
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
}

fn lock_queue(directory: &Path) -> Result<File> {
    let path = directory.join(".lock");
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file(),
                "proposal queue lock must be a regular file"
            );
            verify_private(&path, &metadata)?;
            OpenOptions::new().write(true).open(&path)?
        }
        Err(error) => return Err(error).context("opening the proposal queue lock"),
    };
    file.lock().context("locking the proposal queue")?;
    Ok(file)
}

fn apply_operation(
    commands: &VaultCommands,
    record: &Proposal,
    revision: Revision,
    related_revisions: BTreeMap<Id, Revision>,
    completion_context: Option<mcbtodo_application::WorkUnitCompletionContext>,
) -> Result<(), WriteError> {
    match &record.operation {
        Operation::CaptureTask {
            project_id,
            title,
            body,
        } => {
            commands.capture_task(
                Task {
                    id: record.result_id.expect("validated task proposal"),
                    project_id: *project_id,
                    status: Default::default(),
                    priority: Default::default(),
                    active_period: None,
                    deadline: None,
                    recurrence: None,
                    occurrence: None,
                    tags: Vec::new(),
                },
                title.clone(),
                body.clone(),
                revision,
            )?;
        }
        Operation::TriageInbox {
            inbox_id,
            project_id,
            ..
        } => {
            commands.triage_inbox_entry(*inbox_id, *project_id, revision)?;
        }
        Operation::CompleteWorkUnit { work_unit_id } => {
            let context = completion_context.ok_or_else(|| {
                WriteError::InvalidDocument(anyhow::anyhow!("missing WorkUnit completion context"))
            })?;
            if context.id() != *work_unit_id || context.revision() != &revision {
                return Err(WriteError::InvalidDocument(anyhow::anyhow!(
                    "completion context no longer matches the reviewed WorkUnit"
                )));
            }
            commands
                .complete_work_unit(context, &[], |_, _| Ok(()))
                .map_err(|error| match error {
                    mcbtodo_application::CompleteWorkUnitError::Write(error) => error,
                    mcbtodo_application::CompleteWorkUnitError::InvalidEvidence(error) => {
                        WriteError::InvalidDocument(error)
                    }
                })?;
        }
        Operation::CreateTimeBlock {
            work_unit_id,
            start,
            end,
            availability,
            ..
        } => {
            commands.create_time_block(*work_unit_id, *start, *end, *availability, revision)?;
        }
        Operation::SetDeadline {
            target_id, date, ..
        } => {
            commands.set_deadline(*target_id, Some(Deadline::Date { value: *date }), revision)?;
        }
        Operation::ClearDeadline { target_id, .. } => {
            commands.set_deadline(*target_id, None, revision)?;
        }
        Operation::CreateReminder {
            target_id,
            schedule,
            title,
            body,
            ..
        } => {
            commands.create_reminder(
                record.result_id.expect("validated reminder proposal"),
                Some(*target_id),
                *schedule,
                title.clone(),
                body.clone(),
            )?;
        }
        Operation::WorkUnitGraphDiff(draft) => {
            let diff = record
                .graph
                .as_ref()
                .expect("validated graph review")
                .diff
                .clone();
            commands.apply_work_unit_graph_diff(draft, diff, revision, related_revisions)?;
        }
        Operation::PublishWorkflow { .. } => unreachable!("published before Vault apply"),
    }
    Ok(())
}

fn ensure_pending(status: Status) -> Result<()> {
    match status {
        Status::Pending => Ok(()),
        Status::Applying => {
            bail!(
                "a prior apply has an uncertain outcome. Inspect the Vault before submitting a replacement"
            )
        }
        Status::Applied => bail!("the proposal has already been applied"),
        Status::Rejected => bail!("the proposal has been rejected"),
    }
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    let created = fs::DirBuilder::new().mode(0o700).create(path);
    #[cfg(not(unix))]
    let created = fs::create_dir(path);
    match created {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error).with_context(|| format!("creating {}", path.display())),
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir(),
        "expected a real directory at {}",
        path.display()
    );
    verify_private(path, &metadata)?;
    sync_dir(path.parent().context("proposal directory has no parent")?)
}

fn verify_private(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    ensure!(
        _metadata.permissions().mode() & 0o077 == 0,
        "{} must deny group and other access",
        _path.display()
    );
    Ok(())
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .with_context(|| format!("syncing {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use mcbtodo_vault::SetDeadline;

    const PROJECT: Id = Id::from_u128(0x10000000000040008000000000000001);
    const TASK: Id = Id::from_u128(0x20000000000040008000000000000001);
    const MAPPING: Id = Id::from_u128(0x30000000000040008000000000000002);
    const TRANSLATION: Id = Id::from_u128(0x30000000000040008000000000000003);
    const BRANCH_ONE: Id = Id::from_u128(0x50000000000040008000000000000001);
    const BRANCH_TWO: Id = Id::from_u128(0x50000000000040008000000000000002);
    const JOIN: Id = Id::from_u128(0x50000000000040008000000000000003);
    const TIMEZONE: Tz = chrono_tz::Asia::Shanghai;

    struct Fixture {
        _temp: tempfile::TempDir,
        paths: WorkspacePaths,
        queue: Queue,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let fixture = Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/vault"
            ));
            let root = temp.path().join("vault");
            for relative_path in [
                "projects/rust-os/project.md",
                "projects/rust-os/tasks/memory.md",
                "projects/rust-os/units/read-spec.md",
                "projects/rust-os/units/mapping.md",
                "projects/rust-os/units/translation.md",
            ] {
                let dest = root.join(relative_path);
                fs::create_dir_all(dest.parent().unwrap()).unwrap();
                fs::copy(fixture.join(relative_path), dest).unwrap();
            }
            let paths = WorkspacePaths::from_home(temp.path().to_owned());
            fs::create_dir_all(paths.data_dir()).unwrap();
            fs::set_permissions(paths.data_dir(), fs::Permissions::from_mode(0o700)).unwrap();
            std::os::unix::fs::symlink(&root, paths.vault_root()).unwrap();
            let queue = Queue::open(&root, TIMEZONE, &paths).unwrap();
            Self {
                _temp: temp,
                paths,
                queue,
            }
        }

        fn capture(&self) -> Proposal {
            self.propose(Operation::CaptureTask {
                project_id: PROJECT,
                title: "Test task".to_owned(),
                body: "Literal Markdown\n\n- [ ] Review result\n".to_owned(),
            })
        }

        fn propose(&self, operation: Operation) -> Proposal {
            let id = enqueue(&self.queue.root, TIMEZONE, &self.paths, operation, None).unwrap();
            self.queue.review(id).unwrap()
        }

        fn vault(&self) -> Vault {
            Vault::load(&self.queue.root, TIMEZONE).unwrap()
        }
    }

    fn graph_operation() -> Operation {
        serde_json::from_value(serde_json::json!({
            "kind": "work_unit_graph_diff", "task_id": TASK, "units": [
                {"id": JOIN, "title": "Join", "depends_on": [BRANCH_ONE, BRANCH_TWO]},
                {"id": BRANCH_ONE, "title": "First branch", "depends_on": [MAPPING]},
                {"id": BRANCH_TWO, "title": "Second branch", "body": "Review the map.\n", "estimated_minutes": 25}
            ]
        }))
        .unwrap()
    }

    fn workflow_operation() -> (Operation, String, Vec<u8>) {
        let graph = r#"schema: mcbtodo.workflow/v1
id: reviewed-workflow
name: Reviewed workflow
entry: check
nodes:
  check:
    kind: script
    runner: nushell
    source: scripts/check.nu
    timeout_seconds: 30
    capabilities: []
    outputs: {summary: string}
  report: {kind: action, action: report.create}
edges:
  - {from: check, to: report}
bindings:
  - {from: {node: check, port: summary}, to: {node: report, port: summary}}
"#;
        let source = b"print 'captured source'\n".to_vec();
        let mut sources = mcbtodo_workflow::ScriptSources::new();
        sources.insert("scripts/check.nu".to_owned(), source.clone());
        (
            Operation::PublishWorkflow {
                draft: WorkflowDraft::new(graph.to_owned(), sources).unwrap(),
            },
            graph.to_owned(),
            source,
        )
    }

    #[test]
    fn workflow_proposal_requires_review_and_publishes_the_exact_snapshot() {
        let fixture = Fixture::new();
        let (operation, graph, source) = workflow_operation();
        let proposal = fixture.propose(operation);
        let revision = proposal.workflow_revision.as_deref().unwrap();
        let store = RevisionStore::new(fixture.paths.state_dir());
        assert!(store.list().unwrap().is_empty());
        assert_eq!(proposal.status, Status::Pending);

        fixture.queue.reject(proposal.id).unwrap();
        assert!(store.list().unwrap().is_empty());
        assert_eq!(
            fixture.queue.review(proposal.id).unwrap().status,
            Status::Rejected
        );
        assert!(fixture.queue.apply(&proposal).is_err());

        let approved = fixture.propose(workflow_operation().0);
        assert!(fixture.queue.apply(&approved).unwrap().is_empty());
        assert!(fixture.queue.apply(&approved).unwrap().is_empty());
        assert_eq!(
            fixture.queue.review(approved.id).unwrap().status,
            Status::Applied
        );
        assert_eq!(store.list().unwrap(), [revision.to_owned()]);
        let published = store.load(revision).unwrap();
        assert_eq!(published.graph_bytes(), graph.as_bytes());
        assert_eq!(
            published.script_sources()["scripts/check.nu"],
            source.as_slice()
        );
    }

    #[test]
    fn applying_workflow_proposal_can_retry_after_publication_before_receipt() {
        let fixture = Fixture::new();
        let proposal = fixture.propose(workflow_operation().0);
        let proposal_id = proposal.id;
        let revision = proposal.workflow_revision.clone().unwrap();
        let mut applying = proposal;
        applying.status = Status::Applying;
        fixture.queue.persist(&applying, true).unwrap();

        assert!(fixture.queue.apply(&applying).unwrap().is_empty());
        assert!(fixture.queue.apply(&applying).unwrap().is_empty());
        assert_eq!(
            fixture.queue.review(proposal_id).unwrap().status,
            Status::Applied
        );
        assert_eq!(
            RevisionStore::new(fixture.paths.state_dir())
                .list()
                .unwrap(),
            [revision]
        );
    }

    #[test]
    fn approved_time_block_updates_the_projection_once_and_rejects_concurrent_edits() {
        let fixture = Fixture::new();
        let operation = || {
            let digest = fixture.vault().revision(MAPPING).unwrap().digest();
            serde_json::from_value(serde_json::json!({
                "kind": "create_time_block",
                "work_unit_id": MAPPING,
                "expected_revision": digest,
                "start": "2026-11-21T09:00:00+08:00",
                "end": "2026-11-21T11:00:00+08:00",
                "availability": "free"
            }))
            .unwrap()
        };
        let approved = fixture.propose(operation());
        let stale = fixture.propose(operation());
        let count = |vault: Vault| vault.work_unit(MAPPING).unwrap().data.time_blocks.len();
        let before = count(fixture.vault());
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [MAPPING]);
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [MAPPING]);
        assert_eq!(count(fixture.vault()), before + 1);
        let error = fixture.queue.apply(&stale).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WriteError>(),
            Some(WriteError::StaleRevision { id }) if *id == MAPPING
        ));
        let vault = fixture.vault();
        let blocks = &vault.work_unit(MAPPING).unwrap().data.time_blocks;
        assert_eq!(blocks.len(), before + 1);
        assert_eq!(blocks.last().unwrap().availability, Availability::Free);
        let day = "2026-11-21".parse().unwrap();
        assert!(vault.calendar().iter().any(|entry| {
            entry.id == MAPPING
                && entry.kind == mcbtodo_domain::CalendarKind::TimeBlock
                && !entry.busy
                && entry.on_day(day, TIMEZONE)
        }));
    }

    #[test]
    fn approved_inbox_triage_preserves_capture_and_rejects_stale_revision() {
        let fixture = Fixture::new();
        let inbox_id: Id = "90000000-0000-4000-8000-000000000001".parse().unwrap();
        let inbox_dir = fixture.queue.root.join("inbox");
        fs::create_dir_all(&inbox_dir).unwrap();
        let path = inbox_dir.join(format!("{inbox_id}.md"));
        let source = format!(
            "---\nschema: mcbtodo.inbox-entry/v1\nid: {inbox_id}\ncaptured_at: '2026-11-20T12:00:00Z'\ntags: [review]\n---\n\n# Review recovery\n\nKeep this captured body.\n"
        );
        fs::write(&path, &source).unwrap();
        let revision = Vault::load(&fixture.queue.root, TIMEZONE)
            .unwrap()
            .revision(inbox_id)
            .unwrap()
            .digest();
        let operation = || Operation::TriageInbox {
            inbox_id,
            project_id: PROJECT,
            expected_revision: revision.clone(),
        };

        let approved = fixture.propose(operation());
        let stale = fixture.propose(operation());
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [inbox_id]);
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [inbox_id]);
        assert!(fixture.queue.apply(&stale).is_err());

        let updated = fs::read_to_string(path).unwrap();
        assert!(updated.contains(&format!("project_id: {PROJECT}")));
        assert!(updated.contains("# Review recovery"));
        assert!(updated.contains("Keep this captured body."));
        assert!(updated.contains("tags: [review]"));
    }

    #[test]
    fn deadline_proposals_apply_once_and_reject_stale_revisions() {
        let fixture = Fixture::new();
        let operation = |date| {
            let digest = fixture.vault().revision(MAPPING).unwrap().digest();
            serde_json::from_value(serde_json::json!({
                "kind": "set_deadline",
                "target_id": MAPPING,
                "expected_revision": digest,
                "date": date
            }))
            .unwrap()
        };
        let approved = fixture.propose(operation("2026-12-01"));
        let stale = fixture.propose(operation("2026-12-02"));
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [MAPPING]);
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [MAPPING]);
        assert_eq!(
            fixture.vault().deadline(MAPPING),
            Some(Some(Deadline::Date {
                value: "2026-12-01".parse().unwrap()
            }))
        );
        let error = fixture.queue.apply(&stale).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WriteError>(),
            Some(WriteError::StaleRevision { id }) if *id == MAPPING
        ));
        let clear = fixture.propose(
            serde_json::from_value(serde_json::json!({
                "kind": "clear_deadline",
                "target_id": MAPPING,
                "expected_revision": fixture.vault().revision(MAPPING).unwrap().digest()
            }))
            .unwrap(),
        );
        assert_eq!(fixture.queue.apply(&clear).unwrap(), [MAPPING]);
        assert_eq!(fixture.vault().deadline(MAPPING), Some(None));
    }

    #[test]
    fn reminder_proposals_are_target_checked_atomic_and_revision_bound() {
        let fixture = Fixture::new();
        let project_revision = fixture.vault().revision(PROJECT).unwrap().digest();
        let invalid_target = match submit(
            &fixture.queue.root,
            TIMEZONE,
            &fixture.paths,
            Operation::CreateReminder {
                target_id: PROJECT,
                expected_revision: project_revision,
                schedule: serde_json::from_value(serde_json::json!({
                    "kind": "once",
                    "at": {
                        "value": "2026-11-21T09:00:00+08:00",
                        "timezone": "Asia/Shanghai"
                    }
                }))
                .unwrap(),
                title: "Invalid project reminder".into(),
                body: String::new(),
            },
            None,
        ) {
            Ok(_) => panic!("Project must not be accepted as a reminder target"),
            Err(error) => error,
        };
        assert!(
            invalid_target
                .to_string()
                .contains("existing Task or WorkUnit")
        );

        let make_operation = || {
            let digest = fixture.vault().revision(MAPPING).unwrap().digest();
            serde_json::from_value(serde_json::json!({
                "kind": "create_reminder",
                "target_id": MAPPING,
                "expected_revision": digest,
                "schedule": {
                    "kind": "recurring",
                    "rule": {
                        "start": "2026-11-20",
                        "time": "09:00:00",
                        "timezone": "Asia/Shanghai",
                        "frequency": "weekly",
                        "interval": 2,
                        "dst": {"gap": "skip", "fold": "earlier"}
                    }
                },
                "title": "Review implementation",
                "body": "Check the test results."
            }))
            .unwrap()
        };
        let approved = fixture.propose(make_operation());
        let stale = fixture.propose(make_operation());
        let id = approved.result_id.unwrap();
        let before = fixture.vault().reminders().count();
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [id]);
        assert_eq!(fixture.queue.apply(&approved).unwrap(), [id]);
        let vault = fixture.vault();
        assert_eq!(vault.reminders().count(), before + 1);
        let reminder = vault.reminders().find(|doc| doc.data.id == id).unwrap();
        assert_eq!(reminder.data.target_id, Some(MAPPING));
        assert!(reminder.title == "Review implementation");

        let current = fixture.vault();
        VaultStore::new(&fixture.queue.root, TIMEZONE)
            .set_deadline(SetDeadline {
                id: MAPPING,
                deadline: Some(Deadline::Date {
                    value: "2026-12-01".parse().unwrap(),
                }),
                expected_revision: current.revision(MAPPING).unwrap().clone(),
            })
            .unwrap();
        let error = fixture.queue.apply(&stale).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WriteError>(),
            Some(WriteError::StaleRevision { id }) if *id == MAPPING
        ));
        assert_eq!(fixture.vault().reminders().count(), before + 1);
    }

    #[test]
    fn graph_diff_applies_all_nodes_and_replays_its_receipt() {
        let fixture = Fixture::new();
        let previous = fixture.vault();
        let existing_path = previous.source(MAPPING).unwrap();
        let directory = existing_path.parent().unwrap();
        let ancillary = directory.join("editor-notes.txt");
        fs::write(&ancillary, b"keep these notes").unwrap();
        let existing_bytes = fs::read(existing_path).unwrap();
        #[cfg(unix)]
        fs::set_permissions(directory, fs::Permissions::from_mode(0o750)).unwrap();
        let proposal = fixture.propose(graph_operation());
        let ids = [BRANCH_ONE, BRANCH_TWO, JOIN];
        assert_eq!(fixture.queue.apply(&proposal).unwrap(), ids);
        assert_eq!(fs::read(existing_path).unwrap(), existing_bytes);
        assert_eq!(fs::read(ancillary).unwrap(), b"keep these notes");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o750
        );

        let vault = fixture.vault();
        let graph = vault.task_graph();
        let edges: Vec<_> = graph.edges().collect();
        for edge in [
            (MAPPING, BRANCH_ONE),
            (BRANCH_ONE, JOIN),
            (BRANCH_TWO, JOIN),
        ] {
            assert!(edges.contains(&edge));
        }
        assert!(graph.blocked(BRANCH_ONE));
        assert!(!graph.blocked(BRANCH_TWO));
        assert!(graph.blocked(JOIN));
        let unit = vault.work_unit(BRANCH_TWO).unwrap();
        assert_eq!(unit.title, "Second branch");
        assert_eq!(unit.data.estimated_minutes, Some(25));
        assert_eq!(unit.data.parent_task_id, TASK);
        assert_eq!(unit.data.project_id, PROJECT);
        let source = fs::read_to_string(&unit.path).unwrap();
        assert!(source.ends_with("Review the map.\n"));
        fs::write(&unit.path, format!("{source}\nEdited after apply\n")).unwrap();

        let reopened = Queue::open(&fixture.queue.root, TIMEZONE, &fixture.paths).unwrap();
        assert_eq!(
            reopened.review(proposal.id).unwrap().status,
            Status::Applied
        );
        assert_eq!(reopened.apply(&proposal).unwrap(), ids);
        assert!(
            fs::read_to_string(&unit.path)
                .unwrap()
                .ends_with("Edited after apply\n")
        );
    }

    #[test]
    fn graph_diff_uuid_collision_leaves_no_partial_writes_and_can_retry() {
        let fixture = Fixture::new();
        let proposal = fixture.propose(graph_operation());
        let existing = graph_diff::UnitDraft {
            id: JOIN,
            title: "Independent work".to_owned(),
            body: String::new(),
            depends_on: Vec::new(),
            estimated_minutes: None,
        };
        let collision = VaultStore::new(&fixture.queue.root, TIMEZONE)
            .create_work_unit(mcbtodo_vault::CreateWorkUnit {
                unit: existing.work_unit(PROJECT, TASK),
                title: existing.title,
                body: existing.body,
                expected_revision: fixture.vault().revision(TASK).unwrap().clone(),
            })
            .unwrap();
        let original = fs::read(&collision.path).unwrap();

        let error = fixture.queue.apply(&proposal).unwrap_err();
        assert!(
            matches!(error.downcast_ref(), Some(WriteError::DuplicateId { id }) if *id == JOIN),
            "{error:#}"
        );
        let after = fixture.vault();
        assert!(after.source(BRANCH_ONE).is_none());
        assert!(after.source(BRANCH_TWO).is_none());
        assert_eq!(fs::read(&collision.path).unwrap(), original);
        assert_eq!(fixture.queue.review(proposal.id).unwrap(), proposal);

        fs::remove_file(collision.path).unwrap();
        assert_eq!(
            fixture.queue.apply(&proposal).unwrap(),
            [BRANCH_ONE, BRANCH_TWO, JOIN]
        );
    }

    #[test]
    fn graph_diff_rejects_changes_to_reviewed_documents_without_partial_writes() {
        let fixture = Fixture::new();
        let proposal = fixture.propose(graph_operation());
        for id in [TASK, PROJECT, MAPPING] {
            let vault = fixture.vault();
            let path = vault.source(id).unwrap();
            let original = fs::read_to_string(path).unwrap();
            fs::write(path, format!("{original}\nEdited during review.\n")).unwrap();
            let error = fixture.queue.apply(&proposal).unwrap_err();
            assert!(
                matches!(error.downcast_ref(), Some(WriteError::StaleRevision { id: changed }) if *changed == id)
            );
            assert!(
                [BRANCH_ONE, BRANCH_TWO, JOIN]
                    .into_iter()
                    .all(|id| fixture.vault().source(id).is_none())
            );
            assert_eq!(fixture.queue.review(proposal.id).unwrap(), proposal);
            fs::write(path, original).unwrap();
        }
        fixture.queue.reject(proposal.id).unwrap();
        fixture.queue.reject(proposal.id).unwrap();
        assert!(fixture.queue.apply(&proposal).is_err());
    }

    #[test]
    fn snapshot_validation_checks_graph_edges_and_revision_expectations() {
        let fixture = Fixture::new();
        let default_root = fixture.paths.vault_root();
        fs::remove_file(&default_root).unwrap();
        fs::rename(&fixture.queue.root, &default_root).unwrap();
        let queue = Queue::open(&default_root, TIMEZONE, &fixture.paths).unwrap();
        let id = queue.enqueue(graph_operation(), None).unwrap().proposal_id;
        let staged = tempfile::tempdir().unwrap();
        let staged_paths = WorkspacePaths::from_home(staged.path().to_owned());
        let snapshot = lock_for_snapshot(&fixture.paths).unwrap();
        snapshot.copy_to(&staged_paths).unwrap();
        validate_snapshot(&staged_paths).unwrap();
        let path = staged_paths.proposals_dir().join(format!("{id}.json"));
        let original = fs::read(&path).unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        record["graph"]["diff"]["added_edges"] = serde_json::json!([]);
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(
            validate_snapshot(&staged_paths)
                .unwrap_err()
                .to_string()
                .contains("inconsistent edges")
        );
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        record["graph"]["expected_revisions"] = serde_json::json!({});
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(
            validate_snapshot(&staged_paths)
                .unwrap_err()
                .to_string()
                .contains("inconsistent revision expectations")
        );
    }

    #[test]
    fn task_capture_checks_the_project_revision_and_replay_returns_the_receipt() {
        let fixture = Fixture::new();
        let proposal = fixture.capture();
        let original = fixture.vault();
        let project_path = original.source(PROJECT).unwrap();
        let source = fs::read_to_string(project_path).unwrap();
        assert_eq!(proposal.expected_revision.len(), 64);
        assert!(
            !fs::read_to_string(fixture.queue.path(proposal.id))
                .unwrap()
                .contains("mcbtodo.project/v1")
        );
        fs::write(project_path, format!("{source}\nExternal edit\n")).unwrap();
        let error = fixture.queue.apply(&proposal).unwrap_err();
        assert!(
            matches!(error.downcast_ref(), Some(WriteError::StaleRevision { id }) if *id == PROJECT)
        );
        let task = proposal.result_id.unwrap();
        assert!(fixture.vault().source(task).is_none());
        assert_eq!(
            fixture.queue.review(proposal.id).unwrap().status,
            Status::Pending
        );

        fs::write(project_path, &source).unwrap();
        assert_eq!(fixture.queue.apply(&proposal).unwrap(), [task]);
        let captured = fixture.vault();
        assert_eq!(
            captured
                .outline()
                .iter()
                .filter(|row| row.title == "Test task")
                .count(),
            1
        );
        let path = captured.source(task).unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.ends_with("Literal Markdown\n\n- [ ] Review result\n"));
        fs::write(path, format!("{contents}\nEdited after apply\n")).unwrap();
        assert_eq!(fixture.queue.apply(&proposal).unwrap(), [task]);
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .ends_with("Edited after apply\n")
        );
        assert_eq!(
            fixture.queue.review(proposal.id).unwrap().status,
            Status::Applied
        );
    }

    #[test]
    fn completion_checks_the_work_unit_revision() {
        let fixture = Fixture::new();
        let proposal = fixture.propose(Operation::CompleteWorkUnit {
            work_unit_id: MAPPING,
        });
        let vault = fixture.vault();
        let path = vault.source(MAPPING).unwrap();
        let contents = fs::read_to_string(path).unwrap();
        fs::write(path, format!("{contents}\nUpdated work\n")).unwrap();
        let error = fixture.queue.apply(&proposal).unwrap_err();
        assert!(
            matches!(error.downcast_ref(), Some(WriteError::StaleRevision { id }) if *id == MAPPING)
        );
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("status: in_progress")
        );
    }

    #[test]
    fn blocked_completion_can_retry_when_its_prerequisite_is_done() {
        let fixture = Fixture::new();
        let blocked = fixture.propose(Operation::CompleteWorkUnit {
            work_unit_id: TRANSLATION,
        });
        let error = fixture.queue.apply(&blocked).unwrap_err();
        assert!(
            matches!(error.downcast_ref(), Some(WriteError::Blocked { id }) if *id == TRANSLATION)
        );
        assert_eq!(
            fixture.queue.review(blocked.id).unwrap().status,
            Status::Pending
        );

        let prerequisite = fixture.propose(Operation::CompleteWorkUnit {
            work_unit_id: MAPPING,
        });
        assert_eq!(fixture.queue.apply(&prerequisite).unwrap(), [MAPPING]);
        assert_eq!(fixture.queue.apply(&blocked).unwrap(), [TRANSLATION]);
        assert_eq!(fixture.queue.apply(&blocked).unwrap(), [TRANSLATION]);
        let vault = fixture.vault();
        let source = fs::read_to_string(vault.source(TRANSLATION).unwrap()).unwrap();
        assert!(source.contains("status: completed"));
        let origin = fixture.paths.load_or_create_device_identity().unwrap();
        let events = VaultStore::new(&fixture.queue.root, TIMEZONE)
            .work_unit_completion_export_page(0)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .map(|record| (record.event.work_unit_id, record.origin_device_id))
                .collect::<Vec<_>>(),
            [(MAPPING, Some(origin)), (TRANSLATION, Some(origin))]
        );
    }

    #[test]
    fn interruption_after_a_vault_write_fences_retries() {
        let fixture = Fixture::new();
        let mut proposal = fixture.capture();
        proposal.status = Status::Applying;
        fixture.queue.persist(&proposal, true).unwrap();
        // Simulate process loss after publication but before the queue receipt.
        let revision = fixture.vault().revision(PROJECT).unwrap().clone();
        let commands = VaultCommands::new(&fixture.queue.root, TIMEZONE);
        apply_operation(&commands, &proposal, revision, BTreeMap::new(), None).unwrap();
        assert!(
            fixture
                .queue
                .apply(&proposal)
                .unwrap_err()
                .to_string()
                .contains("uncertain outcome")
        );
        assert!(
            fixture
                .vault()
                .source(proposal.result_id.unwrap())
                .is_some()
        );
        assert!(fixture.queue.reject(proposal.id).is_err());
    }

    #[test]
    fn rejecting_is_idempotent_and_prevents_apply() {
        let fixture = Fixture::new();
        let proposal = fixture.capture();
        fixture.queue.reject(proposal.id).unwrap();
        fixture.queue.reject(proposal.id).unwrap();
        assert_eq!(
            fixture.queue.review(proposal.id).unwrap().status,
            Status::Rejected
        );
        assert!(
            fixture
                .queue
                .apply(&proposal)
                .unwrap_err()
                .to_string()
                .contains("rejected")
        );
        assert!(
            fixture
                .vault()
                .source(proposal.result_id.unwrap())
                .is_none()
        );
    }

    #[test]
    fn queue_review_is_scoped_and_records_are_private() {
        let fixture = Fixture::new();
        let proposal = fixture.capture();
        let other = Queue::open(&fixture.queue.root, chrono_tz::UTC, &fixture.paths).unwrap();
        assert!(other.list().unwrap().is_empty());
        assert!(other.apply(&proposal).is_err());
        #[cfg(unix)]
        for path in [&fixture.queue.directory, &fixture.queue.path(proposal.id)] {
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o077, 0);
        }
    }

    #[test]
    fn snapshot_copies_only_portable_records_and_keeps_their_bytes() {
        let fixture = Fixture::new();
        let default_root = fixture.paths.vault_root();
        fs::remove_file(&default_root).unwrap();
        fs::rename(&fixture.queue.root, &default_root).unwrap();
        let queue = Queue::open(&default_root, TIMEZONE, &fixture.paths).unwrap();
        let external_root = fixture._temp.path().join("external-vault");
        let project_path = external_root.join("projects/rust-os/project.md");
        fs::create_dir_all(project_path.parent().unwrap()).unwrap();
        fs::copy(
            default_root.join("projects/rust-os/project.md"),
            project_path,
        )
        .unwrap();
        let external_queue = Queue::open(&external_root, TIMEZONE, &fixture.paths).unwrap();
        let external = external_queue
            .enqueue(
                Operation::CaptureTask {
                    project_id: PROJECT,
                    title: "External task".to_owned(),
                    body: String::new(),
                },
                None,
            )
            .unwrap()
            .proposal_id;
        let operation = || Operation::CaptureTask {
            project_id: PROJECT,
            title: "Portable task".to_owned(),
            body: String::new(),
        };
        let portable = queue.enqueue(operation(), None).unwrap().proposal_id;
        let rejected = queue.enqueue(operation(), None).unwrap().proposal_id;
        queue.reject(rejected).unwrap();
        let applied = queue.enqueue(operation(), None).unwrap().proposal_id;
        let applied_result = queue.apply(&queue.review(applied).unwrap()).unwrap();
        fs::write(queue.directory.join(".tmp-staging"), b"partial").unwrap();
        fs::write(queue.directory.join("not-a-uuid.json"), b"ignored").unwrap();
        let noncanonical = format!("{}.json", portable.simple());
        fs::write(queue.directory.join(noncanonical), b"ignored").unwrap();

        let expected = fs::read(queue.path(portable)).unwrap();
        let snapshot = lock_for_snapshot(&fixture.paths).unwrap();
        assert!(matches!(
            OpenOptions::new()
                .write(true)
                .open(queue.directory.join(".lock"))
                .unwrap()
                .try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        let restored = tempfile::tempdir().unwrap();
        let restored_paths = WorkspacePaths::from_home(restored.path().to_owned());
        snapshot.copy_to(&restored_paths).unwrap();
        validate_snapshot(&restored_paths).unwrap();
        let restored_root = restored_paths.vault_root();
        fs::rename(default_root, &restored_root).unwrap();
        let restored_queue = Queue::open(&restored_root, TIMEZONE, &restored_paths).unwrap();
        assert_eq!(fs::read(restored_queue.path(portable)).unwrap(), expected);
        assert_eq!(
            fs::read_dir(&restored_queue.directory)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count(),
            3
        );
        assert!(!restored_queue.path(external).exists());
        assert_eq!(
            restored_queue.review(rejected).unwrap().status,
            Status::Rejected
        );
        let receipt = restored_queue.review(applied).unwrap();
        assert_eq!(receipt.status, Status::Applied);
        assert_eq!(restored_queue.apply(&receipt).unwrap(), applied_result);
        let reviewed = restored_queue.review(portable).unwrap();
        let id = restored_queue.apply(&reviewed).unwrap();
        assert_eq!(restored_queue.apply(&reviewed).unwrap(), id);
    }

    #[test]
    fn snapshot_validator_rejects_external_bindings_and_nonrecord_files() {
        let fixture = Fixture::new();
        let external_root = fixture._temp.path().join("external-vault");
        let project_path = external_root.join("projects/rust-os/project.md");
        fs::create_dir_all(project_path.parent().unwrap()).unwrap();
        fs::copy(
            fixture.queue.root.join("projects/rust-os/project.md"),
            project_path,
        )
        .unwrap();
        let external_queue = Queue::open(&external_root, TIMEZONE, &fixture.paths).unwrap();
        let external = external_queue
            .enqueue(
                Operation::CaptureTask {
                    project_id: PROJECT,
                    title: "External task".to_owned(),
                    body: String::new(),
                },
                None,
            )
            .unwrap()
            .proposal_id;
        fs::remove_file(fixture.queue.directory.join(".lock")).unwrap();
        assert!(
            validate_snapshot(&fixture.paths)
                .unwrap_err()
                .to_string()
                .contains("not bound")
        );
        fs::remove_file(external_queue.path(external)).unwrap();
        fs::write(fixture.queue.directory.join(".stage"), b"incomplete").unwrap();
        assert!(
            validate_snapshot(&fixture.paths)
                .unwrap_err()
                .to_string()
                .contains("invalid proposal snapshot filename")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_default_vault_is_bound_to_the_default_path() {
        let fixture = Fixture::new();
        let aliased = Queue::open(&fixture.paths.vault_root(), TIMEZONE, &fixture.paths).unwrap();
        assert_eq!(aliased.binding, Binding::Default);
    }
}
