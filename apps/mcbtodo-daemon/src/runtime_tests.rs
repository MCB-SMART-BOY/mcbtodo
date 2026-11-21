use std::{
    fmt::Write,
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::bail;
use chrono::TimeDelta;

use super::*;

const APPROVAL_GRAPH: &str = r#"
schema: mcbtodo.workflow/v1
id: test-approval
name: Test approval
entry: review
nodes:
  review: {kind: approval}
  report: {kind: action, action: report.create}
  decline: {kind: action, action: report.create}
edges:
  - {from: review, to: report, when: approved}
  - {from: review, to: decline, when: rejected}
literals:
  - {to: {node: report, port: summary}, value: "Approved"}
  - {to: {node: decline, port: summary}, value: "Declined"}
"#;

const SCRIPT_GRAPH: &str = r#"
schema: mcbtodo.workflow/v1
id: test-script
name: Test script
entry: run
nodes:
  run: {kind: script, runner: bash, source: script.sh, timeout_seconds: 5, capabilities: [], outputs: {}}
"#;

const TASK_CREATE_GRAPH: &str = r#"
schema: mcbtodo.workflow/v1
id: test-task-create
name: Test task creation
entry: create
nodes:
  create: {kind: action, action: task.create}
literals:
  - {to: {node: create, port: project_id}, value: "10000000-0000-4000-8000-000000000001"}
  - {to: {node: create, port: title}, value: "Created in selected workspace"}
"#;

const PROJECT: &str = "10000000-0000-4000-8000-000000000001";

fn agent_review_graph(task_count: usize) -> String {
    let mut graph = String::from(
        r#"
schema: mcbtodo.workflow/v1
id: test-agent-review
name: Test Agent review
entry: agent
capabilities:
  - {kind: agent.invoke, scope: propose}
nodes:
  agent: {kind: agent, capability: propose, outputs: {title: string}}
  review: {kind: approval}
  decline: {kind: action, action: report.create}
"#,
    );
    let mut edges = String::from(
        "edges:\n  - {from: agent, to: review}\n  - {from: review, to: decline, when: rejected}\n",
    );
    let mut bindings = String::from("bindings:\n");
    let mut literals =
        String::from("literals:\n  - {to: {node: decline, port: summary}, value: Declined}\n");
    for index in 0..task_count {
        let id = format!("{}-{index}", "task".repeat(23) + "xx");
        writeln!(graph, "  {id}: {{kind: action, action: task.create}}").unwrap();
        writeln!(edges, "  - {{from: review, to: {id}, when: approved}}").unwrap();
        writeln!(
            bindings,
            "  - {{from: {{node: agent, port: title}}, to: {{node: {id}, port: title}}}}"
        )
        .unwrap();
        writeln!(
            literals,
            "  - {{to: {{node: {id}, port: project_id}}, value: '{PROJECT}'}}"
        )
        .unwrap();
    }
    graph + &edges + &bindings + &literals
}

fn publish(base: &Path, graph: &str) -> Result<String> {
    let path = base.join("graph.yaml");
    fs::write(&path, graph)?;
    Ok(RevisionStore::new(base).publish(path)?.hash().into())
}

#[test]
fn daemon_vault_watcher_accepts_valid_changes_and_retains_the_last_valid_snapshot() -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let project_dir = paths.vault_root().join("projects/demo");
    fs::create_dir_all(&project_dir)?;
    let project_path = project_dir.join("project.md");
    fs::write(
        &project_path,
        format!("---\nschema: mcbtodo.project/v1\nid: {PROJECT}\n---\n\n# Initial\n"),
    )?;
    let reminder_dir = paths.vault_root().join("reminders");
    let reminder_path = reminder_dir.join("review.md");
    let reminder_document = |title: &str| {
        format!(
            "---\nschema: mcbtodo.reminder/v1\nid: 20000000-0000-4000-8000-000000000001\nschedule:\n  kind: once\n  at:\n    value: '2099-11-21T09:00:00Z'\n    timezone: UTC\n---\n\n# {title}\n"
        )
    };

    let mut runtime = Runtime::open_with_paths(&paths.state_dir(), paths.clone())?;
    assert_eq!(
        runtime
            .validated_vault
            .as_ref()
            .context("initial Vault snapshot was not loaded")?
            .outline()[0]
            .title,
        "Initial"
    );
    assert!(runtime.reminder_snapshot().1.is_empty());

    fs::write(
        &project_path,
        format!("---\nschema: mcbtodo.project/v1\nid: {PROJECT}\n---\n\n# Updated\n"),
    )?;
    wait_for_vault_watch(&mut runtime, "Updated", false)?;
    assert_eq!(
        runtime
            .validated_vault
            .as_ref()
            .context("updated Vault snapshot was not loaded")?
            .outline()[0]
            .title,
        "Updated"
    );

    fs::create_dir_all(&reminder_dir)?;
    fs::write(&reminder_path, reminder_document("Initial reminder"))?;
    wait_for_reminder_title(&mut runtime, "Initial reminder")?;
    fs::write(&reminder_path, reminder_document("Updated reminder"))?;
    wait_for_reminder_title(&mut runtime, "Updated reminder")?;
    fs::write(&project_path, "invalid Markdown")?;
    wait_for_vault_watch(&mut runtime, "Updated", true)?;
    assert_eq!(
        runtime
            .validated_vault
            .as_ref()
            .context("last valid Vault snapshot was discarded")?
            .outline()[0]
            .title,
        "Updated"
    );
    assert_eq!(runtime.reminder_snapshot().1[0].title, "Updated reminder");
    assert!(runtime.last_vault_error.is_some());
    Ok(())
}

