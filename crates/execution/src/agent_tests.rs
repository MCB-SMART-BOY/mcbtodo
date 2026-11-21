use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{PermissionsExt, symlink},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use mcbtodo_platform::WorkspacePaths;
use mcbtodo_workflow::{
    NodeJob, NodeOutcome, Progress, Scalar, advance, compile, parse_definition,
};
use serde_json::{Value, json};

use super::execute;

const REVISION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CAPABILITY: &str = "triage";
const RESPONSE: &str =
    r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"Review","priority":2}}"#;

struct Fixture {
    _home: tempfile::TempDir,
    paths: WorkspacePaths,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::from_home(home.path().to_path_buf());
        paths.initialize().unwrap();
        Self { _home: home, paths }
    }

    fn config(&self) -> PathBuf {
        self.paths.agent_policy_path()
    }

    fn socket(&self) -> PathBuf {
        self.paths.state_dir().join("agent.sock")
    }

    fn authorize(&self, timeout: u32) {
        fs::write(
            self.config(),
            format!(
                "socket = \"state/agent.sock\"\ntimeout_seconds = {timeout}\n\
                 [[allow]]\ncapability = \"{CAPABILITY}\"\nrevision = \"{REVISION}\"\n\
                 [[allow]]\ncapability = \"other\"\nrevision = \"{OTHER_REVISION}\"\n",
            ),
        )
        .unwrap();
    }

    fn call(&self, revision: &str, job: &NodeJob, cancel: &AtomicBool) -> Option<NodeOutcome> {
        execute(
            &self.paths,
            revision,
            job,
            cancel,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
    }

    fn reply(&self, bytes: &[u8], job: &NodeJob) -> Option<NodeOutcome> {
        let listener = UnixListener::bind(self.socket()).unwrap();
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut socket = accept(&listener);
                read_request(&mut socket);
                socket
                    .write_all(&(bytes.len() as u32).to_be_bytes())
                    .unwrap();
                socket.write_all(bytes).unwrap();
            });
            self.call(REVISION, job, &AtomicBool::new(false))
        })
    }
}

fn job(capability: &str) -> NodeJob {
    let definition = parse_definition(&format!(
        r#"
schema: mcbtodo.workflow/v1
id: adapter-test
name: Adapter test
entry: agent
capabilities:
  - {{kind: agent.invoke, scope: {capability}}}
nodes:
  agent:
    kind: agent
    capability: {capability}
    inputs: {{log: string, count: integer, failed: boolean}}
    outputs: {{summary: string, priority: integer}}
literals:
  - {{to: {{node: agent, port: log}}, value: "test output"}}
  - {{to: {{node: agent, port: count}}, value: 3}}
  - {{to: {{node: agent, port: failed}}, value: true}}
"#,
    ))
    .unwrap();
    let plan = compile(&definition, &Default::default()).unwrap();
    let Progress::Next { job, .. } = advance(&plan, &[]).unwrap() else {
        panic!("expected an agent job");
    };
    job
}

fn accept(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => panic!("waiting for adapter invocation: {err}"),
        }
    };
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    socket
}

fn read_request(socket: &mut UnixStream) -> Value {
    let mut size = [0; 4];
    socket.read_exact(&mut size).unwrap();
    let mut data = vec![0; u32::from_be_bytes(size) as usize];
    socket.read_exact(&mut data).unwrap();
    serde_json::from_slice(&data).unwrap()
}

fn failed(outcome: Option<NodeOutcome>) -> String {
    match outcome {
        Some(NodeOutcome::Failed { message }) => message,
        other => panic!("expected agent failure, got {other:?}"),
    }
}

#[test]
fn sends_only_typed_bound_inputs_and_accepts_declared_outputs() {
    let fixture = Fixture::new();
    fixture.authorize(5);
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    thread::scope(|scope| {
        let adapter = scope.spawn(|| {
            let mut socket = accept(&listener);
            let request = read_request(&mut socket);
            socket
                .write_all(&(RESPONSE.len() as u32).to_be_bytes())
                .unwrap();
            socket.write_all(RESPONSE.as_bytes()).unwrap();
            request
        });
        let outcome = fixture.call(REVISION, &job(CAPABILITY), &AtomicBool::new(false));
        assert_eq!(
            outcome,
            Some(NodeOutcome::Succeeded {
                outputs: [
                    ("summary".into(), Scalar::String("Review".into())),
                    ("priority".into(), Scalar::Integer(2))
                ]
                .into()
            })
        );
        assert_eq!(
            adapter.join().unwrap(),
            json!({
                "schema": "mcbtodo.agent/v1",
                "capability": "triage",
                "inputs": {"log": "test output", "count": 3, "failed": true}
            })
        );
    });
}

