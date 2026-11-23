//! Shared validation for the user-owned `agents.toml` policy.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

/// Maximum byte length of an Agent policy. Readers should read at most one more
/// byte so oversized files can be rejected without reading their entire contents.
pub const MAX_POLICY_BYTES: usize = 64 * 1024;

/// Configured providers, shared timeout, and exact revision-bound routes.
pub struct AgentPolicy {
    default_backend: AgentBackend,
    providers: BTreeMap<AgentBackend, PathBuf>,
    timeout: Duration,
    max_output_bytes: Option<u64>,
    grants: BTreeMap<String, BTreeMap<String, AgentBackend>>,
    inherited_grants: usize,
}

/// The policy and file text captured by the Settings routes editor.
#[cfg(target_os = "linux")]
pub struct AgentPolicySnapshot(crate::ConfigSnapshot<Option<AgentPolicy>>);

#[cfg(target_os = "linux")]
impl AgentPolicySnapshot {
    pub fn policy(&self) -> Result<Option<&AgentPolicy>, &anyhow::Error> {
        self.0.value().map(Option::as_ref)
    }
}

/// Changes the Settings editor can publish without editing authorization entries.
#[cfg(target_os = "linux")]
pub enum AgentPolicyEdit<'a> {
    Default(AgentBackend),
    Endpoint {
        backend: AgentBackend,
        path: &'a str,
    },
}

/// Built-in Provider selected by the host's policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBackend {
    #[default]
    UnixSocket,
    Codex,
}

impl AgentBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::UnixSocket => "unix_socket",
            Self::Codex => "codex",
        }
    }
}

/// An exact grant's resolved provider and endpoint relative to the app home.
pub struct AgentRoute<'a> {
    pub backend: AgentBackend,
    pub socket: &'a Path,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    provider: Option<AgentBackend>,
    #[serde(default)]
    providers: BTreeMap<AgentBackend, String>,
    timeout_seconds: Option<u32>,
    max_output_bytes: Option<u64>,
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
    provider: Option<AgentBackend>,
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
            provider,
            providers,
            timeout_seconds,
            max_output_bytes,
            allow,
        } = toml::from_str(text).map_err(|_| anyhow!("Invalid agents.toml configuration"))?;
        let provider = match provider {
            Some(backend) => backend,
            None if providers.len() == 1 => *providers.first_key_value().unwrap().0,
            None => AgentBackend::UnixSocket,
        };
        ensure!(
            providers.contains_key(&provider),
            "Invalid agents.toml configuration"
        );
        let providers = providers
            .into_iter()
            .map(|(backend, socket)| {
                ensure!(
                    !socket.is_empty()
                        && !socket.contains('\0')
                        && Path::new(&socket)
                            .components()
                            .all(|component| matches!(component, Component::Normal(_))),
                    "Agent socket must be a relative path beneath the app home"
                );
                Ok((backend, PathBuf::from(socket)))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let timeout_seconds = timeout_seconds.unwrap_or(30);
        ensure!(
            (1..=120).contains(&timeout_seconds),
            "Agent timeout_seconds must be between 1 and 120"
        );
        ensure!(
            max_output_bytes.is_none_or(|limit| limit >= 2),
            "Agent max_output_bytes must be at least 2"
        );
        let mut grants = BTreeMap::<String, BTreeMap<String, AgentBackend>>::new();
        let mut inherited_grants = 0;
        for grant in allow {
            let Grant {
                capability,
                revision,
                provider: grant_provider,
            } = grant
                .try_into()
                .map_err(|_| anyhow!("Invalid agents.toml configuration"))?;
            ensure!(
                valid_capability(&capability) && valid_revision(&revision),
                "Invalid Agent authorization entry"
            );
            let backend = grant_provider.unwrap_or(provider);
            ensure!(
                providers.contains_key(&backend),
                "Agent grant selects an unconfigured provider"
            );
            inherited_grants += usize::from(grant_provider.is_none());
            ensure!(
                grants
                    .entry(capability)
                    .or_default()
                    .insert(revision, backend)
                    .is_none(),
                "Duplicate Agent authorization entry"
            );
        }
        Ok(Self {
            default_backend: provider,
            providers,
            timeout: Duration::from_secs(u64::from(timeout_seconds)),
            max_output_bytes,
            grants,
            inherited_grants,
        })
    }

    /// The default Provider used by grants without a `provider` field.
    pub fn backend(&self) -> AgentBackend {
        self.default_backend
    }

    /// The default Provider's socket path relative to the app home.
    pub fn socket(&self) -> &Path {
        &self.providers[&self.default_backend]
    }

    pub fn provider_socket(&self, backend: AgentBackend) -> Option<&Path> {
        self.providers.get(&backend).map(PathBuf::as_path)
    }

    pub fn configured_providers(&self) -> impl Iterator<Item = AgentBackend> + '_ {
        self.providers.keys().copied()
    }

    /// Number of exact capability/revision grants targeting this provider.
    pub fn grant_count(&self, backend: AgentBackend) -> usize {
        self.grants
            .values()
            .flat_map(|revisions| revisions.values())
            .filter(|&&target| target == backend)
            .count()
    }

    /// Number of exact grants whose provider follows the policy default.
    pub fn inherited_grant_count(&self) -> usize {
        self.inherited_grants
    }

    /// The deadline budget for a socket exchange.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Host limit on the compact JSON output object, independent of wire framing.
    pub fn max_output_bytes(&self) -> Option<u64> {
        self.max_output_bytes
    }

    /// Authorize the exact capability and revision and resolve a single
    /// provider. The chosen endpoint is never a fallback candidate.
    pub fn route(&self, capability: &str, revision: &str) -> Option<AgentRoute<'_>> {
        self.grants
            .get(capability)
            .and_then(|revisions| revisions.get(revision))
            .map(|&backend| AgentRoute {
                backend,
                socket: &self.providers[&backend],
            })
    }
}