#[test]
fn production_runtime_uses_paths_from_one_workspace() -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let _runtime = Runtime::open(paths.clone())?;

    assert!(paths.state_dir().join("workflow.sqlite3").exists());
    Ok(())
}

#[test]
fn oversized_worker_trace_fails_run_and_invalidates_started_attempt() -> Result<()> {
    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"printf '{}'\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    let pending = request_run(&mut runtime, enqueue(id, revision))?;
    let revision = runtime.revisions.load(&pending.revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &revision)? else {
        bail!("expected an executable node");
    };
    runtime
        .store
        .transition(&pending, RunState::Running, Some(&job.node), None)?;
    let running = runtime.run(id)?;
    let attempt =
        runtime
            .store
            .begin_worker_attempt(&running, &job.node, running.step_count + 1)?;
    runtime.store.mark_worker_started(&attempt)?;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()?;
    child.wait()?;
    let (_sender, receiver) = mpsc::sync_channel(1);
    let worker = ActiveWorker {
        run_id: id,
        job,
        attempt: attempt.clone(),
        agent_deadline: None,
        child,
        stdin: None,
        receiver,
        result: None,
        cancel_sent_at: None,
    };

    runtime.finish_worker_job(
        &running,
        &worker,
        JobResult {
            outcome: NodeOutcome::Failed {
                message: "x".repeat(mcbtodo_protocol::worker::MAX_WORKER_FRAME_BYTES as usize),
            },
            artifact: None,
        },
        Duration::ZERO,
    )?;

    assert_eq!(runtime.run(id)?.state, RunState::Failed);
    assert_eq!(
        runtime.store.worker_attempt_state(attempt.id)?.as_deref(),
        Some("invalidated")
    );
    Ok(())
}

#[test]
fn task_creation_uses_the_runtime_workspace_paths() -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let project_dir = paths.vault_root().join("projects/demo");
    fs::create_dir_all(&project_dir)?;
    fs::write(
        project_dir.join("project.md"),
        format!("---\nschema: mcbtodo.project/v1\nid: {PROJECT}\n---\n\n# Demo\n"),
    )?;

    let revision = publish(&paths.state_dir(), TASK_CREATE_GRAPH)?;
    let mut runtime = Runtime::open_with_paths(&paths.state_dir(), paths.clone())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;
    runtime.tick()?;
    assert!(runtime.worker.is_none());
    assert_eq!(runtime.run(id)?.state, RunState::Pending);

    let deadline = Instant::now() + Duration::from_secs(2);
    while matches!(
        runtime.run(id)?.state,
        RunState::Pending | RunState::Running
    ) && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
        runtime
            .tick()
            .context("ticking daemon while supervising worker")?;
    }

    assert_eq!(runtime.run(id)?.state, RunState::Succeeded);
    let task_dir = paths.vault_root().join("projects/demo/tasks");
    let tasks: Vec<_> = fs::read_dir(task_dir)?.collect::<std::io::Result<_>>()?;
    assert_eq!(tasks.len(), 1);
    assert!(fs::read_to_string(tasks[0].path())?.contains("# Created in selected workspace"));
    Ok(())
}

