use std::{fs, path::PathBuf, time::Instant};

use mcbtodo_domain::{EvidenceRef, WorkUnitCompleted};
use mcbtodo_vault::CompleteWorkUnit;
use rusqlite::Connection;

use super::*;

const PROJECT: Uuid = Uuid::from_u128(1);
const TASK: Uuid = Uuid::from_u128(2);
const UNIT: Uuid = Uuid::from_u128(3);
const OTHER: Uuid = Uuid::from_u128(4);

struct Fixture {
    _home: tempfile::TempDir,
    paths: WorkspacePaths,
    revision: String,
}

impl Fixture {
    fn new() -> Result<Self> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().into());
        paths.initialize()?;
        let project = paths.vault_root().join("projects/project");
        fs::create_dir_all(project.join("tasks"))?;
        fs::create_dir_all(project.join("units"))?;
        fs::write(
            project.join("project.md"),
            format!("---\nschema: mcbtodo.project/v1\nid: {PROJECT}\n---\n\n# Project\n"),
        )?;
        fs::write(
            project.join("tasks/task.md"),
            format!(
                "---\nschema: mcbtodo.task/v1\nid: {TASK}\nproject_id: {PROJECT}\n---\n\n# Task\n"
            ),
        )?;
        let source = home.path().join("report.yaml");
        fs::write(
            &source,
            "schema: mcbtodo.workflow/v1\nid: report\nname: Report\nentry: report\n\
             nodes:\n  report: {kind: action, action: report.create}\n\
             literals:\n  - {to: {node: report, port: summary}, value: Completed}\n",
        )?;
        let revision = RevisionStore::new(paths.state_dir())
            .publish(source)?
            .hash()
            .into();
        let fixture = Self {
            _home: home,
            paths,
            revision,
        };
        fixture.set_status(UNIT, "todo")?;
        fixture.set_status(OTHER, "todo")?;
        Ok(fixture)
    }

    fn runtime(&self) -> Result<Runtime> {
        let mut runtime = Runtime::open_with_paths(&self.paths.state_dir(), self.paths.clone())?;
        runtime.completions = CompletionPoller::new(self.paths.clone());
        Ok(runtime)
    }

    fn vault(&self) -> VaultStore {
        VaultStore::new(self.paths.vault_root(), chrono_tz::UTC)
    }

    fn set_status(&self, id: Uuid, status: &str) -> Result<()> {
        fs::write(
            self.paths
                .vault_root()
                .join(format!("projects/project/units/{id}.md")),
            format!(
                "---\nschema: mcbtodo.work-unit/v1\nid: {id}\nproject_id: {PROJECT}\nparent_task_id: {TASK}\nstatus: {status}\n---\n\n# Unit\n"
            ),
        )?;
        Ok(())
    }

    fn complete(&self, id: Uuid, evidence: &[EvidenceRef]) -> Result<()> {
        let expected_revision = Vault::load(&self.paths.vault_root(), chrono_tz::UTC)?
            .revision(id)
            .context("missing unit")?
            .clone();
        self.vault().complete_work_unit_with_evidence(
            CompleteWorkUnit {
                id,
                expected_revision,
            },
            evidence,
        )?;
        Ok(())
    }

    fn events(&self) -> Result<Vec<WorkUnitCompleted>> {
        Ok(self.vault().work_unit_completed_events()?)
    }

    // These delivery tests create only local events. Wait for the fixture lock
    // before handing the recorded page directly to the delivery store.
    fn delivery_page(&self, after: i64) -> Result<Vec<(i64, WorkUnitCompleted)>> {
        Ok(self
            .vault()
            .work_unit_completion_export_page(after)?
            .into_iter()
            .map(|record| (record.sequence, record.event))
            .collect())
    }

    fn request(&self, id: Uuid) -> WorkflowRequest {
        WorkflowRequest::AddSchedule {
            id,
            revision: self.revision.clone(),
            trigger: Trigger::WorkUnitCompleted { work_unit_id: UNIT },
            overlap: Overlap::Queue,
            missed: Missed::Skip,
        }
    }

    fn rule(&self, runtime: &mut Runtime, id: Uuid) -> Result<ScheduleSummary> {
        match runtime.handle(self.request(id)) {
            Response::Workflow(WorkflowResponse::Schedule(rule)) => Ok(rule),
            other => anyhow::bail!("expected completion rule, got {other:?}"),
        }
    }

    fn workflow_db(&self) -> PathBuf {
        self.paths.state_dir().join("workflow.sqlite3")
    }
}

