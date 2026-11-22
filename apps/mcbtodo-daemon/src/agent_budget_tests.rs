use std::{
    fs,
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
};

use anyhow::{bail, ensure};
use rusqlite::Connection;

use super::*;

const CHAIN: &str = r#"
schema: mcbtodo.workflow/v1
id: agent-budget
name: Agent budget
entry: first
capabilities:
  - {kind: agent.invoke, scope: summarize}
nodes:
  first: {kind: agent, capability: summarize, outputs: {}}
  second: {kind: agent, capability: summarize, outputs: {}}
  review: {kind: approval}
  third: {kind: agent, capability: summarize, outputs: {}}
  fourth: {kind: agent, capability: summarize, outputs: {}}
  fifth: {kind: agent, capability: summarize, outputs: {}}
  fallback: {kind: action, action: report.create}
  declined: {kind: action, action: report.create}
edges:
  - {from: first, to: second}
  - {from: second, to: review}
  - {from: review, to: third, when: approved}
  - {from: review, to: declined, when: rejected}
  - {from: third, to: fourth}
  - {from: fourth, to: fifth}
  - {from: fifth, to: fallback, when: failure}
literals:
  - {to: {node: fallback, port: summary}, value: unexpected}
  - {to: {node: declined, port: summary}, value: declined}
"#;

const WALL_MS: u64 = 60_000;
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

const CAPACITY_WORKFLOW: &str = r#"
schema: mcbtodo.workflow/v1
id: agent-capacity
name: Agent capacity
entry: call
capabilities:
  - {kind: agent.invoke, scope: summarize}
nodes:
  call: {kind: agent, capability: summarize, outputs: {}}
"#;

struct Fixture {
    _home: tempfile::TempDir,
    paths: WorkspacePaths,
    revision: String,
    listener: UnixListener,
}

impl Fixture {
    fn new(authorized: bool) -> Result<Self> {
        Self::with_workflow(authorized, CHAIN)
    }

