#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use sha2::{Digest, Sha256};

fn xtask(root: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .current_dir(root)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", home);
    command
}

fn agent(root: &Path, home: &Path) -> Command {
    let mut command = xtask(root, home);
    command.arg("agent");
    command
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn validation_fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let agents = root.join(".agents");
    fs::create_dir_all(&agents).unwrap();
    fs::create_dir_all(root.join("xtask")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(root.join("xtask/Cargo.toml"), "[package]\n").unwrap();
    fs::write(root.join("AGENTS.md"), "# Repository instructions\n").unwrap();
    fs::write(
        agents.join("registry.toml"),
        r#"
auto_order = ["test"]
default_workflow = "develop"
instructions = { path = "instructions.md" }
[harnesses.test]
executable = "test-client"
document = { path = "harness.md" }
[harnesses.generic]
document = { path = "generic.md" }
[workflows]
develop = { path = "workflow.md" }
"#,
    )
    .unwrap();
    for name in ["instructions.md", "harness.md", "generic.md", "workflow.md"] {
        fs::write(agents.join(name), format!("# {name}\n")).unwrap();
    }
    fs::write(agents.join("README.md"), "[`develop`](workflow.md)\n").unwrap();
    fs::write(
        root.join("docs/README.md"),
        "[`develop`](../.agents/workflow.md)\n",
    )
    .unwrap();
    temp
}

fn validate_registry(root: &Path) -> Output {
    agent(root, root).arg("validate").output().unwrap()
}

#[cfg(windows)]
#[test]
fn prepare_auto_detects_a_windows_executable_on_path() {
    let fixture = validation_fixture();
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join("test-client.cmd"), "exit /b 1").unwrap();
    let prepare = || {
        let prepared = stdout(
            agent(fixture.path(), home.path())
                .args(["prepare", "--offline"])
                .output()
                .unwrap(),
        );
        fs::read_to_string(prepared.trim_end()).unwrap()
    };
    assert!(prepare().contains("# generic.md"));
    fs::copy(
        env!("CARGO_BIN_EXE_xtask"),
        home.path().join("test-client.exe"),
    )
    .unwrap();
    assert!(prepare().contains("# harness.md"));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn linux_tasks_report_unsupported_before_creating_outputs() {
    let fixture = validation_fixture();
    let home = tempfile::tempdir().unwrap();
    for (args, message) in [
        (
            &["smoke", "--sandbox"][..],
            "workspace smoke requires Linux",
        ),
        (
            &["smoke-systemd"],
            "smoke-systemd requires a Linux user manager",
        ),
        (&["package"], "packaging requires Linux"),
        (
            &["workflow-example"],
            "the sandbox workflow example requires Linux",
        ),
    ] {
        let result = xtask(fixture.path(), home.path())
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(message), "{stderr}");
    }
    assert!(!fixture.path().join("target").exists());
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn bundled_registry_prepares_every_harness_and_workflow_on_a_cold_offline_home() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let read = |relative: &str| fs::read_to_string(root.join(relative)).unwrap();
    let entry = read("AGENTS.md");
    let instructions = read(".agents/instructions.md");
    for harness in ["codex", "claude", "opencode", "pi", "generic"] {
        for workflow in [
            "develop",
            "contribution",
            "review",
            "release",
            "documentation",
            "architecture",
            "architecture-evolution",
            "audit",
            "ports",
            "storage",
            "agent",
            "platform",
            "tui",
            "sync",
            "completion",
        ] {
            let home = tempfile::tempdir().unwrap();
            let result = stdout(
                agent(root, home.path())
                    .args([
                        "prepare",
                        "--harness",
                        harness,
                        "--workflow",
                        workflow,
                        "--offline",
                    ])
                    .output()
                    .unwrap(),
            );
            assert_eq!(
                fs::read_to_string(result.trim_end()).unwrap(),
                [
                    entry.clone(),
                    instructions.clone(),
                    read(&format!(".agents/harness/{harness}.md")),
                    read(&format!(".agents/workflows/{workflow}.md")),
                ]
                .join("\n\n")
            );
            assert!(!home.path().join(".mcbtodo/cache/agents/downloads").exists());
        }
    }
    let home = tempfile::tempdir().unwrap();
    let result = stdout(
        agent(root, home.path())
            .args(["prepare", "--offline"])
            .output()
            .unwrap(),
    );
    let context = fs::read_to_string(result.trim_end()).unwrap();
    assert!(context.contains(&read(".agents/harness/generic.md")));
    assert!(context.contains(&read(".agents/workflows/develop.md")));
}