fn poll_until(
    runtime: &mut Runtime,
    mut ready: impl FnMut(&Runtime) -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        runtime.tick()?;
        if ready(runtime)? {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "completion trigger did not reach expected state"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cutoff_and_matching_survive_retry_and_offline_completion() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.complete(UNIT, &[])?;
    let mut runtime = fixture.runtime()?;
    let first_rule = Uuid::new_v4();
    assert_eq!(fixture.rule(&mut runtime, first_rule)?.next_at, None);
    assert!(runtime.wait_timeout()?.is_some());
    drop(runtime);

    fixture.set_status(UNIT, "todo")?;
    fixture.complete(UNIT, &[])?;
    let eligible = fixture.events()?.last().unwrap().event_id;
    fixture.complete(OTHER, &[])?;

    let mut runtime = fixture.runtime()?;
    assert_eq!(fixture.rule(&mut runtime, first_rule)?.id, first_rule);
    let late_rule = Uuid::new_v4();
    fixture.rule(&mut runtime, late_rule)?;
    poll_until(&mut runtime, |runtime| {
        Ok(runtime
            .store
            .list(None)?
            .0
            .iter()
            .any(|run| run.state == RunState::Succeeded))
    })?;
    let runs = runtime.store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.schedule_id, Some(first_rule));
    assert_eq!(run.source_event_id, Some(eligible));
    assert_eq!(run.revision, fixture.revision);
    assert_eq!(run.due_at, None);
    assert_eq!(run.source_run_id, None);
    drop(runtime);

    let mut runtime = fixture.runtime()?;
    assert_eq!(
        runtime.store.get(run.id)?.unwrap().source_event_id,
        Some(eligible)
    );
    assert_eq!(runtime.store.next_completion_cursor()?, Some(3));
    runtime.tick()?;
    assert_eq!(runtime.store.list(None)?.0.len(), 1);
    Ok(())
}

#[test]
fn markdown_and_evidence_only_changes_do_not_queue_runs() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    fixture.rule(&mut runtime, Uuid::new_v4())?;
    fixture.set_status(UNIT, "completed")?;
    fixture.complete(
        UNIT,
        &[EvidenceRef {
            run_id: Uuid::new_v4(),
            step_index: 0,
        }],
    )?;
    assert!(fixture.events()?.is_empty());
    fixture.complete(OTHER, &[])?;
    poll_until(&mut runtime, |runtime| {
        Ok(runtime.store.next_completion_cursor()? == Some(1))
    })?;
    assert!(runtime.store.list(None)?.0.is_empty());
    Ok(())
}

#[test]
fn imported_completion_does_not_queue_a_run_or_hide_later_local_completions() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    fixture.rule(&mut runtime, Uuid::new_v4())?;
    let vault = fixture.vault();
    let observed = Vault::load(&fixture.paths.vault_root(), chrono_tz::UTC)?
        .revision(UNIT)
        .context("missing unit")?
        .digest();
    let imported = WorkUnitCompleted {
        event_id: Uuid::new_v4(),
        work_unit_id: UNIT,
        project_id: PROJECT,
        parent_task_id: TASK,
        occurred_at: Utc::now(),
        previous_status: mcbtodo_domain::WorkUnitStatus::Todo,
        evidence: Vec::new(),
    };
    assert!(vault.import_work_unit_completion(&imported, &observed, None)?);
    fixture.set_status(UNIT, "todo")?;
    fixture.complete(UNIT, &[])?;
    poll_until(&mut runtime, |runtime| {
        Ok(runtime.store.next_completion_cursor()? == Some(2))
    })?;
    let events = vault.work_unit_completion_export_page(0)?;
    assert_eq!(events[0].event, imported);
    assert_eq!(events[0].origin_device_id, None);
    let runs = runtime.store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].source_event_id, Some(events[1].event.event_id));
    Ok(())
}