#[test]
fn daemon_supervises_worker_process_and_persists_its_result() -> Result<()> {
    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), &agent_review_graph(1))?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Err(error) = runtime.tick() {
            bail!(
                "daemon tick failed: {error:#}; current run: {:?}",
                runtime.run(id)?
            );
        }
        if runtime.run(id)?.state != RunState::Pending && runtime.worker.is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::Failed);
    let traces = runtime.store.traces(id)?;
    assert!(traces.iter().any(|trace| trace.contains("failed")));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn worker_spawn_failure_fails_the_claimed_run_and_invalidates_its_attempt() -> Result<()> {
    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"exit 0\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;
    let pending = runtime.run(id)?;
    let revision = runtime.revisions.load(&pending.revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &revision)? else {
        bail!("expected an executable node");
    };
    let JobKind::Script {
        source,
        runner: ScriptRunner::Bash,
        timeout_seconds,
        ..
    } = &job.kind
    else {
        bail!("expected a Bash script node");
    };
    let source_bytes = revision
        .script_sources()
        .get(source)
        .context("published script bytes are missing")?
        .clone();

    runtime.spawn_worker_at(
        &pending,
        job.clone(),
        WorkerJob::Script {
            source: source_bytes,
            runner: WorkerScriptRunner::Bash,
            timeout_seconds: *timeout_seconds,
            inputs: worker_inputs(&job)?,
            executable: None,
        },
        None,
        base.path().join("missing-worker"),
    )?;

    let failed = runtime.run(id)?;
    assert_eq!(failed.state, RunState::Failed);
    assert!(
        failed
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Could not start mcbtodo-worker"))
    );
    assert!(runtime.store.traces(id)?.is_empty());
    assert!(runtime.worker.is_none());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn malformed_worker_readiness_frame_fails_closed_in_production_supervisor() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"exit 0\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;
    let pending = runtime.run(id)?;
    let revision = runtime.revisions.load(&pending.revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &revision)? else {
        bail!("expected an executable node");
    };
    let JobKind::Script {
        source,
        runner: ScriptRunner::Bash,
        timeout_seconds,
        ..
    } = &job.kind
    else {
        bail!("expected a Bash script node");
    };
    let source_bytes = revision
        .script_sources()
        .get(source)
        .context("published script bytes are missing")?
        .clone();

    let malformed_worker = base.path().join("malformed-worker");
    fs::write(
        &malformed_worker,
        b"#!/bin/sh\nprintf '\\000\\000\\000\\001x'\n",
    )?;
    let mut permissions = fs::metadata(&malformed_worker)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&malformed_worker, permissions)?;

    runtime.spawn_worker_at(
        &pending,
        job.clone(),
        WorkerJob::Script {
            source: source_bytes,
            runner: WorkerScriptRunner::Bash,
            timeout_seconds: *timeout_seconds,
            inputs: worker_inputs(&job)?,
            executable: None,
        },
        None,
        malformed_worker,
    )?;

    let failed = runtime.run(id)?;
    assert_eq!(failed.state, RunState::Failed);
    assert!(
        failed
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Worker failed readiness handshake"))
    );
    assert_eq!(failed.step_count, 0);
    assert!(runtime.store.traces(id)?.is_empty());
    assert!(runtime.worker.is_none());
    Ok(())
}

fn wait_for_reminder_title(runtime: &mut Runtime, expected_title: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        runtime.refresh_vault_snapshot();
        if runtime
            .reminder_snapshot()
            .1
            .iter()
            .any(|reminder| reminder.title == expected_title)
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    bail!("timed out waiting for reminder snapshot title {expected_title:?}")
}