    fn with_workflow(authorized: bool, workflow: &str) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let state_dir = paths.state_dir();
        let graph = home.path().join("budget.yaml");
        fs::write(&graph, workflow)?;
        let revision = RevisionStore::new(&state_dir)
            .publish(&graph)?
            .hash()
            .into();
        if authorized {
            fs::write(
                paths.agent_policy_path(),
                format!(
                    "timeout_seconds = 30\n[providers]\nunix_socket = \"state/agent.sock\"\n\
                     [[allow]]\ncapability = \"summarize\"\nrevision = \"{revision}\"\n"
                ),
            )?;
        }
        let listener = UnixListener::bind(state_dir.join("agent.sock"))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            _home: home,
            paths,
            revision,
            listener,
        })
    }

    fn open(&self) -> Result<Runtime> {
        Runtime::open_with_paths(&self.paths.state_dir(), self.paths.clone())
    }

    fn open_with_slots(&self, slots: u8) -> Result<Runtime> {
        Runtime::new(
            &self.paths.state_dir(),
            self.paths.clone(),
            mcbtodo_storage_sqlite::run_store::RunStore::open(
                &self.paths.workflow_database_path(),
            )?,
            crate::worker_process::LinuxWorkerLauncher,
            slots,
        )
    }

    fn enqueue(&self, runtime: &mut Runtime) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let response = runtime.handle(WorkflowRequest::Enqueue {
            run_id: id,
            revision: self.revision.clone(),
            due_at: None,
        });
        ensure!(
            matches!(response, Response::Workflow(WorkflowResponse::Run(_))),
            "{response:?}"
        );
        Ok(id)
    }

    fn db(&self) -> Result<Connection> {
        Ok(Connection::open(
            self.paths.state_dir().join("workflow.sqlite3"),
        )?)
    }

    fn usage(&self, id: Uuid) -> Result<(u32, u64)> {
        Ok(self.db()?.query_row(
            "SELECT attempts, wall_ms FROM workflow_agent_budgets WHERE run_id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, u64::from(row.get::<_, u32>(1)?))),
        )?)
    }

    fn accept_request(&self, runtime: &mut Runtime) -> Result<UnixStream> {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut socket = loop {
            match self.listener.accept() {
                Ok((socket, _)) => break socket,
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    runtime.tick()?;
                    thread::sleep(Duration::from_millis(2));
                }
                Err(err) => return Err(err).context("waiting for local test adapter connection"),
            }
        };
        socket.set_read_timeout(Some(TEST_TIMEOUT))?;
        socket.set_write_timeout(Some(TEST_TIMEOUT))?;
        let mut header = [0; 4];
        socket.read_exact(&mut header)?;
        let size = u32::from_be_bytes(header) as usize;
        ensure!(size <= 16 * 1024, "unexpected request size");
        let mut request = vec![0; size];
        socket.read_exact(&mut request)?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request)?,
            json!({"schema": "mcbtodo.agent/v1", "capability": "summarize", "inputs": {}})
        );
        Ok(socket)
    }

    fn reply(&self, runtime: &mut Runtime) -> Result<()> {
        Self::send_reply(self.accept_request(runtime)?)
    }

    fn send_reply(mut socket: UnixStream) -> Result<()> {
        let body = br#"{"schema":"mcbtodo.agent/v1","outputs":{}}"#;
        socket.write_all(&(body.len() as u32).to_be_bytes())?;
        socket.write_all(body)?;
        Ok(())
    }

    fn assert_no_call(&self) {
        assert_eq!(
            self.listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    fn record_first_call(&self, runtime: &mut Runtime, id: Uuid, elapsed_ms: u64) -> Result<()> {
        let run = runtime.run(id)?;
        let revision = runtime.revisions.load(&run.revision)?;
        let Progress::Next { job, .. } = runtime.progress(&run, &revision)? else {
            bail!("expected the first Agent job");
        };
        let AgentAttemptClaim {
            attempt,
            execution_window: reserved,
        } = runtime
            .store
            .claim_agent(worker_claim(&run, &job.node)?, false)?;
        runtime.store.mark_started(&attempt)?;
        let step = job.complete(NodeOutcome::Succeeded {
            outputs: Default::default(),
        })?;
        runtime.store.commit_result(AttemptResult {
            attempt: &attempt,
            trace: &encode_trace(&step)?,
            artifact: None,
            agent_unused: reserved - Duration::from_millis(elapsed_ms),
        })
    }
}

fn wait_until(
    runtime: &mut Runtime,
    id: Uuid,
    predicate: impl Fn(&RunSummary) -> bool,
) -> Result<RunSummary> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        runtime.tick()?;
        let run = runtime.run(id)?;
        if predicate(&run) {
            return Ok(run);
        }
        ensure!(Instant::now() < deadline, "unexpected run state: {run:?}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn wait_at(runtime: &mut Runtime, id: Uuid, node: &str) -> Result<RunSummary> {
    wait_until(runtime, id, |run| run.current_node.as_deref() == Some(node))
}

#[test]
fn occupied_worker_keeps_the_next_agent_pending_without_claiming_budget() -> Result<()> {
    let fixture = Fixture::with_workflow(true, CAPACITY_WORKFLOW)?;
    let mut runtime = fixture.open()?;
    let first = fixture.enqueue(&mut runtime)?;
    let active_socket = fixture.accept_request(&mut runtime)?;
    let second = fixture.enqueue(&mut runtime)?;
    runtime.tick()?;
    assert_eq!(runtime.run(first)?.state, RunState::Running);
    assert_eq!(runtime.run(second)?.state, RunState::Pending);
    let attempts: i64 = fixture.db()?.query_row(
        "SELECT count(*) FROM workflow_attempts WHERE run_id=?1",
        [second.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(attempts, 0);
    assert_eq!(fixture.usage(second)?, (0, 0));
    fixture.assert_no_call();

    drop(active_socket);
    fixture.reply(&mut runtime)?;
    assert_eq!(
        wait_until(&mut runtime, second, |run| run.state == RunState::Succeeded)?.step_count,
        1
    );
    assert_eq!(runtime.run(first)?.state, RunState::OutcomeUnknown);
    assert_eq!(fixture.usage(second)?.0, 1);
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn two_worker_slots_bound_claims_and_isolate_cancellation() -> Result<()> {
    let fixture = Fixture::with_workflow(true, CAPACITY_WORKFLOW)?;
    let mut runtime = fixture.open_with_slots(2)?;
    let first = fixture.enqueue(&mut runtime)?;
    let first_socket = fixture.accept_request(&mut runtime)?;
    let second = fixture.enqueue(&mut runtime)?;
    let second_socket = fixture.accept_request(&mut runtime)?;
    assert_eq!(runtime.run(first)?.state, RunState::Running);
    assert_eq!(runtime.run(second)?.state, RunState::Running);
    let third = fixture.enqueue(&mut runtime)?;
    runtime.tick()?;
    assert_eq!(runtime.run(third)?.state, RunState::Pending);
    assert_eq!(fixture.usage(third)?, (0, 0));
    let attempts: i64 = fixture.db()?.query_row(
        "SELECT count(*) FROM workflow_attempts WHERE run_id=?1",
        [third.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(attempts, 0);
    fixture.assert_no_call();

    assert!(matches!(
        runtime.handle(WorkflowRequest::Cancel { run_id: first }),
        Response::Workflow(WorkflowResponse::Run(run)) if run.state == RunState::Cancelling
    ));
    assert_eq!(
        wait_until(&mut runtime, first, |run| run.state
            == RunState::OutcomeUnknown)?
        .step_count,
        0
    );
    drop(first_socket);

    fixture.reply(&mut runtime)?;
    assert_eq!(
        wait_until(&mut runtime, third, |run| run.state == RunState::Succeeded)?.step_count,
        1
    );
    assert_eq!(runtime.run(second)?.state, RunState::Running);
    Fixture::send_reply(second_socket)?;
    assert_eq!(
        wait_until(&mut runtime, second, |run| run.state == RunState::Succeeded)?.step_count,
        1
    );
    assert_eq!(fixture.usage(second)?.0, 1);
    assert_eq!(fixture.usage(third)?.0, 1);
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn restart_fences_both_started_workers_and_preserves_pending_work() -> Result<()> {
    let fixture = Fixture::with_workflow(true, CAPACITY_WORKFLOW)?;
    let mut runtime = fixture.open_with_slots(2)?;
    let first = fixture.enqueue(&mut runtime)?;
    let first_socket = fixture.accept_request(&mut runtime)?;
    let second = fixture.enqueue(&mut runtime)?;
    let second_socket = fixture.accept_request(&mut runtime)?;
    let third = fixture.enqueue(&mut runtime)?;
    drop(runtime);
    for mut socket in [first_socket, second_socket] {
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte)?, 0);
    }

    let mut recovered = fixture.open_with_slots(2)?;
    for id in [first, second] {
        let run = recovered.run(id)?;
        assert_eq!(run.state, RunState::OutcomeUnknown);
        assert_eq!(run.step_count, 0);
        assert_eq!(fixture.usage(id)?.0, 1);
    }
    assert_eq!(recovered.run(third)?.state, RunState::Pending);
    fixture.reply(&mut recovered)?;
    assert_eq!(
        wait_until(&mut recovered, third, |run| run.state
            == RunState::Succeeded)?
        .step_count,
        1
    );
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn agent_disconnect_after_receiving_request_is_uncertain_and_not_replayed() -> Result<()> {
    assert_uncertain_agent_reply(&Fixture::new(true)?, None, true)
}

#[test]
fn malformed_agent_reply_keeps_provider_identity_without_payload() -> Result<()> {
    assert_uncertain_agent_reply(
        &Fixture::new(true)?,
        Some(br#"{"schema":"mcbtodo.agent/v1","outputs":{"note":["private provider data"]}}"#),
        true,
    )
}

#[test]
fn agent_reply_with_undeclared_output_is_uncertain_and_not_replayed() -> Result<()> {
    assert_uncertain_agent_reply(
        &Fixture::new(true)?,
        Some(br#"{"schema":"mcbtodo.agent/v1","outputs":{"unexpected":"value"}}"#),
        false,
    )
}

#[test]
fn agent_output_budget_accepts_exact_bytes_and_fences_overflow() -> Result<()> {
    let workflow = CHAIN.replace(
        "first: {kind: agent, capability: summarize, outputs: {}}",
        "first: {kind: agent, capability: summarize, outputs: {title: string}}",
    );
    let fixture = Fixture::with_workflow(true, &workflow)?;
    let policy_path = fixture.paths.agent_policy_path();
    let policy = fs::read_to_string(&policy_path)?;
    let output = r#"{"title":"任务"}"#;
    let response = format!(r#"{{"schema":"mcbtodo.agent/v1","outputs":{output}}}"#);
    let output_bytes = output.len();
    fs::write(
        &policy_path,
        format!("max_output_bytes = {}\n{policy}", output_bytes - 1),
    )?;
    assert_uncertain_agent_reply(&fixture, Some(response.as_bytes()), true)?;

    fs::write(
        policy_path,
        format!("max_output_bytes = {output_bytes}\n{policy}"),
    )?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    {
        let mut socket = fixture.accept_request(&mut runtime)?;
        socket.write_all(&u32::try_from(response.len())?.to_be_bytes())?;
        socket.write_all(response.as_bytes())?;
    }
    fixture.reply(&mut runtime)?;
    let run = wait_until(&mut runtime, id, |run| run.state == RunState::Waiting)?;
    assert_eq!(run.current_node.as_deref(), Some("review"));
    assert_eq!(run.step_count, 2);
    Ok(())
}

fn assert_uncertain_agent_reply(
    fixture: &Fixture,
    response: Option<&[u8]>,
    expect_provider_detail: bool,
) -> Result<()> {
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    wait_at(&mut runtime, id, "first")?;
    {
        let mut socket = fixture.accept_request(&mut runtime)?;
        if let Some(response) = response {
            socket.write_all(&u32::try_from(response.len())?.to_be_bytes())?;
            socket.write_all(response)?;
        }
    }

    let terminal = wait_until(&mut runtime, id, |run| {
        matches!(run.state, RunState::Failed | RunState::OutcomeUnknown) || run.step_count != 0
    })?;
    assert_eq!(terminal.state, RunState::OutcomeUnknown);
    if expect_provider_detail {
        assert_eq!(
            terminal.detail.as_deref(),
            Some("Agent provider mcbtodo-local-agent did not yield a trustworthy result")
        );
    }
    assert_eq!(terminal.step_count, 0);
    assert!(runtime.store.history(id)?.is_empty());
    assert_eq!(fixture.usage(id)?, (1, WALL_MS));
    drop(runtime);

    let mut runtime = fixture.open()?;
    runtime.tick()?;
    let reopened = runtime.run(id)?;
    assert_eq!(reopened.state, RunState::OutcomeUnknown);
    assert_eq!(reopened.detail, terminal.detail);
    assert_eq!(fixture.usage(id)?, (1, WALL_MS));
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn agent_budget_call_cap_is_shared_across_nodes_and_survives_restart() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    wait_at(&mut runtime, id, "first")?;
    fixture.reply(&mut runtime)?;
    wait_at(&mut runtime, id, "second")?;
    fixture.reply(&mut runtime)?;
    assert_eq!(
        wait_at(&mut runtime, id, "review")?.state,
        RunState::Waiting
    );
    let (attempts, elapsed) = fixture.usage(id)?;
    assert_eq!(attempts, 2);
    assert!(elapsed < WALL_MS);
    drop(runtime);

    let mut runtime = fixture.open()?;
    let review = runtime.run(id)?.approval_review();
    let response = runtime.handle(WorkflowRequest::Decide {
        run_id: id,
        node: "review".into(),
        approved: true,
        review,
    });
    assert!(matches!(
        response,
        Response::Workflow(WorkflowResponse::Run(_))
    ));
    wait_at(&mut runtime, id, "third")?;
    fixture.reply(&mut runtime)?;
    wait_at(&mut runtime, id, "fourth")?;
    fixture.reply(&mut runtime)?;
    let failed = wait_until(&mut runtime, id, |run| run.state == RunState::Failed)?;
    assert!(
        failed
            .detail
            .unwrap()
            .contains("Agent call budget exhausted")
    );
    let history = runtime.store.history(id)?;
    assert_eq!(
        history
            .iter()
            .filter(|step| step.status != NodeStatus::Skipped)
            .map(|step| step.node.as_str())
            .collect::<Vec<_>>(),
        ["first", "second", "review", "third", "fourth"]
    );
    assert_eq!(fixture.usage(id)?.0, 4);
    fixture.assert_no_call();

    let another = fixture.enqueue(&mut runtime)?;
    wait_at(&mut runtime, another, "first")?;
    fixture.reply(&mut runtime)?;
    wait_at(&mut runtime, another, "second")?;
    fixture.reply(&mut runtime)?;
    assert_eq!(
        wait_at(&mut runtime, another, "review")?.state,
        RunState::Waiting
    );
    assert_eq!(fixture.usage(another)?.0, 2);
    Ok(())
}

#[test]
fn agent_budget_deadline_cancels_worker_without_replaying_uncertain_call() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    // Inject time already spent on the first node to exercise a short residual
    // deadline without a long test sleep.
    fixture.record_first_call(&mut runtime, id, WALL_MS - 3_000)?;
    assert_eq!(fixture.usage(id)?, (1, WALL_MS - 3_000));
    drop(runtime);

    let mut runtime = fixture.open()?;
    wait_at(&mut runtime, id, "second")?;
    let mut socket = fixture.accept_request(&mut runtime)?;
    assert_eq!(
        socket.read(&mut [0])?,
        0,
        "adapter must disconnect at the run deadline"
    );
    let unknown = wait_until(&mut runtime, id, |run| {
        run.state == RunState::OutcomeUnknown
    })?;
    let detail = unknown.detail.unwrap();
    assert!(
        detail.contains("Worker exceeded its Agent execution budget"),
        "unexpected uncertain-outcome detail: {detail}"
    );
    assert_eq!(fixture.usage(id)?, (2, WALL_MS));
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn agent_budget_interrupted_claim_is_never_refunded_or_replayed() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    fixture.record_first_call(&mut runtime, id, 25)?;
    let run = runtime.run(id)?;
    let claim = runtime
        .store
        .claim_agent(worker_claim(&run, "second")?, true)?;
    assert_eq!(claim.execution_window, Duration::from_millis(WALL_MS - 25));
    assert_eq!(runtime.run(id)?.state, RunState::Running);
    drop(runtime);

    let mut runtime = fixture.open()?;
    runtime.tick()?;
    assert_eq!(runtime.run(id)?.state, RunState::OutcomeUnknown);
    assert_eq!(fixture.usage(id)?, (2, WALL_MS));
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn agent_attempt_claim_failure_rolls_back_the_budget_and_run_state() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    fixture.db()?.execute_batch(
        "CREATE TRIGGER reject_agent_claim BEFORE INSERT ON workflow_attempts
         BEGIN SELECT RAISE(ABORT, 'test attempt insert failure'); END;",
    )?;

    let error = runtime.tick().unwrap_err();
    assert!(format!("{error:#}").contains("test attempt insert failure"));
    assert!(runtime.workers.is_empty());
    assert_eq!(fixture.usage(id)?, (0, 0));
    assert_eq!(runtime.run(id)?.state, RunState::Pending);
    fixture.assert_no_call();
    drop(runtime);

    fixture
        .db()?
        .execute_batch("DROP TRIGGER reject_agent_claim")?;
    let mut runtime = fixture.open()?;
    assert_eq!(runtime.run(id)?.state, RunState::Pending);
    wait_at(&mut runtime, id, "first")?;
    fixture.reply(&mut runtime)?;
    assert_eq!(fixture.usage(id)?.0, 1);
    Ok(())
}

#[test]
fn agent_budget_rejects_a_stale_claim_without_charging_or_dispatching() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    let before = runtime.run(id)?;
    fixture.record_first_call(&mut runtime, id, 25)?;

    let store: &dyn WorkflowRunStore = runtime.store.as_ref();
    let result = store.claim_agent(worker_claim(&before, "first")?, false);
    let Err(error) = result else {
        bail!("stale claim acquired Agent execution rights");
    };
    assert!(error.to_string().contains("step count mismatch"));
    assert_eq!(fixture.usage(id)?, (1, 25));
    let after = runtime.run(id)?;
    assert_eq!(after.state, RunState::Pending);
    assert_eq!(after.step_count, 1);
    assert!(runtime.workers.is_empty());
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn agent_budget_authorization_denial_is_an_attempt_without_adapter_contact() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    wait_until(&mut runtime, id, |run| run.state == RunState::Failed)?;
    let (attempts, elapsed) = fixture.usage(id)?;
    assert_eq!(attempts, 1);
    assert!(elapsed < WALL_MS);
    let step = runtime
        .store
        .recorded_step(id, 0)?
        .context("missing denial trace")?;
    assert_eq!(step.trace.status, NodeStatus::Failed);
    fixture.assert_no_call();
    Ok(())
}

#[test]
fn agent_budget_fails_closed_on_unaccounted_agent_history() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    fixture.record_first_call(&mut runtime, id, 25)?;
    fixture
        .db()?
        .execute("DROP TABLE workflow_agent_budgets", [])?;
    drop(runtime);

    let mut runtime = fixture.open()?;
    runtime.tick()?;
    let failed = runtime.run(id)?;
    assert_eq!(failed.state, RunState::Failed);
    assert!(failed.detail.unwrap().contains("unaccounted Agent history"));
    fixture.assert_no_call();
    Ok(())
}
