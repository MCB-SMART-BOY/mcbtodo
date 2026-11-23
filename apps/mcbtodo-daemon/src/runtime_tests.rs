use std::{
    fmt::Write,
    fs, io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::bail;
use chrono::TimeDelta;

use super::*;
use crate::worker_process::LinuxWorkerProcess;

#[test]
fn workflow_storage_requires_atomic_batches_and_a_durable_cursor() -> Result<()> {
    for (atomic_batches, durable_cursor, diagnostic) in [
        (false, true, "atomic batches"),
        (true, false, "durable completion cursor"),
    ] {
        let error = require_workflow_storage(WorkflowStorageCapabilities {
            backend_id: "test-store",
            atomic_batches,
            durable_cursor,
        })
        .expect_err("the daemon must reject insufficient storage");
        assert!(error.to_string().contains("test-store"));
        assert!(error.to_string().contains(diagnostic));
    }

    let dir = tempfile::tempdir()?;
    let sqlite =
        mcbtodo_storage_sqlite::run_store::RunStore::open(&dir.path().join("workflow.sqlite3"))?;
    require_workflow_storage(sqlite.capabilities())?;
    Ok(())
}

struct FinishedWorker;

impl ProcessController for FinishedWorker {
    fn id(&self) -> u32 {
        0
    }

    fn try_wait(&mut self) -> io::Result<Option<WorkerExit>> {
        Ok(Some(WorkerExit::Success))
    }

    fn terminate(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn worker_release_fences_start_and_preserves_uncertainty_with_a_substitute_store() {
    use std::sync::Mutex;

    struct Store {
        state: Mutex<&'static str>,
        fail_start: bool,
    }

    impl WorkflowRunStore for Store {
        fn claim_attempt(&self, _: ClaimAttempt<'_>) -> Result<WorkerAttempt> {
            panic!("release receives an existing claim");
        }

        fn claim_agent(&self, _: ClaimAttempt<'_>, _: bool) -> Result<AgentAttemptClaim> {
            panic!("release receives an existing claim");
        }

        fn mark_started(&self, _: &WorkerAttempt) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            assert_eq!(*state, "claimed");
            if self.fail_start {
                bail!("injected storage error");
            }
            *state = "started";
            Ok(())
        }

        fn commit_result(&self, _: AttemptResult<'_>) -> Result<()> {
            panic!("release cannot commit a result");
        }

        fn fail_attempt(&self, _: &WorkerAttempt, detail: &str) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            assert_eq!(*state, "claimed");
            assert!(detail.contains("injected storage error"));
            *state = "failed";
            Ok(())
        }

        fn cancel_claimed_attempt(&self, _: &WorkerAttempt) -> Result<()> {
            panic!("release cannot cancel a claim");
        }

        fn mark_unknown(&self, _: &WorkerAttempt, detail: &str) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            assert_eq!(*state, "started");
            assert!(detail.contains("Start delivery failed"));
            *state = "unknown";
            Ok(())
        }
    }

    struct StartPipe<'a> {
        store: &'a Store,
        data: Vec<u8>,
        fail_delivery: bool,
    }

    impl io::Write for StartPipe<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            assert_eq!(*self.store.state.lock().unwrap(), "started");
            if self.fail_delivery {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.data.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl WorkerInput for StartPipe<'_> {
        fn set_deadline(&mut self, _: Instant) {}
    }

    let attempt = WorkerAttempt {
        id: Uuid::from_u128(1),
        run_id: Uuid::from_u128(2),
        revision: "a".repeat(64),
        node: "run".into(),
        epoch: 1,
        expected_steps: 1,
    };
    let identity = attempt_identity(&attempt).unwrap();
    for (fail_start, fail_delivery, state) in [
        (false, false, "started"),
        (true, false, "failed"),
        (false, true, "unknown"),
    ] {
        let store = Store {
            state: Mutex::new("claimed"),
            fail_start,
        };
        let mut pipe = StartPipe {
            store: &store,
            data: Vec::new(),
            fail_delivery,
        };
        let released = release_worker(
            &store,
            &attempt,
            identity.clone(),
            &mut pipe,
            Instant::now() + WORKER_START_TIMEOUT,
            Duration::from_secs(1),
        )
        .unwrap()
        .is_some();
        assert_eq!(released, !fail_start && !fail_delivery);
        assert_eq!(*store.state.lock().unwrap(), state);
        let data = pipe.data;
        if released {
            let message =
                mcbtodo_protocol::worker::read::<DaemonMessage>(&mut data.as_slice()).unwrap();
            assert!(matches!(message, DaemonMessage::Start { identity: sent } if sent == identity));
        } else {
            assert!(data.is_empty());
        }
    }
}

