//! Shared conversion between workflow script jobs and the sandbox adapter.

use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool};

use anyhow::Result;
use mcbtodo_workflow::{NodeOutcome, Scalar};
use serde_json::Value;

use crate::sandbox::{self, SandboxError, Script};

pub struct WorkflowScriptResult {
    pub outcome: NodeOutcome,
    pub artifact: Option<Vec<u8>>,
}

pub fn execute(
    state_dir: &Path,
    source: &[u8],
    runner: &str,
    timeout_seconds: u32,
    inputs: &BTreeMap<String, Scalar>,
    executable: Option<&[u8]>,
    cancelled: &AtomicBool,
) -> Result<Option<WorkflowScriptResult>> {
    let inputs = inputs
        .iter()
        .map(|(name, value)| {
            serde_json::to_value(value)
                .map(|value| (name.clone(), value))
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<BTreeMap<_, Value>>>()?;
    let output = match sandbox::execute(
        state_dir,
        Script {
            source,
            runner,
            executable,
        },
        timeout_seconds,
        &inputs,
        cancelled,
    ) {
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
