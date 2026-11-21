//! Shared validation for the user-owned `agents.toml` policy.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;

/// Maximum byte length of an Agent policy. Readers should read at most one more
/// byte so oversized files can be rejected without reading their entire contents.
pub const MAX_POLICY_BYTES: usize = 64 * 1024;

/// A validated socket path, timeout, and set of exact authorization pairs.
pub struct AgentPolicy {
    socket: PathBuf,
    timeout: Duration,
    grants: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    socket: String,
    timeout_seconds: Option<u32>,
    #[serde(default)]
    // Require named fields. Serde struct deserialization can also accept
    // positional arrays, so each grant must first deserialize as a TOML table.
    allow: Vec<toml::Table>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    capability: String,
    revision: String,
}

impl AgentPolicy {
    /// Validate policy bytes without accessing the filesystem.
    ///
    /// Callers are responsible for checking file ownership and permissions.
    /// The daemon also checks the socket path and its peer before sending inputs.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_POLICY_BYTES,
            "agents.toml exceeds the size limit"
        );
        let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("agents.toml must be UTF-8"))?;
        // TOML diagnostics can include the contents of the policy.
        let Document {
            socket,
            timeout_seconds,
            allow,
        } = toml::from_str(text).map_err(|_| anyhow!("Invalid agents.toml configuration"))?;
        let socket_path = Path::new(&socket);
        ensure!(
            !socket.is_empty()
                && !socket.contains('\0')
                && socket_path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "Agent socket must be a relative path beneath the app home"
        );
        let timeout_seconds = timeout_seconds.unwrap_or(30);
        ensure!(
            (1..=120).contains(&timeout_seconds),
            "Agent timeout_seconds must be between 1 and 120"
        );
        let mut grants = BTreeMap::<String, BTreeSet<String>>::new();
        for grant in allow {
            let Grant {
                capability,
                revision,
            } = grant
                .try_into()
                .map_err(|_| anyhow!("Invalid agents.toml configuration"))?;
            ensure!(
                valid_capability(&capability) && valid_revision(&revision),
                "Invalid Agent authorization entry"
            );
            ensure!(
                grants.entry(capability).or_default().insert(revision),
                "Duplicate Agent authorization entry"
            );
        }
        Ok(Self {
            socket: PathBuf::from(socket),
            timeout: Duration::from_secs(u64::from(timeout_seconds)),
            grants,
        })
    }

    /// The socket path relative to the mcbtodo app home.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The deadline budget for a socket exchange.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Whether the same grant matches both strings exactly.
    pub fn allows(&self, capability: &str, revision: &str) -> bool {
        self.grants
            .get(capability)
            .is_some_and(|revisions| revisions.contains(revision))
    }
}

fn valid_capability(capability: &str) -> bool {
    let mut bytes = capability.bytes();
    capability.len() <= 96
        && bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_revision(revision: &str) -> bool {
    revision.len() == 64
        && revision
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}
