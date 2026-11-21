use std::{
    fs,
    io::{Read, Write},
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

struct Fixture {
    _home: tempfile::TempDir,
    paths: WorkspacePaths,
    revision: String,
    listener: UnixListener,
}

impl Fixture {
    fn new(authorized: bool) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let state_dir = paths.state_dir();
        let graph = home.path().join("budget.yaml");
        fs::write(&graph, CHAIN)?;
        let revision = RevisionStore::new(&state_dir)
            .publish(&graph)?
            .hash()
            .into();
        if authorized {
            fs::write(
                paths.agent_policy_path(),
                format!(
                    "socket = \"state/agent.sock\"\ntimeout_seconds = 30\n\
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

    fn accept_request(&self) -> Result<UnixStream> {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut socket = loop {
            match self.listener.accept() {
                Ok((socket, _)) => break socket,
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
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

    fn reply(&self) -> Result<()> {
        let mut socket = self.accept_request()?;
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
        let reserved = runtime.store.claim_agent(&run, &job.node, || Ok(false))?;
        runtime.finish_job(
            &runtime.run(id)?,
            &job,
            JobResult {
                outcome: NodeOutcome::Succeeded {
                    outputs: Default::default(),
                },
                artifact: None,
            },
            reserved - Duration::from_millis(elapsed_ms),
        )
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
fn agent_budget_call_cap_is_shared_across_nodes_and_survives_restart() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    wait_at(&mut runtime, id, "first")?;
    fixture.reply()?;
    wait_at(&mut runtime, id, "second")?;
    fixture.reply()?;
    assert_eq!(
        wait_at(&mut runtime, id, "review")?.state,
        RunState::Waiting
    );
    let (attempts, elapsed) = fixture.usage(id)?;
    assert_eq!(attempts, 2);
    assert!(elapsed < WALL_MS);
    drop(runtime);

    let mut runtime = fixture.open()?;
    let response = runtime.handle(WorkflowRequest::Decide {
        run_id: id,
        node: "review".into(),
        approved: true,
    });
    assert!(matches!(
        response,
        Response::Workflow(WorkflowResponse::Run(_))
    ));
    wait_at(&mut runtime, id, "third")?;
    fixture.reply()?;
    wait_at(&mut runtime, id, "fourth")?;
    fixture.reply()?;
    let failed = wait_until(&mut runtime, id, |run| run.state == RunState::Failed)?;
    assert!(
        failed
            .detail
            .unwrap()
            .contains("Agent call budget exhausted")
    );
    let history = runtime.history(id)?;
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
    fixture.reply()?;
    wait_at(&mut runtime, another, "second")?;
    fixture.reply()?;
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
    fixture.record_first_call(&mut runtime, id, WALL_MS - 500)?;
    assert_eq!(fixture.usage(id)?, (1, WALL_MS - 500));
    drop(runtime);

    let mut runtime = fixture.open()?;
    wait_at(&mut runtime, id, "second")?;
    let mut socket = fixture.accept_request()?;
    assert_eq!(
        socket.read(&mut [0])?,
        0,
        "adapter must disconnect at the run deadline"
    );
    let unknown = wait_until(&mut runtime, id, |run| {
        run.state == RunState::OutcomeUnknown
    })?;
    assert!(
        unknown
            .detail
            .unwrap()
            .contains("Worker exceeded its Agent execution budget")
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
    assert_eq!(
        runtime.store.claim_agent(&run, "second", || Ok(false))?,
        Duration::from_millis(WALL_MS - 25),
    );
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
fn agent_budget_authorization_denial_is_an_attempt_without_adapter_contact() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let mut runtime = fixture.open()?;
    let id = fixture.enqueue(&mut runtime)?;
    wait_until(&mut runtime, id, |run| run.state == RunState::Failed)?;
    let (attempts, elapsed) = fixture.usage(id)?;
    assert_eq!(attempts, 1);
    assert!(elapsed < WALL_MS);
    let step = runtime.store.step(id, 0)?.context("missing denial trace")?;
    assert_eq!(step["trace"]["status"], "failed");
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
