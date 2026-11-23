//! The Linux Worker owns the isolated Codex launch and its model connection.

use std::{
    fs::{self, File},
    io,
    net::{Ipv4Addr, TcpListener},
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use mcbtodo_agent_codex::CodexProvider;
use mcbtodo_platform::{WorkspacePaths, codex_config::CodexConfig};
use mcbtodo_ports::{AgentError, AgentExecutionContext, AgentProvider};
use rustix::{
    io::fcntl_dupfd_cloexec,
    net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with, sockopt},
    process::{DumpableBehavior, geteuid, getpid, set_dumpable_behavior},
    stdio::dup2_stderr,
};
use serde_json::json;

use crate::{
    model_relay::relay_model_exchange,
    sandbox::{codex_command, protected_executable},
    unix_agent::socket_path,
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CODEX: &str = "/bin/codex";

pub(crate) fn provider<'a>(
    paths: &'a WorkspacePaths,
    relative_socket: &'a std::path::Path,
) -> Result<impl AgentProvider + 'a, AgentError> {
    let config = load_config(paths)?;
    let properties = config
        .outputs
        .iter()
        .map(|(name, kind)| (name.clone(), json!({"type": kind.schema_type()})))
        .collect::<serde_json::Map<_, _>>();
    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": config.outputs.keys().collect::<Vec<_>>(),
        "additionalProperties": false
    });
    // The file is kept alive by the provider until the supervised launch ends.
    let filter = tempfile::NamedTempFile::new_in(paths.state_dir())
        .map_err(|_| AgentError::Unavailable("Cannot stage the Codex sandbox filter".into()))?;
    Ok(CodexProvider::new(
        config.capability,
        config.instructions,
        schema,
        move |context: AgentExecutionContext<'_>| {
            let binary =
                protected_executable(&config.executable).context("resolve Codex executable")?;
            let mut command = codex_command(
                filter.path(),
                &binary,
                context.deadline.saturating_duration_since(Instant::now()),
            )?;
            let socket = socket_path(paths, relative_socket)?;
            command.stderr(Stdio::from(connect_model(&socket)?));
            Ok(command)
        },
    ))
}

/// Read the configured capability and check executable trust without
/// starting the app-server or contacting the model socket.
pub fn check_installation(paths: &WorkspacePaths) -> Result<String, AgentError> {
    let config = load_config(paths)?;
    protected_executable(&config.executable)
        .map_err(|_| AgentError::Unavailable("Codex executable is missing or untrusted".into()))?;
    Ok(config.capability)
}

fn load_config(paths: &WorkspacePaths) -> Result<CodexConfig, AgentError> {
    let bytes = paths
        .read_codex_provider_config()
        .map_err(|error| {
            AgentError::Unavailable(format!("Cannot read a protected codex.toml: {error:#}"))
        })?
        .ok_or_else(|| AgentError::Unavailable("Cannot read a protected codex.toml".into()))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| AgentError::Rejected("codex.toml must be UTF-8".into()))?;
    CodexConfig::parse(text).map_err(|error| AgentError::Rejected(error.to_string()))
}

fn connect_model(path: &std::path::Path) -> Result<OwnedFd> {
    let model = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    // AF_UNIX returns EAGAIN for a full queue. Fail this call instead of
    // implicitly retrying or falling through to a different provider.
    connect(&model, &SocketAddrUnix::new(path)?)?;
    ensure!(
        sockopt::socket_peercred(&model)?.uid == geteuid(),
        "Codex model socket requires a same-user peer"
    );
    Ok(model)
}

/// Internal Worker entry point. Bubblewrap runs this helper as PID 1 and
/// passes an authenticated model stream through stderr. Replacing stderr
/// before any diagnostics or child launch keeps that stream private.
pub fn run_isolated_session(milliseconds: &str) -> Result<()> {
    ensure!(
        getpid().as_raw_pid() == 1,
        "Codex session requires a private PID namespace"
    );
    // The mounted procfs is needed by Codex. Its children must not be able
    // to inspect the relay process or acquire its connected host descriptor.
    set_dumpable_behavior(DumpableBehavior::NotDumpable)?;
    let model = UnixStream::from(fcntl_dupfd_cloexec(io::stderr(), 3)?);
    dup2_stderr(File::options().write(true).open("/dev/null")?)?;
    let timeout = Duration::from_millis(milliseconds.parse::<u64>()?.min(120_000));
    let deadline = Instant::now() + timeout;
    fs::create_dir("/scratch/.codex")?;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let provider = format!(
        "model_providers.mcbtodo_broker={{name=\"mcbtodo-broker\",base_url=\"http://127.0.0.1:{port}/v1\",wire_api=\"responses\",supports_websockets=false,request_max_retries=0,stream_max_retries=0}}"
    );
    let mut server = ChildGuard(
        Command::new(CODEX)
            .args([
                "-c",
                "model_provider=\"mcbtodo_broker\"",
                "-c",
                &provider,
                "app-server",
                "--listen",
                "stdio://",
            ])
            .spawn()?,
    );
    let (loopback, _) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if let Some(status) = server.0.try_wait()? {
                    // A successful process exit with no model request can be
                    // valid during protocol shutdown or interruption.
                    ensure!(status.success(), "Codex exited before model dispatch");
                    return Ok(());
                }
                pause(deadline)?;
            }
            Err(error) => return Err(error.into()),
        }
    };
    drop(listener);
    let (result, relay) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("mcbtodo-model-relay".into())
        .spawn(move || {
            let _ = result.send(relay_model_exchange(&loopback, &model, deadline));
        })?;
    let mut relay_complete = false;
    loop {
        if !relay_complete {
            match relay.try_recv() {
                Ok(result) => {
                    result?;
                    relay_complete = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => bail!("Model relay terminated"),
            }
        }
        if let Some(status) = server.0.try_wait()? {
            ensure!(status.success(), "Codex app-server terminated");
            if relay_complete {
                return Ok(());
            }
        }
        pause(deadline)?;
    }
}

fn pause(deadline: Instant) -> Result<()> {
    let left = deadline.saturating_duration_since(Instant::now());
    ensure!(!left.is_zero(), "Codex session deadline expired");
    thread::sleep(POLL_INTERVAL.min(left));
    Ok(())
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