fn wait_for_vault_watch(
    runtime: &mut Runtime,
    expected_title: &str,
    expect_error: bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        runtime.refresh_vault_snapshot();
        let has_expected_title = runtime
            .validated_vault
            .as_ref()
            .is_some_and(|vault| vault.outline()[0].title == expected_title);
        if has_expected_title && runtime.last_vault_error.is_some() == expect_error {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    bail!("daemon did not process the Vault change")
}

fn request_run(runtime: &mut Runtime, request: WorkflowRequest) -> Result<RunSummary> {
    match runtime.handle(request) {
        Response::Workflow(WorkflowResponse::Run(run)) => Ok(run),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn enqueue(run_id: Uuid, revision: String) -> WorkflowRequest {
    WorkflowRequest::Enqueue {
        run_id,
        revision,
        due_at: None,
    }
}

fn record_agent_proposal(runtime: &mut Runtime, revision: &str, title: &str) -> Result<Uuid> {
    let id = Uuid::new_v4();
    let run = request_run(runtime, enqueue(id, revision.into()))?;
    let revision = runtime.revisions.load(revision)?;
    let Progress::Next { job, .. } = runtime.progress(&run, &revision)? else {
        bail!("expected an Agent job");
    };
    assert!(matches!(job.kind, JobKind::Agent { .. }));
    let step = job.complete(NodeOutcome::Succeeded {
        outputs: BTreeMap::from([("title".into(), Scalar::String(title.into()))]),
    })?;
    runtime.record(&run, &step, None, Duration::ZERO)?;
    Ok(id)
}

#[test]
fn task_review_is_persisted_and_missing_review_prevents_a_write() -> Result<()> {
    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), &agent_review_graph(1))?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = record_agent_proposal(&mut runtime, &revision, "  Fix \"demo\"  ")?;
    runtime.tick()?;
    let waiting = runtime.run(id)?;
    assert_eq!(waiting.state, RunState::Waiting);
    let prompt = waiting.detail.as_deref().context("missing task review")?;
    assert!(prompt.contains(&format!(
        "in project {PROJECT} with title \"Fix \\\"demo\\\"\""
    )));
    drop(runtime);

    let mut runtime = Runtime::open_for_test(base.path())?;
    assert_eq!(runtime.run(id)?, waiting);
    runtime.store.transition(
        &waiting,
        RunState::Waiting,
        Some("review"),
        Some("Approve workflow node `review`?"),
    )?;
    let approve = WorkflowRequest::Decide {
        run_id: id,
        node: "review".into(),
        approved: true,
    };
    assert!(matches!(
        runtime.handle(approve.clone()),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert_eq!(runtime.run(id)?, waiting);
    let approved = request_run(&mut runtime, approve)?;
    assert_eq!(approved.state, RunState::Pending);
    let traces = runtime.store.traces(id)?;
    let step: StepTrace = serde_json::from_str(&traces[1])?;
    assert_eq!(step.approval_review.as_deref(), Some(prompt));

    let unreviewed = record_agent_proposal(&mut runtime, &revision, "Unreviewed task")?;
    let mut incomplete_step = step;
    incomplete_step.approval_review = None;
    runtime.record(
        &runtime.run(unreviewed)?,
        &incomplete_step,
        None,
        Duration::ZERO,
    )?;
    request_run(&mut runtime, WorkflowRequest::Cancel { run_id: id })?;
    drop(runtime);

    let mut runtime = Runtime::open_for_test(base.path())?;
    runtime.tick()?;
    let run = runtime.run(unreviewed)?;
    assert_eq!(run.state, RunState::Failed);
    assert!(
        run.detail
            .unwrap_or_default()
            .contains("required task review")
    );
    assert!(runtime.worker.is_none());
    Ok(())
}

#[test]
fn a_run_list_page_of_full_task_reviews_fits_the_existing_ipc_frame() -> Result<()> {
    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), &agent_review_graph(4))?;
    let error = publish(base.path(), &agent_review_graph(5)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("can review at most 4 task creations")
    );
    let mut runtime = Runtime::open_for_test(base.path())?;
    let title = "\\".repeat(128);
    let page_size = crate::run_store::RUN_PAGE_SIZE;
    for _ in 0..=page_size {
        let id = record_agent_proposal(&mut runtime, &revision, &title)?;
        runtime.tick()?;
        assert_eq!(runtime.run(id)?.state, RunState::Waiting);
    }
    let response = runtime.handle(WorkflowRequest::List { before: None });
    let Response::Workflow(WorkflowResponse::Runs { runs, next_before }) = &response else {
        bail!("expected a run-list page");
    };
    assert_eq!(runs.len(), page_size);
    assert!(next_before.is_some());
    assert!(
        runs.iter()
            .all(|run| run.detail.as_ref().unwrap().lines().count() == 5)
    );
    let mut frame = Vec::new();
    wire::write(&mut frame, &response)?;
    Ok(())
}

#[test]
fn approval_survives_restart_and_rejection_produces_a_report() -> Result<()> {
    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), APPROVAL_GRAPH)?;
    let id = Uuid::new_v4();
    let mut runtime = Runtime::open_for_test(base.path())?;
    let request = enqueue(id, revision.clone());
    assert_eq!(
        request_run(&mut runtime, request.clone())?.state,
        RunState::Pending
    );
    runtime.tick()?;
    assert_eq!(runtime.run(id)?.state, RunState::Waiting);
    drop(runtime);

    let mut runtime = Runtime::open_for_test(base.path())?;
    assert_eq!(request_run(&mut runtime, request)?.state, RunState::Waiting);
    assert!(matches!(
        runtime.handle(WorkflowRequest::Decide {
            run_id: id,
            node: "wrong".into(),
            approved: true,
        }),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    let decide = WorkflowRequest::Decide {
        run_id: id,
        node: "review".into(),
        approved: false,
    };
    assert_eq!(
        request_run(&mut runtime, decide.clone())?.state,
        RunState::Pending
    );
    assert!(matches!(
        runtime.handle(decide),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    runtime.tick()?;
    runtime.tick()?;
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(run.step_count, 3);
    let report = runtime.store.step(id, 1)?.context("missing report")?;
    assert_eq!(report["trace"]["node"], "decline");
    assert_eq!(report["artifact"]["media_type"], "text/markdown");
    assert_eq!(report["artifact"]["size_bytes"], 8);
    assert_eq!(
        runtime.store.artifact(id, 1, 0)?,
        Some((b"Declined".to_vec(), None))
    );
    let skipped = runtime.store.step(id, 2)?.context("missing skipped step")?;
    assert_eq!(skipped["trace"]["status"], "skipped");
    assert!(skipped["artifact"].is_null());
    Ok(())
}

#[test]
fn enqueue_deduplicates_and_a_future_run_can_be_cancelled() -> Result<()> {
    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), APPROVAL_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    let scheduled = WorkflowRequest::Enqueue {
        run_id: id,
        revision: revision.clone(),
        due_at: Some(Utc::now() + TimeDelta::hours(1)),
    };
    let first = request_run(&mut runtime, scheduled.clone())?;
    assert_eq!(request_run(&mut runtime, scheduled)?, first);
    assert!(matches!(
        runtime.handle(enqueue(id, revision)),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    runtime.tick()?;
    assert_eq!(runtime.run(id)?.state, RunState::Pending);
    let cancel = WorkflowRequest::Cancel { run_id: id };
    assert_eq!(
        request_run(&mut runtime, cancel.clone())?.state,
        RunState::Cancelled
    );
    assert_eq!(
        request_run(&mut runtime, cancel)?.state,
        RunState::Cancelled
    );
    assert_eq!(runtime.wait_timeout()?, None);
    Ok(())
}

#[test]
fn cancelled_running_worker_does_not_commit_a_late_outcome() -> Result<()> {
    {
        let base = tempfile::tempdir()?;
        let revision = publish(base.path(), APPROVAL_GRAPH)?;
        let mut runtime = Runtime::open_for_test(base.path())?;
        let id = Uuid::new_v4();
        request_run(&mut runtime, enqueue(id, revision))?;
        let pending = runtime.run(id)?;
        let revision = runtime.revisions.load(&pending.revision)?;
        let Progress::Next { job, .. } = advance(revision.plan(), &[])? else {
            bail!("expected an executable node");
        };
        runtime
            .store
            .transition(&pending, RunState::Running, Some(&job.node), None)?;
        let running = runtime.run(id)?;
        let attempt =
            runtime
                .store
                .begin_worker_attempt(&running, &job.node, running.step_count + 1)?;
        runtime.store.mark_worker_started(&attempt)?;
        runtime
            .store
            .transition(&running, RunState::Cancelling, Some(&job.node), None)?;
        let cancelling = runtime.run(id)?;
        runtime.mark_worker_unknown(
            &cancelling,
            "Cancellation raced with active work; side effects may have occurred",
        )?;

        let run = runtime.run(id)?;
        assert_eq!(run.state, RunState::OutcomeUnknown);
        assert_eq!(run.step_count, cancelling.step_count);
        assert_eq!(run.current_node, cancelling.current_node);
        assert_eq!(attempt.run_id, id);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn terminating_worker_kills_its_process_group_and_reaps_the_worker() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let descendant_pid_path = directory.path().join("descendant.pid");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "sleep 30 & echo $! > {}; wait",
            descendant_pid_path.display()
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(2);
    while !descendant_pid_path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let descendant_pid: u32 = fs::read_to_string(&descendant_pid_path)?.trim().parse()?;

    terminate_worker(&mut child)?;

    let descendant_stat = Path::new("/proc")
        .join(descendant_pid.to_string())
        .join("stat");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = fs::read_to_string(&descendant_stat)
            .ok()
            .and_then(|stat| stat.split_whitespace().nth(2).map(str::to_owned));
        if state.as_deref().is_none_or(|state| state == "Z") {
            break;
        }
        if Instant::now() >= deadline {
            bail!("Worker descendant remained alive after process-group termination");
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn cancelling_running_script_through_daemon_reaps_worker_and_records_unknown() -> Result<()> {
    let base = tempfile::tempdir()?;
    fs::write(
        base.path().join("script.sh"),
        b"while :; do :; done &\nwait\n",
    )?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let paths = WorkspacePaths::from_home(base.path().to_owned());
    paths.initialize()?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    runtime.store.set_authorization(&revision, "run", true)?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let (worker_pid, descendants) = loop {
        runtime.tick()?;
        let run = runtime.run(id)?;
        if run.state == RunState::Running
            && let Some(worker) = runtime.worker.as_ref()
        {
            let worker_pid = worker.child.id();
            let descendants = process_descendants(worker_pid);
            if !descendants.is_empty() {
                break (worker_pid, descendants);
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "script Worker or sandbox descendants did not start: state={:?}, detail={:?}",
                run.state,
                run.detail
            );
        }
        thread::sleep(Duration::from_millis(10));
    };

    let cancelling = request_run(&mut runtime, WorkflowRequest::Cancel { run_id: id })?;
    assert_eq!(cancelling.state, RunState::Cancelling);
    let deadline = Instant::now() + WORKER_CANCEL_GRACE + Duration::from_secs(3);
    while Instant::now() < deadline {
        runtime.tick()?;
        if runtime.worker.is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    assert!(runtime.worker.is_none(), "cancelled Worker was not reaped");
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::OutcomeUnknown);
    assert_eq!(run.step_count, cancelling.step_count);
    assert!(
        run.detail.is_some(),
        "uncertain cancellation needs a diagnostic"
    );
    assert!(
        !Path::new("/proc")
            .join(worker_pid.to_string())
            .join("stat")
            .exists(),
        "Worker process {worker_pid} remained after cancellation and reap"
    );
    for pid in descendants {
        let stat = Path::new("/proc").join(pid.to_string()).join("stat");
        let state = fs::read_to_string(&stat)
            .ok()
            .and_then(|stat| stat.split_whitespace().nth(2).map(str::to_owned));
        assert!(
            state.as_deref().is_none_or(|state| state == "Z"),
            "sandbox descendant {pid} remained alive after cancellation: {state:?}"
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn worker_crash_after_start_becomes_unknown_and_is_not_replayed_after_runtime_restart() -> Result<()>
{
    let base = tempfile::tempdir()?;
    fs::write(
        base.path().join("script.sh"),
        b"while :; do :; done &\nwait\n",
    )?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let paths = WorkspacePaths::from_home(base.path().to_owned());
    paths.initialize()?;
    let id = Uuid::new_v4();
    let mut runtime = Runtime::open_for_test(base.path())?;
    runtime.store.set_authorization(&revision, "run", true)?;
    request_run(&mut runtime, enqueue(id, revision))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let (worker_pid, descendants) = loop {
        runtime.tick()?;
        if runtime.run(id)?.state == RunState::Running
            && let Some(worker) = runtime.worker.as_ref()
        {
            let descendants = process_descendants(worker.child.id());
            if !descendants.is_empty() {
                break (worker.child.id(), descendants);
            }
        }
        if Instant::now() >= deadline {
            bail!("Worker did not start its sandbox workload");
        }
        thread::sleep(Duration::from_millis(10));
    };

    use rustix::process::{Pid, Signal, kill_process_group};
    let pid = Pid::from_raw(worker_pid as i32).context("invalid Worker pid")?;
    kill_process_group(pid, Signal::KILL)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while runtime.worker.is_some() && Instant::now() < deadline {
        runtime.tick()?;
        thread::sleep(Duration::from_millis(10));
    }
    assert!(runtime.worker.is_none(), "crashed Worker was not reaped");
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::OutcomeUnknown);
    assert_eq!(run.step_count, 0);
    assert!(runtime.store.traces(id)?.is_empty());
    drop(runtime);

    let recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    assert_eq!(recovered.run(id)?.step_count, 0);
    assert!(recovered.store.traces(id)?.is_empty());
    for pid in descendants {
        let stat = Path::new("/proc").join(pid.to_string()).join("stat");
        let state = fs::read_to_string(&stat)
            .ok()
            .and_then(|stat| stat.split_whitespace().nth(2).map(str::to_owned));
        assert!(
            state.as_deref().is_none_or(|state| state == "Z"),
            "sandbox descendant {pid} remained alive after Worker crash: {state:?}"
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn mismatched_worker_result_after_start_becomes_unknown_without_replay() -> Result<()> {
    use std::os::unix::process::CommandExt;

    fn shell_printf(bytes: &[u8]) -> String {
        let escaped = bytes
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect::<String>();
        format!("printf '{escaped}'")
    }

    let base = tempfile::tempdir()?;
    fs::write(
        base.path().join("script.sh"),
        b"# identity mismatch fixture\n",
    )?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;

    let pending = runtime.run(id)?;
    let revision = runtime.revisions.load(&pending.revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &revision)? else {
        bail!("expected an executable node");
    };
    runtime
        .store
        .transition(&pending, RunState::Running, Some(&job.node), None)?;
    let running = runtime.run(id)?;
    let attempt =
        runtime
            .store
            .begin_worker_attempt(&running, &job.node, running.step_count + 1)?;
    runtime.store.mark_worker_started(&attempt)?;

    let mut mismatched_identity = attempt_identity(&attempt)?;
    mismatched_identity.attempt_id = Uuid::new_v4();
    let mut frame = Vec::new();
    mcbtodo_protocol::worker::write(
        &mut frame,
        &WorkerMessage::Result {
            identity: mismatched_identity,
            outcome: WorkerOutcome::Succeeded {
                outputs: Default::default(),
            },
            artifact: None,
        },
    )?;
    let mut child = std::process::Command::new("sh")
        .args(["-c", &shell_printf(&frame)])
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let worker_pid = child.id();
    let stdout = child
        .stdout
        .take()
        .context("fixture stdout was not piped")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || read_worker_messages(stdout, sender));
    let worker = ActiveWorker {
        run_id: id,
        job,
        attempt,
        agent_deadline: None,
        child,
        stdin: None,
        receiver,
        result: None,
        cancel_sent_at: None,
    };

    runtime.worker = Some(worker);
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime.run(id)?.state == RunState::Running && Instant::now() < deadline {
        runtime.tick()?;
        thread::sleep(Duration::from_millis(5));
    }
    let uncertain = runtime.run(id)?;
    assert_eq!(uncertain.state, RunState::OutcomeUnknown);
    assert_eq!(uncertain.step_count, 0);
    assert!(runtime.store.traces(id)?.is_empty());
    assert!(runtime.worker.is_none());
    assert!(
        !Path::new("/proc")
            .join(worker_pid.to_string())
            .join("stat")
            .exists(),
        "mismatched-result Worker process {worker_pid} was not reaped"
    );
    drop(runtime);

    let mut recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    recovered.tick()?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    assert_eq!(recovered.run(id)?.step_count, 0);
    assert!(recovered.store.traces(id)?.is_empty());
    assert!(recovered.worker.is_none());
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_descendants(pid: u32) -> Vec<u32> {
    let children_path = |pid: u32| {
        Path::new("/proc")
            .join(pid.to_string())
            .join("task")
            .join(pid.to_string())
            .join("children")
    };
    let mut descendants = Vec::new();
    let mut pending = vec![pid];
    while let Some(parent) = pending.pop() {
        let Ok(children) = fs::read_to_string(children_path(parent)) else {
            continue;
        };
        for child in children
            .split_whitespace()
            .filter_map(|child| child.parse::<u32>().ok())
        {
            descendants.push(child);
            pending.push(child);
        }
    }
    descendants
}

#[test]
fn result_commit_failure_becomes_unknown_without_replay_after_restart() -> Result<()> {
    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"# result failure fixture\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;
    let pending = runtime.run(id)?;
    let revision = runtime.revisions.load(&pending.revision)?;
    let Progress::Next { job, .. } = advance(revision.plan(), &[])? else {
        bail!("expected an executable node");
    };
    runtime
        .store
        .transition(&pending, RunState::Running, Some(&job.node), None)?;
    let running = runtime.run(id)?;
    let attempt =
        runtime
            .store
            .begin_worker_attempt(&running, &job.node, running.step_count + 1)?;
    runtime.store.mark_worker_started(&attempt)?;
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()?;
    child.wait()?;
    let worker = ActiveWorker {
        run_id: id,
        job,
        attempt,
        agent_deadline: None,
        child,
        stdin: None,
        receiver: mpsc::sync_channel(1).1,
        result: None,
        cancel_sent_at: None,
    };
    let db = rusqlite::Connection::open(base.path().join("workflow.sqlite3"))?;
    db.execute_batch(
        "CREATE TRIGGER reject_step BEFORE INSERT ON workflow_steps
         BEGIN SELECT RAISE(ABORT, 'injected result commit failure'); END;",
    )?;

    runtime.finish_worker_job(
        &running,
        &worker,
        JobResult {
            outcome: NodeOutcome::Succeeded {
                outputs: Default::default(),
            },
            artifact: None,
        },
        Duration::ZERO,
    )?;
    assert_eq!(runtime.run(id)?.state, RunState::OutcomeUnknown);
    assert_eq!(runtime.store.traces(id)?.len(), 0);
    drop(runtime);
    drop(db);

    let mut recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    recovered.tick()?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    assert_eq!(recovered.store.traces(id)?.len(), 0);
    assert!(recovered.worker.is_none());
    Ok(())
}

#[test]
fn authorization_is_for_one_exact_published_node_and_no_interrupted_claim_is_replayed() -> Result<()>
{
    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"# original\n")?;
    let original = publish(base.path(), SCRIPT_GRAPH)?;
    fs::write(base.path().join("script.sh"), b"# updated\n")?;
    let updated = publish(base.path(), SCRIPT_GRAPH)?;
    assert_ne!(original, updated);

    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    assert!(matches!(
        runtime.handle(WorkflowRequest::AuthorizeScript {
            revision: original.clone(),
            node: "run".into(),
            granted: true,
        }),
        Response::Workflow(WorkflowResponse::ScriptAuthorization { granted: true, .. })
    ));
    request_run(&mut runtime, enqueue(id, updated))?;
    runtime.tick()?;
    let denied = runtime.run(id)?;
    assert_eq!(denied.state, RunState::Failed);
    assert!(
        denied
            .detail
            .unwrap_or_default()
            .contains("requires authorization")
    );
    assert_eq!(denied.step_count, 0);
    assert!(runtime.worker.is_none());

    let claimed = request_run(&mut runtime, enqueue(Uuid::new_v4(), original))?;
    runtime
        .store
        .transition(&claimed, RunState::Running, Some("run"), None)?;
    drop(runtime);
    let mut runtime = Runtime::open_for_test(base.path())?;
    assert_eq!(runtime.run(claimed.id)?.state, RunState::OutcomeUnknown);
    runtime.tick()?;
    assert_eq!(runtime.run(claimed.id)?.state, RunState::OutcomeUnknown);
    assert!(runtime.worker.is_none());

    let resolution = WorkflowRequest::ResolveUnknown {
        run_id: claimed.id,
        outcome: mcbtodo_protocol::workflow::ObservedOutcome::NotStarted,
        note: "Verified that the external operation did not start.".into(),
    };
    assert!(matches!(
        runtime.handle(resolution.clone()),
        Response::Workflow(WorkflowResponse::Resolution(_))
    ));
    let inspected = runtime.run(claimed.id)?;
    assert_eq!(inspected.state, RunState::OutcomeUnknown);
    assert_eq!(
        inspected
            .resolution
            .as_ref()
            .map(|resolution| &resolution.outcome),
        Some(&mcbtodo_protocol::workflow::ObservedOutcome::NotStarted)
    );
    assert!(matches!(
        runtime.handle(resolution),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    let invalid_note_run = Uuid::new_v4();
    runtime
        .store
        .insert(invalid_note_run, &"c".repeat(64), None)?;
    let pending = runtime.run(invalid_note_run)?;
    runtime
        .store
        .transition(&pending, RunState::OutcomeUnknown, Some("node"), None)?;
    assert!(matches!(
        runtime.handle(WorkflowRequest::ResolveUnknown {
            run_id: invalid_note_run,
            outcome: mcbtodo_protocol::workflow::ObservedOutcome::Completed,
            note: "  ".into(),
        }),
        Response::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
    Ok(())
}

#[test]
fn recurring_delivery_survives_restart_and_records_lateness_and_overlap() -> Result<()> {
    use mcbtodo_protocol::workflow::{Missed, Overlap, Trigger};

    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), APPROVAL_GRAPH)?;
    let id = Uuid::new_v4();
    let anchor = chrono::DateTime::from_timestamp(Utc::now().timestamp() + 60, 0).unwrap();
    let add = WorkflowRequest::AddSchedule {
        id,
        revision,
        trigger: Trigger::Interval {
            anchor,
            seconds: 60,
        },
        missed: Missed::RunOnce,
        overlap: Overlap::Skip,
    };
    let mut runtime = Runtime::open_for_test(base.path())?;
    let first = runtime.command(add.clone()).map_err(|error| error.source)?;
    assert!(
        matches!(&first, WorkflowResponse::Schedule(summary) if summary.next_at == Some(anchor))
    );
    assert_eq!(runtime.command(add).map_err(|error| error.source)?, first);
    drop(runtime);

    let mut runtime = Runtime::open_for_test(base.path())?;
    let through = anchor + TimeDelta::minutes(3) + TimeDelta::seconds(5);
    runtime.store.schedule_tick(through)?;
    runtime.store.schedule_tick(through)?;
    let (runs, cursor) = runtime.store.list(None)?;
    assert_eq!(cursor, None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].due_at, Some(anchor + TimeDelta::minutes(3)));
    assert_eq!(runs[0].schedule_id, Some(id));

    runtime
        .store
        .schedule_tick(anchor + TimeDelta::minutes(4))?;
    let schedule = runtime.store.schedule(id)?.context("missing schedule")?;
    assert_eq!(schedule.missed_occurrences, 4);
    assert_eq!(schedule.overlap_skipped, 1);
    assert_eq!(schedule.next_at, Some(anchor + TimeDelta::minutes(5)));
    assert_eq!(runtime.store.list(None)?.0.len(), 1);
    let disabled = runtime.store.disable_schedule(schedule)?;
    assert!(!disabled.enabled && disabled.next_at.is_none());
    runtime.store.schedule_tick(anchor + TimeDelta::days(1))?;
    assert_eq!(runtime.store.list(None)?.0.len(), 1);
    Ok(())
}

#[test]
fn nightly_tokyo_cron_recovers_only_the_latest_missed_run() -> Result<()> {
    use mcbtodo_protocol::workflow::{Fold, Missed, Overlap, Trigger};

    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), APPROVAL_GRAPH)?;
    let schedule_id = Uuid::new_v4();
    let mut runtime = Runtime::open_for_test(base.path())?;
    let response = runtime
        .command(WorkflowRequest::AddSchedule {
            id: schedule_id,
            revision: revision.clone(),
            trigger: Trigger::Cron {
                expression: "0 23 * * *".into(),
                timezone: "Asia/Tokyo".into(),
                fold: Fold::Earlier,
            },
            missed: Missed::RunOnce,
            overlap: Overlap::Skip,
        })
        .map_err(|error| error.source)?;
    let WorkflowResponse::Schedule(schedule) = response else {
        anyhow::bail!("expected a registered Cron schedule")
    };
    let first = schedule.next_at.context("Cron has no next occurrence")?;
    assert_eq!(first.time().to_string(), "14:00:00");
    drop(runtime);

    let mut runtime = Runtime::open_for_test(base.path())?;
    let latest = first + TimeDelta::days(2);
    let restart = latest + TimeDelta::hours(1);
    runtime.store.schedule_tick(restart)?;
    runtime.store.schedule_tick(restart)?;
    let (runs, cursor) = runtime.store.list(None)?;
    assert_eq!(cursor, None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].revision, revision);
    assert_eq!(runs[0].due_at, Some(latest));
    assert_eq!(runs[0].schedule_id, Some(schedule_id));
    let schedule = runtime
        .store
        .schedule(schedule_id)?
        .context("missing nightly schedule")?;
    assert_eq!(schedule.missed_occurrences, 3);
    assert_eq!(schedule.next_at, Some(latest + TimeDelta::days(1)));
    Ok(())
}
