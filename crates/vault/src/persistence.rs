use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, ensure};
use chrono::Utc;
use chrono_tz::Tz;
use mcbtodo_domain::{
    CalendarEvent, EvidenceRef, Id, InboxEntry, Priority, Project, Task, TaskGraph, TaskStatus,
    TimeBlock, WorkUnit, WorkUnitCompleted, WorkUnitStatus, WorkflowRevisionHash,
    recurrence::Reminder, time::Deadline,
};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{
    Vault,
    completion_journal::{CompletionJournal, PendingCompletion},
    document, is_kind,
};

mod graph_batch;
pub use graph_batch::{AddWorkUnitGraph, GRAPH_STAGE_PREFIX, NewWorkUnit};

/// An opaque, byte-exact comparison token for one Markdown document.
/// Clone it from a validated `Vault` snapshot or a successful write result.
/// The token holds shared source bytes; use [`Self::digest`] for a fingerprint.
#[derive(Clone, PartialEq, Eq)]
pub struct Revision(Arc<str>);

impl Revision {
    pub(crate) fn from_source(source: String) -> Self {
        Self(source.into())
    }

    /// Returns the lowercase hexadecimal SHA-256 digest of the exact Markdown bytes.
    pub fn digest(&self) -> String {
        format!("{:x}", Sha256::digest(self.0.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const UNIT: Id = Id::from_u128(3);
    const DATABASE: &str = ".mcbtodo-events.sqlite3";
    const ROLLBACK: &str = ".mcbtodo-events.sqlite3-journal";

    struct InterruptedCompletion {
        root: tempfile::TempDir,
        path: PathBuf,
        before: Revision,
        after: Revision,
        event: WorkUnitCompleted,
    }

    impl InterruptedCompletion {
        fn prepare() -> Self {
            let root = tempfile::tempdir().unwrap();
            let project = root.path().join("projects/project");
            fs::create_dir_all(project.join("tasks")).unwrap();
            fs::create_dir_all(project.join("units")).unwrap();
            let project_id = Id::from_u128(1);
            let task_id = Id::from_u128(2);
            fs::write(
                project.join("project.md"),
                format!("---\nschema: mcbtodo.project/v1\nid: {project_id}\n---\n\n# Project\n"),
            )
            .unwrap();
            fs::write(
                project.join("tasks/task.md"),
                format!(
                    "---\nschema: mcbtodo.task/v1\nid: {task_id}\nproject_id: {project_id}\n---\n\n# Task\n"
                ),
            )
            .unwrap();
            let path = project.join("units/unit.md");
            let before = Revision::from_source(format!(
                "---\nschema: mcbtodo.work-unit/v1\nid: {UNIT}\nproject_id: {project_id}\nparent_task_id: {task_id}\nstatus: todo\n---\n\n# Unit\n"
            ));
            fs::write(&path, before.0.as_bytes()).unwrap();
            let after =
                Revision::from_source(before.0.replace("status: todo", "status: completed"));
            let event = WorkUnitCompleted {
                event_id: Id::new_v4(),
                work_unit_id: UNIT,
                project_id,
                parent_task_id: task_id,
                occurred_at: Utc::now(),
                previous_status: WorkUnitStatus::Todo,
                evidence: Vec::new(),
            };
            let _lock = lock(root.path()).unwrap();
            let journal = CompletionJournal::open(root.path(), true).unwrap().unwrap();
            journal
                .prepare(&event, &before.digest(), &after.digest())
                .unwrap();
            assert!(journal.events().unwrap().is_empty());
            Self {
                root,
                path,
                before,
                after,
                event,
            }
        }

        fn store(&self) -> VaultStore {
            VaultStore::new(self.root.path(), chrono_tz::UTC)
        }

        fn publish_markdown(&self) {
            let _lock = lock(self.root.path()).unwrap();
            replace(self.path.clone(), UNIT, &self.before, self.after.clone()).unwrap();
        }

        fn validate_staged(&self) -> Result<()> {
            fs::remove_file(self.root.path().join(".mcbtodo-vault.lock")).unwrap();
            assert_staged_validation_preserves_files(&self.store())
        }
    }

    fn staged_files(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                files.insert(path.clone(), None);
                files.extend(staged_files(&path));
            } else {
                files.insert(path.clone(), Some(fs::read(&path).unwrap()));
            }
        }
        files
    }

    fn assert_staged_validation_preserves_files(store: &VaultStore) -> Result<()> {
        let before = staged_files(&store.root);
        let result = store.validate_completion_snapshot();
        assert!(
            before == staged_files(&store.root),
            "snapshot files changed"
        );
        result
    }

    #[test]
    fn completion_snapshot_accepts_vault_without_an_event_store() {
        let root = tempfile::tempdir().unwrap();
        let store = VaultStore::new(root.path(), chrono_tz::UTC);
        assert_staged_validation_preserves_files(&store).unwrap();
    }

    #[test]
    fn completion_snapshot_accepts_pending_before_markdown_publication() {
        InterruptedCompletion::prepare().validate_staged().unwrap();
    }

    #[test]
    fn completion_snapshot_accepts_pending_after_markdown_publication() {
        let interrupted = InterruptedCompletion::prepare();
        interrupted.publish_markdown();
        interrupted.validate_staged().unwrap();
    }

    #[test]
    fn completion_snapshot_rejects_a_pending_digest_conflict() {
        let interrupted = InterruptedCompletion::prepare();
        fs::write(
            &interrupted.path,
            format!("{}\nHuman edit.\n", interrupted.after.0),
        )
        .unwrap();
        let error = interrupted.validate_staged().unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WriteError>(),
            Some(WriteError::CompletionRecoveryConflict { id: UNIT, .. })
        ));
    }

    #[test]
    fn completion_snapshot_accepts_committed_events_after_markdown_edits() {
        let interrupted = InterruptedCompletion::prepare();
        interrupted.publish_markdown();
        assert_eq!(
            interrupted.store().work_unit_completed_events().unwrap(),
            std::slice::from_ref(&interrupted.event)
        );
        fs::write(
            &interrupted.path,
            format!("{}\nHuman edit.\n", interrupted.after.0),
        )
        .unwrap();
        interrupted.validate_staged().unwrap();
    }

    #[test]
    fn completion_snapshot_rejects_an_invalid_committed_event() {
        let interrupted = InterruptedCompletion::prepare();
        let db = rusqlite::Connection::open(interrupted.root.path().join(DATABASE)).unwrap();
        db.execute(
            "UPDATE work_unit_completions SET pending = 0, event = '{}'",
            [],
        )
        .unwrap();
        db.close().unwrap();
        assert!(interrupted.validate_staged().is_err());
    }

    #[test]
    fn completion_snapshot_rejects_an_orphan_rollback_journal() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(ROLLBACK), b"orphan journal").unwrap();
        let store = VaultStore::new(root.path(), chrono_tz::UTC);
        assert!(assert_staged_validation_preserves_files(&store).is_err());
    }

    #[test]
    fn completion_snapshot_rejects_wal_sidecars() {
        let interrupted = InterruptedCompletion::prepare();
        fs::write(interrupted.root.path().join(format!("{DATABASE}-wal")), []).unwrap();
        assert!(interrupted.validate_staged().is_err());
    }

    #[test]
    fn completion_snapshot_rejects_super_journal_framing() {
        let interrupted = InterruptedCompletion::prepare();
        // This synthetic footer has zero length and no filename.
        let mut footer = [0_u8; 16];
        footer[8..].copy_from_slice(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]);
        fs::write(interrupted.root.path().join(ROLLBACK), footer).unwrap();
        let error = interrupted.validate_staged().unwrap_err();
        assert!(format!("{error:#}").contains("super-journal"));
    }

    #[test]
    fn completion_snapshot_recovers_a_hot_journal_on_a_disposable_copy() {
        let interrupted = InterruptedCompletion::prepare();
        let database = interrupted.root.path().join(DATABASE);
        let rollback = interrupted.root.path().join(ROLLBACK);
        let mut db = rusqlite::Connection::open(&database).unwrap();
        let tx = db.transaction().unwrap();
        let invalid_event = "uncommitted invalid event";
        tx.execute(
            "UPDATE work_unit_completions SET event = ?1",
            [invalid_event],
        )
        .unwrap();
        tx.cache_flush().unwrap();
        let hot_database = fs::read(&database).unwrap();
        let hot_rollback = fs::read(&rollback).unwrap();
        assert!(
            hot_database
                .windows(invalid_event.len())
                .any(|bytes| bytes == invalid_event.as_bytes()),
            "the test requires an uncommitted change on disk"
        );
        tx.rollback().unwrap();
        db.close().unwrap();

        // Re-create the file pair left by an interruption after the page was flushed.
        fs::write(&database, hot_database).unwrap();
        fs::write(&rollback, hot_rollback).unwrap();
        interrupted.validate_staged().unwrap();
    }

    #[test]
    fn revision_digest_is_lowercase_sha256() {
        assert_eq!(
            Revision::from_source("abc".into()).digest(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn completion_watermark_reports_contention_without_waiting() {
        let root = tempfile::tempdir().unwrap();
        let held = lock(root.path()).unwrap();
        let store = VaultStore::new(root.path(), chrono_tz::UTC);
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            send.send(store.work_unit_completion_watermark()).unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        drop(held);
        reader.join().unwrap();
        assert!(matches!(
            result.expect("watermark waited for the writer lock"),
            Err(WriteError::Storage(error))
                if error.downcast_ref::<io::Error>().map(io::Error::kind)
                    == Some(io::ErrorKind::WouldBlock)
        ));
    }

    #[test]
    fn completion_watermark_and_page_are_empty_without_an_event_store() {
        let root = tempfile::tempdir().unwrap();
        let store = VaultStore::new(root.path(), chrono_tz::UTC);
        assert_eq!(store.work_unit_completion_watermark().unwrap(), 0);
        assert!(store.work_unit_completion_page(0).unwrap().is_empty());
        assert!(!root.path().join(DATABASE).exists());
    }

    #[test]
    fn completion_watermark_reconciles_before_returning_the_latest_sequence() {
        let interrupted = InterruptedCompletion::prepare();
        let unpublished = InterruptedCompletion::prepare();
        interrupted.publish_markdown();
        assert_eq!(
            interrupted
                .store()
                .work_unit_completion_watermark()
                .unwrap(),
            1
        );
        assert_eq!(
            unpublished
                .store()
                .work_unit_completion_watermark()
                .unwrap(),
            0
        );
        let journal = CompletionJournal::open(unpublished.root.path(), false)
            .unwrap()
            .unwrap();
        assert!(journal.pending().unwrap().is_none());
    }

    #[test]
    fn completion_page_reconciles_a_pending_publication() {
        let interrupted = InterruptedCompletion::prepare();
        interrupted.publish_markdown();
        assert_eq!(
            interrupted.store().work_unit_completion_page(0).unwrap(),
            [(1, interrupted.event)]
        );
    }

    #[test]
    fn completion_pages_are_bounded_and_resume_after_an_exclusive_cursor() {
        let interrupted = InterruptedCompletion::prepare();
        let expected: Vec<_> = (1_i64..=129)
            .map(|sequence| {
                let event = WorkUnitCompleted {
                    event_id: Id::new_v4(),
                    ..interrupted.event.clone()
                };
                (sequence, event)
            })
            .collect();
        {
            let _lock = lock(interrupted.root.path()).unwrap();
            let journal = CompletionJournal::open(interrupted.root.path(), false)
                .unwrap()
                .unwrap();
            journal.abort().unwrap();
            let before = interrupted.before.digest();
            let after = interrupted.after.digest();
            for (_, event) in &expected {
                journal.prepare(event, &before, &after).unwrap();
                journal.commit().unwrap();
            }
        }

        let store = interrupted.store();
        assert_eq!(store.work_unit_completion_watermark().unwrap(), 129);
        assert_eq!(store.work_unit_completion_page(0).unwrap(), expected[..64]);
        assert_eq!(
            store.work_unit_completion_page(64).unwrap(),
            expected[64..128]
        );
        assert_eq!(
            store.work_unit_completion_page(128).unwrap(),
            expected[128..]
        );
        assert!(store.work_unit_completion_page(129).unwrap().is_empty());
    }

    #[test]
    fn recovery_discards_intent_before_markdown_publication() {
        let interrupted = InterruptedCompletion::prepare();
        assert!(
            interrupted
                .store()
                .work_unit_completed_events()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fs::read(&interrupted.path).unwrap(),
            interrupted.before.0.as_bytes()
        );
        interrupted
            .store()
            .complete_work_unit(CompleteWorkUnit {
                id: UNIT,
                expected_revision: interrupted.before.clone(),
            })
            .unwrap();
        let events = interrupted.store().work_unit_completed_events().unwrap();
        assert_eq!(events.len(), 1);
        assert_ne!(events[0].event_id, interrupted.event.event_id);
    }

    #[test]
    fn recovery_commits_the_original_event_after_markdown_publication() {
        let interrupted = InterruptedCompletion::prepare();
        interrupted.publish_markdown();
        // A retry with a current revision reconciles before applying its no-op.
        interrupted
            .store()
            .complete_work_unit(CompleteWorkUnit {
                id: UNIT,
                expected_revision: interrupted.after.clone(),
            })
            .unwrap();
        assert_eq!(
            interrupted.store().work_unit_completed_events().unwrap(),
            [interrupted.event]
        );
    }

    #[test]
    fn recovery_retains_an_ambiguous_event_and_preserves_the_human_edit() {
        let interrupted = InterruptedCompletion::prepare();
        interrupted.publish_markdown();
        let edited = format!("{}\nHuman edit.\n", interrupted.after.0);
        fs::write(&interrupted.path, &edited).unwrap();
        let store = interrupted.store();
        assert!(matches!(
            store.work_unit_completed_events(),
            Err(WriteError::CompletionRecoveryConflict { id: UNIT, .. })
        ));
        assert!(matches!(
            store.complete_work_unit(CompleteWorkUnit {
                id: UNIT,
                expected_revision: Revision::from_source(edited.clone()),
            }),
            Err(WriteError::CompletionRecoveryConflict { id: UNIT, .. })
        ));
        assert_eq!(fs::read_to_string(&interrupted.path).unwrap(), edited);
        fs::write(&interrupted.path, interrupted.after.0.as_bytes()).unwrap();
        assert_eq!(
            store.work_unit_completed_events().unwrap(),
            [interrupted.event]
        );
    }
}

impl fmt::Debug for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Revision").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct CaptureTask {
    pub task: Task,
    pub title: String,
    /// Markdown after the generated title, included verbatim.
    pub body: String,
    /// Revision of the destination Project document.
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct CaptureInboxEntry {
    pub entry: InboxEntry,
    pub title: String,
    pub body: String,
}

#[derive(Debug)]
pub struct TriageInboxEntry {
    pub id: Id,
    pub project_id: Id,
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct MaterializeTaskOccurrences {
    pub template_id: Id,
    pub window: mcbtodo_domain::time::DateSpan,
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct CompleteWorkUnit {
    pub id: Id,
    /// Revision of the WorkUnit document.
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct BindWorkflow {
    pub work_unit_id: Id,
    pub workflow_revision: WorkflowRevisionHash,
    /// Revision of the WorkUnit receiving the binding.
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct CreateTimeBlock {
    pub work_unit_id: Id,
    pub block: TimeBlock,
    /// Revision of the WorkUnit receiving the block.
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct SetDeadline {
    pub id: Id,
    pub deadline: Option<Deadline>,
    /// Revision of the Project, Task or WorkUnit document.
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct SetWorkUnitDependencies {
    pub work_unit_id: Id,
    pub depends_on: Vec<Id>,
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct CreateProject {
    pub project: Project,
    /// Single directory name under `projects/`. Defaults to the project UUID.
    pub slug: Option<String>,
    pub title: String,
    pub body: String,
}

#[derive(Debug)]
pub struct CreateEvent {
    pub event: CalendarEvent,
    pub title: String,
    pub body: String,
}

#[derive(Debug)]
pub struct CreateReminder {
    pub reminder: Reminder,
    pub title: String,
    pub body: String,
}

#[derive(Debug)]
pub struct CreateWorkUnit {
    pub unit: WorkUnit,
    pub title: String,
    pub body: String,
    /// Revision of the parent Task.
    pub expected_revision: Revision,
}

#[derive(Debug)]
pub struct PersistedDocument {
    pub id: Id,
    pub path: PathBuf,
    pub revision: Revision,
}

#[derive(Debug)]
pub enum WriteError {
    ProjectNotFound {
        id: Id,
    },
    TaskNotFound {
        id: Id,
    },
    InboxEntryNotFound {
        id: Id,
    },
    WorkUnitNotFound {
        id: Id,
    },
    DeadlineTargetNotFound {
        id: Id,
    },
    ReminderTargetNotFound {
        id: Id,
    },
    StaleRevision {
        id: Id,
    },
    DuplicateId {
        id: Id,
    },
    DestinationExists {
        path: PathBuf,
    },
    Blocked {
        id: Id,
    },
    Cancelled {
        id: Id,
    },
    CompletionRecoveryConflict {
        id: Id,
        before: String,
        after: String,
    },
    InvalidVault(anyhow::Error),
    InvalidDocument(anyhow::Error),
    Storage(anyhow::Error),
    /// The file is visible at its destination, but durability could not be
    /// confirmed. A prepared completion may still need reconciliation.
    PublishedButNotSynced {
        document: PersistedDocument,
        source: io::Error,
    },
    /// The directory swap succeeded, but durability or the final directory
    /// check failed. Do not replay the batch.
    GraphPublicationUncertain {
        directory: PathBuf,
        source: anyhow::Error,
    },
    /// The calendar directory swap succeeded, but durability could not be confirmed.
    CalendarPublicationUncertain {
        directory: PathBuf,
        source: anyhow::Error,
    },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectNotFound { id } => write!(f, "project {id} was not found"),
            Self::TaskNotFound { id } => write!(f, "task {id} was not found"),
            Self::InboxEntryNotFound { id } => write!(f, "inbox entry {id} was not found"),
            Self::WorkUnitNotFound { id } => write!(f, "work unit {id} was not found"),
            Self::DeadlineTargetNotFound { id } => {
                write!(f, "project, task or work unit {id} was not found")
            }
            Self::ReminderTargetNotFound { id } => {
                write!(f, "reminder Task or WorkUnit target {id} was not found")
            }
            Self::StaleRevision { id } => write!(f, "stale revision for document {id}"),
            Self::DuplicateId { id } => write!(f, "document ID {id} already exists"),
            Self::DestinationExists { path } => write!(f, "{} already exists", path.display()),
            Self::Blocked { id } => write!(f, "work unit {id} has incomplete prerequisites"),
            Self::Cancelled { id } => write!(f, "work unit {id} is cancelled"),
            Self::CompletionRecoveryConflict { id, before, after } => write!(
                f,
                "cannot recover completion for work unit {id}: expected Markdown revision \
                 {before} (before) or {after} (after); the journal has been retained"
            ),
            Self::InvalidVault(error) => write!(f, "invalid vault snapshot: {error:#}"),
            Self::InvalidDocument(error) => write!(f, "invalid Markdown update: {error:#}"),
            Self::Storage(error) => write!(f, "vault write failed: {error:#}"),
            Self::PublishedButNotSynced { document, source } => write!(
                f,
                "published {} but could not confirm durability: {source}",
                document.path.display()
            ),
            Self::GraphPublicationUncertain { directory, source } => write!(
                f,
                "published graph at {} but its outcome is uncertain: {source:#}",
                directory.display()
            ),
            Self::CalendarPublicationUncertain { directory, source } => write!(
                f,
                "published calendar at {} but its outcome is uncertain: {source:#}",
                directory.display()
            ),
        }
    }
}

impl Error for WriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidVault(error)
            | Self::InvalidDocument(error)
            | Self::Storage(error)
            | Self::GraphPublicationUncertain { source: error, .. }
            | Self::CalendarPublicationUncertain { source: error, .. } => Some(error.as_ref()),
            Self::PublishedButNotSynced { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Durable writes against freshly validated Markdown snapshots.
///
/// Writers using this API serialize on `.mcbtodo-vault.lock` in the canonical
/// vault root. Keep the lock file in place so writers all lock the same inode.
/// External editors can bypass the lock. The source is checked again just before
/// publication, leaving a race between the final check and the atomic operation.
///
/// For a completion, SQLite records intent before publication and commits the
/// event after the Markdown is synced. If interrupted, the next write or event
/// query reconciles using the recorded byte digests. If neither digest matches,
/// the pending entry is retained and callers get a recovery conflict.
#[derive(Debug)]
pub struct VaultStore {
    root: PathBuf,
    timezone: Tz,
}

impl VaultStore {
    pub fn new(root: impl Into<PathBuf>, timezone: Tz) -> Self {
        Self {
            root: root.into(),
            timezone,
        }
    }

    /// Captures a Task under its Project's `tasks/<uuid>.md` without replacing an existing file.
    pub fn capture_task(&self, request: CaptureTask) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let project_id = request.task.project_id;
        let project = vault
            .projects
            .get(&project_id)
            .ok_or(WriteError::ProjectNotFound { id: project_id })?;
        check_revision(&vault, project_id, &request.expected_revision)?;
        let id = request.task.id;
        check_new_id(&vault, id)?;
        let source = document::render(
            "mcbtodo.task/v1",
            &request.task,
            &request.title,
            &request.body,
        )
        .map_err(WriteError::InvalidDocument)?;
        publish_child(&project.path, "tasks", id, source, || {
            check_on_disk(&project.path, project_id, &request.expected_revision)?;
            Ok(())
        })
    }

    /// Captures an unclassified Markdown item in `inbox/<uuid>.md`.
    pub fn capture_inbox_entry(
        &self,
        request: CaptureInboxEntry,
    ) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        if let Some(project_id) = request.entry.project_id
            && !vault.projects.contains_key(&project_id)
        {
            return Err(WriteError::ProjectNotFound { id: project_id });
        }
        check_new_id(&vault, request.entry.id)?;
        let id = request.entry.id;
        let source = document::render(
            "mcbtodo.inbox-entry/v1",
            &request.entry,
            &request.title,
            &request.body,
        )
        .map_err(WriteError::InvalidDocument)?;
        publish_in_directory(&self.root, "inbox", id, source)
    }

    /// Converts an Inbox entry into a Task while retaining the source entry.
    ///
    /// The Task ID is deterministic. If interrupted after Task publication, retry
    /// verifies the exact generated document before completing Inbox triage.
    pub fn triage_inbox_entry(
        &self,
        request: TriageInboxEntry,
    ) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let entry = vault
            .inbox
            .get(&request.id)
            .ok_or(WriteError::InboxEntryNotFound { id: request.id })?;
        check_revision(&vault, request.id, &request.expected_revision)?;
        if !vault.projects.contains_key(&request.project_id) {
            return Err(WriteError::ProjectNotFound {
                id: request.project_id,
            });
        }
        let source = fs::read_to_string(&entry.path)
            .map_err(|error| storage(error, "reading", &entry.path))?;
        let (title, body) =
            document::inbox_task_content(&source).map_err(WriteError::InvalidDocument)?;
        let task_id = entry.data.task_id();
        let task = Task {
            id: task_id,
            project_id: request.project_id,
            status: TaskStatus::Todo,
            priority: Priority::Normal,
            active_period: None,
            deadline: None,
            recurrence: None,
            occurrence: None,
            tags: entry.data.tags.clone(),
        };
        let task_source = document::render("mcbtodo.task/v1", &task, &title, &body)
            .map_err(WriteError::InvalidDocument)?;
        if let Some(existing) = vault.tasks.get(&task_id) {
            let existing_source = fs::read_to_string(&existing.path)
                .map_err(|error| storage(error, "reading", &existing.path))?;
            if existing_source != task_source {
                return Err(WriteError::DestinationExists {
                    path: existing.path.clone(),
                });
            }
        } else {
            let project = vault
                .projects
                .get(&request.project_id)
                .expect("target Project was validated");
            publish_child(&project.path, "tasks", task_id, task_source, || {
                check_on_disk(
                    &project.path,
                    request.project_id,
                    vault
                        .revision(request.project_id)
                        .expect("loaded Project revision"),
                )
                .map(|_| ())
            })?;
        }
        let updated = document::set_inbox_project(&source, request.project_id)
            .map_err(WriteError::InvalidDocument)?;
        replace(
            entry.path.clone(),
            request.id,
            &request.expected_revision,
            Revision::from_source(updated),
        )
    }

    /// Materializes recurring Task dates as stable, independent Markdown Tasks.
    /// Existing occurrences are retained unchanged, making retries idempotent.
    pub fn materialize_task_occurrences(
        &self,
        request: MaterializeTaskOccurrences,
    ) -> Result<Vec<Id>, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let template = vault
            .tasks
            .get(&request.template_id)
            .ok_or(WriteError::TaskNotFound {
                id: request.template_id,
            })?;
        check_revision(&vault, request.template_id, &request.expected_revision)?;
        let rule = template.data.recurrence.ok_or_else(|| {
            WriteError::InvalidDocument(anyhow!(
                "Task {} is not a recurring template",
                request.template_id
            ))
        })?;
        let template_id = request.template_id;
        let project_id = template.data.project_id;
        let project = vault
            .projects
            .get(&project_id)
            .expect("validated Task project exists");
        let template_path = template.path.clone();
        let title = template.title.clone();
        let template_revision = request.expected_revision.clone();
        let project_path = project.path.clone();
        let project_revision = vault
            .revision(project_id)
            .expect("validated Project has a revision")
            .clone();
        let body = fs::read_to_string(&template_path)
            .map_err(|error| storage(error, "reading", &template_path))?;
        let (_, _, _, markdown_body) =
            document::split(&body).map_err(WriteError::InvalidDocument)?;
        let title_heading = format!("\n# {title}\n\n");
        let body = markdown_body.strip_prefix(&title_heading).ok_or_else(|| {
            WriteError::InvalidDocument(anyhow!(
                "Task template body no longer matches its parsed title"
            ))
        })?;
        let mut ids = Vec::new();
        for date in rule
            .occurrence_dates(request.window)
            .map_err(|error| WriteError::InvalidDocument(error.into()))?
        {
            let occurrence = mcbtodo_domain::TaskOccurrence { template_id, date };
            let id = occurrence.id();
            if let Some(existing) = vault.tasks.get(&id) {
                if existing.data.occurrence != Some(occurrence) {
                    return Err(WriteError::DuplicateId { id });
                }
                ids.push(id);
                continue;
            }
            check_new_id(&vault, id)?;
            let mut task = template.data.clone();
            task.id = id;
            task.status = mcbtodo_domain::TaskStatus::Todo;
            task.recurrence = None;
            task.occurrence = Some(occurrence);
            task.active_period = None;
            task.deadline = Some(mcbtodo_domain::time::Deadline::Date { value: date });
            let source =
                document::render("mcbtodo.task/v1", &task, &format!("{title} ({date})"), body)
                    .map_err(WriteError::InvalidDocument)?;
            publish_child(&project.path, "tasks", id, source, || {
                check_on_disk(&template_path, template_id, &template_revision)?;
                check_on_disk(&project_path, project_id, &project_revision)?;
                Ok(())
            })?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Completes an unblocked WorkUnit. An already completed unit with the
    /// expected revision succeeds without rewriting the file.
    pub fn complete_work_unit(
        &self,
        request: CompleteWorkUnit,
    ) -> Result<PersistedDocument, WriteError> {
        self.complete_work_unit_with_evidence(request, &[])
    }

    /// Completes a WorkUnit and appends references in the same atomic replacement.
    /// Callers validate the references against the runtime store before invoking this method.
    /// A status transition also records a durable [`WorkUnitCompleted`] event.
    pub fn complete_work_unit_with_evidence(
        &self,
        request: CompleteWorkUnit,
        evidence: &[EvidenceRef],
    ) -> Result<PersistedDocument, WriteError> {
        let (lock, vault) = self.locked_snapshot()?;
        let id = request.id;
        let unit = vault
            .units
            .get(&id)
            .ok_or(WriteError::WorkUnitNotFound { id })?;
        let revision = request.expected_revision;
        check_revision(&vault, id, &revision)?;
        let path = unit.path.clone();
        match unit.data.status {
            WorkUnitStatus::Completed => (),
            WorkUnitStatus::Cancelled => return Err(WriteError::Cancelled { id }),
            _ if vault.graph.blocked(id) => return Err(WriteError::Blocked { id }),
            _ => (),
        }
        let mut added = Vec::new();
        for &reference in evidence {
            if !unit.data.evidence.contains(&reference) && !added.contains(&reference) {
                added.push(reference);
            }
        }
        if unit.data.status == WorkUnitStatus::Completed && added.is_empty() {
            check_on_disk(&path, id, &revision)?;
            return Ok(PersistedDocument { id, path, revision });
        }
        let updated = document::complete_unit(&revision.0, &unit.data, &added)
            .map_err(WriteError::InvalidDocument)?;
        let next_revision = Revision::from_source(updated);
        if unit.data.status == WorkUnitStatus::Completed {
            return replace(path, id, &revision, next_revision);
        }

        let event = WorkUnitCompleted {
            event_id: Id::new_v4(),
            work_unit_id: id,
            project_id: unit.data.project_id,
            parent_task_id: unit.data.parent_task_id,
            occurred_at: Utc::now(),
            previous_status: unit.data.status,
            evidence: unit.data.evidence.iter().copied().chain(added).collect(),
        };
        let journal = match lock.completions.as_ref() {
            Some(journal) => journal,
            None => &CompletionJournal::open(&lock.root, true)
                .map_err(WriteError::Storage)?
                .expect("create requested"),
        };
        check_on_disk(&path, id, &revision)?;
        // The database directory entry must survive a crash before Markdown is published.
        sync_directory(open_directory(&lock.root).map_err(WriteError::Storage)?)
            .map_err(|error| storage(error, "syncing", &lock.root))?;
        journal
            .prepare(&event, &revision.digest(), &next_revision.digest())
            .context("preparing WorkUnit completion event")
            .map_err(WriteError::Storage)?;
        match replace(path, id, &revision, next_revision) {
            Ok(document) => match journal.commit() {
                Ok(()) => Ok(document),
                Err(error) => Err(WriteError::PublishedButNotSynced {
                    document,
                    source: io::Error::other(error.context("committing WorkUnit completion event")),
                }),
            },
            Err(error @ WriteError::PublishedButNotSynced { .. }) => Err(error),
            Err(error) => {
                journal
                    .abort()
                    .with_context(|| format!("{error}. Could not clear the prepared completion"))
                    .map_err(WriteError::Storage)?;
                Err(error)
            }
        }
    }

    /// Reads committed completion events in publication order, reconciling any
    /// interrupted publication under the same lock used by completion.
    pub fn work_unit_completed_events(&self) -> Result<Vec<WorkUnitCompleted>, WriteError> {
        let lock = self.lock_completion_reader(lock)?;
        match lock.completions.as_ref() {
            Some(journal) => journal.events().map_err(WriteError::Storage),
            None => Ok(Vec::new()),
        }
    }

    /// Returns the latest committed completion sequence after reconciliation, or
    /// zero when no events are committed. If the Vault writer lock is held,
    /// returns [`WriteError::Storage`] with [`io::ErrorKind::WouldBlock`].
    pub fn work_unit_completion_watermark(&self) -> Result<i64, WriteError> {
        let lock = self.lock_completion_reader(try_lock)?;
        match lock.completions.as_ref() {
            Some(journal) => journal.watermark().map_err(WriteError::Storage),
            None => Ok(0),
        }
    }

    /// Reconciles interrupted publication and returns up to 64 committed events
    /// whose sequence exceeds `after`, in ascending sequence order.
    pub fn work_unit_completion_page(
        &self,
        after: i64,
    ) -> Result<Vec<(i64, WorkUnitCompleted)>, WriteError> {
        let lock = self.lock_completion_reader(lock)?;
        match lock.completions.as_ref() {
            Some(journal) => journal.page(after).map_err(WriteError::Storage),
            None => Ok(Vec::new()),
        }
    }

    fn lock_completion_reader(
        &self,
        acquire_lock: fn(&Path) -> Result<File>,
    ) -> Result<LockedRoot, WriteError> {
        let lock = self.lock_root_with(acquire_lock)?;
        if let Some(journal) = lock.completions.as_ref()
            && let Some(pending) = journal.pending().map_err(WriteError::Storage)?
        {
            let vault = Vault::load(&lock.root, self.timezone).map_err(WriteError::InvalidVault)?;
            reconcile_completion(journal, pending, &vault)?;
        }
        Ok(lock)
    }

    /// Validates completion events in a private, stable Vault staging directory.
    ///
    /// This takes no Vault lock and does not modify the snapshot. SQLite recovery
    /// runs on a disposable copy. A pending completion may match either the
    /// before or after Markdown digest. Call [`Vault::load`] for general Markdown
    /// validation, including when the event database is absent.
    pub fn validate_completion_snapshot(&self) -> Result<()> {
        if let Some(pending) = CompletionJournal::validate_snapshot(&self.root)
            .context("validating WorkUnit completion journal")?
        {
            let vault = Vault::load(&self.root, self.timezone)?;
            match_pending_completion(&vault, pending)?;
        }
        Ok(())
    }

    /// Records a published workflow hash. Callers verify publication in their revision store.
    pub fn bind_workflow(&self, request: BindWorkflow) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let id = request.work_unit_id;
        let unit = vault
            .units
            .get(&id)
            .ok_or(WriteError::WorkUnitNotFound { id })?;
        let revision = request.expected_revision;
        check_revision(&vault, id, &revision)?;
        let path = unit.path.clone();
        if unit.data.workflow_revision.as_ref() == Some(&request.workflow_revision) {
            check_on_disk(&path, id, &revision)?;
            return Ok(PersistedDocument { id, path, revision });
        }
        let updated = document::bind_workflow(&revision.0, &request.workflow_revision)
            .map_err(WriteError::InvalidDocument)?;
        replace(path, id, &revision, Revision::from_source(updated))
    }

    /// Appends a validated calendar block to the WorkUnit's Markdown front matter.
    pub fn create_time_block(
        &self,
        request: CreateTimeBlock,
    ) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let id = request.work_unit_id;
        let unit = vault
            .units
            .get(&id)
            .ok_or(WriteError::WorkUnitNotFound { id })?;
        let revision = request.expected_revision;
        check_revision(&vault, id, &revision)?;
        let updated = document::append_time_block(&revision.0, &unit.data, &request.block)
            .map_err(WriteError::InvalidDocument)?;
        replace(
            unit.path.clone(),
            id,
            &revision,
            Revision::from_source(updated),
        )
    }

    /// Sets or clears a Project, Task or WorkUnit deadline without rewriting
    /// unrelated front matter or Markdown content.
    pub fn set_deadline(&self, request: SetDeadline) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let id = request.id;
        let path = vault
            .projects
            .get(&id)
            .map(|doc| doc.path.clone())
            .or_else(|| vault.tasks.get(&id).map(|doc| doc.path.clone()))
            .or_else(|| vault.units.get(&id).map(|doc| doc.path.clone()))
            .ok_or(WriteError::DeadlineTargetNotFound { id })?;
        let revision = request.expected_revision;
        check_revision(&vault, id, &revision)?;
        if vault.deadline(id) == Some(request.deadline) {
            check_on_disk(&path, id, &revision)?;
            return Ok(PersistedDocument { id, path, revision });
        }
        let updated = document::set_deadline(&revision.0, request.deadline)
            .map_err(WriteError::InvalidDocument)?;
        replace(path, id, &revision, Revision::from_source(updated))
    }

    /// Replaces one WorkUnit's prerequisites after validating the complete candidate DAG.
    pub fn set_work_unit_dependencies(
        &self,
        request: SetWorkUnitDependencies,
    ) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let id = request.work_unit_id;
        let unit = vault
            .units
            .get(&id)
            .ok_or(WriteError::WorkUnitNotFound { id })?;
        let revision = request.expected_revision;
        check_revision(&vault, id, &revision)?;
        if unit.data.depends_on == request.depends_on {
            check_on_disk(&unit.path, id, &revision)?;
            return Ok(PersistedDocument {
                id,
                path: unit.path.clone(),
                revision,
            });
        }
        TaskGraph::build_with_dependencies(
            vault.units.values().map(|document| &document.data),
            id,
            &request.depends_on,
        )
        .map_err(|error| WriteError::InvalidDocument(error.into()))?;
        let updated = document::set_dependencies(&revision.0, &request.depends_on)
            .map_err(WriteError::InvalidDocument)?;
        replace(
            unit.path.clone(),
            id,
            &revision,
            Revision::from_source(updated),
        )
    }

    /// Publishes a Project with a new UUID and an unused directory name.
    pub fn create_project(&self, request: CreateProject) -> Result<PersistedDocument, WriteError> {
        let (lock, vault) = self.locked_snapshot()?;
        let id = request.project.id;
        check_new_id(&vault, id)?;
        let slug = request.slug.unwrap_or_else(|| id.to_string());
        if slug.is_empty()
            || !slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || slug.starts_with('-')
            || slug.ends_with('-')
        {
            return Err(WriteError::InvalidDocument(anyhow!(
                "project slug must contain only lowercase ASCII letters, digits and internal hyphens"
            )));
        }
        let source = document::render(
            "mcbtodo.project/v1",
            &request.project,
            &request.title,
            &request.body,
        )
        .map_err(WriteError::InvalidDocument)?;
        publish_project(&lock.root, &slug, id, source)
    }

    /// Publishes an independent calendar event under `calendar/`.
    pub fn create_event(&self, request: CreateEvent) -> Result<PersistedDocument, WriteError> {
        let (lock, vault) = self.locked_snapshot()?;
        let id = request.event.id;
        check_new_id(&vault, id)?;
        let source = document::render(
            "mcbtodo.calendar-event/v1",
            &request.event,
            &request.title,
            &request.body,
        )
        .map_err(WriteError::InvalidDocument)?;
        publish_calendar_event(&lock.root, id, source)
    }

    /// Publishes a batch of independent calendar events with one directory swap.
    ///
    /// Existing calendar documents are retained byte-for-byte. The batch is
    /// rejected before publication if any event has an invalid or duplicate ID.
    pub fn create_events_atomic(&self, requests: Vec<CreateEvent>) -> Result<usize, WriteError> {
        if requests.is_empty() {
            return Ok(0);
        }
        let (lock, vault) = self.locked_snapshot()?;
        let mut ids = std::collections::BTreeSet::new();
        let mut rendered = Vec::with_capacity(requests.len());
        for request in requests {
            let id = request.event.id;
            check_new_id(&vault, id)?;
            if !ids.insert(id) {
                return Err(WriteError::DuplicateId { id });
            }
            let source = document::render(
                "mcbtodo.calendar-event/v1",
                &request.event,
                &request.title,
                &request.body,
            )
            .map_err(WriteError::InvalidDocument)?;
            rendered.push((id, source));
        }
        publish_calendar_batch(&lock.root, &rendered)?;
        Ok(rendered.len())
    }

    /// Publishes a reminder after validating its optional Task or WorkUnit target.
    pub fn create_reminder(
        &self,
        request: CreateReminder,
    ) -> Result<PersistedDocument, WriteError> {
        let (lock, vault) = self.locked_snapshot()?;
        let id = request.reminder.id;
        check_new_id(&vault, id)?;
        if let Some(target_id) = request.reminder.target_id
            && !vault.is_reminder_target(target_id)
        {
            return Err(WriteError::ReminderTargetNotFound { id: target_id });
        }
        let source = document::render(
            "mcbtodo.reminder/v1",
            &request.reminder,
            &request.title,
            &request.body,
        )
        .map_err(WriteError::InvalidDocument)?;
        publish_reminder(&lock.root, id, source)
    }

    /// Creates a WorkUnit under its Project after validating the candidate DAG.
    pub fn create_work_unit(
        &self,
        request: CreateWorkUnit,
    ) -> Result<PersistedDocument, WriteError> {
        let (_lock, vault) = self.locked_snapshot()?;
        let unit = &request.unit;
        let project_id = unit.project_id;
        let project = vault
            .projects
            .get(&project_id)
            .ok_or(WriteError::ProjectNotFound { id: project_id })?;
        let task_id = unit.parent_task_id;
        let task = vault
            .tasks
            .get(&task_id)
            .ok_or(WriteError::TaskNotFound { id: task_id })?;
        check_revision(&vault, task_id, &request.expected_revision)?;
        if task.data.project_id != project_id {
            return Err(WriteError::InvalidDocument(anyhow!(
                "work unit has a different project from its parent task {task_id}"
            )));
        }
        let id = unit.id;
        check_new_id(&vault, id)?;
        TaskGraph::build(vault.units.values().map(|doc| &doc.data).chain([unit]))
            .map_err(|error| WriteError::InvalidDocument(error.into()))?;
        let source = document::render("mcbtodo.work-unit/v1", unit, &request.title, &request.body)
            .map_err(WriteError::InvalidDocument)?;
        let project_revision = vault.revision(project_id).expect("project has a revision");
        publish_child(&project.path, "units", id, source, || {
            check_on_disk(&project.path, project_id, project_revision)?;
            check_on_disk(&task.path, task_id, &request.expected_revision)?;
            Ok(())
        })
    }

    fn locked_snapshot(&self) -> Result<(LockedRoot, Vault), WriteError> {
        let lock = self.lock_root()?;
        let vault = Vault::load(&lock.root, self.timezone).map_err(WriteError::InvalidVault)?;
        if let Some(journal) = lock.completions.as_ref()
            && let Some(pending) = journal.pending().map_err(WriteError::Storage)?
        {
            reconcile_completion(journal, pending, &vault)?;
        }
        Ok((lock, vault))
    }

    fn lock_root(&self) -> Result<LockedRoot, WriteError> {
        self.lock_root_with(lock)
    }

    fn lock_root_with(
        &self,
        acquire_lock: fn(&Path) -> Result<File>,
    ) -> Result<LockedRoot, WriteError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|err| storage(err, "opening", &self.root))?;
        let lock = acquire_lock(&root).map_err(WriteError::Storage)?;
        let completions = CompletionJournal::open(&root, false)
            .context("opening WorkUnit completion journal")
            .map_err(WriteError::Storage)?;
        Ok(LockedRoot {
            root,
            completions,
            _lock: lock,
        })
    }
}

