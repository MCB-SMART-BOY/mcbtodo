//! Optional same-user socket adapter for workflow Agent nodes.

use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{self, Cursor, Read, Write},
    os::{
        fd::AsFd,
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use mcbtodo_platform::{
    WorkspacePaths,
    agent_policy::{AgentPolicy, MAX_POLICY_BYTES},
};
use mcbtodo_workflow::{JobKind, NodeJob, NodeOutcome, Scalar};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{Mode, OFlags, open},
    io::Errno,
    net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with, sockopt},
    process::geteuid,
};
use serde::{Deserialize, Deserializer, Serialize, de};

const MAX_FRAME_BYTES: usize = 16 * 1024;
const CANCEL_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Deserialize, Serialize)]
enum Schema {
    #[serde(rename = "mcbtodo.agent/v1")]
    V1,
}

#[derive(Serialize)]
struct Request<'a> {
    schema: Schema,
    capability: &'a str,
    inputs: &'a BTreeMap<String, Scalar>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    #[serde(rename = "schema")]
    _schema: Schema,
    #[serde(deserialize_with = "unique_outputs")]
    outputs: BTreeMap<String, Scalar>,
}

/// Call an adapter for a compiled Agent job from the supplied publication hash.
/// Cancellation returns `None`. Operational failures become typed node failures.
pub fn execute(
    paths: &WorkspacePaths,
    revision_hash: &str,
    job: &NodeJob,
    cancel: &AtomicBool,
    run_deadline: Instant,
) -> Result<Option<NodeOutcome>> {
    let JobKind::Agent { capability } = &job.kind else {
        anyhow::bail!("Agent adapter requires an Agent job");
    };
    execute_request(
        paths,
        revision_hash,
        capability,
        &job.inputs,
        cancel,
        run_deadline,
    )
}

/// Executes an authorized Agent request from a process-boundary worker envelope.
pub fn execute_request(
    paths: &WorkspacePaths,
    revision_hash: &str,
    capability: &str,
    inputs: &BTreeMap<String, Scalar>,
    cancel: &AtomicBool,
    run_deadline: Instant,
) -> Result<Option<NodeOutcome>> {
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }

    let outcome = invoke(
        paths,
        revision_hash,
        capability,
        inputs,
        cancel,
        run_deadline,
    );
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    Ok(Some(outcome.unwrap_or_else(|error| NodeOutcome::Failed {
        message: format!("{error:#}"),
    })))
}

fn invoke(
    paths: &WorkspacePaths,
    revision_hash: &str,
    capability: &str,
    inputs: &BTreeMap<String, Scalar>,
    cancel: &AtomicBool,
    run_deadline: Instant,
) -> Result<NodeOutcome> {
    Deadline {
        expires: run_deadline,
        cancel,
    }
    .remaining()?;
    let policy = load_policy(paths)?;
    ensure!(
        policy.allows(capability, revision_hash),
        "Agent capability is not authorized for this revision"
    );
    let deadline = Deadline {
        expires: run_deadline.min(Instant::now() + policy.timeout()),
        cancel,
    };
    let request = Request {
        schema: Schema::V1,
        capability,
        inputs,
    };
    // The cursor writes into a fixed-length slice, even for an oversized input.
    let mut payload = [0; MAX_FRAME_BYTES];
    let mut buffer = Cursor::new(payload.as_mut_slice());
    serde_json::to_writer(&mut buffer, &request)
        .map_err(|_| anyhow::anyhow!("Agent request exceeds the size limit"))?;
    let length = buffer.position() as usize;

    let path = socket_path(paths, &policy)?;
    let mut socket = AdapterStream::connect(&path, deadline)?;
    socket
        .write_all(&(length as u32).to_be_bytes())
        .and_then(|()| socket.write_all(&payload[..length]))
        .context("Sending Agent request")?;

    let mut header = [0; 4];
    socket
        .read_exact(&mut header)
        .context("Reading Agent response")?;
    let size = u32::from_be_bytes(header) as usize;
    ensure!(
        (1..=MAX_FRAME_BYTES).contains(&size),
        "Invalid Agent response size"
    );
    socket
        .read_exact(&mut payload[..size])
        .context("Reading Agent response")?;
    let response: Response = serde_json::from_slice(&payload[..size])
        .map_err(|_| anyhow::anyhow!("Invalid Agent response JSON or output values"))?;
    let outcome = NodeOutcome::Succeeded {
        outputs: response.outputs,
    };
    socket.deadline.remaining()?;
    Ok(outcome)
}

