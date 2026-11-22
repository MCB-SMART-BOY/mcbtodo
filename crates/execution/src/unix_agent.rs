//! Same-user Unix socket adapter for authorized workflow Agent calls.

use std::{
    collections::BTreeMap,
    fs,
    io::Cursor,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use mcbtodo_platform::{WorkspacePaths, local_transport::UnixSocketTransport};
use mcbtodo_ports::{
    AgentError, AgentExecutionContext, AgentProvider, AgentRequest, AgentResult, ProviderMetadata,
    Scalar,
    transport::{LocalConnection, LocalPeer, LocalTransport},
};
use rustix::process::geteuid;
use serde::{Deserialize, Serialize};

use crate::agent::{check_context, classify_before_dispatch, owned_and_protected};

const MAX_FRAME_BYTES: usize = 16 * 1024;
pub(crate) const UNIX_AGENT_PROVIDER_ID: &str = "mcbtodo-local-agent";

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
    #[serde(deserialize_with = "AgentResult::deserialize_outputs")]
    outputs: BTreeMap<String, Scalar>,
}

pub(crate) struct UnixSocketAgentProvider<'a> {
    paths: &'a WorkspacePaths,
    relative_socket: &'a Path,
    metadata: ProviderMetadata,
    transport: &'a dyn LocalTransport,
}

impl<'a> UnixSocketAgentProvider<'a> {
    pub(crate) fn new(paths: &'a WorkspacePaths, relative_socket: &'a Path) -> Self {
        Self::with_transport(paths, relative_socket, &UnixSocketTransport)
    }

    pub(crate) fn with_transport(
        paths: &'a WorkspacePaths,
        relative_socket: &'a Path,
        transport: &'a dyn LocalTransport,
    ) -> Self {
        let mut metadata = ProviderMetadata::new(
            UNIX_AGENT_PROVIDER_ID,
            env!("CARGO_PKG_VERSION"),
            "mcbtodo.agent/v1",
        );
        metadata.capabilities.insert("agent.invoke".to_owned());
        metadata
            .supported_platforms
            .insert(std::env::consts::OS.to_owned());
        metadata
            .required_permissions
            .insert("workspace.agent_policy".to_owned());
        metadata
            .required_permissions
            .insert("workspace.agent_socket".to_owned());
        metadata
            .resource_limits
            .insert("max_frame_bytes".to_owned(), MAX_FRAME_BYTES as u64);
        Self {
            paths,
            relative_socket,
            metadata,
            transport,
        }
    }
}

impl AgentProvider for UnixSocketAgentProvider<'_> {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    fn execute(
        &self,
        request: AgentRequest,
        context: AgentExecutionContext<'_>,
    ) -> std::result::Result<AgentResult, AgentError> {
        // The authenticated v1 peer handles the capability authorized by the host.
        self.invoke(&request, context)
    }
}

impl UnixSocketAgentProvider<'_> {
    fn invoke(
        &self,
        request: &AgentRequest,
        context: AgentExecutionContext<'_>,
    ) -> std::result::Result<AgentResult, AgentError> {
        let classify = |error| classify_before_dispatch(&error, context);
        check_context(context)?;
        let request = Request {
            schema: Schema::V1,
            capability: &request.capability,
            inputs: &request.inputs,
        };
        // The cursor writes into a fixed-length slice, even for an oversized input.
        let mut payload = [0; MAX_FRAME_BYTES];
        let mut buffer = Cursor::new(payload.as_mut_slice());
        serde_json::to_writer(&mut buffer, &request)
            .map_err(|_| AgentError::Rejected("Agent request exceeds the size limit".into()))?;
        let length = buffer.position() as usize;

        let path = socket_path(self.paths, self.relative_socket).map_err(classify)?;
        let mut socket = self
            .transport
            .connect(&path, context.deadline, context.cancellation)
            .context("Connecting to Agent socket")
            .map_err(classify)?;
        if socket.peer() != LocalPeer::SameUser {
            return Err(AgentError::Rejected(
                "Agent socket peer must run as the daemon user".into(),
            ));
        }
        // An I/O error from the first write onward cannot prove that the provider
        // did not act. Keep such failures out of ordinary workflow failure edges.
        check_context(context)?;
        let result = exchange(&mut *socket, &mut payload, length)
            .map_err(|error| AgentError::OutcomeUnknown(format!("{error:#}")))?;
        check_context(context).map_err(|error| AgentError::OutcomeUnknown(error.to_string()))?;
        Ok(result)
    }
}

fn exchange(
    socket: &mut dyn LocalConnection,
    payload: &mut [u8; MAX_FRAME_BYTES],
    length: usize,
) -> Result<AgentResult> {
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
    Ok(AgentResult {
        outputs: response.outputs,
    })
}

pub(crate) fn socket_path(paths: &WorkspacePaths, relative_socket: &Path) -> Result<PathBuf> {
    let mut components = relative_socket.components().peekable();
    let state_dir = paths.state_dir();
    let mut path = state_dir
        .parent()
        .expect("WorkspacePaths app home directory")
        .to_path_buf();
    let app_home = fs::symlink_metadata(&path).context("Inspecting the Agent socket root")?;
    ensure!(
        app_home.is_dir() && app_home.uid() == geteuid().as_raw() && app_home.mode() & 0o077 == 0,
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