#[test]
fn agent_worker_timeout_rounds_up_to_cover_the_daemon_budget() {
    assert_eq!(agent_worker_timeout_seconds(Duration::ZERO).unwrap(), 1);
    assert_eq!(
        agent_worker_timeout_seconds(Duration::from_secs(1)).unwrap(),
        1
    );
    assert_eq!(
        agent_worker_timeout_seconds(Duration::from_millis(1_001)).unwrap(),
        2
    );
}

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
    let _server = crate::server::Server::open(paths.clone(), crate::DEFAULT_WORKER_SLOTS)?;

    assert!(paths.focus_database_path().exists());
    assert!(paths.socket_path().exists());
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
    let attempt = runtime.store.claim_attempt(ClaimAttempt {
        run_id: running.id,
        revision: &running.revision,
        node: &job.node,
        expected_steps: running.step_count + 1,
    })?;
    runtime.store.mark_started(&attempt)?;
    let (_sender, receiver) = mpsc::sync_channel(1);
    let worker = ActiveWorker {
        run_id: id,
        job,
        attempt: attempt.clone(),
        deadline: WorkerDeadline::Script(Instant::now() + WORKER_START_TIMEOUT),
        child: Box::new(FinishedWorker),
        stdin: None,
        receiver,
        result: None,
        cancel_sent_at: None,
        exit: None,
    };

    runtime.finish_worker_job(
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
        worker_attempt_state(base.path(), attempt.id)?.as_deref(),
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
    assert!(runtime.workers.is_empty());
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
        if runtime.run(id)?.state != RunState::Pending && runtime.workers.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::Failed);
    let traces = runtime.store.history(id)?;
    assert!(
        traces
            .iter()
            .any(|trace| trace.status == NodeStatus::Failed)
    );
    Ok(())
}

#[test]
fn sandbox_setup_failure_persists_a_failed_step_without_an_artifact() -> Result<()> {
    let base = tempfile::tempdir()?;
    let home = base.path().join("worker-home");
    fs::create_dir(&home)?;
    let paths = WorkspacePaths::from_home(home);
    paths.initialize()?;
    // Keep the supervisor database in a separate directory so only the
    // sandbox's scratch setup fails when the Worker's state path is a file.
    fs::remove_dir(paths.state_dir())?;
    fs::write(paths.state_dir(), b"not a state directory")?;
    fs::write(
        base.path().join("script.sh"),
        b"printf '{}' > \"$PROD_OUTPUT_JSON\"\n",
    )?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_with_paths(base.path(), paths.clone())?;
    runtime.store.set_authorization(&revision, "run", true)?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        runtime.tick()?;
        let run = runtime.run(id)?;
        if !matches!(run.state, RunState::Pending | RunState::Running) {
            assert_eq!(run.state, RunState::Failed, "{run:?}");
            assert!(runtime.workers.is_empty());
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "Worker did not finish: {run:?}");
        thread::sleep(Duration::from_millis(10));
    }

    drop(runtime);
    let runtime = Runtime::open_with_paths(base.path(), paths)?;
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.step_count, 1);
    let step = runtime
        .store
        .recorded_step(id, 0)?
        .context("missing failed step")?;
    assert_eq!(step.trace.status, NodeStatus::Failed);
    assert!(
        step.trace
            .reason
            .contains("sandbox could not start the script"),
        "{step:?}"
    );
    assert!(step.artifact.is_none());
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
    assert!(runtime.store.history(id)?.is_empty());
    assert!(runtime.workers.is_empty());
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
    // Keep stdin open until the supervisor handles the malformed frame.
    // An early exit can race the Prepare write and test a broken pipe instead.
    fs::write(
        &malformed_worker,
        b"#!/bin/sh\nprintf '\\000\\000\\000\\001x'\nexec /bin/cat >/dev/null\n",
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
        malformed_worker,
    )?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while !runtime.workers.is_empty() && Instant::now() < deadline {
        runtime.tick()?;
        thread::sleep(Duration::from_millis(10));
    }
    let failed = runtime.run(id)?;
    assert_eq!(failed.state, RunState::Failed);
    assert!(
        failed.detail.as_deref().is_some_and(|detail| {
            detail.contains("Worker failed readiness handshake")
                && detail.contains("worker IPC JSON error")
        }),
        "unexpected failure detail: {:?}",
        failed.detail
    );
    assert_eq!(failed.step_count, 0);
    assert!(runtime.store.history(id)?.is_empty());
    assert!(runtime.workers.is_empty());
    Ok(())
}

#[test]
fn worker_exit_before_ready_fails_the_claim_even_while_a_descendant_holds_the_pipes() -> Result<()>
{
    use std::os::unix::fs::PermissionsExt;

    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

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
    let binary = base.path().join("worker-exits-before-ready");
    // A background command may get /dev/null as stdin. Preserve the Prepare
    // pipe on a separate descriptor through the fork and stdin redirection.
    fs::write(
        &binary,
        b"#!/bin/sh\nexec 3<&0\n/bin/sleep 10 <&3 &\nprintf '%s\\n' \"$!\" > \"$HOME/descendant.pid\"\nexit 23\n",
    )?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    let attempt = runtime
        .store
        .claim_attempt(worker_claim(&pending, &job.node)?)?;
    runtime.launch_claimed_worker(
        attempt.clone(),
        job,
        WorkerJob::Script {
            source: b"exit 0\n".into(),
            runner: WorkerScriptRunner::Bash,
            timeout_seconds: 1,
            inputs: Default::default(),
            executable: None,
        },
        None,
        Ok(binary),
    )?;

    let Some(WorkerSlot::Starting(worker)) = runtime.workers.front() else {
        bail!("expected a starting Worker");
    };
    let pid = Pid::from_raw(i32::try_from(worker.child.id())?).context("invalid fixture PID")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )?
    .is_none()
    {
        anyhow::ensure!(Instant::now() < deadline, "fixture Worker did not exit");
        thread::sleep(Duration::from_millis(5));
    }
    let descendant = fs::read_to_string(base.path().join("descendant.pid"))?
        .trim()
        .parse()?;
    runtime.tick()?;

    let failed = runtime.run(id)?;
    assert_eq!(failed.state, RunState::Failed);
    assert_eq!(failed.detail.as_deref(), Some("Worker exited before Start"));
    assert_eq!(failed.step_count, 0);
    assert_eq!(
        worker_attempt_state(base.path(), attempt.id)?.as_deref(),
        Some("invalidated")
    );
    assert!(runtime.store.history(id)?.is_empty());
    assert!(runtime.workers.is_empty());
    wait_until_descendant_exits(descendant)
}

