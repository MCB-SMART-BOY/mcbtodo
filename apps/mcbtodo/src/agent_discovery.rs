//! Agent policy summary shared by Settings and doctor.

#[cfg(target_os = "linux")]
use mcbtodo_platform::agent_policy::AgentPolicy;
use mcbtodo_platform::{WorkspacePaths, agent_policy::AgentBackend};
use serde::Serialize;

pub(crate) struct Discovery {
    pub(crate) policy: Policy,
    pub(crate) providers: Vec<Provider>,
}

pub(crate) enum Policy {
    #[cfg(target_os = "linux")]
    Configured,
    #[cfg(target_os = "linux")]
    Absent,
    #[cfg(target_os = "linux")]
    Unusable(String),
    #[cfg(not(target_os = "linux"))]
    Unsupported,
}

impl Policy {
    pub(crate) fn message(&self) -> &str {
        match self {
            #[cfg(target_os = "linux")]
            Self::Configured => "Agent policy is configured",
            #[cfg(target_os = "linux")]
            Self::Absent => "No agents.toml configured",
            #[cfg(target_os = "linux")]
            Self::Unusable(error) => error,
            #[cfg(not(target_os = "linux"))]
            Self::Unsupported => "Workflow Agents require Linux",
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Provider {
    pub(crate) backend: AgentBackend,
    pub(crate) is_default: bool,
    pub(crate) grant_count: usize,
    pub(crate) installation: Installation,
    pub(crate) health: &'static str,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Installation {
    #[cfg(target_os = "linux")]
    NotChecked,
    #[cfg(target_os = "linux")]
    PreflightPassed { declared_capability: String },
    #[cfg(target_os = "linux")]
    Unavailable { message: String },
}

impl Discovery {
    #[cfg(target_os = "linux")]
    pub(crate) fn inspect(paths: &WorkspacePaths) -> Self {
        Self::from_policy(paths.load_agent_policy().as_ref().map(Option::as_ref))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn from_policy(policy: Result<Option<&AgentPolicy>, &anyhow::Error>) -> Self {
        match policy {
            Ok(Some(policy)) => Self {
                policy: Policy::Configured,
                providers: policy
                    .configured_providers()
                    .map(|backend| Provider {
                        backend,
                        is_default: policy.backend() == backend,
                        grant_count: policy.grant_count(backend),
                        health: "unchecked",
                        installation: Installation::NotChecked,
                    })
                    .collect(),
            },
            Ok(None) => Self {
                policy: Policy::Absent,
                providers: Vec::new(),
            },
            Err(error) => Self {
                policy: Policy::Unusable(format!("Agent policy is unusable: {error:#}")),
                providers: Vec::new(),
            },
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn check_installations(&mut self, paths: &WorkspacePaths) {
        if let Some(codex) = self
            .providers
            .iter_mut()
            .find(|provider| provider.backend == AgentBackend::Codex)
        {
            codex.installation = match mcbtodo_execution::codex_host::check_installation(paths) {
                Ok(declared_capability) => Installation::PreflightPassed {
                    declared_capability,
                },
                Err(error) => Installation::Unavailable {
                    message: error.to_string(),
                },
            };
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn inspect(_paths: &WorkspacePaths) -> Self {
        Self {
            policy: Policy::Unsupported,
            providers: Vec::new(),
        }
    }
}