fn load_policy(paths: &WorkspacePaths) -> Result<AgentPolicy> {
    let config_path = paths.agent_policy_path();
    let config_dir = config_path
        .parent()
        .expect("WorkspacePaths config directory");
    let config_parent = config_dir
        .parent()
        .expect("WorkspacePaths .config directory");
    // Protect the authorization file from substitutions through its parent directories.
    for path in [config_parent, config_dir] {
        let metadata =
            fs::symlink_metadata(path).context("Inspecting Agent configuration directory")?;
        ensure!(
            metadata.is_dir() && owned_and_protected(&metadata),
            "Agent configuration directories must be user-owned and protected from other writers"
        );
    }
    let fd = match open(
        &config_path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => anyhow::bail!("Agent adapter is not configured"),
        Err(error) => return Err(error).context("Opening agents.toml"),
    };
    let file = File::from(fd);
    let metadata = file.metadata().context("Inspecting agents.toml")?;
    ensure!(
        metadata.is_file() && owned_and_protected(&metadata),
        "agents.toml must be a user-owned regular file protected from other writers"
    );
    let mut bytes = Vec::new();
    file.take((MAX_POLICY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("Reading agents.toml")?;
    AgentPolicy::parse(&bytes)
}

fn owned_and_protected(metadata: &Metadata) -> bool {
    metadata.uid() == geteuid().as_raw() && metadata.mode() & 0o022 == 0
}

fn socket_path(paths: &WorkspacePaths, policy: &AgentPolicy) -> Result<PathBuf> {
    let mut components = policy.socket().components().peekable();
    let state_dir = paths.state_dir();
    let mut path = state_dir
        .parent()
        .expect("WorkspacePaths app home directory")
        .to_path_buf();
    let app_home = fs::symlink_metadata(&path).context("Inspecting the Agent socket root")?;
    ensure!(
        app_home.is_dir() && owned_and_protected(&app_home) && app_home.mode() & 0o077 == 0,
        "Agent socket root must be a private user-owned directory"
    );
    while let Some(component) = components.next() {
        path.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&path).context("Inspecting the Agent socket path")?;
        if components.peek().is_some() {
            ensure!(
                metadata.is_dir() && owned_and_protected(&metadata),
                "Agent socket directories must be user-owned and protected from other writers"
            );
        } else {
            ensure!(
                metadata.file_type().is_socket() && metadata.uid() == geteuid().as_raw(),
                "Agent socket must be a user-owned Unix socket"
            );
        }
    }
    Ok(path)
}

fn unique_outputs<'de, D: Deserializer<'de>>(de: D) -> Result<BTreeMap<String, Scalar>, D::Error> {
    struct Outputs;

    impl<'de> de::Visitor<'de> for Outputs {
        type Value = BTreeMap<String, Scalar>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object with unique scalar output fields")
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut outputs = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if outputs.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate Agent output"));
                }
            }
            Ok(outputs)
        }
    }
    de.deserialize_map(Outputs)
}

struct Deadline<'a> {
    expires: Instant,
    cancel: &'a AtomicBool,
}

impl Deadline<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::Error::other("Agent call cancelled"));
        }
        let left = self.expires.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Agent call timed out",
            ));
        }
        Ok(left)
    }

    fn wait(&self, socket: &impl AsFd, flags: PollFlags) -> io::Result<()> {
        let mut fds = [PollFd::new(socket, flags)];
        loop {
            let interval = self.remaining()?.min(CANCEL_POLL);
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: interval.subsec_nanos().into(),
            };
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(Errno::INTR) => {}
                Ok(_) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }
}

struct AdapterStream<'a> {
    socket: UnixStream,
    deadline: Deadline<'a>,
}

impl<'a> AdapterStream<'a> {
    fn connect(path: &Path, deadline: Deadline<'a>) -> Result<Self> {
        deadline.remaining()?;
        let addr = SocketAddrUnix::new(path).context("Agent socket address")?;
        let socket = socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )
        .context("Opening Agent socket")?;
        match connect(&socket, &addr) {
            Ok(()) => {}
            Err(Errno::INPROGRESS) => {
                deadline.wait(&socket, PollFlags::OUT)?;
                sockopt::socket_error(&socket)?.context("Connecting to Agent socket")?;
            }
            Err(error) => return Err(error).context("Connecting to Agent socket"),
        }
        let peer = sockopt::socket_peercred(&socket).context("Checking Agent socket peer")?;
        ensure!(
            peer.uid == geteuid(),
            "Agent socket peer must run as the daemon user"
        );
        Ok(Self {
            socket: UnixStream::from(socket),
            deadline,
        })
    }

    fn transfer(
        &self,
        flags: PollFlags,
        mut operation: impl FnMut(&UnixStream) -> io::Result<usize>,
    ) -> io::Result<usize> {
        loop {
            self.deadline.remaining()?;
            match operation(&self.socket) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.deadline.wait(&self.socket, flags)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl Read for AdapterStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.transfer(PollFlags::IN, |mut socket| socket.read(buffer))
    }
}

impl Write for AdapterStream<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.transfer(PollFlags::OUT, |mut socket| socket.write(buffer))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