fn publish_calendar_event(
    root: &Path,
    id: Id,
    source: String,
) -> Result<PersistedDocument, WriteError> {
    let calendar = root.join("calendar");
    ensure_subdirectory(root, &calendar).map_err(WriteError::Storage)?;
    let path = calendar.join(format!("{id}.md"));
    let temp = stage(&calendar, &source, None).map_err(WriteError::Storage)?;
    let directory = open_directory(&calendar).map_err(WriteError::Storage)?;
    temp.persist_noclobber(&path)
        .map_err(|err| publish_error(err.error, &path))?;
    sync_published(directory, id, path, Revision::from_source(source))
}

#[cfg(unix)]
fn publish_calendar_batch(root: &Path, events: &[(Id, String)]) -> Result<(), WriteError> {
    use std::os::unix::fs::PermissionsExt;

    let destination = root.join("calendar");
    ensure_subdirectory(root, &destination).map_err(WriteError::Storage)?;
    let parent_dir = open_directory(root).map_err(WriteError::Storage)?;
    let before = CalendarDirectorySnapshot::read(&destination).map_err(WriteError::Storage)?;
    let staged = tempfile::Builder::new()
        .prefix(".mcbtodo-calendar-")
        .tempdir_in(root)
        .map_err(|error| storage(error, "staging calendar events in", root))?;

    for name in before.entries.keys() {
        fs::hard_link(destination.join(name), staged.path().join(name)).map_err(|error| {
            storage(
                error,
                "staging existing calendar file",
                &destination.join(name),
            )
        })?;
    }
    for (id, source) in events {
        let name = format!("{id}.md");
        let path = staged.path().join(&name);
        stage(staged.path(), source, None)
            .map_err(WriteError::Storage)?
            .persist_noclobber(&path)
            .map_err(|error| publish_error(error.error, &destination.join(name)))?;
    }
    let linked = CalendarDirectorySnapshot::read(staged.path()).map_err(WriteError::Storage)?;
    if !before
        .entries
        .iter()
        .all(|(name, identity)| linked.entries.get(name) == Some(identity))
    {
        return Err(WriteError::Storage(anyhow!(
            "calendar changed while staging imported events"
        )));
    }
    fs::set_permissions(staged.path(), fs::Permissions::from_mode(before.mode))
        .map_err(|error| storage(error, "setting permissions for", staged.path()))?;
    sync_directory(open_directory(staged.path()).map_err(WriteError::Storage)?)
        .map_err(|error| storage(error, "syncing", staged.path()))?;

    if CalendarDirectorySnapshot::read(&destination).map_err(WriteError::Storage)? != before {
        return Err(WriteError::Storage(anyhow!(
            "calendar changed while staging imported events"
        )));
    }
    exchange_directories(staged.path(), &destination)
        .map_err(|error| storage(error, "publishing calendar events at", &destination))?;

    let result = sync_directory(parent_dir)
        .context("syncing the Vault root after calendar publication")
        .and_then(|()| {
            ensure!(
                CalendarDirectorySnapshot::read(staged.path())? == before,
                "the displaced calendar directory changed during publication"
            );
            Ok(())
        });
    if let Err(source) = result {
        return Err(WriteError::CalendarPublicationUncertain {
            directory: destination,
            source: source.context(format!(
                "previous calendar retained at {}",
                staged.keep().display()
            )),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn publish_calendar_batch(_root: &Path, _events: &[(Id, String)]) -> Result<(), WriteError> {
    Err(WriteError::Storage(anyhow!(
        "atomic calendar publication is unsupported on this platform"
    )))
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct CalendarDirectorySnapshot {
    identity: (u64, u64),
    mode: u32,
    entries: std::collections::BTreeMap<std::ffi::OsString, (u64, u64)>,
}

#[cfg(unix)]
impl CalendarDirectorySnapshot {
    fn read(directory: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir(),
            "expected directory at {}",
            directory.display()
        );
        let mut entries = std::collections::BTreeMap::new();
        for item in fs::read_dir(directory)? {
            let item = item?;
            ensure!(
                item.file_type()?.is_file(),
                "expected regular file at {}",
                item.path().display()
            );
            let file = item.metadata()?;
            entries.insert(item.file_name(), (file.dev(), file.ino()));
        }
        Ok(Self {
            identity: (metadata.dev(), metadata.ino()),
            mode: metadata.mode(),
            entries,
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn exchange_directories(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, from, CWD, to, RenameFlags::EXCHANGE).map_err(Into::into)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn exchange_directories(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic calendar directory exchange unavailable",
    ))
}

fn publish_reminder(root: &Path, id: Id, source: String) -> Result<PersistedDocument, WriteError> {
    ensure_subdirectory(root, &root.join("reminders")).map_err(WriteError::Storage)?;
    let reminders = root.join("reminders");
    let path = reminders.join(format!("{id}.md"));
    let temp = stage(&reminders, &source, None).map_err(WriteError::Storage)?;
    let directory = open_directory(&reminders).map_err(WriteError::Storage)?;
    temp.persist_noclobber(&path)
        .map_err(|err| publish_error(err.error, &path))?;
    sync_published(directory, id, path, Revision::from_source(source))
}

struct LockedRoot {
    root: PathBuf,
    completions: Option<CompletionJournal>,
    _lock: File,
}

fn reconcile_completion(
    journal: &CompletionJournal,
    pending: PendingCompletion,
    vault: &Vault,
) -> Result<(), WriteError> {
    let id = pending.event.work_unit_id;
    let (path, revision, published) = match_pending_completion(vault, pending)?;
    check_on_disk(path, id, revision)?;
    // Both branches rely on this path surviving a subsequent crash.
    let parent = path.parent().expect("work unit has a directory");
    sync_directory(open_directory(parent).map_err(WriteError::Storage)?)
        .map_err(|error| storage(error, "syncing", parent))?;
    if published {
        journal.commit()
    } else {
        journal.abort()
    }
    .context("reconciling the WorkUnit completion journal")
    .map_err(WriteError::Storage)
}

fn match_pending_completion(
    vault: &Vault,
    pending: PendingCompletion,
) -> Result<(&Path, &Revision, bool), WriteError> {
    let id = pending.event.work_unit_id;
    if let Some(unit) = vault.units.get(&id)
        && let Some(revision) = vault.revision(id)
    {
        let digest = revision.digest();
        let published = digest == pending.after;
        if published || digest == pending.before {
            return Ok((&unit.path, revision, published));
        }
    }
    Err(WriteError::CompletionRecoveryConflict {
        id,
        before: pending.before,
        after: pending.after,
    })
}

fn lock(root: &Path) -> Result<File> {
    let path = root.join(".mcbtodo-vault.lock");
    let file = open_lock_file(&path)?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    Ok(file)
}

fn try_lock(root: &Path) -> Result<File> {
    let path = root.join(".mcbtodo-vault.lock");
    let file = open_lock_file(&path)?;
    file.try_lock()
        .map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
            TryLockError::Error(error) => error,
        })
        .with_context(|| format!("locking {}", path.display()))?;
    Ok(file)
}

fn open_lock_file(path: &Path) -> Result<File> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => Ok(file),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            ensure!(
                is_kind(path, fs::FileType::is_file)?,
                "expected a file at {}",
                path.display()
            );
            OpenOptions::new()
                .write(true)
                .open(path)
                .with_context(|| format!("opening {}", path.display()))
        }
        Err(err) => Err(err).with_context(|| format!("creating {}", path.display())),
    }
}

fn check_revision(vault: &Vault, id: Id, expected: &Revision) -> Result<(), WriteError> {
    if vault.revision(id) != Some(expected) {
        return Err(WriteError::StaleRevision { id });
    }
    Ok(())
}

fn check_new_id(vault: &Vault, id: Id) -> Result<(), WriteError> {
    if vault.source(id).is_some() {
        return Err(WriteError::DuplicateId { id });
    }
    Ok(())
}

fn check_on_disk(path: &Path, id: Id, revision: &Revision) -> Result<fs::Metadata, WriteError> {
    let parent = path.parent().expect("document has a directory");
    check_directory(parent).map_err(WriteError::Storage)?;
    let check = |err: io::Error| {
        if err.kind() == io::ErrorKind::NotFound {
            WriteError::StaleRevision { id }
        } else {
            storage(err, "reading", path)
        }
    };
    let metadata = fs::symlink_metadata(path).map_err(check)?;
    if !metadata.is_file() || fs::read(path).map_err(check)? != revision.0.as_bytes() {
        return Err(WriteError::StaleRevision { id });
    }
    Ok(metadata)
}

fn check_directory(dir: &Path) -> Result<()> {
    ensure!(
        is_kind(dir, fs::FileType::is_dir)? && dir.canonicalize()? == dir,
        "expected a canonical directory at {}",
        dir.display()
    );
    Ok(())
}

fn ensure_subdirectory(parent: &Path, child: &Path) -> Result<()> {
    check_directory(parent)?;
    match fs::create_dir(child) {
        Ok(()) => sync_directory(open_directory(parent)?)
            .with_context(|| format!("syncing {}", parent.display())),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => check_directory(child),
        Err(err) => Err(err).with_context(|| format!("creating {}", child.display())),
    }
}

fn publish_child(
    project: &Path,
    subdirectory: &str,
    id: Id,
    source: String,
    verify: impl Fn() -> Result<(), WriteError>,
) -> Result<PersistedDocument, WriteError> {
    let parent = project.parent().expect("project has a directory");
    let child_dir = parent.join(subdirectory);
    verify()?;
    ensure_subdirectory(parent, &child_dir).map_err(WriteError::Storage)?;
    let path = child_dir.join(format!("{id}.md"));
    let temp = stage(&child_dir, &source, None).map_err(WriteError::Storage)?;
    let directory = open_directory(&child_dir).map_err(WriteError::Storage)?;
    verify()?;
    temp.persist_noclobber(&path)
        .map_err(|err| publish_error(err.error, &path))?;
    sync_published(directory, id, path, Revision::from_source(source))
}

fn publish_in_directory(
    root: &Path,
    subdirectory: &str,
    id: Id,
    source: String,
) -> Result<PersistedDocument, WriteError> {
    let child_dir = root.join(subdirectory);
    ensure_subdirectory(root, &child_dir).map_err(WriteError::Storage)?;
    let path = child_dir.join(format!("{id}.md"));
    let temp = stage(&child_dir, &source, None).map_err(WriteError::Storage)?;
    let directory = open_directory(&child_dir).map_err(WriteError::Storage)?;
    temp.persist_noclobber(&path)
        .map_err(|err| publish_error(err.error, &path))?;
    sync_published(directory, id, path, Revision::from_source(source))
}

fn replace(
    path: PathBuf,
    id: Id,
    revision: &Revision,
    updated: Revision,
) -> Result<PersistedDocument, WriteError> {
    let parent = path.parent().expect("work unit has a directory");
    let permissions = check_on_disk(&path, id, revision)?.permissions();
    let temp = stage(parent, &updated.0, Some(permissions)).map_err(WriteError::Storage)?;
    let directory = open_directory(parent).map_err(WriteError::Storage)?;
    check_on_disk(&path, id, revision)?;
    temp.persist(&path)
        .map_err(|err| storage(err.error, "replacing", &path))?;
    sync_published(directory, id, path, updated)
}

fn publish_project(
    root: &Path,
    slug: &str,
    id: Id,
    source: String,
) -> Result<PersistedDocument, WriteError> {
    let projects = root.join("projects");
    ensure_subdirectory(root, &projects).map_err(WriteError::Storage)?;
    // Stage outside the loader's `projects/` scan. A reader sees the whole
    // project directory appear in a single rename.
    let staged =
        tempfile::tempdir_in(root).map_err(|err| storage(err, "staging a project in", root))?;
    let staged_file = staged.path().join("project.md");
    stage(staged.path(), &source, None)
        .map_err(WriteError::Storage)?
        .persist_noclobber(&staged_file)
        .map_err(|err| publish_error(err.error, &staged_file))?;
    sync_directory(open_directory(staged.path()).map_err(WriteError::Storage)?)
        .map_err(|err| storage(err, "syncing", staged.path()))?;

    let destination = projects.join(slug);
    let root_dir = open_directory(root).map_err(WriteError::Storage)?;
    let projects_dir = open_directory(&projects).map_err(WriteError::Storage)?;
    rename_directory_noclobber(staged.path(), &destination)
        .map_err(|err| publish_error(err, &destination))?;
    let document = sync_published(
        projects_dir,
        id,
        destination.join("project.md"),
        Revision::from_source(source),
    )?;
    if let Err(source) = sync_directory(root_dir) {
        return Err(WriteError::PublishedButNotSynced { document, source });
    }
    Ok(document)
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_directory_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn rename_directory_noclobber(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace directory rename is unsupported on this platform",
    ))
}