#[test]
fn worker_cancelled_during_prepare_never_crosses_the_start_fence() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"exit 0\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision.clone()))?;
    let pending = runtime.run(id)?;
    let published = runtime.revisions.load(&revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &published)? else {
        bail!("expected an executable node");
    };
    let binary = base.path().join("worker-waiting-before-ready");
    fs::write(&binary, b"#!/bin/sh\nexec /bin/sleep 8\n")?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    let attempt = runtime
        .store
        .claim_attempt(worker_claim(&pending, &job.node)?)?;
    runtime.launch_claimed_worker(
        attempt.clone(),
        job,
        WorkerJob::Script {
            source: b"exit 0\n".to_vec(),
            runner: WorkerScriptRunner::Bash,
            timeout_seconds: 5,
            inputs: Default::default(),
            executable: None,
        },
        None,
        Ok(binary),
    )?;
    let Some(WorkerSlot::Starting(worker)) = runtime.workers.front() else {
        bail!("expected a Worker awaiting Ready");
    };
    let pid = worker.child.id();

    assert_eq!(
        request_run(&mut runtime, WorkflowRequest::Cancel { run_id: id })?.state,
        RunState::Cancelling
    );
    runtime.tick()?;
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::Cancelled);
    assert_eq!(run.detail.as_deref(), Some("Cancelled before Worker Start"));
    assert!(runtime.store.mark_started(&attempt).is_err());
    assert!(runtime.store.history(id)?.is_empty());
    assert!(runtime.workers.is_empty());
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
    drop(runtime);

    let mut recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(
        request_run(&mut recovered, enqueue(id, revision))?.state,
        RunState::Cancelled
    );
    recovered.tick()?;
    assert!(recovered.workers.is_empty());
    Ok(())
}

#[test]
fn worker_that_does_not_read_prepare_releases_the_slot_before_start() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"exit 0\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision.clone()))?;
    let pending = runtime.run(id)?;
    let published = runtime.revisions.load(&revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &published)? else {
        bail!("expected an executable node");
    };
    let binary = base.path().join("worker-ignoring-prepare");
    // Keep stdin open without consuming it. Bound the fixture on failure so
    // that even the old blocking write cannot hang the test suite.
    fs::write(
        &binary,
        b"#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nexec /bin/sleep 8\n",
    )?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    let pid_path = binary.with_extension("pid");
    let next_revision = publish(base.path(), APPROVAL_GRAPH)?;
    let next_id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(next_id, next_revision))?;
    let started_at = Instant::now();
    runtime.spawn_worker_at(
        &pending,
        job,
        WorkerJob::Script {
            source: vec![b' '; 512 * 1024],
            runner: WorkerScriptRunner::Bash,
            timeout_seconds: 5,
            inputs: Default::default(),
            executable: None,
        },
        binary,
    )?;

    assert!(
        started_at.elapsed() < Duration::from_secs(2),
        "Worker startup blocked the daemon loop for {:?}",
        started_at.elapsed()
    );
    assert_eq!(runtime.run(id)?.state, RunState::Running);
    runtime.tick()?;
    assert_eq!(runtime.run(next_id)?.state, RunState::Pending);
    let deadline = started_at + Duration::from_secs(8);
    while runtime.run(id)?.state == RunState::Running && Instant::now() < deadline {
        runtime.tick()?;
        thread::sleep(Duration::from_millis(10));
    }
    let failed = runtime.run(id)?;
    assert_eq!(failed.state, RunState::Failed);
    assert!(
        failed
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("startup deadline")),
        "unexpected failure detail: {:?}",
        failed.detail
    );
    assert!(started_at.elapsed() < Duration::from_secs(8));
    let pid: u32 = fs::read_to_string(pid_path)?.trim().parse()?;
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
    assert!(runtime.workers.is_empty());
    assert!(runtime.store.history(id)?.is_empty());
    runtime.tick()?;
    assert_eq!(runtime.run(next_id)?.state, RunState::Waiting);
    drop(runtime);
    let mut recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(
        request_run(&mut recovered, enqueue(id, revision))?.state,
        RunState::Failed
    );
    recovered.tick()?;
    assert!(recovered.workers.is_empty());
    Ok(())
}

