use std::{
    io::{BufReader, BufWriter, Write},
    process::{Child, Command, Stdio},
};

use mcbtodo_protocol::worker::{AttemptIdentity, DaemonMessage, WorkerJob, WorkerMessage};
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
