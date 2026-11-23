#[cfg(target_os = "linux")]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use anyhow::{Result, ensure};
use chrono_tz::Tz;
use mcbtodo_application::VaultQueries;
#[cfg(target_os = "linux")]
use mcbtodo_execution::{codex_host, notification::NotifySend, sandbox::SandboxExecutable};
#[cfg(target_os = "linux")]
use mcbtodo_platform::agent_policy::AgentBackend;
use mcbtodo_platform::{AppConfig, WorkspacePaths};
use serde::Serialize;

use crate::{VaultLocation, agent_discovery};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    Warning,
    #[cfg(target_os = "linux")]
    Offline,
    #[cfg(not(target_os = "linux"))]
    Unsupported,
    Skipped,
    Error,
}

#[derive(Serialize)]
struct Check {
    id: &'static str,
    status: Status,
    path: Option<String>,
    message: String,
}

impl Check {
    fn new(id: &'static str, status: Status, path: &Path, message: impl Into<String>) -> Self {
        Self {
            id,
            status,
            path: Some(path.display().to_string()),
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
struct Report {
    ok: bool,
    timezone: Option<Tz>,
    checks: Vec<Check>,
    agent_providers: Vec<agent_discovery::Provider>,
}

pub(crate) fn run(
    paths: Result<WorkspacePaths>,
    location: VaultLocation,
    json: bool,
) -> Result<()> {
    let report = match paths {
        Ok(paths) => inspect(&paths, location),
        Err(error) => Report {
            ok: false,
            timezone: None,
            checks: vec![Check {
                id: "home",
                status: Status::Error,
                path: None,
                message: error.to_string(),
            }],
            agent_providers: Vec::new(),
        },
    };
    let mut out = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
    } else {
        for check in &report.checks {
            write!(out, "[{:?}] {}", check.status, check.id)?;
            if let Some(path) = &check.path {
                write!(out, " ({})", path.escape_debug())?;
            }
            writeln!(out, " — {}", check.message.escape_debug())?;
        }
    }
    out.flush()?;
    ensure!(report.ok, "doctor found workspace errors");
    Ok(())
}

fn inspect(paths: &WorkspacePaths, location: VaultLocation) -> Report {
    let (config, configured_timezone) = inspect_config(
        paths,
        location.timezone_override.is_some() || location.root_override.is_some(),
    );
    let timezone = location
        .timezone_override
        .or(configured_timezone)
        .or_else(|| {
            (location.root_override.is_some() && config.status == Status::Warning)
                .then_some(AppConfig::default().timezone)
        });
    let vault_root = location.root_override.unwrap_or_else(|| paths.vault_root());
    let state_dir = paths.state_dir();
    let state = inspect_private_dir("state", &state_dir, false);
    let daemon = inspect_daemon(&paths.socket_path(), state.status);
    let mut checks = vec![
        config,
        inspect_vault(&vault_root, timezone),
        state,
        inspect_private_dir("cache", &paths.cache_dir(), true),
        daemon,
    ];
    let agents = agent_discovery::Discovery::inspect(paths);
    #[cfg(target_os = "linux")]
    let agents = check_agent_installations(paths, agents);
    checks.extend(inspect_agents(paths, &agents));
    #[cfg(target_os = "linux")]
    checks.extend([
        inspect_sandbox_executable("sandbox_bubblewrap", SandboxExecutable::Bubblewrap),
        inspect_sandbox_executable("sandbox_bash", SandboxExecutable::Bash),
        inspect_sandbox_executable("sandbox_nushell", SandboxExecutable::Nushell),
        inspect_desktop_notification(),
        inspect_executable("systemd_systemctl", Path::new("/usr/bin/systemctl")),
    ]);
    #[cfg(not(target_os = "linux"))]
    checks.extend(
        [
            ("sandbox_bubblewrap", "Bubblewrap isolation requires Linux"),
            ("sandbox_bash", "The sandboxed Bash runner requires Linux"),
            (
                "sandbox_nushell",
                "The sandboxed Nushell runner requires Linux",
            ),
            (
                "notification_desktop",
                "The desktop notification adapter requires Linux",
            ),
            (
                "systemd_systemctl",
                "The systemd user service requires Linux",
            ),
        ]
        .map(|(id, message)| Check {
            id,
            status: Status::Unsupported,
            path: None,
            message: message.into(),
        }),
    );
    Report {
        ok: checks.iter().all(|check| check.status != Status::Error),
        timezone,
        checks,
        agent_providers: agents.providers,
    }
}

#[cfg(target_os = "linux")]
fn inspect_desktop_notification() -> Check {
    let id = "notification_desktop";
    let Some(sink) = NotifySend::for_session() else {
        return Check {
            id,
            status: Status::Skipped,
            path: None,
            message: "No desktop session bus configured in this process environment".into(),
        };
    };
    match sink.check_installation() {
        Ok(path) => Check::new(
            id,
            Status::Ok,
            &path,
            "Notification helper is executable in this environment. Desktop delivery was not tested",
        ),
        Err(error) => Check {
            id,
            status: Status::Warning,
            path: None,
            message: error.to_string(),
        },
    }
}

#[cfg(target_os = "linux")]
fn check_agent_installations(
    paths: &WorkspacePaths,
    mut agents: agent_discovery::Discovery,
) -> agent_discovery::Discovery {
    use agent_discovery::Installation;

    if let Some(provider) = agents
        .providers
        .iter_mut()
        .find(|provider| provider.backend == AgentBackend::Codex)
    {
        provider.installation = match codex_host::check_installation(paths) {
            Ok(declared_capability) => Installation::PreflightPassed {
                declared_capability,
            },
            Err(error) => Installation::Unavailable {
                message: error.to_string(),
            },
        };
    }
    agents
}

#[cfg(target_os = "linux")]
fn inspect_agents(paths: &WorkspacePaths, agents: &agent_discovery::Discovery) -> [Check; 2] {
    use agent_discovery::{Installation, Policy};

    let (status, message) = match &agents.policy {
        Policy::Configured => {
            let configured = agents
                .providers
                .iter()
                .map(|provider| provider.backend.name())
                .collect::<Vec<_>>()
                .join(", ");
            let default = agents
                .providers
                .iter()
                .find(|provider| provider.is_default)
                .map_or("", |provider| provider.backend.name());
            (
                Status::Ok,
                format!(
                    "Agent providers: {configured}. Default: {default}. Endpoints were not contacted"
                ),
            )
        }
        Policy::Absent => (Status::Skipped, agents.policy.message().into()),
        Policy::Unusable(error) => (Status::Warning, error.clone()),
    };
    let (codex_status, codex_message) = match agents
        .providers
        .iter()
        .find(|provider| provider.backend == AgentBackend::Codex)
        .map(|provider| &provider.installation)
    {
        Some(Installation::PreflightPassed { .. }) => (
            Status::Ok,
            "Codex installation preflight passed. Model connectivity and protocol compatibility were not tested".into(),
        ),
        Some(Installation::Unavailable { message }) => (Status::Warning, message.clone()),
        _ => (
            Status::Skipped,
            "Codex requires a valid Agent policy that configures it".into(),
        ),
    };
    [
        Check::new("agent_policy", status, &paths.agent_policy_path(), message),
        Check::new(
            "agent_codex",
            codex_status,
            &paths.codex_provider_path(),
            codex_message,
        ),
    ]
}

#[cfg(not(target_os = "linux"))]
fn inspect_agents(_paths: &WorkspacePaths, agents: &agent_discovery::Discovery) -> [Check; 2] {
    ["agent_policy", "agent_codex"].map(|id| Check {
        id,
        status: Status::Unsupported,
        path: None,
        message: agents.policy.message().into(),
    })
}

fn inspect_config(paths: &WorkspacePaths, allow_missing: bool) -> (Check, Option<Tz>) {
    let path = paths.config_path();
    let check = |status, message| Check::new("config", status, &path, message);
    match paths.load_optional_config() {
        Ok(Some(config)) => (
            check(
                Status::Ok,
                format!("Configured timezone: {}", config.timezone),
            ),
            Some(config.timezone),
        ),
        Ok(None) => (
            if allow_missing {
                check(
                    Status::Warning,
                    "Config is missing. This invocation can use a timezone without it".into(),
                )
            } else {
                check(
                    Status::Error,
                    "Config is missing. Run `mcbtodo init` to create it".into(),
                )
            },
            None,
        ),
        Err(error) => (check(Status::Error, format!("{error:#}")), None),
    }
}

fn inspect_vault(root: &Path, timezone: Option<Tz>) -> Check {
    let (status, message) = match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => match timezone {
            Some(timezone) => match VaultQueries::new(root, timezone).validate() {
                Ok(()) => (Status::Ok, format!("Vault is valid ({timezone})")),
                Err(error) => (Status::Error, format!("{error:#}")),
            },
            None => (
                Status::Skipped,
                "Vault validation needs a valid configured timezone or `--timezone`".into(),
            ),
        },
        Ok(_) => (Status::Error, "Expected a Vault directory".into()),
        Err(error) => (
            Status::Error,
            format!("Cannot open the Vault directory: {error}"),
        ),
    };
    Check::new("vault", status, root, message)
}

fn inspect_private_dir(id: &'static str, path: &Path, optional: bool) -> Check {
    let (status, message) = match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => (Status::Error, "Expected a real directory".into()),
        #[cfg(unix)]
        Ok(metadata)
            if !owned_by_us(&metadata)
                || metadata.mode() & 0o077 != 0
                || metadata.mode() & 0o700 != 0o700 =>
        {
            (
                Status::Error,
                "Directory must be private, owned by the current user, and accessible to its owner"
                    .into(),
            )
        }
        #[cfg(unix)]
        Ok(_) => (Status::Ok, "Private directory exists".into()),
        #[cfg(not(unix))]
        Ok(_) => (
            Status::Skipped,
            "Directory exists. Ownership and access checks are unavailable on this platform".into(),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound && optional => (
            Status::Skipped,
            "Cache directory has not been created".into(),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            Status::Warning,
            "State directory is missing. Run `mcbtodo init` to create it".into(),
        ),
        Err(error) => (Status::Error, format!("Cannot inspect directory: {error}")),
    };
    Check::new(id, status, path, message)
}

#[cfg(unix)]
fn owned_by_us(metadata: &fs::Metadata) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(target_os = "linux")]
fn inspect_daemon(path: &Path, state_status: Status) -> Check {
    let (status, message) = match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_socket() || !owned_by_us(&metadata) => (
            Status::Error,
            "Expected a Unix socket owned by the current user".into(),
        ),
        Ok(_) if state_status != Status::Ok => (
            Status::Skipped,
            "Listener check needs a usable state directory".into(),
        ),
        Ok(_) => inspect_listener(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            Status::Offline,
            "No daemon socket. The CLI can use a valid Vault offline".into(),
        ),
        Err(error) => (
            Status::Error,
            format!("Cannot inspect daemon socket: {error}"),
        ),
    };
    Check::new("daemon", status, path, message)
}