fn publish_error(error: io::Error, path: &Path) -> WriteError {
    if error.kind() == io::ErrorKind::AlreadyExists {
        WriteError::DestinationExists {
            path: path.to_owned(),
        }
    } else {
        storage(error, "publishing", path)
    }
}

fn open_directory(dir: &Path) -> Result<Option<File>> {
    check_directory(dir)?;
    #[cfg(unix)]
    return File::open(dir)
        .map(Some)
        .with_context(|| format!("opening {} for sync", dir.display()));
    #[cfg(not(unix))]
    Ok(None)
}

fn stage(dir: &Path, source: &str, permissions: Option<fs::Permissions>) -> Result<NamedTempFile> {
    check_directory(dir)?;
    let mut temp = NamedTempFile::new_in(dir)
        .with_context(|| format!("staging an update in {}", dir.display()))?;
    temp.write_all(source.as_bytes())
        .context("writing staged Markdown")?;
    if let Some(permissions) = permissions {
        temp.as_file()
            .set_permissions(permissions)
            .context("setting staged file permissions")?;
    }
    temp.as_file()
        .sync_all()
        .context("syncing staged Markdown")?;
    Ok(temp)
}

fn sync_directory(directory: Option<File>) -> io::Result<()> {
    match directory {
        Some(directory) => directory.sync_all(),
        None => Ok(()),
    }
}

fn sync_published(
    directory: Option<File>,
    id: Id,
    path: PathBuf,
    revision: Revision,
) -> Result<PersistedDocument, WriteError> {
    let document = PersistedDocument { id, path, revision };
    match sync_directory(directory) {
        Ok(()) => Ok(document),
        Err(source) => Err(WriteError::PublishedButNotSynced { document, source }),
    }
}

fn storage(error: io::Error, action: &str, path: &Path) -> WriteError {
    WriteError::Storage(anyhow::Error::new(error).context(format!("{action} {}", path.display())))
}
