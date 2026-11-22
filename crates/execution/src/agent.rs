//! Host authorization and result handling for workflow Agent nodes.

use std::{
    collections::BTreeMap,
    fs::Metadata,
    io,
    os::unix::fs::MetadataExt,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use anyhow::Result;
use mcbtodo_platform::{
    WorkspacePaths,
    agent_policy::{AgentBackend, AgentPolicy, AgentRoute},
};
use mcbtodo_ports::{AgentError, AgentExecutionContext, AgentProvider, AgentRequest, AgentResult};
use mcbtodo_workflow::{NodeOutcome, Scalar};
use rustix::process::geteuid;

use crate::{codex_host, unix_agent::UnixSocketAgentProvider};

/// Host disposition of a single authorized Agent execution.
#[derive(Debug, PartialEq)]
pub enum Execution {
    Finished(NodeOutcome),
    Cancelled,
    /// The provider identity comes from adapter metadata.
    OutcomeUnknown {
        provider_id: String,
    },
}

/// Executes an authorized Agent request from a process-boundary worker envelope.
pub fn execute_request(
    paths: &WorkspacePaths,
    revision_hash: &str,
    capability: &str,
    inputs: &BTreeMap<String, Scalar>,
    cancel: &AtomicBool,
    run_deadline: Instant,
) -> Execution {
    if cancel.load(Ordering::Acquire) {
        return Execution::Cancelled;
    }

    let request = AgentRequest {
        capability: capability.to_owned(),
        revision: revision_hash.to_owned(),
        inputs: inputs.clone(),
    };
    let context = AgentExecutionContext {
        deadline: run_deadline,
        cancellation: cancel,
    };
    let result = (|| {
        check_context(context)?;
        let policy = paths
            .load_agent_policy()
            .map_err(|error| classify_before_dispatch(&error, context))?
            .ok_or_else(|| AgentError::Unavailable("Agent adapter is not configured".into()))?;
        let mut context = context;
        let route = authorize(&policy, &request, &mut context)?;
        let execute = |provider: &dyn AgentProvider| match invoke(
            provider,
            request,
            context,
            policy.max_output_bytes(),
        ) {
            Ok(result) if !cancel.load(Ordering::Acquire) => {
                Ok(Execution::Finished(NodeOutcome::Succeeded {
                    outputs: result.outputs,
                }))
            }
            Ok(_) | Err(AgentError::OutcomeUnknown(_)) => Ok(Execution::OutcomeUnknown {
                provider_id: provider.metadata().provider_id.clone(),
            }),
            Err(error) => Err(error),
        };
        match route.backend {
            AgentBackend::UnixSocket => execute(&UnixSocketAgentProvider::new(paths, route.socket)),
            AgentBackend::Codex => execute(&codex_host::provider(paths, route.socket)?),
        }
    })();
    match result {
        Ok(execution) => execution,
        Err(_) if cancel.load(Ordering::Acquire) => Execution::Cancelled,
        Err(error) => Execution::Finished(NodeOutcome::Failed {
            message: error.to_string(),
        }),
    }
}

#[cfg(test)]
fn dispatch_authorized(
    policy: &AgentPolicy,
    provider: &dyn AgentProvider,
    request: AgentRequest,
    mut context: AgentExecutionContext<'_>,
) -> Result<AgentResult, AgentError> {
    authorize(policy, &request, &mut context)?;
    invoke(provider, request, context, policy.max_output_bytes())
}

fn authorize<'a>(
    policy: &'a AgentPolicy,
    request: &AgentRequest,
    context: &mut AgentExecutionContext<'_>,
) -> Result<AgentRoute<'a>, AgentError> {
    check_context(*context)?;
    let route = policy
        .route(&request.capability, &request.revision)
        .ok_or_else(|| {
            AgentError::Rejected("Agent capability is not authorized for this revision".into())
        })?;
    context.deadline = context.deadline.min(Instant::now() + policy.timeout());
    Ok(route)
}

fn invoke(
    provider: &dyn AgentProvider,
    request: AgentRequest,
    context: AgentExecutionContext<'_>,
    max_output_bytes: Option<u64>,
) -> Result<AgentResult, AgentError> {
    check_context(context)?;
    let result = provider.execute(request, context)?;
    if let Some(limit) = max_output_bytes {
        serde_json::to_writer(OutputBudget(limit), &result.outputs).map_err(|_| {
            AgentError::OutcomeUnknown("Agent outputs exceeded the host byte budget".into())
        })?;
    }
    // A provider must enforce its deadline while running. The host still
    // fences late success before handing a result to the Worker.
    check_context(context).map_err(|error| AgentError::OutcomeUnknown(error.to_string()))?;
    Ok(result)
}

/// Counts compact JSON bytes without allocating a second output buffer.
struct OutputBudget(u64);

impl io::Write for OutputBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("Agent output budget exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn check_context(context: AgentExecutionContext<'_>) -> Result<(), AgentError> {
    if context.cancellation.is_cancelled() {
        return Err(AgentError::Cancelled);
    }
    if Instant::now() >= context.deadline {
        return Err(AgentError::TimedOut);
    }
    Ok(())
}

pub(crate) fn classify_before_dispatch(
    error: &anyhow::Error,
    context: AgentExecutionContext<'_>,
) -> AgentError {
    if context.cancellation.is_cancelled() {
        return AgentError::Cancelled;
    }
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::TimedOut)
    }) {
        return AgentError::TimedOut;
    }
    AgentError::Transport(format!("{error:#}"))
}

pub(crate) fn owned_and_protected(metadata: &Metadata) -> bool {
    metadata.uid() == geteuid().as_raw() && metadata.mode() & 0o022 == 0
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