#[test]
fn script_worker_that_ignores_cancel_loses_its_execution_slot_without_replay() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"exit 0\n")?;
    let revision = publish(
        base.path(),
        &SCRIPT_GRAPH.replace("timeout_seconds: 5", "timeout_seconds: 1"),
    )?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision.clone()))?;
    let pending = runtime.run(id)?;
    let published = runtime.revisions.load(&revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &published)? else {
        bail!("expected an executable node");
    };
    let attempt = runtime
        .store
        .claim_attempt(worker_claim(&pending, &job.node)?)?;
    let mut ready = Vec::new();
    mcbtodo_protocol::worker::write(
        &mut ready,
        &WorkerMessage::Ready {
            identity: attempt_identity(&attempt)?,
        },
    )?;
    let mut command = String::from("#!/bin/sh\nprintf '");
    for byte in ready {
        write!(command, "\\{byte:03o}")?;
    }
    command.push_str("'\nexec /bin/sleep 60\n");
    let binary = base.path().join("stalled-worker");
    fs::write(&binary, command)?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;

    runtime.launch_claimed_worker(
        attempt,
        job,
        WorkerJob::Script {
            source: b"exit 0\n".to_vec(),
            runner: WorkerScriptRunner::Bash,
            timeout_seconds: 1,
            inputs: Default::default(),
            executable: None,
        },
        None,
        Ok(binary),
    )?;
    let start_deadline = Instant::now() + WORKER_START_TIMEOUT;
    let pid = loop {
        runtime.tick()?;
        if let Some(WorkerSlot::Active(worker)) = runtime.workers.front() {
            break worker.child.id();
        }
        anyhow::ensure!(Instant::now() < start_deadline, "Worker did not start");
        thread::sleep(Duration::from_millis(10));
    };
    let next_revision = publish(base.path(), APPROVAL_GRAPH)?;
    let next_id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(next_id, next_revision))?;
    runtime.tick()?;
    assert_eq!(runtime.run(next_id)?.state, RunState::Pending);

    let deadline = Instant::now() + Duration::from_secs(8);
    while !runtime.workers.is_empty() && Instant::now() < deadline {
        runtime.tick()?;
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        runtime.workers.is_empty(),
        "stalled script Worker kept the execution slot"
    );
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::OutcomeUnknown);
    assert!(
        run.detail
            .as_deref()
            .unwrap_or_default()
            .contains("script supervision deadline")
    );
    assert!(runtime.store.history(id)?.is_empty());
    assert_eq!(runtime.run(next_id)?.state, RunState::Waiting);
    drop(runtime);

    let mut recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(
        request_run(&mut recovered, enqueue(id, revision))?.state,
        RunState::OutcomeUnknown
    );
    recovered.tick()?;
    assert!(recovered.store.history(id)?.is_empty());
    assert!(recovered.workers.is_empty());
    Ok(())
}