#[cfg(target_os = "linux")]
impl crate::WorkspacePaths {
    /// Load the optional host policy without opening its provider endpoint.
    pub fn load_agent_policy(&self) -> Result<Option<AgentPolicy>> {
        self.read_agent_config(&self.agent_policy_path())?
            .map(|bytes| AgentPolicy::parse(&bytes))
            .transpose()
    }

    /// Capture a policy under the configuration editor's private-file rules.
    pub fn read_agent_policy_snapshot(&self) -> Result<AgentPolicySnapshot> {
        self.read_config_snapshot(self.agent_policy_path(), |text, _| {
            AgentPolicy::parse(text.as_bytes()).map(Some)
        })
        .map(AgentPolicySnapshot)
    }

    /// Update a configured Provider or the default route against the captured policy.
    pub fn save_agent_policy(
        &self,
        snapshot: &AgentPolicySnapshot,
        edit: AgentPolicyEdit<'_>,
    ) -> Result<bool> {
        let policy = snapshot
            .policy()
            .ok()
            .flatten()
            .ok_or_else(|| anyhow!("A valid agents.toml is required to edit routing"))?;
        let backend = match edit {
            AgentPolicyEdit::Default(backend) | AgentPolicyEdit::Endpoint { backend, .. } => {
                backend
            }
        };
        let current_socket = policy
            .provider_socket(backend)
            .ok_or_else(|| anyhow!("Agent provider is not configured"))?;
        let source = snapshot
            .0
            .source
            .as_deref()
            .expect("a validated Agent policy has source text");
        let unchanged = match edit {
            AgentPolicyEdit::Default(backend) => policy.backend() == backend,
            AgentPolicyEdit::Endpoint { path, .. } => current_socket.as_os_str() == path,
        };
        if unchanged {
            return self.save_config_file(&self.agent_policy_path(), Some(source), None);
        }
        let mut document: toml::Table = toml::from_str(source)?;
        match edit {
            AgentPolicyEdit::Default(backend) => {
                document.insert("provider".into(), backend.name().into());
            }
            AgentPolicyEdit::Endpoint { backend, path } => {
                document
                    .get_mut("providers")
                    .and_then(toml::Value::as_table_mut)
                    .expect("a validated Agent policy has a providers table")
                    .insert(backend.name().into(), path.into());
            }
        }
        let replacement = toml::to_string(&document)?;
        AgentPolicy::parse(replacement.as_bytes())?;
        self.save_config_file(&self.agent_policy_path(), Some(source), Some(&replacement))
    }

    /// Read bounded provider configuration under the same ownership rules as
    /// the host policy. The `codex_config` module validates its contents.
    pub fn read_codex_provider_config(&self) -> Result<Option<Vec<u8>>> {
        self.read_agent_config(&self.codex_provider_path())
    }

    fn read_agent_config(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match self.read_workspace_config_bytes(path, crate::ConfigFileKind::Agent) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if crate::is_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// Whether a capability name can be granted by the host policy.
pub fn valid_capability(capability: &str) -> bool {
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
