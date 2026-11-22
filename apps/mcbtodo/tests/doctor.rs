#![cfg(target_os = "linux")]

use std::{
    fs,
    io::ErrorKind,
    os::unix::{
        fs::{PermissionsExt, symlink},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    process::{Command, Output},
};

use mcbtodo_platform::WorkspacePaths;
use serde_json::Value;

fn doctor(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcbtodo"));
    command.env("HOME", home).arg("doctor");
    command
}

fn read_report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("missing {id} check in {report}"))
}

fn initialized_home() -> (tempfile::TempDir, WorkspacePaths) {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    (home, paths)
}

#[test]
fn offline_workspace_is_usable_and_doctor_leaves_runtime_untouched() {
    let (home, paths) = initialized_home();
    let state = paths.state_dir().join("workflow.sqlite3");
    let journal = paths.vault_root().join(".mcbtodo-events.sqlite3");
    let before = b"sentinel: doctor must not try to open the runtime database";
    for path in [&state, &journal] {
        fs::write(path, before).unwrap();
    }

    let output = doctor(home.path()).arg("--json").output().unwrap();
    assert!(output.status.success());
    let report = read_report(&output);
    assert_eq!(report["ok"], true);
    assert_eq!(report["timezone"], "UTC");
    for id in ["config", "vault", "state"] {
        assert_eq!(check(&report, id)["status"], "ok");
    }
    assert_eq!(check(&report, "daemon")["status"], "offline");
    assert_eq!(
        check(&report, "daemon")["path"],
        paths.socket_path().to_str().unwrap()
    );
    assert_eq!(check(&report, "cache")["status"], "skipped");
    assert_eq!(fs::read(&state).unwrap(), before);
    assert_eq!(fs::read(&journal).unwrap(), before);
    assert_eq!(fs::read_dir(paths.state_dir()).unwrap().count(), 1);
    assert_eq!(fs::read_dir(paths.vault_root()).unwrap().count(), 1);
    assert!(!paths.cache_dir().exists());

    let output = doctor(home.path()).output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Vault is valid (UTC)"));
    assert!(text.contains("No daemon socket"));
}