#[test]
fn expired_script_result_is_fenced_at_commit() -> Result<()> {
    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"exit 0\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision.clone()))?;
    let pending = runtime.run(id)?;
    let revision = runtime.revisions.load(&revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &revision)? else {
        bail!("expected an executable node");
    };
    let attempt = runtime
        .store
        .claim_attempt(worker_claim(&pending, &job.node)?)?;
    runtime.store.mark_started(&attempt)?;
    runtime.finish_worker(
        ActiveWorker {
            run_id: id,
            job,
            attempt,
            deadline: WorkerDeadline::Script(Instant::now()),
            child: Box::new(FinishedWorker),
            stdin: None,
            receiver: mpsc::sync_channel(1).1,
            result: None,
            cancel_sent_at: None,
            exit: None,
        },
        AdmittedWorkerResult {
            outcome: WorkerOutcome::Succeeded {
                outputs: Default::default(),
            },
            artifact: None,
        },
    )?;
    assert_eq!(runtime.run(id)?.state, RunState::OutcomeUnknown);
    assert!(runtime.store.history(id)?.is_empty());
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
    runtime.record(&run, &step)?;
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
    let stale = runtime.run(id)?;
    let stale_approval = WorkflowRequest::Decide {
        run_id: id,
        node: "review".into(),
        approved: true,
        review: stale.approval_review(),
    };
    for request in [
        stale_approval.clone(),
        stale_approval,
        WorkflowRequest::Decide {
            run_id: id,
            node: "review".into(),
            approved: true,
            review: None,
        },
    ] {
        assert!(matches!(
            runtime.handle(request),
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert_eq!(runtime.run(id)?, waiting);
        assert_eq!(runtime.store.history(id)?.len(), 1);
    }
    let approved = request_run(
        &mut runtime,
        WorkflowRequest::Decide {
            run_id: id,
            node: "review".into(),
            approved: true,
            review: waiting.approval_review(),
        },
    )?;
    assert_eq!(approved.state, RunState::Pending);
    let step = runtime
        .store
        .history(id)?
        .pop()
        .context("missing approval step")?;
    assert_eq!(step.approval_review.as_deref(), Some(prompt));

    let unreviewed = record_agent_proposal(&mut runtime, &revision, "Unreviewed task")?;
    let mut incomplete_step = step;
    incomplete_step.approval_review = None;
    runtime.record(&runtime.run(unreviewed)?, &incomplete_step)?;
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
    assert!(runtime.workers.is_empty());
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
    let page_size = mcbtodo_storage_sqlite::run_store::RUN_PAGE_SIZE;
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
            review: None,
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
        review: None,
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
    let WorkflowResponse::Step { step: report } = runtime
        .command(WorkflowRequest::Step {
            run_id: id,
            index: 1,
        })
        .map_err(|error| error.source)?
    else {
        bail!("missing report");
    };
    assert_eq!(report["trace"]["node"], "decline");
    assert_eq!(report["artifact"]["media_type"], "text/markdown");
    assert_eq!(report["artifact"]["size_bytes"], 8);
    let WorkflowResponse::Artifact { bytes, next_offset } = runtime
        .command(WorkflowRequest::Artifact {
            run_id: id,
            index: 1,
            offset: 0,
        })
        .map_err(|error| error.source)?
    else {
        bail!("missing artifact");
    };
    assert_eq!(bytes, b"Declined");
    assert_eq!(next_offset, None);
    let skipped = runtime
        .store
        .recorded_step(id, 2)?
        .context("missing skipped step")?;
    assert_eq!(skipped.trace.status, NodeStatus::Skipped);
    assert!(skipped.artifact.is_none());
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
        let attempt = runtime.store.claim_attempt(ClaimAttempt {
            run_id: running.id,
            revision: &running.revision,
            node: &job.node,
            expected_steps: running.step_count + 1,
        })?;
        runtime.store.mark_started(&attempt)?;
        runtime
            .store
            .transition(&running, RunState::Cancelling, Some(&job.node), None)?;
        let cancelling = runtime.run(id)?;
        runtime.store.mark_unknown(
            &attempt,
            "Cancellation raced with active work; side effects may have occurred",
        )?;

        let run = runtime.run(id)?;
        assert_eq!(run.state, RunState::OutcomeUnknown);
        assert_eq!(run.step_count, cancelling.step_count);
        assert_eq!(run.current_node, cancelling.current_node);
        assert_eq!(attempt.run_id, id);
        assert_eq!(
            worker_attempt_state(base.path(), attempt.id)?.as_deref(),
            Some("invalidated")
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn terminating_worker_kills_its_process_group_and_reaps_the_worker() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let descendant_pid_path = directory.path().join("descendant.pid");
    let child = Command::new("sh")
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

    let mut process = LinuxWorkerProcess::new(child);
    process.terminate()?;
    assert_eq!(process.try_wait()?, Some(WorkerExit::Failure));
    process.terminate()?;

    wait_until_descendant_exits(descendant_pid)
}

#[cfg(target_os = "linux")]
#[test]
fn observing_worker_exit_terminates_remaining_group_members() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let descendant_pid_path = directory.path().join("descendant.pid");
    let child = Command::new("sh")
        .arg("-c")
        .arg(r#"sleep 10 & printf '%s\n' "$!" > "$1"; exit 0"#)
        .arg("worker-fixture")
        .arg(&descendant_pid_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let mut process = LinuxWorkerProcess::new(child);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(exit) = process.try_wait()? {
            assert_eq!(exit, WorkerExit::Success);
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "Worker did not exit");
        thread::sleep(Duration::from_millis(5));
    }
    let descendant_pid = fs::read_to_string(&descendant_pid_path)?.trim().parse()?;
    wait_until_descendant_exits(descendant_pid)?;
    assert_eq!(process.try_wait()?, Some(WorkerExit::Success));
    process
        .terminate()
        .context("terminating an already reaped Worker")
}

#[cfg(target_os = "linux")]
fn wait_until_descendant_exits(descendant_pid: u32) -> Result<()> {
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
// The Bash sandbox does not mount sleep. Stop a child after it writes the
// marker so the parent remains in wait until the test injects the fault.
const BLOCKING_SCRIPT: &[u8] =
    b"(printf '{}' > \"$PROD_OUTPUT_JSON\" && kill -STOP \"$BASHPID\") &\nwait\n";

#[cfg(target_os = "linux")]
fn wait_for_script_effect(
    runtime: &mut Runtime,
    id: Uuid,
    state_dir: &Path,
) -> Result<(u32, Vec<u32>)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        runtime.tick()?;
        let run = runtime.run(id)?;
        if run.state == RunState::Running
            && let Some(WorkerSlot::Active(worker)) = runtime.workers.front()
        {
            for entry in fs::read_dir(state_dir)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b"script-sandbox-")
                    && fs::read(entry.path().join("output.json")).is_ok_and(|bytes| bytes == b"{}")
                {
                    let worker_pid = worker.child.id();
                    let descendants = process_descendants(worker_pid);
                    if !descendants.is_empty() {
                        return Ok((worker_pid, descendants));
                    }
                }
            }
        }
        if !matches!(run.state, RunState::Pending | RunState::Running) || Instant::now() >= deadline
        {
            let history = runtime.store.history(id)?;
            bail!(
                "sandbox script did not write its marker: state={:?}, detail={:?}, trace={history:?}",
                run.state,
                run.detail
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cancelling_running_script_through_daemon_reaps_worker_and_records_unknown() -> Result<()> {
    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), BLOCKING_SCRIPT)?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let paths = WorkspacePaths::from_home(base.path().to_owned());
    paths.initialize()?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    runtime.store.set_authorization(&revision, "run", true)?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;

    let (worker_pid, descendants) = wait_for_script_effect(&mut runtime, id, &paths.state_dir())?;

    let cancelling = request_run(&mut runtime, WorkflowRequest::Cancel { run_id: id })?;
    assert_eq!(cancelling.state, RunState::Cancelling);
    let deadline = Instant::now() + WORKER_CANCEL_GRACE + Duration::from_secs(3);
    while Instant::now() < deadline {
        runtime.tick()?;
        if runtime.workers.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    assert!(
        runtime.workers.is_empty(),
        "cancelled Worker was not reaped"
    );
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
    fs::write(base.path().join("script.sh"), BLOCKING_SCRIPT)?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let paths = WorkspacePaths::from_home(base.path().to_owned());
    paths.initialize()?;
    let id = Uuid::new_v4();
    let mut runtime = Runtime::open_for_test(base.path())?;
    runtime.store.set_authorization(&revision, "run", true)?;
    request_run(&mut runtime, enqueue(id, revision))?;

    let (worker_pid, descendants) = wait_for_script_effect(&mut runtime, id, &paths.state_dir())?;

    use rustix::process::{Pid, Signal, kill_process_group};
    let pid = Pid::from_raw(worker_pid as i32).context("invalid Worker pid")?;
    kill_process_group(pid, Signal::KILL)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !runtime.workers.is_empty() && Instant::now() < deadline {
        runtime.tick()?;
        thread::sleep(Duration::from_millis(10));
    }
    assert!(runtime.workers.is_empty(), "crashed Worker was not reaped");
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::OutcomeUnknown);
    assert_eq!(run.step_count, 0);
    assert!(runtime.store.history(id)?.is_empty());
    drop(runtime);

    let recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    assert_eq!(recovered.run(id)?.step_count, 0);
    assert!(recovered.store.history(id)?.is_empty());
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
    let attempt = runtime.store.claim_attempt(ClaimAttempt {
        run_id: running.id,
        revision: &running.revision,
        node: &job.node,
        expected_steps: running.step_count + 1,
    })?;
    runtime.store.mark_started(&attempt)?;

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
        deadline: WorkerDeadline::Script(Instant::now() + WORKER_START_TIMEOUT),
        child: Box::new(LinuxWorkerProcess::new(child)),
        stdin: None,
        receiver,
        result: None,
        cancel_sent_at: None,
        exit: None,
    };

    runtime.workers.push_back(WorkerSlot::Active(worker));
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime.run(id)?.state == RunState::Running && Instant::now() < deadline {
        runtime.tick()?;
        thread::sleep(Duration::from_millis(5));
    }
    let uncertain = runtime.run(id)?;
    assert_eq!(uncertain.state, RunState::OutcomeUnknown);
    assert_eq!(uncertain.step_count, 0);
    assert!(runtime.store.history(id)?.is_empty());
    assert!(runtime.workers.is_empty());
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
    assert!(recovered.store.history(id)?.is_empty());
    assert!(recovered.workers.is_empty());
    Ok(())
}

#[test]
fn runtime_wakes_at_worker_deadlines_instead_of_the_next_poll() -> Result<()> {
    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), APPROVAL_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision))?;
    let pending = runtime.run(id)?;
    let revision = runtime.revisions.load(&pending.revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &revision)? else {
        bail!("expected an executable node");
    };
    let attempt = runtime
        .store
        .claim_attempt(worker_claim(&pending, &job.node)?)?;
    let (_sender, handshake) = mpsc::sync_channel(1);
    runtime
        .workers
        .push_back(WorkerSlot::Starting(StartingWorker {
            job,
            attempt,
            child: Box::new(FinishedWorker),
            startup_deadline: Instant::now(),
            execution_window: Duration::from_secs(1),
            execution_deadline: WorkerDeadline::Agent,
            handshake,
        }));
    assert_eq!(runtime.wait_timeout()?, Some(Duration::ZERO), "startup");

    let Some(WorkerSlot::Starting(starting)) = runtime.workers.pop_front() else {
        bail!("expected a starting Worker");
    };
    runtime.store.mark_started(&starting.attempt)?;
    let (_sender, receiver) = mpsc::sync_channel(1);
    runtime.workers.push_back(WorkerSlot::Active(ActiveWorker {
        run_id: id,
        job: starting.job,
        attempt: starting.attempt,
        deadline: WorkerDeadline::Agent(Instant::now()),
        child: starting.child,
        stdin: None,
        receiver,
        result: None,
        cancel_sent_at: None,
        exit: None,
    }));
    assert_eq!(runtime.wait_timeout()?, Some(Duration::ZERO), "execution");

    let Some(WorkerSlot::Active(worker)) = runtime.workers.front_mut() else {
        bail!("expected an active Worker");
    };
    worker.cancel_sent_at = Some(Instant::now());
    assert_eq!(
        runtime.wait_timeout()?,
        Some(WORKER_POLL),
        "an elapsed execution deadline must not busy-loop after Cancel",
    );
    let Some(WorkerSlot::Active(worker)) = runtime.workers.front_mut() else {
        bail!("expected an active Worker");
    };
    worker.cancel_sent_at = Some(Instant::now() - WORKER_CANCEL_GRACE);
    assert_eq!(
        runtime.wait_timeout()?,
        Some(Duration::ZERO),
        "cancellation"
    );

    let Some(WorkerSlot::Active(worker)) = runtime.workers.front_mut() else {
        bail!("expected an active Worker");
    };
    worker.cancel_sent_at = None;
    worker.exit = Some(ObservedWorkerExit {
        status: WorkerExit::Success,
        drain_until: Instant::now() + WORKER_RESULT_DRAIN_GRACE,
    });
    assert_eq!(
        runtime.wait_timeout()?,
        Some(Duration::ZERO),
        "execution can expire while draining a reaped Worker",
    );
    Ok(())
}

