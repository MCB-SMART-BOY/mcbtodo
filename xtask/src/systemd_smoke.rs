use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use mcbtodo_platform::WorkspacePaths;
use serde_json::Value;

const WORK_UNIT: &str = "30000000-0000-4000-8000-000000000002";

pub(crate) fn run(root: &Path, bin_dir: &Path) -> Result<()> {
    let home = WorkspacePaths::discover()?.home_dir().canonicalize()?;
    let bwrap = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("bwrap"))
        .find(|path| path.is_file())
        .context("smoke-systemd requires Bubblewrap (bwrap)")?
        .canonicalize()?;
    checked_output(
        Command::new("systemctl").args(["--user", "show-environment"]),
        "contacting the systemd user manager",
    )?;

    let staging = tempfile::Builder::new()
        .prefix("mcbtodo-smoke-")
        .tempdir_in("/tmp")?;
    let directory = staging.path();
    let workspace = directory.join("workspace");
    fs::create_dir(&workspace)?;
    for binary in ["mcbtodo", "mcbtodo-daemon"] {
        fs::copy(bin_dir.join(binary), directory.join(binary))
            .with_context(|| format!("staging {binary} from {}", bin_dir.display()))?;
    }
    let smoke = SystemdSmoke {
        bwrap,
        home,
        workspace,
        directory,
    };
    smoke.cli(&["init"])?;
    crate::smoke::copy_fixture(
        &root.join("examples/vault"),
        &WorkspacePaths::from_home(smoke.workspace.clone()).vault_root(),
    )?;

    let mut service = UserService {
        name: format!(
            "{}.service",
            directory
                .file_name()
                .context("missing staging name")?
                .display()
        ),
        diagnostics: true,
    };
    checked_output(
        Command::new("systemd-run")
            .args(["--user", "--collect"])
            .arg(format!("--unit={}", service.name))
            .args([
                "--property=Type=exec",
                "--property=Restart=on-failure",
                "--property=RestartSec=3s",
                "--property=NoNewPrivileges=yes",
                "--property=UMask=0077",
            ])
            .arg("--")
            .arg(&smoke.bwrap)
            .args(["--unsetenv", "DBUS_SESSION_BUS_ADDRESS"])
            .args(smoke.sandbox_args())
            .arg(directory.join("mcbtodo-daemon")),
        "starting the transient user service",
    )?;

    smoke.wait_for_timer("idle with no session", |status| {
        status["state"] == "idle" && status.get("session").is_some_and(Value::is_null)
    })?;
    let started = smoke.json(&["timer", "start", "--unit", WORK_UNIT, "--json"])?;
    let started_id = started["session"]["id"]
        .as_str()
        .context("timer start response has no session ID")?;
    let date = started["session"]["started_at"]
        .as_str()
        .and_then(|stamp| stamp.get(..10))
        .context("timer start response has no start date")?;
    smoke.read_timer_in_tui(started_id)?;
    let elapsed_at_exit = smoke.json(&["timer", "status", "--json"])?["session"]["elapsed_seconds"]
        .as_u64()
        .context("running timer has no elapsed time after TUI exit")?;
    smoke.wait_for_timer("running past the checkpoint", |status| {
        status["state"] == "running"
            && status["session"]["id"] == started_id
            && status["session"]["elapsed_seconds"]
                .as_u64()
                .is_some_and(|elapsed| elapsed >= 6 && elapsed > elapsed_at_exit)
    })?;
    smoke.read_timer_in_tui(started_id)?;

    const CAPTURED_TASK: &str = "Task from systemd smoke";
    crate::smoke::drive_default_tui(
        smoke.client(),
        &[
            ("Month", " ul ct"),
            ("Title", &format!("{CAPTURED_TASK}\r")),
            ("Task saved", "q"),
        ],
    )?;
    ensure!(
        smoke.cli(&["outline"])?.contains(CAPTURED_TASK),
        "the Task captured over IPC is absent from the Vault"
    );

    const CAPTURED_EVENT: &str = "All-day event from systemd smoke";
    let mut client = smoke.client();
    client.args(["tui", "--date", "2026-11-30"]);
    crate::smoke::drive_default_tui(
        client,
        &[
            ("Month", "vl ce"),
            ("Title", &format!("{CAPTURED_EVENT}\r\r\r")),
            ("All-day event saved", "q"),
        ],
    )?;
    let mut created_id = None;
    for (day, expected) in [
        ("2026-11-30", true),
        ("2026-12-01", true),
        ("2026-12-02", false),
    ] {
        let calendar = smoke.json(&["calendar", "--date", day, "--json"])?;
        let events: Vec<_> = calendar
            .as_array()
            .context("calendar response is not an array")?
            .iter()
            .filter(|item| item["title"] == CAPTURED_EVENT)
            .collect();
        ensure!(
            events.len() == usize::from(expected),
            "unexpected all-day event count on {day}: {events:?}"
        );
        if let [event] = events.as_slice() {
            let id = event["id"].as_str().context("event has no ID")?;
            ensure!(
                event["kind"] == "event"
                    && event["busy"] == true
                    && *created_id.get_or_insert_with(|| id.to_owned()) == id,
                "unexpected all-day event on {day}: {event}"
            );
        }
    }

    const REMINDER_TITLE: &str = "Review notes from systemd smoke";
    let timezone = WorkspacePaths::from_home(smoke.workspace.clone())
        .load_config()?
        .timezone;
    let at = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() - 60, 0)
        .context("smoke reminder timestamp out of range")?;
    let input = at.with_timezone(&timezone).to_rfc3339();
    crate::smoke::drive_default_tui(
        smoke.client(),
        &[
            ("Month", " cr"),
            ("Title", &format!("{REMINDER_TITLE}\t{input}\r")),
            ("Reminder saved", "q"),
        ],
    )?;
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let page = smoke.json(&["notifications", "--reminders"])?;
        if page["notifications"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["title"] == REMINDER_TITLE
                    && row["scheduled_at"].as_str().and_then(|timestamp| {
                        timestamp.parse::<chrono::DateTime<chrono::Utc>>().ok()
                    }) == Some(at)
            })
        }) {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "the saved reminder did not appear in the daemon Inbox: {page}"
        );
        thread::sleep(Duration::from_millis(250));
    }

    for (key, title, dates, saved, kind) in [
        (
            "p",
            "Research from systemd smoke",
            "\r\r",
            "Project saved",
            "active_period",
        ),
        (
            "d",
            "Deadline from systemd smoke",
            "\r",
            "Deadline saved",
            "deadline",
        ),
    ] {
        let mut client = smoke.client();
        client.args(["tui", "--date", "2026-11-30"]);
        crate::smoke::drive_default_tui(
            client,
            &[
                ("Month", &format!(" c{key}")),
                ("Title", &format!("{title}\r{dates}")),
                (saved, "q"),
            ],
        )?;
        let calendar = smoke.json(&["calendar", "--date", "2026-11-30", "--json"])?;
        ensure!(
            calendar
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry["title"] == title && entry["kind"] == kind && entry["busy"] == false
                })),
            "the saved {kind} is absent from the Vault or has unexpected calendar semantics"
        );
    }

    checked_output(
        &mut service.systemctl(&["kill", "--signal=KILL", "--kill-whom=all"]),
        "killing the transient service to test restart",
    )?;
    let recovered = smoke.wait_for_timer("needs_review after the forced restart", |status| {
        status["state"] == "needs_review"
    })?;
    let elapsed = recovered["session"]["elapsed_seconds"]
        .as_u64()
        .context("recovered timer has no elapsed time")?;
    ensure!(
        recovered["session"]["id"] == started_id && elapsed >= 5,
        "restart did not preserve the timer identity and checkpoint duration: {recovered}"
    );

    let args = ["--timezone", "UTC", "timer", "report", "--date", date];
    let mut json_args = args.to_vec();
    json_args.push("--json");
    let report = smoke.json(&json_args)?;
    ensure!(
        report["date"] == date
            && report["session_count"] == 1
            && report["needs_review_count"] == 1
            && report["total_elapsed_seconds"] == elapsed
            && report["by_work_unit"]
                .as_array()
                .is_some_and(|rows| { rows.len() == 1 && rows[0]["work_unit_id"] == WORK_UNIT }),
        "unexpected recovered focus report: {report}"
    );
    let markdown = smoke.cli(&args)?;
    ensure!(
        markdown.contains(date) && markdown.contains(WORK_UNIT),
        "Markdown focus report omitted the date or WorkUnit"
    );
    smoke.cli(&["timer", "stop"])?;
    checked_output(
        &mut service.systemctl(&["stop"]),
        "stopping the transient service",
    )?;
    service.diagnostics = false;
    println!(
        "systemd user service smoke passed (TUI reopen, Task, Project, deadline and all-day event creation, reminder Inbox, timer recovery, and focus report)"
    );
    Ok(())
}

