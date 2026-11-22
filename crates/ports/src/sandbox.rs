//! Required isolation for executing a published workflow script.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::Cancellation;

pub struct Script<'a> {
    pub source: &'a [u8],
    pub runner: &'a str,
    pub executable: Option<&'a [u8]>,
}

#[derive(Debug)]
pub struct SandboxOutput {
    pub outputs: BTreeMap<String, Value>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox execution cancelled")]
    Cancelled,
    #[error("sandbox process failed: {0:#}")]
    Process(#[source] anyhow::Error),
    /// The sandbox can prove that the script was never released.
    #[error("sandbox could not start the script: {0:#}")]
    NotStarted(#[source] anyhow::Error),
    /// The adapter lost the outcome and cannot prove that release never occurred.
    #[error("sandbox infrastructure failed: {0:#}")]
    Infrastructure(#[source] anyhow::Error),
}

/// Runs an authorized script synchronously with isolated files and no network.
/// An adapter must enforce bounded output and time, and stop its process tree
/// on cancellation and timeout. If it cannot establish the required isolation,
/// it must reject execution without releasing the script.
pub trait Sandbox {
    fn execute(
        &self,
        script: Script<'_>,
        timeout_seconds: u32,
        inputs: &BTreeMap<String, Value>,
        cancelled: &dyn Cancellation,
    ) -> Result<SandboxOutput, SandboxError>;
}
