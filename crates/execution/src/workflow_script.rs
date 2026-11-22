//! Shared conversion between workflow script jobs and the sandbox adapter.

use std::collections::BTreeMap;

use anyhow::Result;
use mcbtodo_ports::{
    Cancellation,
    sandbox::{Sandbox, SandboxError, Script},
};
use mcbtodo_workflow::{NodeOutcome, Scalar};
use serde_json::Value;

pub struct WorkflowScriptResult {
    pub outcome: NodeOutcome,
    pub artifact: Option<Vec<u8>>,
}

pub fn execute(
    sandbox: &dyn Sandbox,
    script: Script<'_>,
    timeout_seconds: u32,
    inputs: &BTreeMap<String, Scalar>,
    cancelled: &dyn Cancellation,
) -> Result<Option<WorkflowScriptResult>> {
    let inputs = inputs
        .iter()
        .map(|(name, value)| {
            serde_json::to_value(value)
                .map(|value| (name.clone(), value))
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<BTreeMap<_, Value>>>()?;
    let output = match sandbox.execute(script, timeout_seconds, &inputs, cancelled) {
        Ok(output) => output,
        Err(SandboxError::Cancelled) => return Ok(None),
        Err(SandboxError::Process(error)) => {
            return Ok(Some(WorkflowScriptResult {
                outcome: NodeOutcome::Failed {
                    message: error.to_string().chars().take(1024).collect(),
                },
                artifact: None,
            }));
        }
        Err(error @ SandboxError::NotStarted(_)) => {
            return Ok(Some(WorkflowScriptResult {
                outcome: NodeOutcome::Failed {
                    message: error.to_string().chars().take(1024).collect(),
                },
                artifact: None,
            }));
        }
        Err(error) => return Err(error.into()),
    };
    let outputs = output
        .outputs
        .into_iter()
        .map(|(name, value)| {
            serde_json::from_value::<Scalar>(value)
                .map(|value| (name, value))
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<BTreeMap<_, _>>>();
    let outcome = match outputs {
        Ok(outputs) => NodeOutcome::Succeeded { outputs },
        Err(error) => NodeOutcome::Failed {
            message: format!("Invalid structured output: {error}"),
        },
    };
    let artifact = serde_json::to_vec(&serde_json::json!({
        "stdout": output.stdout,
        "stderr": output.stderr,
    }))?;
    Ok(Some(WorkflowScriptResult {
        outcome,
        artifact: Some(artifact),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcbtodo_ports::sandbox::SandboxOutput;
    use std::{cell::RefCell, sync::atomic::AtomicBool};

    struct Stub(RefCell<Option<Result<SandboxOutput, SandboxError>>>);

    impl Sandbox for Stub {
        fn execute(
            &self,
            script: Script<'_>,
            timeout_seconds: u32,
            inputs: &BTreeMap<String, Value>,
            cancelled: &dyn Cancellation,
        ) -> Result<SandboxOutput, SandboxError> {
            assert_eq!(script.runner, "bash");
            assert_eq!(script.source, b"published script");
            assert_eq!(timeout_seconds, 3);
            assert_eq!(inputs["seed"], 42);
            assert!(!cancelled.is_cancelled());
            self.0.borrow_mut().take().expect("one execution")
        }
    }

    #[test]
    fn workflow_normalizes_outputs_and_does_not_hide_isolation_failure() -> Result<()> {
        let run = |output| {
            execute(
                &Stub(RefCell::new(Some(output))),
                Script {
                    source: b"published script",
                    runner: "bash",
                    executable: None,
                },
                3,
                &BTreeMap::from([(
                    "seed".to_owned(),
                    serde_json::from_value(42.into()).unwrap(),
                )]),
                &AtomicBool::new(false),
            )
        };
        let output = run(Ok(SandboxOutput {
            outputs: BTreeMap::from([("answer".to_owned(), 42.into())]),
            stdout: "log\n".into(),
            stderr: String::new(),
        }))?
        .expect("completed execution");
        assert!(matches!(output.outcome, NodeOutcome::Succeeded { outputs }
            if serde_json::to_value(&outputs["answer"])? == 42));
        let artifact: Value = serde_json::from_slice(&output.artifact.expect("captured logs"))?;
        assert_eq!(artifact["stdout"], "log\n");
        assert!(run(Err(SandboxError::Cancelled))?.is_none());
        let refused = run(Err(SandboxError::NotStarted(anyhow::anyhow!(
            "required isolation unavailable"
        ))))?
        .expect("known pre-start failure");
        assert!(matches!(refused.outcome, NodeOutcome::Failed { message }
            if message.contains("required isolation unavailable")));
        assert!(refused.artifact.is_none());
        let error = run(Err(SandboxError::Infrastructure(anyhow::anyhow!(
            "lost result from released script"
        ))))
        .err()
        .expect("uncertain failure propagates to the supervisor");
        assert!(error.downcast_ref::<SandboxError>().is_some());
        Ok(())
    }
}