struct SystemdSmoke<'a> {
    bwrap: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    directory: &'a Path,
}

impl SystemdSmoke<'_> {
    fn sandbox_args(&self) -> [&std::ffi::OsStr; 12] {
        [
            "--ro-bind".as_ref(),
            "/".as_ref(),
            "/".as_ref(),
            "--bind".as_ref(),
            self.workspace.as_os_str(),
            self.home.as_os_str(),
            "--setenv".as_ref(),
            "HOME".as_ref(),
            self.home.as_os_str(),
            "--chdir".as_ref(),
            "/".as_ref(),
            "--".as_ref(),
        ]
    }

    fn client(&self) -> Command {
        let mut command = Command::new(&self.bwrap);
        command
            .args(self.sandbox_args())
            .arg(self.directory.join("mcbtodo"));
        command
    }

    fn read_timer_in_tui(&self, session_id: &str) -> Result<()> {
        crate::smoke::drive_default_tui(self.client(), &[("Month", " ti"), (session_id, "q")])
    }

    fn cli(&self, args: &[&str]) -> Result<String> {
        checked_output(
            self.client().args(args),
            &format!("running isolated mcbtodo {args:?}"),
        )
    }

    fn json(&self, args: &[&str]) -> Result<Value> {
        let stdout = self.cli(args)?;
        serde_json::from_str(&stdout).with_context(|| format!("decoding mcbtodo {args:?} JSON"))
    }

    fn wait_for_timer(&self, expected: &str, ready: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let observed = match self.json(&["timer", "status", "--json"]) {
                Ok(status) if ready(&status) => return Ok(status),
                Ok(status) => status.to_string(),
                Err(error) => format!("{error:#}"),
            };
            if Instant::now() >= deadline {
                bail!("timer did not reach {expected}; last observation: {observed}");
            }
            thread::sleep(Duration::from_millis(250));
        }
    }
}

struct UserService {
    name: String,
    diagnostics: bool,
}

impl UserService {
    fn systemctl(&self, args: &[&str]) -> Command {
        let mut command = Command::new("systemctl");
        command.arg("--user").args(args).arg(&self.name);
        command
    }
}

impl Drop for UserService {
    fn drop(&mut self) {
        if self.diagnostics {
            let _ = self.systemctl(&["status", "--no-pager"]).status();
            let _ = Command::new("journalctl")
                .args(["-n", "30", "--no-pager", "--user-unit"])
                .arg(&self.name)
                .status();
            let _ = self
                .systemctl(&["stop"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn checked_output(command: &mut Command, operation: &str) -> Result<String> {
    let output = command.output().with_context(|| operation.to_owned())?;
    ensure!(
        output.status.success(),
        "{operation} exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).with_context(|| format!("reading output from {operation}"))
}