#[test]
fn disable_during_page_read_keeps_queued_runs_and_blocks_new_deliveries() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    let id = Uuid::new_v4();
    fixture.rule(&mut runtime, id)?;
    fixture.complete(UNIT, &[])?;
    let pages = fixture.delivery_page(0)?;
    runtime.store.deliver_completion_page(&pages)?;
    let queued = runtime.store.next_pending_at(Utc::now())?.unwrap().id;
    fixture.set_status(UNIT, "todo")?;
    fixture.complete(UNIT, &[])?;
    let stale_page = fixture.delivery_page(1)?;
    let response = runtime.handle(WorkflowRequest::DisableSchedule { id });
    assert!(
        matches!(response, Response::Workflow(WorkflowResponse::Schedule(rule)) if !rule.enabled)
    );
    runtime.store.deliver_completion_page(&stale_page)?;
    assert_eq!(runtime.store.next_completion_cursor()?, None);
    assert_eq!(
        runtime
            .store
            .list(None)?
            .0
            .iter()
            .map(|run| run.id)
            .collect::<Vec<_>>(),
        [queued]
    );
    Ok(())
}

#[test]
fn delivery_rolls_back_receipt_run_and_cursor_and_deduplicates_replayed_event() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    fixture.rule(&mut runtime, Uuid::new_v4())?;
    fixture.complete(UNIT, &[])?;
    let page = fixture.delivery_page(0)?;
    let db = Connection::open(fixture.workflow_db())?;
    db.execute_batch(
        "CREATE TRIGGER force_rollback BEFORE UPDATE OF cursor ON workflow_completion_rules
                       BEGIN SELECT RAISE(ABORT, 'test cursor failure'); END;",
    )?;
    assert!(runtime.store.deliver_completion_page(&page).is_err());
    assert_eq!(runtime.store.next_completion_cursor()?, Some(0));
    assert!(runtime.store.list(None)?.0.is_empty());
    db.execute_batch("DROP TRIGGER force_rollback")?;
    drop(runtime);

    let runtime = fixture.runtime()?;
    runtime.store.deliver_completion_page(&page)?;
    let replayed = [(page[0].0 + 1, page[0].1.clone())];
    runtime.store.deliver_completion_page(&replayed)?;
    assert_eq!(runtime.store.next_completion_cursor()?, Some(2));
    let runs = runtime.store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].source_event_id, Some(page[0].1.event_id));
    Ok(())
}

#[test]
fn unrelated_full_page_does_not_hide_a_later_matching_event() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    fixture.rule(&mut runtime, Uuid::new_v4())?;
    for _ in 0..64 {
        fixture.set_status(OTHER, "todo")?;
        fixture.complete(OTHER, &[])?;
    }
    fixture.complete(UNIT, &[])?;
    let eligible = fixture.events()?.last().unwrap().event_id;
    poll_until(&mut runtime, |runtime| {
        Ok(runtime.store.next_completion_cursor()? == Some(65))
    })?;
    let runs = runtime.store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].source_event_id, Some(eligible));
    Ok(())
}

#[test]
fn busy_journal_reader_backs_off_without_advancing_the_cursor() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    fixture.rule(&mut runtime, Uuid::new_v4())?;
    fixture.complete(UNIT, &[])?;
    let eligible = fixture.events()?.last().unwrap().event_id;
    let lock = fs::File::open(fixture.paths.vault_root().join(".mcbtodo-vault.lock"))?;
    lock.lock()?;

    poll_until(&mut runtime, |runtime| {
        Ok(runtime
            .wait_timeout()?
            .is_some_and(|delay| delay > Duration::from_millis(500)))
    })
    .context("completion reader did not back off while the writer held the lock")?;
    assert_eq!(runtime.store.next_completion_cursor()?, Some(0));
    assert!(runtime.store.list(None)?.0.is_empty());
    drop(lock);

    poll_until(&mut runtime, |runtime| {
        Ok(runtime.store.next_completion_cursor()? == Some(1))
    })?;
    let runs = runtime.store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].source_event_id, Some(eligible));
    Ok(())
}