#[test]
fn default_denial_and_exact_pair_matching_happen_before_connecting() {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let cancel = AtomicBool::new(false);
    failed(fixture.call(REVISION, &job(CAPABILITY), &cancel));
    fixture.authorize(5);
    for (revision, capability) in [
        (OTHER_REVISION, CAPABILITY),
        (REVISION, "other"),
        (REVISION, "TRIAGE"),
        ("01234567", CAPABILITY),
    ] {
        failed(fixture.call(revision, &job(capability), &cancel));
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn invalid_policy_denies_otherwise_authorized_calls_before_connecting() {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    listener.set_nonblocking(true).unwrap();
    fixture.authorize(5);
    let mut bytes = fs::read(fixture.config()).unwrap();
    bytes.extend_from_slice(
        format!("\n[[allow]]\ncapability = \"{CAPABILITY}\"\nrevision = \"{REVISION}\"\n")
            .as_bytes(),
    );
    fs::write(fixture.config(), bytes).unwrap();
    assert_eq!(
        fixture.call(REVISION, &job(CAPABILITY), &AtomicBool::new(false)),
        Some(NodeOutcome::Failed {
            message: "Duplicate Agent authorization entry".into(),
        })
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn rejects_extra_duplicate_missing_and_wrongly_typed_outputs() {
    for response in [
        r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"ok","priority":2,"tool":"denied"}}"#,
        r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"a","summary":"b","priority":2}}"#,
        r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"ok"}}"#,
        r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"ok","priority":"2"}}"#,
        r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"ok","priority":2},"files":[]}"#,
        r#"{"schema":"mcbtodo.agent/v2","outputs":{"summary":"ok","priority":2}}"#,
        r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"ok","priority":2}}false"#,
    ] {
        let fixture = Fixture::new();
        fixture.authorize(5);
        let job = job(CAPABILITY);
        let outcome = fixture.reply(response.as_bytes(), &job);
        let message = match outcome {
            Some(outcome @ NodeOutcome::Succeeded { .. }) => {
                job.complete(outcome).unwrap_err().to_string()
            }
            Some(NodeOutcome::Failed { message }) => message,
            other => panic!("expected rejected Agent response, got {other:?}"),
        };
        assert!(message.contains("output") || message.contains("response"));
        assert!(!message.contains("denied"));
    }
}

#[test]
fn rejects_oversized_request_before_connecting() {
    let fixture = Fixture::new();
    fixture.authorize(5);
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut job = job(CAPABILITY);
    job.inputs
        .insert("log".into(), Scalar::String("x".repeat(16 * 1024)));
    let message = failed(fixture.call(REVISION, &job, &AtomicBool::new(false)));
    assert!(message.contains("request"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn rejects_oversized_response_from_header_alone() {
    let fixture = Fixture::new();
    fixture.authorize(5);
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            socket
                .write_all(&(16 * 1024_u32 + 1).to_be_bytes())
                .unwrap();
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let message = failed(fixture.call(REVISION, &job(CAPABILITY), &AtomicBool::new(false)));
        assert!(message.contains("size"), "{message}");
    });
}

#[test]
fn times_out_on_a_stalled_partial_response_and_disconnects() {
    let fixture = Fixture::new();
    fixture.authorize(1);
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            socket.write_all(&[0, 0]).unwrap();
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let message = failed(fixture.call(REVISION, &job(CAPABILITY), &AtomicBool::new(false)));
        assert!(message.contains("timed out"), "{message}");
    });
}

#[test]
fn cancellation_disconnects_a_waiting_adapter() {
    let fixture = Fixture::new();
    fixture.authorize(5);
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let cancel = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            cancel.store(true, Ordering::Release);
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        assert_eq!(fixture.call(REVISION, &job(CAPABILITY), &cancel), None);
    });
}

#[test]
fn rejects_work_on_a_pre_cancelled_call() {
    let fixture = Fixture::new();
    fixture.authorize(5);
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        fixture.call(REVISION, &job(CAPABILITY), &AtomicBool::new(true)),
        None
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn refuses_symlinked_socket_and_writable_authorization_file() {
    let fixture = Fixture::new();
    fixture.authorize(5);
    let target = fixture.paths.state_dir().join("target.sock");
    let listener = UnixListener::bind(&target).unwrap();
    listener.set_nonblocking(true).unwrap();
    symlink(&target, fixture.socket()).unwrap();
    let job = job(CAPABILITY);
    let cancel = AtomicBool::new(false);
    failed(fixture.call(REVISION, &job, &cancel));
    fs::remove_file(fixture.socket()).unwrap();
    fs::rename(target, fixture.socket()).unwrap();
    fs::set_permissions(fixture.config(), fs::Permissions::from_mode(0o662)).unwrap();
    failed(fixture.call(REVISION, &job, &cancel));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}
