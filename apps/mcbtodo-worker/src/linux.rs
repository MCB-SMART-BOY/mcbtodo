use std::{
    collections::BTreeMap,
    io::{self, BufReader, BufWriter},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use mcbtodo_execution::agent::{self, Execution};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::worker::{
    AttemptIdentity, DaemonMessage, ScriptRunner, WorkerArtifact, WorkerJob, WorkerMessage,
    WorkerOutcome,
};
use mcbtodo_workflow::{NodeOutcome, Scalar};
use serde_json::Value;

pub(super) fn run() -> Result<()> {
    if let Some(parent_pid) = std::env::var_os("MCBTODO_PARENT_PID") {
        let parent_pid = parent_pid
            .to_str()
            .context("invalid daemon parent PID")?
            .parse()
            .context("invalid daemon parent PID")?;
        mcbtodo_platform::process::arm_parent_death_signal(parent_pid)
            .context("arming daemon parent death signal")?;
    }
    let mut reader = BufReader::new(io::stdin());
    let mut writer = BufWriter::new(io::stdout());
    let (identity, job) = match mcbtodo_protocol::worker::read::<DaemonMessage>(&mut reader)
        .context("reading worker preparation")?
    {
        DaemonMessage::Prepare { identity, job } => (identity, job),
        _ => bail!("worker expected Prepare as its first message"),
    };
    mcbtodo_protocol::worker::write(
        &mut writer,
        &WorkerMessage::Ready {
            identity: identity.clone(),
        },
    )
    .context("sending worker readiness")?;

    match mcbtodo_protocol::worker::read::<DaemonMessage>(&mut reader)
        .context("waiting for execution authorization")?
    {
        DaemonMessage::Start { identity: started } if started == identity => {}
        DaemonMessage::Cancel {
            identity: cancelled,
        } if cancelled == identity => {
            bail!("worker cancelled before execution started")
        }
        _ => bail!("worker received an invalid start authorization"),
    }

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::clone(&cancelled);
    let cancel_identity = identity.clone();
    thread::Builder::new()
        .name("mcbtodo-worker-control".into())
        .spawn(move || {
            let mut reader = reader;
            match mcbtodo_protocol::worker::read::<DaemonMessage>(&mut reader) {
                Ok(DaemonMessage::Cancel { identity }) if identity == cancel_identity => {
                    cancel_flag.store(true, Ordering::Release);
                }
                // Losing the supervisor means the attempt can no longer be fenced or recorded.
                _ => cancel_flag.store(true, Ordering::Release),
            }
        })
        .context("starting worker control reader")?;

    let paths = WorkspacePaths::discover().context("discovering worker workspace paths")?;
    let (outcome, artifact) = execute(&paths, &identity, job, &cancelled)?;
    // A cancelled Worker may report uncertainty, but may not return outputs.
    if cancelled.load(Ordering::Acquire)
        && !matches!(&outcome, WorkerOutcome::AgentOutcomeUnknown { .. })
    {
        bail!("worker was cancelled before producing a result");
    }
    mcbtodo_protocol::worker::write(
        &mut writer,
        &WorkerMessage::Result {
            identity,
            outcome,
            artifact,
        },
    )
    .context("sending worker result")
}

fn execute(
    paths: &WorkspacePaths,
    identity: &AttemptIdentity,
    job: WorkerJob,
    cancelled: &AtomicBool,
) -> Result<(WorkerOutcome, Option<WorkerArtifact>)> {
    let result = match job {
        WorkerJob::Script {
            source,
            runner,
            timeout_seconds,
            inputs,
            executable,
        } => execute_script(
            paths.state_dir().as_path(),
            source,
            runner,
            timeout_seconds,
            inputs,
            executable,
            cancelled,
        ),
        WorkerJob::Agent {
            capability,
            timeout_seconds,
            inputs,
        } => {
            let deadline = Instant::now() + Duration::from_secs(u64::from(timeout_seconds));
            let inputs = scalar_inputs(inputs)?;
            match agent::execute_request(
                paths,
                &identity.revision,
                &capability,
                &inputs,
                cancelled,
                deadline,
            ) {
                Execution::Finished(outcome) => Ok(Some((outcome, None))),
                Execution::Cancelled => Ok(None),
                Execution::OutcomeUnknown { provider_id } => {
                    return Ok((WorkerOutcome::AgentOutcomeUnknown { provider_id }, None));
                }
            }
        }
    };
    let Some((outcome, artifact)) = result? else {
        bail!("worker execution cancelled");
    };
    Ok((worker_outcome(outcome), artifact))
}

fn execute_script(
    state_dir: &std::path::Path,
    source: Vec<u8>,
    runner: ScriptRunner,
    timeout_seconds: u32,
    inputs: serde_json::Map<String, Value>,
    executable: Option<Vec<u8>>,
    cancelled: &AtomicBool,
) -> Result<Option<(NodeOutcome, Option<WorkerArtifact>)>> {
    let runner = match runner {
        ScriptRunner::Bash => "bash",
        ScriptRunner::Nushell => "nushell",
    };
    let result = mcbtodo_execution::workflow_script::execute(
        &mcbtodo_execution::sandbox::Bubblewrap::new(state_dir),
        mcbtodo_ports::sandbox::Script {
            source: &source,
            runner,
            executable: executable.as_deref(),
        },
        timeout_seconds,
        &scalar_inputs(inputs)?,
        cancelled,
    )?;
    let Some(result) = result else {
        return Ok(None);
    };
    Ok(Some((
        result.outcome,
        result.artifact.map(|bytes| WorkerArtifact {
            media_type: "application/json".into(),
            bytes,
        }),
    )))
}

fn scalar_inputs(inputs: serde_json::Map<String, Value>) -> Result<BTreeMap<String, Scalar>> {
    inputs
        .into_iter()
        .map(|(name, value)| {
            serde_json::from_value(value)
                .with_context(|| format!("invalid scalar input `{name}`"))
                .map(|value| (name, value))
        })
        .collect()
}

fn worker_outcome(outcome: NodeOutcome) -> WorkerOutcome {
    match outcome {
        NodeOutcome::Succeeded { outputs } => WorkerOutcome::Succeeded {
            outputs: outputs
                .into_iter()
                .map(|(key, value)| {
                    (
                        key,
                        serde_json::to_value(value).expect("Scalar serialization is infallible"),
                    )
                })
                .collect(),
        },
        NodeOutcome::Failed { message } => WorkerOutcome::Failed { message },
        NodeOutcome::Approved | NodeOutcome::Rejected => WorkerOutcome::Failed {
            message: "Worker cannot return an approval outcome".into(),
        },
    }
}