#[test]
fn journal_read_failure_preserves_cursor_and_retries() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    fixture.rule(&mut runtime, Uuid::new_v4())?;
    fixture.complete(UNIT, &[])?;
    let eligible = fixture.events()?.last().unwrap().event_id;
    let journal = fixture.paths.vault_root().join(".mcbtodo-events.sqlite3");
    let original = fixture._home.path().join("saved-events.sqlite3");
    fs::rename(&journal, &original)?;
    fs::create_dir(&journal)?;

    // Both the request loop and the page reader ran, and the reader backed off.
    poll_until(&mut runtime, |runtime| {
        Ok(runtime
            .wait_timeout()?
            .is_some_and(|delay| delay > Duration::from_millis(500)))
    })?;
    assert_eq!(runtime.store.next_completion_cursor()?, Some(0));
    assert!(runtime.store.list(None)?.0.is_empty());

    fs::remove_dir(&journal)?;
    fs::rename(&original, &journal)?;
    poll_until(&mut runtime, |runtime| {
        Ok(runtime.store.next_completion_cursor()? == Some(1))
    })?;
    let runs = runtime.store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].source_event_id, Some(eligible));
    Ok(())
}

#[test]
fn injected_completion_store_uses_the_registered_cutoff() -> Result<()> {
    use mcbtodo_ports::{CompletionEventStore, CompletionPage, CompletionReadError};

    struct MemoryJournal(WorkUnitCompleted);

    impl CompletionEventStore for MemoryJournal {
        fn watermark(&self) -> Result<i64, CompletionReadError> {
            Ok(41)
        }

        fn page(&self, after: i64) -> Result<CompletionPage, CompletionReadError> {
            match after {
                41 => Ok(vec![(42, self.0.clone())]),
                42 => Ok(Vec::new()),
                _ => Err(CompletionReadError::Storage(format!(
                    "unexpected cursor {after}"
                ))),
            }
        }
    }

    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    let event = WorkUnitCompleted {
        event_id: Uuid::new_v4(),
        work_unit_id: UNIT,
        project_id: PROJECT,
        parent_task_id: TASK,
        occurred_at: Utc::now(),
        previous_status: mcbtodo_domain::WorkUnitStatus::Todo,
        evidence: Vec::new(),
    };
    runtime.completions = CompletionPoller::with_store(MemoryJournal(event.clone()));
    let schedule = fixture.rule(&mut runtime, Uuid::new_v4())?;
    assert_eq!(runtime.store.next_completion_cursor()?, Some(41));
    poll_until(&mut runtime, |runtime| {
        Ok(runtime.store.next_completion_cursor()? == Some(42))
    })?;
    let runs = runtime.store.list(None)?.0;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].source_event_id, Some(event.event_id));
    assert_eq!(runs[0].schedule_id, Some(schedule.id));
    assert!(fixture.events()?.is_empty());
    Ok(())
}

#[test]
fn busy_registration_can_retry_without_a_partial_rule() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut runtime = fixture.runtime()?;
    let lock_path = fixture.paths.vault_root().join(".mcbtodo-vault.lock");
    let lock = fs::File::create(lock_path)?;
    lock.lock()?;
    let id = Uuid::new_v4();
    let request = fixture.request(id);
    let response = thread::scope(|scope| {
        let runtime = &mut runtime;
        let (send, receive) = mpsc::channel();
        scope.spawn(move || send.send(runtime.handle(request)).unwrap());
        let result = receive.recv_timeout(Duration::from_secs(2));
        // Unlock explicitly in case a concurrent process test has forked
        // and briefly holds a copy of this descriptor before exec.
        lock.unlock()?;
        Ok::<_, anyhow::Error>(result?)
    })?;
    assert!(matches!(
        response,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert!(runtime.store.schedule(id)?.is_none());

    assert!(fixture.rule(&mut runtime, id)?.enabled);
    Ok(())
}