#[cfg(not(target_os = "linux"))]
fn inspect_daemon(path: &Path, _state_status: Status) -> Check {
    Check::new(
        "daemon",
        Status::Unsupported,
        path,
        "The local daemon transport is unavailable on this platform",
    )
}

#[cfg(target_os = "linux")]
fn inspect_listener(path: &Path) -> (Status, String) {
    use std::{io::BufRead, os::unix::ffi::OsStrExt};

    // Connecting wakes the workflow loop, which can start work before reading an IPC frame.
    // The kernel's table provides a passive, advisory check in the current network namespace.
    let name = path.as_os_str().as_bytes();
    if name.contains(&b'\n') {
        return (
            Status::Skipped,
            "Kernel listener listing cannot represent this path unambiguously".into(),
        );
    }
    let listening = || -> io::Result<bool> {
        let mut lines = io::BufReader::new(fs::File::open("/proc/self/net/unix")?).split(b'\n');
        if !lines
            .next()
            .transpose()?
            .is_some_and(|header| header.starts_with(b"Num "))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected Unix socket table header",
            ));
        }
        for line in lines {
            if is_listening_at(&line?, name) {
                return Ok(true);
            }
        }
        Ok(false)
    };
    match listening() {
        Ok(true) => (Status::Ok, "Unix stream listener listed at this path (passive check)".into()),
        Ok(false) => (
            Status::Warning,
            "Socket exists without a matching listener in the kernel listing. The socket may be stale".into(),
        ),
        Err(error) => (Status::Warning, format!("Cannot inspect the kernel's Unix socket table: {error}")),
    }
}