#[test]
fn doctor_distinguishes_notification_session_and_helper_availability() {
    let (home, paths) = initialized_home();
    let bin = home.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let inspect = |session: Option<&str>| {
        let mut command = doctor(home.path());
        command
            .env("PATH", &bin)
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .arg("--json");
        if let Some(session) = session {
            command.env("DBUS_SESSION_BUS_ADDRESS", session);
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        let report = read_report(&output);
        assert_eq!(report["ok"], true);
        assert!(!report.to_string().contains("private-session-address"));
        check(&report, "notification_desktop").clone()
    };

    assert_eq!(inspect(None)["status"], "skipped");
    assert_eq!(inspect(Some(""))["status"], "skipped");

    let session = Some("unix:path=/private-session-address");
    assert_eq!(inspect(session)["status"], "warning");
    let helper = bin.join("notify-send");
    fs::write(&helper, "#!/bin/sh\n: > \"$0.launched\"\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(inspect(session)["status"], "warning");

    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let installed = inspect(session);
    assert_eq!(installed["status"], "ok");
    assert_eq!(installed["path"], helper.to_str().unwrap());
    assert!(!helper.with_extension("launched").exists());
    assert_eq!(fs::read_dir(paths.state_dir()).unwrap().count(), 0);
}

#[test]
fn uninitialized_home_and_invalid_vault_produce_complete_error_reports() {
    let home = tempfile::tempdir().unwrap();
    let output = doctor(home.path()).arg("--json").output().unwrap();
    assert!(!output.status.success());
    let report = read_report(&output);
    assert_eq!(report["ok"], false);
    assert_eq!(check(&report, "config")["status"], "error");
    assert_eq!(check(&report, "vault")["status"], "error");
    assert_eq!(check(&report, "daemon")["status"], "offline");
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);

    let (home, paths) = initialized_home();
    let calendar = paths.vault_root().join("calendar");
    fs::create_dir(&calendar).unwrap();
    let bad_document = calendar.join("broken.md");
    let before = b"# Missing front matter\n";
    fs::write(&bad_document, before).unwrap();

    let output = doctor(home.path()).arg("--json").output().unwrap();
    assert!(!output.status.success());
    let report = read_report(&output);
    let vault = check(&report, "vault");
    assert_eq!(vault["status"], "error");
    assert!(vault["message"].as_str().unwrap().contains("broken.md"));
    assert_eq!(check(&report, "daemon")["status"], "offline");
    assert_eq!(fs::read(&bad_document).unwrap(), before);
}

#[test]
fn external_vault_uses_default_or_explicit_timezone_without_creating_config() {
    let home = tempfile::tempdir().unwrap();
    let vault = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault");
    let output = doctor(home.path())
        .arg("--vault")
        .arg(&vault)
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let report = read_report(&output);
    assert_eq!(report["timezone"], "UTC");
    assert_eq!(check(&report, "vault")["status"], "ok");
    assert_eq!(check(&report, "vault")["path"], vault.to_str().unwrap());
    let output = doctor(home.path())
        .arg("--vault")
        .arg(&vault)
        .args(["--timezone", "Asia/Shanghai", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(read_report(&output)["timezone"], "Asia/Shanghai");
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn doctor_reports_malformed_config_even_with_a_timezone_override() {
    let (home, paths) = initialized_home();
    fs::write(paths.config_path(), b"timezone = 'not-a-zone'\n").unwrap();
    let output = doctor(home.path())
        .args(["--timezone", "Asia/Shanghai", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report = read_report(&output);
    assert_eq!(check(&report, "config")["status"], "error");
    assert_eq!(check(&report, "vault")["status"], "ok");
    assert_eq!(report["timezone"], "Asia/Shanghai");
}

#[test]
fn doctor_reports_a_dangling_config_link_as_an_error() {
    let (home, paths) = initialized_home();
    let config = paths.config_path();
    fs::remove_file(&config).unwrap();
    symlink("missing.toml", &config).unwrap();
    let output = doctor(home.path())
        .args(["--timezone", "Asia/Shanghai", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(check(&read_report(&output), "config")["status"], "error");
}

#[test]
fn doctor_reports_optional_agent_policy_without_contacting_the_provider() {
    let (home, paths) = initialized_home();
    let listener = UnixListener::bind(paths.state_dir().join("agent.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let policy = paths.agent_policy_path();
    let inspect = || {
        let output = doctor(home.path()).arg("--json").output().unwrap();
        assert!(output.status.success());
        read_report(&output)
    };

    let report = inspect();
    assert_eq!(check(&report, "agent_policy")["status"], "skipped");
    assert_eq!(report["agent_providers"], serde_json::json!([]));
    fs::write(&policy, b"socket = 'state/agent.sock'\n").unwrap();
    let report = inspect();
    assert_eq!(check(&report, "agent_policy")["status"], "warning");
    assert_eq!(report["agent_providers"], serde_json::json!([]));
    fs::write(&policy, b"[providers]\nunix_socket = 'state/agent.sock'\n").unwrap();
    let report = inspect();
    let agent = check(&report, "agent_policy");
    assert_eq!(agent["status"], "ok");
    assert!(agent["message"].as_str().unwrap().contains("unix_socket"));
    fs::write(
        &policy,
        b"provider = 'codex'\n[providers]\ncodex = 'state/model.sock'\n",
    )
    .unwrap();
    let report = inspect();
    let agent = check(&report, "agent_policy");
    assert_eq!(agent["status"], "ok");
    assert!(agent["message"].as_str().unwrap().contains("codex"));
    fs::write(
        &policy,
        format!(
            "provider = 'unix_socket'\n[providers]\n\
            unix_socket = 'state/agent.sock'\ncodex = 'state/model.sock'\n\
            [[allow]]\ncapability = 'classify'\nrevision = '{}'\n\
            [[allow]]\ncapability = 'summarize'\nrevision = '{}'\nprovider = 'codex'\n",
            "a".repeat(64),
            "b".repeat(64)
        ),
    )
    .unwrap();
    let report = inspect();
    let agent = check(&report, "agent_policy");
    assert_eq!(agent["status"], "ok");
    assert!(
        agent["message"]
            .as_str()
            .unwrap()
            .contains("unix_socket, codex")
    );
    assert_eq!(check(&report, "agent_codex")["status"], "warning");
    assert_eq!(
        report["agent_providers"][0],
        serde_json::json!({
            "backend": "unix_socket",
            "is_default": true,
            "grant_count": 1,
            "installation": { "status": "not_checked" },
            "health": "unchecked"
        })
    );
    let codex = &report["agent_providers"][1];
    assert_eq!(codex["backend"], "codex");
    assert_eq!(codex["is_default"], false);
    assert_eq!(codex["grant_count"], 1);
    assert_eq!(codex["installation"]["status"], "unavailable");
    assert_eq!(codex["health"], "unchecked");
    assert!(!report.to_string().contains(&"a".repeat(64)));
    assert!(!report.to_string().contains(&"b".repeat(64)));

    fs::set_permissions(&policy, fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(check(&inspect(), "agent_policy")["status"], "warning");
    fs::set_permissions(&policy, fs::Permissions::from_mode(0o600)).unwrap();
    let malformed = b"unexpected = 'do-not-report-this-value'\n";
    fs::write(&policy, malformed).unwrap();
    let report = inspect();
    assert_eq!(check(&report, "agent_policy")["status"], "warning");
    assert!(!report.to_string().contains("do-not-report-this-value"));
    assert_eq!(fs::read(&policy).unwrap(), malformed);
    fs::write(
        &policy,
        vec![b' '; mcbtodo_platform::agent_policy::MAX_POLICY_BYTES + 1],
    )
    .unwrap();
    let report = inspect();
    let agent = check(&report, "agent_policy");
    assert_eq!(agent["status"], "warning");
    assert!(agent["message"].as_str().unwrap().contains("size limit"));
    let relocated = policy.with_extension("moved");
    fs::rename(&policy, &relocated).unwrap();
    symlink(&relocated, &policy).unwrap();
    assert_eq!(check(&inspect(), "agent_policy")["status"], "warning");
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn doctor_validates_selected_codex_installation_without_launching_or_contacting_it() {
    let (home, paths) = initialized_home();
    let listener = UnixListener::bind(paths.state_dir().join("model.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = paths.codex_provider_path();
    let inspect = || {
        let output = doctor(home.path()).arg("--json").output().unwrap();
        assert!(output.status.success());
        read_report(&output)
    };
    assert_eq!(check(&inspect(), "agent_codex")["status"], "skipped");
    fs::write(
        paths.agent_policy_path(),
        b"provider = 'codex'\n[providers]\ncodex = 'state/model.sock'\n",
    )
    .unwrap();
    assert_eq!(check(&inspect(), "agent_codex")["status"], "warning");
    fs::write(&config, b"instructions = 'do-not-report-this-value'\n").unwrap();
    let report = inspect();
    assert_eq!(check(&report, "agent_codex")["status"], "warning");
    assert!(!report.to_string().contains("do-not-report-this-value"));

    let write_config = |executable: &str| {
        fs::write(
            &config,
            format!(
                "executable = '{executable}'\ncapability = 'classify'\n\
                instructions = 'do-not-report-this-value'\n[outputs]\nlabel = 'string'\n"
            ),
        )
        .unwrap();
    };
    write_config("/dev/null");
    let report = inspect();
    let codex = check(&report, "agent_codex");
    assert_eq!(codex["status"], "warning");
    assert!(codex["message"].as_str().unwrap().contains("executable"));
    // A trusted executable can pass installation checks even if it does not
    // speak the Codex protocol. That requires the separate runtime probe.
    write_config("/bin/bash");
    let report = inspect();
    let codex = check(&report, "agent_codex");
    assert_eq!(codex["status"], "ok");
    let provider = &report["agent_providers"][0];
    assert_eq!(
        provider["installation"],
        serde_json::json!({
            "status": "preflight_passed",
            "declared_capability": "classify"
        })
    );
    assert_eq!(provider["health"], "unchecked");
    assert!(!report.to_string().contains("do-not-report-this-value"));
    assert!(
        codex["message"]
            .as_str()
            .unwrap()
            .contains("protocol compatibility were not tested")
    );
    let mut oversized = fs::read(&config).unwrap();
    oversized.resize(mcbtodo_platform::agent_policy::MAX_POLICY_BYTES + 1, b' ');
    fs::write(&config, oversized).unwrap();
    let report = inspect();
    let codex = check(&report, "agent_codex");
    assert_eq!(codex["status"], "warning");
    assert!(codex["message"].as_str().unwrap().contains("size limit"));
    assert!(!report.to_string().contains("do-not-report-this-value"));
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    assert_eq!(fs::read_dir(paths.state_dir()).unwrap().count(), 1);
}

#[test]
fn doctor_scopes_sandbox_checks_to_installation_metadata() {
    let (home, _) = initialized_home();
    let output = doctor(home.path())
        .env("PATH", "")
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let report = read_report(&output);
    assert_eq!(report["ok"], true);
    for id in ["sandbox_bubblewrap", "sandbox_bash", "sandbox_nushell"] {
        let check = check(&report, id);
        assert!(Path::new(check["path"].as_str().unwrap()).is_absolute());
        match check["status"].as_str().unwrap() {
            "ok" => assert!(
                check["message"]
                    .as_str()
                    .unwrap()
                    .contains("Sandbox startup and isolation were not tested")
            ),
            "warning" => assert!(
                check["message"]
                    .as_str()
                    .unwrap()
                    .contains("Sandbox executable preflight failed")
            ),
            status => panic!("unexpected sandbox check status: {status}"),
        }
    }
}

#[test]
fn doctor_reports_unsafe_state_and_rejects_a_symlink_socket() {
    let (home, paths) = initialized_home();
    fs::set_permissions(paths.state_dir(), fs::Permissions::from_mode(0o755)).unwrap();
    let target = home.path().join("other.sock");
    let listener = UnixListener::bind(&target).unwrap();
    listener.set_nonblocking(true).unwrap();
    symlink(&target, paths.socket_path()).unwrap();

    let output = doctor(home.path()).arg("--json").output().unwrap();
    assert!(!output.status.success());
    let report = read_report(&output);
    assert_eq!(check(&report, "state")["status"], "error");
    assert_eq!(check(&report, "daemon")["status"], "error");
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    assert_eq!(fs::read_link(paths.socket_path()).unwrap(), target);
}

#[cfg(target_os = "linux")]
#[test]
fn doctor_detects_the_listener_without_connecting_and_preserves_a_stale_socket() {
    let home = tempfile::Builder::new()
        .prefix("doctor with spaces-")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let socket = paths.socket_path();
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();

    let output = doctor(home.path()).arg("--json").output().unwrap();
    assert!(output.status.success());
    assert_eq!(check(&read_report(&output), "daemon")["status"], "ok");
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    drop(listener);
    let output = doctor(home.path()).arg("--json").output().unwrap();
    assert!(output.status.success());
    assert_eq!(check(&read_report(&output), "daemon")["status"], "warning");
    assert!(socket.exists());
}
