#![cfg(unix)]

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