#[cfg(target_os = "linux")]
fn is_listening_at(mut line: &[u8], path: &[u8]) -> bool {
    // Skip the seven fixed fields. The remaining bytes are the path, including its spaces.
    for column in 0..7 {
        line = line.trim_ascii_start();
        let Some(end) = line.iter().position(u8::is_ascii_whitespace) else {
            return false;
        };
        // Linux prints __SO_ACCEPTCON in Flags for listening sockets and 0001 for SOCK_STREAM.
        if (column == 3 && &line[..end] != b"00010000") || (column == 4 && &line[..end] != b"0001")
        {
            return false;
        }
        line = &line[end..];
    }
    line.strip_prefix(b" ") == Some(path)
}

#[cfg(target_os = "linux")]
fn inspect_sandbox_executable(id: &'static str, executable: SandboxExecutable) -> Check {
    let (status, message) = match executable.check_installation() {
        Ok(()) => (
            Status::Ok,
            "Sandbox executable preflight passed. Sandbox startup and isolation were not tested"
                .into(),
        ),
        Err(error) => (
            Status::Warning,
            format!("Sandbox executable preflight failed: {error:#}"),
        ),
    };
    Check::new(id, status, executable.path(), message)
}

#[cfg(target_os = "linux")]
fn inspect_executable(id: &'static str, path: &Path) -> Check {
    let (status, message) = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.mode() & 0o111 != 0 => (
            Status::Ok,
            "Executable file present (metadata check only)".into(),
        ),
        Ok(_) => (
            Status::Warning,
            "Expected an executable regular file".into(),
        ),
        Err(error) => (
            Status::Warning,
            format!("Optional executable unavailable: {error}"),
        ),
    };
    Check::new(id, status, path, message)
}
