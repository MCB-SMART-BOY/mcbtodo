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
use mcbtodo_ports::{
    AgentError, AgentExecutionContext, AgentProvider, AgentRequest, AgentResult, Cancellation,
    ProviderMetadata,
    transport::{LocalConnection, LocalPeer, LocalTransport},
};
use mcbtodo_workflow::{
    JobKind, NodeJob, NodeOutcome, Progress, Scalar, advance, compile, parse_definition,
};
use serde_json::{Value, json};

use super::{Execution, classify_before_dispatch, dispatch_authorized, execute_request};
use crate::unix_agent::UnixSocketAgentProvider;

const REVISION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CAPABILITY: &str = "triage";
const RESPONSE: &str =
    r#"{"schema":"mcbtodo.agent/v1","outputs":{"summary":"Review","priority":2}}"#;

#[test]
fn transport_classification_does_not_infer_policy_state_from_error_text() {
    let context = AgentExecutionContext {
        deadline: Instant::now() + Duration::from_secs(5),
        cancellation: &AtomicBool::new(false),
    };
    let error = anyhow::anyhow!("opening endpoint named 'not configured.sock'");
    assert!(matches!(
        classify_before_dispatch(&error, context),
        AgentError::Transport(message) if message.contains("not configured.sock")
    ));
}

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
                "timeout_seconds = {timeout}\n[providers]\nunix_socket = \"state/agent.sock\"\n\
                 [[allow]]\ncapability = \"{CAPABILITY}\"\nrevision = \"{REVISION}\"\n\
                 [[allow]]\ncapability = \"other\"\nrevision = \"{OTHER_REVISION}\"\n",
            ),
        )
        .unwrap();
    }

    fn call(&self, revision: &str, job: &NodeJob, cancel: &AtomicBool) -> Execution {
        let JobKind::Agent { capability } = &job.kind else {
            panic!("fixture requires a compiled Agent job");
        };
        execute_request(
            &self.paths,
            revision,
            capability,
            &job.inputs,
            cancel,
            Instant::now() + Duration::from_secs(60),
        )
    }

    fn call_local_adapter(&self, cancel: &AtomicBool) -> Result<AgentResult, AgentError> {
        let policy = self.paths.load_agent_policy().unwrap().unwrap();
        let route = policy.route(CAPABILITY, REVISION).unwrap();
        dispatch_authorized(
            &policy,
            &UnixSocketAgentProvider::new(&self.paths, route.endpoint),
            AgentRequest {
                capability: CAPABILITY.into(),
                revision: REVISION.into(),
                inputs: job(CAPABILITY).inputs,
            },
            AgentExecutionContext {
                deadline: Instant::now() + Duration::from_secs(60),
                cancellation: cancel,
            },
        )
    }

    fn reply(&self, bytes: &[u8], job: &NodeJob) -> Execution {
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

fn failed(outcome: Execution) -> String {
    match outcome {
        Execution::Finished(NodeOutcome::Failed { message }) => message,
        other => panic!("expected agent failure, got {other:?}"),
    }
}

fn uncertain(outcome: Result<AgentResult, AgentError>) -> String {
    match outcome {
        Err(AgentError::OutcomeUnknown(message)) => message,
        other => panic!("expected an unknown dispatch outcome, got {other:?}"),
    }
}

#[test]
fn local_provider_metadata_describes_its_runtime_contract() {
    let fixture = Fixture::new();
    let provider = UnixSocketAgentProvider::new(&fixture.paths, "state/agent.sock".as_ref());
    let metadata = provider.metadata();

    assert_eq!(metadata.provider_id, "mcbtodo-local-agent");
    assert_eq!(metadata.protocol_version, "mcbtodo.agent/v1");
    assert_eq!(serde_json::to_value(metadata.health).unwrap(), "unchecked");
    assert!(metadata.capabilities.contains("agent.invoke"));
    assert!(
        metadata
            .required_permissions
            .contains("workspace.agent_policy")
    );
    assert!(
        metadata
            .required_permissions
            .contains("workspace.agent_socket")
    );
    assert_eq!(
        metadata.resource_limits.get("max_frame_bytes"),
        Some(&16_384)
    );
}

#[test]
fn host_rejects_a_different_user_before_sending_any_request_bytes() {
    struct OtherUserTransport;
    struct OtherUserConnection;

    impl LocalTransport for OtherUserTransport {
        fn connect<'a>(
            &self,
            _: &std::path::Path,
            _: Instant,
            _: &'a dyn Cancellation,
        ) -> io::Result<Box<dyn LocalConnection + 'a>> {
            Ok(Box::new(OtherUserConnection))
        }
    }

    impl LocalConnection for OtherUserConnection {
        fn peer(&self) -> LocalPeer {
            LocalPeer::DifferentUser
        }
    }

    impl Read for OtherUserConnection {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("must reject the peer before reading")
        }
    }

    impl Write for OtherUserConnection {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("must reject the peer before writing")
        }

        fn flush(&mut self) -> io::Result<()> {
            panic!("must reject the peer before flushing")
        }
    }

    let fixture = Fixture::new();
    fixture.authorize(5);
    let _listener = UnixListener::bind(fixture.socket()).unwrap();
    let policy = fixture.paths.load_agent_policy().unwrap().unwrap();
    let provider = UnixSocketAgentProvider::with_transport(
        &fixture.paths,
        policy.endpoint(),
        &OtherUserTransport,
    );
    let result = dispatch_authorized(
        &policy,
        &provider,
        AgentRequest {
            capability: CAPABILITY.to_owned(),
            revision: REVISION.to_owned(),
            inputs: Default::default(),
        },
        AgentExecutionContext {
            deadline: Instant::now() + Duration::from_secs(5),
            cancellation: &AtomicBool::new(false),
        },
    );
    assert!(matches!(result, Err(AgentError::Rejected(_))));
}

