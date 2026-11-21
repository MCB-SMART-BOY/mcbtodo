use std::{
    fs::{self, Metadata},
    io::{self, Write},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};

use anyhow::{Result, ensure};
use chrono_tz::Tz;
use mcbtodo_application::VaultQueries;
use mcbtodo_platform::{AppConfig, WorkspacePaths};
use serde::Serialize;

use crate::VaultLocation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    Warning,
    Offline,
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
    #[cfg(target_os = "linux")]
    for (id, path) in [
        ("sandbox_bubblewrap", "/usr/bin/bwrap"),
        ("sandbox_bash", "/usr/bin/bash"),
        ("sandbox_nushell", "/usr/bin/nu"),
        ("systemd_systemctl", "/usr/bin/systemctl"),
    ] {
        checks.push(inspect_executable(id, Path::new(path)));
    }
    Report {
        ok: checks.iter().all(|check| check.status != Status::Error),
        timezone,
        checks,
    }
}

fn inspect_config(paths: &WorkspacePaths, allow_missing: bool) -> (Check, Option<Tz>) {
    let path = paths.config_path();
    let check = |status, message| Check::new("config", status, &path, message);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => match paths.load_config() {
            Ok(config) => (
                check(
                    Status::Ok,
                    format!("Configured timezone: {}", config.timezone),
                ),
                Some(config.timezone),
            ),
            Err(error) => (check(Status::Error, format!("{error:#}")), None),
        },
        Ok(_) => (
            check(Status::Error, "Expected a regular config file".into()),
            None,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
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
        Err(error) => (
            check(Status::Error, format!("Cannot inspect config: {error}")),
            None,
        ),
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
        Ok(_) => (Status::Ok, "Private directory exists".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound && optional => (
            Status::Skipped,
            "Cache directory has not been created".into(),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            Status::Warning,
            "State directory is missing. Run `mcbtodo init` before starting the daemon".into(),
        ),
        Err(error) => (Status::Error, format!("Cannot inspect directory: {error}")),
    };
    Check::new(id, status, path, message)
}

fn owned_by_us(metadata: &Metadata) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw()
}

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

#[cfg(not(target_os = "linux"))]
fn inspect_listener(_path: &Path) -> (Status, String) {
    (
        Status::Skipped,
        "Passive daemon listener inspection is available on Linux".into(),
    )
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