#[test]
fn validate_accepts_the_bundled_registry() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let home = tempfile::tempdir().unwrap();
    let result = stdout(agent(root, home.path()).arg("validate").output().unwrap());
    assert_eq!(result, "agent registry valid\n");
    assert!(!home.path().join(".mcbtodo").exists());
}

#[test]
fn validate_rejects_missing_or_stale_workflow_index_links() {
    for (index, from, to, expected) in [
        (
            ".agents/README.md",
            "workflow.md",
            "removed.md",
            ".agents/README.md",
        ),
        (
            "docs/README.md",
            "develop",
            "old-workflow",
            "docs/README.md",
        ),
    ] {
        let temp = validation_fixture();
        let path = temp.path().join(index);
        let contents = fs::read_to_string(&path).unwrap();
        fs::write(&path, contents.replace(from, to)).unwrap();

        let result = validate_registry(temp.path());
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn validate_rejects_a_missing_or_invalid_repository_entrypoint() {
    let temp = validation_fixture();
    let entry = temp.path().join("AGENTS.md");
    fs::remove_file(&entry).unwrap();

    let result = validate_registry(temp.path());
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("AGENTS.md"));

    fs::write(&entry, [0xff]).unwrap();
    let result = validate_registry(temp.path());
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("UTF-8"));
}

#[test]
fn validate_rejects_missing_default_workflow() {
    let temp = validation_fixture();
    let registry = temp.path().join(".agents/registry.toml");
    let contents = fs::read_to_string(&registry).unwrap();
    fs::write(
        &registry,
        contents.replace(
            "default_workflow = \"develop\"",
            "default_workflow = \"missing\"",
        ),
    )
    .unwrap();

    let result = validate_registry(temp.path());
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("default workflow"));
}