#[test]
fn worker_execution_budget_includes_start_delivery() -> Result<()> {
    use std::sync::{Arc, Mutex};

    struct LiveWorker;

    impl ProcessController for LiveWorker {
        fn id(&self) -> u32 {
            0
        }

        fn try_wait(&mut self) -> io::Result<Option<WorkerExit>> {
            Ok(None)
        }

        fn terminate(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct DelayedStart {
        observed: Arc<Mutex<Option<(Instant, Instant)>>>,
        deadline: Option<Instant>,
    }

    impl io::Write for DelayedStart {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            *self.observed.lock().unwrap() = Some((Instant::now(), self.deadline.unwrap()));
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            // The Worker can consume Start while the transport is returning.
            thread::sleep(Duration::from_millis(20));
            Ok(())
        }
    }

    impl WorkerInput for DelayedStart {
        fn set_deadline(&mut self, deadline: Instant) {
            self.deadline = Some(deadline);
        }
    }

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
    let attempt = runtime
        .store
        .claim_attempt(worker_claim(&pending, &job.node)?)?;
    let (ready, handshake) = mpsc::sync_channel(1);
    let (_frames, receiver) = mpsc::sync_channel(1);
    let observation = Arc::new(Mutex::new(None));
    ready
        .send(Ok(ReadyWorker {
            identity: attempt_identity(&attempt)?,
            stdin: Box::new(DelayedStart {
                observed: observation.clone(),
                deadline: None,
            }),
            receiver,
        }))
        .map_err(|_| anyhow::anyhow!("handshake receiver closed"))?;
    let window = WORKER_CONTROL_DELIVERY_TIMEOUT / 2;
    let active = runtime
        .poll_worker_start(StartingWorker {
            job,
            attempt,
            child: Box::new(LiveWorker),
            startup_deadline: Instant::now() + WORKER_START_TIMEOUT,
            execution_window: window,
            execution_deadline: WorkerDeadline::Script,
            handshake,
        })?
        .context("expected Worker start to produce an active slot")?;
    runtime.workers.push_back(active);
    let Some(WorkerSlot::Active(worker)) = runtime.workers.front() else {
        bail!("expected an active Worker");
    };
    let (delivered_at, transport_deadline) = observation
        .lock()
        .unwrap()
        .context("Start was not written")?;
    assert!(
        worker.deadline.at() <= delivered_at + window,
        "execution budget grew while Start delivery was returning"
    );
    assert_eq!(
        transport_deadline,
        worker.deadline.at(),
        "Start delivery must end when the shorter execution allowance expires"
    );
    Ok(())
}

#[test]
fn reaped_worker_drains_the_final_result_without_blocking_the_runtime_tick() -> Result<()> {
    assert_reaped_worker_result_fence(false, false)
}

#[test]
fn reaped_worker_result_after_drain_expiry_is_not_committed_or_replayed() -> Result<()> {
    assert_reaped_worker_result_fence(true, false)
}

#[test]
fn cancellation_during_worker_result_drain_fences_the_result() -> Result<()> {
    assert_reaped_worker_result_fence(false, true)
}

fn assert_reaped_worker_result_fence(expired: bool, cancelled: bool) -> Result<()> {
    let base = tempfile::tempdir()?;
    fs::write(base.path().join("script.sh"), b"exit 0\n")?;
    let revision = publish(base.path(), SCRIPT_GRAPH)?;
    let mut runtime = Runtime::open_for_test(base.path())?;
    let id = Uuid::new_v4();
    request_run(&mut runtime, enqueue(id, revision.clone()))?;
    let pending = runtime.run(id)?;
    let revision = runtime.revisions.load(&revision)?;
    let Progress::Next { job, .. } = runtime.progress(&pending, &revision)? else {
        bail!("expected an executable node");
    };
    runtime
        .store
        .transition(&pending, RunState::Running, Some(&job.node), None)?;
    let attempt = runtime
        .store
        .claim_attempt(worker_claim(&pending, &job.node)?)?;
    runtime.store.mark_started(&attempt)?;
    let identity = attempt_identity(&attempt)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    runtime.workers.push_back(WorkerSlot::Active(ActiveWorker {
        run_id: id,
        job,
        attempt,
        deadline: WorkerDeadline::Script(Instant::now() + WORKER_START_TIMEOUT),
        child: Box::new(FinishedWorker),
        stdin: None,
        receiver,
        result: None,
        cancel_sent_at: None,
        exit: None,
    }));

    // The reader cannot deliver its final frame until after this tick.
    runtime.tick()?;
    assert_eq!(runtime.run(id)?.state, RunState::Running);
    assert_eq!(runtime.run(id)?.step_count, 0);
    assert!(!runtime.workers.is_empty(), "the slot must remain occupied");

    if expired {
        let Some(WorkerSlot::Active(ActiveWorker {
            exit: Some(exit), ..
        })) = runtime.workers.front_mut()
        else {
            bail!("expected a reaped Worker in the supervised slot");
        };
        exit.drain_until = Instant::now();
        assert_eq!(runtime.wait_timeout()?, Some(Duration::ZERO));
    }
    if cancelled {
        let run = request_run(&mut runtime, WorkflowRequest::Cancel { run_id: id })?;
        assert_eq!(run.state, RunState::Cancelling);
    }
    sender.send(Ok(WorkerMessage::Result {
        identity,
        outcome: WorkerOutcome::Succeeded {
            outputs: Default::default(),
        },
        artifact: None,
    }))?;
    runtime.tick()?;
    let (state, step_count) = if expired || cancelled {
        (RunState::OutcomeUnknown, 0)
    } else {
        (RunState::Succeeded, 1)
    };
    assert_eq!(runtime.run(id)?.state, state);
    assert_eq!(runtime.run(id)?.step_count, step_count);
    assert!(runtime.workers.is_empty());
    drop(runtime);
    let mut recovered = Runtime::open_for_test(base.path())?;
    recovered.tick()?;
    assert_eq!(recovered.run(id)?.state, state);
    assert_eq!(recovered.run(id)?.step_count, step_count);
    assert!(recovered.workers.is_empty());
    Ok(())
}

#[test]
fn worker_protocol_error_delivered_during_exit_check_is_preserved() -> Result<()> {
    struct ExitingWorker(Option<mpsc::SyncSender<Result<WorkerMessage>>>);

    impl ProcessController for ExitingWorker {
        fn id(&self) -> u32 {
            0
        }

        fn try_wait(&mut self) -> io::Result<Option<WorkerExit>> {
            self.0
                .take()
                .unwrap()
                .send(Err(anyhow::anyhow!("malformed result frame")))
                .map_err(io::Error::other)?;
            Ok(Some(WorkerExit::Success))
        }

        fn terminate(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let base = tempfile::tempdir()?;
    let revision = publish(base.path(), APPROVAL_GRAPH)?;
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
    let attempt = runtime.store.claim_attempt(ClaimAttempt {
        run_id: id,
        revision: &pending.revision,
        node: &job.node,
        expected_steps: pending.step_count + 1,
    })?;
    runtime.store.mark_started(&attempt)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    assert!(
        runtime
            .poll_worker(ActiveWorker {
                run_id: id,
                job,
                attempt,
                deadline: WorkerDeadline::Script(Instant::now() + WORKER_START_TIMEOUT),
                child: Box::new(ExitingWorker(Some(sender))),
                stdin: None,
                receiver,
                result: None,
                cancel_sent_at: None,
                exit: None,
            })?
            .is_none()
    );
    let run = runtime.run(id)?;
    assert_eq!(run.state, RunState::OutcomeUnknown);
    assert_eq!(run.step_count, 0);
    assert_eq!(
        run.detail.as_deref(),
        Some("Worker protocol failed: malformed result frame")
    );
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
    let attempt = runtime.store.claim_attempt(ClaimAttempt {
        run_id: running.id,
        revision: &running.revision,
        node: &job.node,
        expected_steps: running.step_count + 1,
    })?;
    runtime.store.mark_started(&attempt)?;
    let worker = ActiveWorker {
        run_id: id,
        job,
        attempt,
        deadline: WorkerDeadline::Script(Instant::now() + WORKER_START_TIMEOUT),
        child: Box::new(FinishedWorker),
        stdin: None,
        receiver: mpsc::sync_channel(1).1,
        result: None,
        cancel_sent_at: None,
        exit: None,
    };
    let db = rusqlite::Connection::open(base.path().join("workflow.sqlite3"))?;
    db.execute_batch(
        "CREATE TRIGGER reject_step BEFORE INSERT ON workflow_steps
         BEGIN SELECT RAISE(ABORT, 'injected result commit failure'); END;",
    )?;

    runtime.finish_worker_job(
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
    assert_eq!(runtime.store.history(id)?.len(), 0);
    assert_eq!(
        worker_attempt_state(base.path(), worker.attempt.id)?.as_deref(),
        Some("invalidated")
    );
    drop(runtime);
    drop(db);

    let mut recovered = Runtime::open_for_test(base.path())?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    recovered.tick()?;
    assert_eq!(recovered.run(id)?.state, RunState::OutcomeUnknown);
    assert_eq!(recovered.store.history(id)?.len(), 0);
    assert!(recovered.workers.is_empty());
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
    assert!(runtime.workers.is_empty());

    let claimed = request_run(&mut runtime, enqueue(Uuid::new_v4(), original))?;
    runtime
        .store
        .transition(&claimed, RunState::Running, Some("run"), None)?;
    drop(runtime);
    let mut runtime = Runtime::open_for_test(base.path())?;
    assert_eq!(runtime.run(claimed.id)?.state, RunState::OutcomeUnknown);
    runtime.tick()?;
    assert_eq!(runtime.run(claimed.id)?.state, RunState::OutcomeUnknown);
    assert!(runtime.workers.is_empty());

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
        .enqueue_if_absent(invalid_note_run, &"c".repeat(64), None)?;
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
    runtime.store.dispatch_due_schedule(through)?;
    runtime.store.dispatch_due_schedule(through)?;
    let (runs, cursor) = runtime.store.list(None)?;
    assert_eq!(cursor, None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].due_at, Some(anchor + TimeDelta::minutes(3)));
    assert_eq!(runs[0].schedule_id, Some(id));

    runtime
        .store
        .dispatch_due_schedule(anchor + TimeDelta::minutes(4))?;
    let schedule = runtime.store.schedule(id)?.context("missing schedule")?;
    assert_eq!(schedule.missed_occurrences, 4);
    assert_eq!(schedule.overlap_skipped, 1);
    assert_eq!(schedule.next_at, Some(anchor + TimeDelta::minutes(5)));
    assert_eq!(runtime.store.list(None)?.0.len(), 1);
    let disabled = runtime
        .store
        .disable_schedule(id)?
        .context("missing schedule")?;
    assert!(!disabled.enabled && disabled.next_at.is_none());
    runtime
        .store
        .dispatch_due_schedule(anchor + TimeDelta::days(1))?;
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
    runtime.store.dispatch_due_schedule(restart)?;
    runtime.store.dispatch_due_schedule(restart)?;
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

fn worker_attempt_state(state_dir: &Path, id: Uuid) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let db = rusqlite::Connection::open(state_dir.join("workflow.sqlite3"))?;
    Ok(db
        .query_row(
            "SELECT state FROM workflow_attempts WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?)
}
