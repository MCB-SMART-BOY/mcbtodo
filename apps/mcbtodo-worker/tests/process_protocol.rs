#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{self, BufReader, BufWriter, Read, Write},
    os::unix::net::UnixListener,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::worker::{
    self, AttemptIdentity, DaemonMessage, WorkerJob, WorkerMessage, WorkerOutcome,
};
use serde_json::Map;
use uuid::Uuid;

struct WorkerChild(Child);

impl Drop for WorkerChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn worker_waits_for_start_and_cancels_before_execution() {
    let identity = AttemptIdentity {
        run_id: Uuid::from_u128(1),
        revision: "a".repeat(64),
        node: "agent-review".into(),
        attempt_id: Uuid::from_u128(2),
        epoch: 1,
        expected_step: 1,
    };
    let mut child = WorkerChild(
        Command::new(env!("CARGO_BIN_EXE_mcbtodo-worker"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the worker executable"),
    );
    let mut input = BufWriter::new(child.0.stdin.take().expect("worker stdin"));
    let mut output = BufReader::new(child.0.stdout.take().expect("worker stdout"));

    mcbtodo_protocol::worker::write(
        &mut input,
        &DaemonMessage::Prepare {
            identity: identity.clone(),
            job: WorkerJob::Agent {
                capability: "review".into(),
                timeout_seconds: 30,
                inputs: Map::new(),
            },
        },
    )
    .expect("send worker preparation");

    assert_eq!(
        mcbtodo_protocol::worker::read::<WorkerMessage>(&mut output)
            .expect("read worker readiness"),
        WorkerMessage::Ready {
            identity: identity.clone()
        }
    );
    assert!(
        child.0.try_wait().expect("check worker state").is_none(),
        "worker must remain alive while awaiting Start"
    );

    mcbtodo_protocol::worker::write(&mut input, &DaemonMessage::Cancel { identity })
        .expect("cancel before worker execution");
    drop(input);

    assert!(
        !child.0.wait().expect("wait for cancelled worker").success(),
        "pre-start cancellation must exit without returning a result"
    );
}

#[test]
fn worker_rejects_a_malformed_prepare_frame_before_readiness() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mcbtodo-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the worker executable");
    child
        .stdin
        .as_mut()
        .expect("worker stdin")
        .write_all(&[0, 0, 0, 1, b'x'])
        .expect("send malformed preparation frame");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait for worker rejection");
    assert!(
        !output.status.success(),
        "malformed Prepare must be rejected"
    );
    assert!(
        output.stdout.is_empty(),
        "worker must not send readiness for malformed preparation"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("reading worker preparation"),
        "worker should identify the rejected preparation frame"
    );
}

#[test]
fn worker_reports_only_uncertainty_after_cancelling_a_dispatched_agent() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    let listener = UnixListener::bind(paths.state_dir().join("agent.sock"))?;
    listener.set_nonblocking(true)?;
    let identity = AttemptIdentity {
        run_id: Uuid::from_u128(1),
        revision: "a".repeat(64),
        node: "agent-review".into(),
        attempt_id: Uuid::from_u128(2),
        epoch: 1,
        expected_step: 1,
    };
    fs::write(
        paths.agent_policy_path(),
        format!(
            "timeout_seconds = 5\n[providers]\nunix_socket = 'state/agent.sock'\n\
             [[allow]]\ncapability = 'review'\nrevision = '{}'\n",
            identity.revision
        ),
    )?;

    let mut child = WorkerChild(
        Command::new(env!("CARGO_BIN_EXE_mcbtodo-worker"))
            .env_clear()
            .env("HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut input = child.0.stdin.take().unwrap();
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    worker::write(
        &mut input,
        &DaemonMessage::Prepare {
            identity: identity.clone(),
            job: WorkerJob::Agent {
                capability: "review".into(),
                timeout_seconds: 10,
                inputs: Map::new(),
            },
        },
    )?;
    assert_eq!(
        worker::read::<WorkerMessage>(&mut output)?,
        WorkerMessage::Ready {
            identity: identity.clone()
        }
    );
    worker::write(
        &mut input,
        &DaemonMessage::Start {
            identity: identity.clone(),
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut agent = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "expected Worker to call the Agent"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.into()),
        }
    };
    agent.set_read_timeout(Some(Duration::from_secs(4)))?;
    let mut size = [0; 4];
    agent.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    assert!(size <= 16 * 1024);
    let mut request = vec![0; size];
    agent.read_exact(&mut request)?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request)?["capability"],
        "review"
    );

    worker::write(
        &mut input,
        &DaemonMessage::Cancel {
            identity: identity.clone(),
        },
    )?;
    assert_eq!(agent.read(&mut [0])?, 0);
    assert_eq!(
        worker::read::<WorkerMessage>(&mut output)?,
        WorkerMessage::Result {
            identity,
            outcome: WorkerOutcome::AgentOutcomeUnknown {
                provider_id: "mcbtodo-local-agent".into(),
            },
            artifact: None,
        }
    );
    assert!(child.0.wait()?.success());
    Ok(())
}