#[test]
fn validate_rejects_unknown_and_duplicate_auto_order_entries() {
    for (from, to, expected) in [
        (
            "auto_order = [\"test\"]",
            "auto_order = [\"test\", \"missing\"]",
            "unregistered harness",
        ),
        (
            "auto_order = [\"test\"]",
            "auto_order = [\"test\", \"test\"]",
            "duplicate harness",
        ),
    ] {
        let temp = validation_fixture();
        let registry = temp.path().join(".agents/registry.toml");
        let contents = fs::read_to_string(&registry).unwrap();
        fs::write(&registry, contents.replace(from, to)).unwrap();

        let result = validate_registry(temp.path());
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn validate_rejects_missing_generic_fallback() {
    let temp = validation_fixture();
    let registry = temp.path().join(".agents/registry.toml");
    let contents = fs::read_to_string(&registry).unwrap();
    fs::write(
        &registry,
        contents.replace(
            "[harnesses.generic]\ndocument = { path = \"generic.md\" }\n",
            "",
        ),
    )
    .unwrap();

    let result = validate_registry(temp.path());
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("generic fallback"));
}

#[test]
fn validate_rejects_missing_and_escaping_local_sources() {
    for (path, expected) in [
        ("missing.md", "resolving local source"),
        ("../outside.md", "relative Markdown path"),
    ] {
        let temp = validation_fixture();
        let registry = temp.path().join(".agents/registry.toml");
        let contents = fs::read_to_string(&registry).unwrap();
        fs::write(
            &registry,
            contents.replace(
                "instructions = { path = \"instructions.md\" }",
                &format!("instructions = {{ path = \"{path}\" }}"),
            ),
        )
        .unwrap();

        let result = validate_registry(temp.path());
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn validate_rejects_remote_sources_without_https_markdown_and_lowercase_sha256() {
    for (url, sha256, expected) in [
        (
            "http://example.invalid/workflow.md",
            "a".repeat(64),
            "HTTPS",
        ),
        (
            "https://example.invalid/workflow.txt",
            "a".repeat(64),
            ".md path",
        ),
        (
            "https://example.invalid/workflow.md",
            "A".repeat(64),
            "lowercase SHA-256",
        ),
    ] {
        let temp = validation_fixture();
        let registry = temp.path().join(".agents/registry.toml");
        let contents = fs::read_to_string(&registry).unwrap();
        let remote = format!("develop = {{ url = \"{url}\", sha256 = \"{sha256}\" }}");
        fs::write(
            &registry,
            contents.replace("develop = { path = \"workflow.md\" }", &remote),
        )
        .unwrap();

        let result = validate_registry(temp.path());
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn validate_checks_remote_pin_format_without_fetching() {
    let temp = validation_fixture();
    let root = temp.path();
    let registry = root.join(".agents/registry.toml");
    let contents = fs::read_to_string(&registry).unwrap();
    let payload = "# Remote workflow\n";
    let pin = hex::encode(Sha256::digest(payload));
    fs::write(
        &registry,
        contents.replace(
            "develop = { path = \"workflow.md\" }",
            &format!(
                "develop = {{ url = \"https://example.invalid/workflow.md\", sha256 = \"{pin}\" }}"
            ),
        ),
    )
    .unwrap();

    assert_eq!(stdout(validate_registry(root)), "agent registry valid\n");
    assert!(!root.join(".mcbtodo").exists());
}

#[test]
#[cfg(unix)]
fn run_from_a_member_delivers_the_selected_context_and_task_to_the_client() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("other-checkout");
    let agents = root.join(".agents");
    let member = root.join("xtask");
    fs::create_dir_all(&agents).unwrap();
    fs::create_dir_all(&member).unwrap();
    fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(member.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "# Root instructions from the other checkout\n",
    )
    .unwrap();
    fs::write(
        agents.join("registry.toml"),
        r#"
auto_order = ["missing", "test"]
default_workflow = "unused"
instructions = { path = "instructions.md" }
[harnesses.missing]
executable = "missing-client"
session_vars = ["MISSING_SESSION"]
document = { path = "missing.md" }
[harnesses.test]
executable = "test-client"
args = ["--prompt"]
document = { path = "harness.md" }
[harnesses.generic]
document = { path = "missing.md" }
[workflows]
test = { path = "workflow.md" }
unused = { path = "missing.md" }
"#,
    )
    .unwrap();
    fs::write(agents.join("instructions.md"), "# Common instructions\n").unwrap();
    fs::write(agents.join("harness.md"), "# Harness instructions\n").unwrap();
    fs::write(agents.join("workflow.md"), "# Workflow instructions\n").unwrap();
    script(
        &temp.path().join("test-client"),
        r#"#!/bin/sh
[ "$#" -eq 2 ] && [ "$1" = "--prompt" ] || exit 64
printf '%s\n%s\n%s\n%s' "$PWD" "$MCBTODO_HARNESS" "$MCBTODO_WORKFLOW" "$2"
"#,
    );

    let invalid = agent(&member, temp.path())
        .env("MCBTODO_HARNESS", "missing")
        .args(["run", "--workflow", "test", "--offline", "task"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("was not found on PATH"));
    assert!(!temp.path().join(".mcbtodo").exists());

    let result = stdout(
        agent(&member, temp.path())
            .env("MISSING_SESSION", "1")
            .env("MCBTODO_WORKFLOW", "unused")
            .args([
                "run",
                "--workflow",
                "test",
                "--offline",
                "--",
                "--task\nwith a second line",
            ])
            .output()
            .unwrap(),
    );
    let (working_dir, prompt) = result.split_once('\n').unwrap();
    assert_eq!(
        Path::new(working_dir).canonicalize().unwrap(),
        root.canonicalize().unwrap()
    );
    assert_eq!(
        prompt,
        "test\ntest\nFollow the repository instructions, harness guide and workflow below.\n\n\
         # Root instructions from the other checkout\n\n\n# Common instructions\n\n\n\
         # Harness instructions\n\n\n# Workflow instructions\n\n\nTask:\n--task\nwith a second line"
    );

    let long_workflow = "review this change\n".repeat(6700);
    fs::write(agents.join("workflow.md"), &long_workflow).unwrap();
    let run = stdout(
        agent(&member, temp.path())
            .args(["run", "--workflow", "test", "--offline", "task"])
            .output()
            .unwrap(),
    );
    let prepared = stdout(
        agent(&member, temp.path())
            .args([
                "prepare",
                "--harness",
                "test",
                "--workflow",
                "test",
                "--offline",
            ])
            .output()
            .unwrap(),
    );
    assert!(run.contains(prepared.trim_end()));
    let context = fs::read_to_string(prepared.trim_end()).unwrap();
    assert!(context.starts_with("# Root instructions from the other checkout\n"));
    assert!(context.ends_with(&long_workflow));
}

#[test]
#[cfg(unix)]
fn remote_selection_fetches_only_online_and_reuses_verified_bytes_offline() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join(".agents")).unwrap();
    fs::create_dir(root.join("xtask")).unwrap();
    fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(root.join("xtask/Cargo.toml"), "[package]\n").unwrap();
    fs::write(root.join("AGENTS.md"), "# Root\n").unwrap();
    fs::write(root.join(".agents/local.md"), "# Bundled\n").unwrap();
    let payload = "# Pinned remote workflow\n";
    let pin = hex::encode(Sha256::digest(payload));
    fs::write(
        root.join(".agents/registry.toml"),
        format!(
            r#"
auto_order = []
default_workflow = "remote"
instructions = {{ path = "local.md" }}
[harnesses.generic]
document = {{ path = "local.md" }}
[workflows]
local = {{ path = "local.md" }}
remote = {{ url = "https://example.invalid/workflow.md", sha256 = "{pin}" }}
"#
        ),
    )
    .unwrap();
    script(
        &root.join("curl"),
        r#"#!/bin/sh
: > fetched
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) target=$2; shift ;;
        --url) [ "$2" = "https://example.invalid/workflow.md" ] || exit 64; shift ;;
    esac
    shift
done
printf '%s' "$FETCH_PAYLOAD" > "$target"
"#,
    );
    let prepare = |args: &[&str], response| {
        agent(root, root)
            .env("FETCH_PAYLOAD", response)
            .arg("prepare")
            .args(args)
            .output()
            .unwrap()
    };
    let marker = root.join("fetched");
    stdout(prepare(&["--offline", "--workflow", "local"], ""));
    let cold = prepare(&["--offline"], payload);
    assert!(!cold.status.success());
    assert!(String::from_utf8_lossy(&cold.stderr).contains("not cached"));
    assert!(!marker.exists());

    let download = root.join(format!(".mcbtodo/cache/agents/downloads/{pin}.md"));
    let mismatch = prepare(&[], "wrong bytes");
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("digest mismatch"));
    assert!(marker.exists());
    assert!(!download.exists());

    let online = stdout(prepare(&[], payload));
    assert_eq!(fs::read_to_string(download).unwrap(), payload);
    assert!(
        fs::read_to_string(online.trim_end())
            .unwrap()
            .ends_with(payload)
    );
    fs::remove_file(&marker).unwrap();
    assert_eq!(stdout(prepare(&["--offline"], "")), online);
    assert_eq!(stdout(prepare(&[], "")), online);
    assert!(!marker.exists());
}
