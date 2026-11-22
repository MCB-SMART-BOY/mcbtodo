use std::{
    io::{BufReader, Cursor, Write},
    process::{Child, Command, Stdio},
    thread,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use mcbtodo_ports::{
    AgentError, AgentExecutionContext, AgentProvider, AgentRequest, AgentResult, ProviderMetadata,
};
use rustix::fs::{OFlags, fcntl_setfl};
use serde_json::Value;

use super::{
    MAX_FRAME, MAX_OUTPUT, MAX_REQUEST, MAX_SESSION_RESPONSE, MAX_STREAM_MESSAGES, POLL_INTERVAL,
    TurnAction, TurnResult, check_context, run_turn,
};

/// A single-capability Codex adapter for a host-supplied, isolated launch.
///
/// The trusted launcher supplies a verified Codex binary in the expected
/// scratch layout, enforces filesystem and network policy and owns descendant
/// cleanup when the returned child exits or is killed. It also configures
/// stderr so Codex diagnostics cannot disclose inputs. Each call starts a
/// fresh ephemeral session.
pub struct CodexProvider<L> {
    metadata: ProviderMetadata,
    capability: String,
    instructions: String,
    output_schema: Value,
    launch: L,
}

impl<L> CodexProvider<L>
where
    L: Fn(AgentExecutionContext<'_>) -> Result<Command> + Send + Sync,
{
    pub fn new(capability: String, instructions: String, output_schema: Value, launch: L) -> Self {
        let mut metadata =
            ProviderMetadata::new("mcbtodo-codex", env!("CARGO_PKG_VERSION"), "app-server/v2");
        metadata.capabilities.insert("agent.invoke".into());
        metadata.capabilities.insert(capability.clone());
        metadata.supported_platforms.insert("linux".into());
        metadata.resource_limits.extend([
            ("max_request_bytes".into(), MAX_REQUEST as u64),
            ("max_frame_bytes".into(), MAX_FRAME as u64),
            ("max_session_response_bytes".into(), MAX_SESSION_RESPONSE),
            ("max_stream_messages".into(), MAX_STREAM_MESSAGES as u64),
            ("max_output_bytes".into(), MAX_OUTPUT as u64),
        ]);
        Self {
            metadata,
            capability,
            instructions,
            output_schema,
            launch,
        }
    }
}

impl<L> AgentProvider for CodexProvider<L>
where
    L: Fn(AgentExecutionContext<'_>) -> Result<Command> + Send + Sync,
{
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    fn execute(
        &self,
        request: AgentRequest,
        context: AgentExecutionContext<'_>,
    ) -> Result<AgentResult, AgentError> {
        check_context(context)?;
        if request.capability != self.capability {
            return Err(AgentError::Rejected("Unsupported Codex capability".into()));
        }
        let mut bytes = [0; MAX_REQUEST];
        let mut prompt = Cursor::new(bytes.as_mut_slice());
        prompt
            .write_all(self.instructions.as_bytes())
            .and_then(|()| prompt.write_all(b"\n\nInputs (JSON): "))
            .and_then(|()| serde_json::to_writer(&mut prompt, &request.inputs).map_err(io_error))
            .map_err(|_| AgentError::Rejected("Codex prompt exceeds input limit".into()))?;
        let len = prompt.position() as usize;
        let prompt = std::str::from_utf8(&bytes[..len])
            .map_err(|_| AgentError::Rejected("Codex prompt must be UTF-8".into()))?;

        let mut command = (self.launch)(context).map_err(redact_launcher_error)?;
        check_context(context)?;
        let (mut process, mut input, mut output) =
            start(&mut command).map_err(redact_launcher_error)?;
        drop(command);
        let result = run_turn(
            &mut input,
            &mut output,
            context,
            prompt,
            TurnAction::Structured(&self.output_schema),
        )?;
        drop(output);
        process.exit_cleanly(context)?;
        let TurnResult::Structured(result) = result else {
            return Err(AgentError::OutcomeUnknown(
                "Codex returned an unexpected result".into(),
            ));
        };
        Ok(result)
    }
}

fn io_error(error: serde_json::Error) -> std::io::Error {
    std::io::Error::other(error)
}

fn redact_launcher_error(_: anyhow::Error) -> AgentError {
    AgentError::Unavailable("Unable to start an isolated Codex app-server".into())
}

fn start(
    command: &mut Command,
) -> Result<(
    Running,
    BufReader<std::process::ChildStdout>,
    std::process::ChildStdin,
)> {
    let mut process = Running(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let input = process.0.stdout.take().context("open Codex stdout")?;
    let output = process.0.stdin.take().context("open Codex stdin")?;
    fcntl_setfl(&input, OFlags::NONBLOCK)?;
    fcntl_setfl(&output, OFlags::NONBLOCK)?;
    Ok((process, BufReader::new(input), output))
}

struct Running(Child);

impl Running {
    fn exit_cleanly(&mut self, context: AgentExecutionContext<'_>) -> Result<(), AgentError> {
        let mut wait = || -> Result<()> {
            loop {
                check_context(context)?;
                if let Some(status) = self.0.try_wait()? {
                    ensure!(status.success(), "Codex exited with {status}");
                    return Ok(());
                }
                let remaining = context.deadline.saturating_duration_since(Instant::now());
                thread::sleep(POLL_INTERVAL.min(remaining));
            }
        };
        wait().map_err(|error| AgentError::OutcomeUnknown(error.to_string()))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::*;

    #[test]
    fn capability_mismatch_is_rejected_before_launch() {
        let provider = CodexProvider::new(
            "review".into(),
            "Review the input".into(),
            serde_json::json!({}),
            |_| panic!("an unsupported capability must not launch Codex"),
        );
        let result = provider.execute(
            AgentRequest {
                capability: "summarize".into(),
                revision: "a".repeat(64),
                inputs: Default::default(),
            },
            AgentExecutionContext {
                deadline: Instant::now() + Duration::from_secs(5),
                cancellation: &AtomicBool::new(false),
            },
        );
        assert!(matches!(result, Err(AgentError::Rejected(_))), "{result:?}");
    }
}
