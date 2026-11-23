//! Stdio Agent plugin adapter supervised by the Linux Worker.

use std::{
    io::{self, Read, Write},
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

use mcbtodo_platform::WorkspacePaths;
use mcbtodo_plugin::invoke;
use mcbtodo_ports::{
    AgentError, AgentExecutionContext, AgentProvider, AgentRequest, AgentResult, ProviderMetadata,
};
use rustix::fs::{OFlags, fcntl_setfl};
use tempfile::NamedTempFile;

use crate::{
    agent::check_context,
    sandbox::{READY, Running, plugin_command, protected_executable},
};

const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(crate) struct StdioPluginProvider<'a> {
    paths: &'a WorkspacePaths,
    endpoint: &'a Path,
    metadata: ProviderMetadata,
}

impl<'a> StdioPluginProvider<'a> {
    pub(crate) fn new(paths: &'a WorkspacePaths, endpoint: &'a Path) -> Self {
        let mut metadata = ProviderMetadata::new(
            "mcbtodo-stdio-plugin",
            env!("CARGO_PKG_VERSION"),
            "stdio/v1",
        );
        metadata.supported_platforms.insert("linux".into());
        Self {
            paths,
            endpoint,
            metadata,
        }
    }
}

impl AgentProvider for StdioPluginProvider<'_> {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    fn execute(
        &self,
        request: AgentRequest,
        context: AgentExecutionContext<'_>,
    ) -> Result<AgentResult, AgentError> {
        check_context(context)?;
        let executable =
            protected_executable(&self.paths.home_dir().join(self.endpoint)).map_err(|_| {
                AgentError::Unavailable("Plugin executable is missing or untrusted".into())
            })?;
        let filter = NamedTempFile::new_in(self.paths.state_dir()).map_err(|_| {
            AgentError::Unavailable("Cannot stage the plugin sandbox filter".into())
        })?;
        let mut command = plugin_command(filter.path(), &executable)
            .map_err(|_| AgentError::Unavailable("Plugin sandbox is unavailable".into()))?;
        check_context(context)?;
        let mut process = Running(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|_| AgentError::Unavailable("Unable to start the Agent plugin".into()))?,
        );
        let mut input = process.0.stdin.take().expect("piped plugin stdin");
        let mut output = process.0.stdout.take().expect("piped plugin stdout");
        fcntl_setfl(&input, OFlags::NONBLOCK)
            .and_then(|()| fcntl_setfl(&output, OFlags::NONBLOCK))
            .map_err(|_| AgentError::Unavailable("Plugin pipes are unavailable".into()))?;
        let mut ready = [0; READY.len()];
        startup_io(context, ready.len(), |offset| {
            output.read(&mut ready[offset..])
        })?;
        if ready != READY || process.0.try_wait().map_err(startup_error)?.is_some() {
            return Err(startup_error("startup gate closed"));
        }
        let start = b"start\n";
        startup_io(context, start.len(), |offset| input.write(&start[offset..]))?;

        let result = invoke(&mut output, &mut input, request, context)?;
        drop(input);
        loop {
            check_context(context).map_err(uncertain_exit)?;
            match process.0.try_wait().map_err(uncertain_exit)? {
                Some(status) if status.success() => return Ok(result),
                Some(status) => return Err(uncertain_exit(status)),
                None => pause(context),
            }
        }
    }
}

fn uncertain_exit(reason: impl std::fmt::Display) -> AgentError {
    AgentError::OutcomeUnknown(format!("Plugin exit unconfirmed: {reason}"))
}

fn startup_error(reason: impl std::fmt::Display) -> AgentError {
    AgentError::Unavailable(format!("Plugin sandbox startup failed: {reason}"))
}

fn pause(context: AgentExecutionContext<'_>) {
    thread::sleep(POLL_INTERVAL.min(context.deadline.saturating_duration_since(Instant::now())));
}

fn startup_io(
    context: AgentExecutionContext<'_>,
    len: usize,
    mut transfer: impl FnMut(usize) -> io::Result<usize>,
) -> Result<(), AgentError> {
    let mut offset = 0;
    while offset < len {
        check_context(context)?;
        match transfer(offset) {
            Ok(0) => return Err(startup_error("startup pipe closed")),
            Ok(n) => offset += n,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => pause(context),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(startup_error(error)),
        }
    }
    Ok(())
}