#[test]
fn host_fences_a_provider_independently_of_its_metadata_and_transport() {
    use std::sync::Mutex;

    struct RecordingProvider {
        metadata: ProviderMetadata,
        calls: Mutex<Vec<(AgentRequest, Instant)>>,
    }

    impl AgentProvider for RecordingProvider {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }

        fn execute(
            &self,
            request: AgentRequest,
            context: AgentExecutionContext<'_>,
        ) -> Result<AgentResult, AgentError> {
            self.calls.lock().unwrap().push((request, context.deadline));
            Ok(AgentResult {
                outputs: Default::default(),
            })
        }
    }

    let fixture = Fixture::new();
    fixture.authorize(5);
    let policy = fixture.paths.load_agent_policy().unwrap().unwrap();
    let provider = RecordingProvider {
        metadata: ProviderMetadata::new("test-provider", "1", "1"),
        calls: Mutex::default(),
    };
    let now = Instant::now();
    let cancel = AtomicBool::new(false);
    let runner_deadline = now + Duration::from_secs(30);
    let context = AgentExecutionContext {
        deadline: runner_deadline,
        cancellation: &cancel,
    };
    let mut request = AgentRequest {
        capability: CAPABILITY.into(),
        revision: OTHER_REVISION.into(),
        inputs: Default::default(),
    };
    assert!(matches!(
        dispatch_authorized(&policy, &provider, request.clone(), context),
        Err(AgentError::Rejected(_))
    ));
    request.revision = REVISION.into();
    assert!(matches!(
        dispatch_authorized(
            &policy,
            &provider,
            request.clone(),
            AgentExecutionContext {
                deadline: now,
                ..context
            },
        ),
        Err(AgentError::TimedOut)
    ));
    cancel.store(true, Ordering::Release);
    assert!(matches!(
        dispatch_authorized(&policy, &provider, request.clone(), context),
        Err(AgentError::Cancelled)
    ));

    cancel.store(false, Ordering::Release);
    assert!(provider.calls.lock().unwrap().is_empty());

    dispatch_authorized(&policy, &provider, request.clone(), context).unwrap();
    let short_deadline = Instant::now() + Duration::from_secs(1);
    dispatch_authorized(
        &policy,
        &provider,
        request.clone(),
        AgentExecutionContext {
            deadline: short_deadline,
            ..context
        },
    )
    .unwrap();

    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, request);
    assert!(calls[0].1 < runner_deadline);
    assert!(calls[0].1 <= Instant::now() + policy.timeout());
    assert_eq!(calls[1], (request, short_deadline));
}

#[test]
fn host_rejects_success_after_provider_deadline_or_cancellation() {
    struct LateProvider {
        metadata: ProviderMetadata,
        cancel_on_call: Option<AtomicBool>,
    }

    impl AgentProvider for LateProvider {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }

        fn execute(
            &self,
            _: AgentRequest,
            context: AgentExecutionContext<'_>,
        ) -> Result<AgentResult, AgentError> {
            if let Some(cancel) = &self.cancel_on_call {
                cancel.store(true, Ordering::Release);
            } else {
                thread::sleep(context.deadline.saturating_duration_since(Instant::now()));
            }
            Ok(AgentResult {
                outputs: Default::default(),
            })
        }
    }

    let fixture = Fixture::new();
    fixture.authorize(5);
    let policy = fixture.paths.load_agent_policy().unwrap().unwrap();
    for (cancel_on_call, reason) in [(false, "timed out"), (true, "cancelled")] {
        let provider = LateProvider {
            metadata: ProviderMetadata::new("late-provider", "1", "1"),
            cancel_on_call: cancel_on_call.then(|| AtomicBool::new(false)),
        };
        let never_cancel = AtomicBool::new(false);
        let result = dispatch_authorized(
            &policy,
            &provider,
            AgentRequest {
                capability: CAPABILITY.into(),
                revision: REVISION.into(),
                inputs: Default::default(),
            },
            AgentExecutionContext {
                deadline: Instant::now() + Duration::from_millis(250),
                cancellation: provider.cancel_on_call.as_ref().unwrap_or(&never_cancel),
            },
        );
        assert!(
            matches!(&result, Err(AgentError::OutcomeUnknown(message)) if message.contains(reason)),
            "{result:?}"
        );
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
            Execution::Finished(NodeOutcome::Succeeded {
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
        Execution::Finished(NodeOutcome::Failed {
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
        match outcome {
            Execution::Finished(outcome @ NodeOutcome::Succeeded { .. }) => {
                let message = job.complete(outcome).unwrap_err().to_string();
                assert!(message.contains("output"));
                assert!(!message.contains("denied"));
            }
            Execution::OutcomeUnknown { provider_id } => {
                assert_eq!(provider_id, "mcbtodo-local-agent");
            }
            other => panic!("expected rejected Agent response, got {other:?}"),
        }
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
        let message = uncertain(fixture.call_local_adapter(&AtomicBool::new(false)));
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
        let message = uncertain(fixture.call_local_adapter(&AtomicBool::new(false)));
        assert!(message.contains("timed out"), "{message}");
    });
}

#[test]
fn cancellation_disconnects_an_already_dispatched_adapter_and_preserves_uncertainty() {
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
        let message = uncertain(fixture.call_local_adapter(&cancel));
        assert!(message.contains("cancel"), "{message}");
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
        Execution::Cancelled
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
