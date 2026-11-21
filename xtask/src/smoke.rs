use std::{
    fs,
    io::{ErrorKind, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{TimeDelta, Utc};
use mcbtodo_platform::WorkspacePaths;
use rustix::{
    fs::{OFlags, fcntl_setfl},
    io::Errno,
    pty::{OpenptFlags, ioctl_tiocgptpeer, openpt, unlockpt},
    termios::{Winsize, tcsetwinsize},
};
use serde_json::{Value, json};

const GRAPH: &str = r##"
schema: mcbtodo.workflow/v1
id: smoke
name: Smoke workflow
entry: review
nodes:
  review: {kind: approval}
  report: {kind: action, action: report.create}
  declined: {kind: action, action: report.create}
edges:
  - {from: review, to: report, when: approved}
  - {from: review, to: declined, when: rejected}
literals:
  - {to: {node: report, port: summary}, value: "# Approved"}
  - {to: {node: declined, port: summary}, value: "# Declined"}
"##;

const MCP_WORKFLOW_GRAPH: &str = r##"
schema: mcbtodo.workflow/v1
id: mcp-review-smoke
name: MCP review smoke
entry: check
nodes:
  check:
    kind: script
    runner: nushell
    source: scripts/check.nu
    timeout_seconds: 30
    capabilities: []
    outputs: {summary: string}
  report: {kind: action, action: report.create}
edges:
  - {from: check, to: report}
bindings:
  - {from: {node: check, port: summary}, to: {node: report, port: summary}}
"##;
const MCP_WORKFLOW_SOURCE: &str = "print 'MCP reviewed workflow'\n";

const MAPPING_UNIT: &str = "30000000-0000-4000-8000-000000000002";
const READ_SPEC_UNIT: &str = "30000000-0000-4000-8000-000000000001";
const TRANSLATION_UNIT: &str = "30000000-0000-4000-8000-000000000003";
const TEST_UNIT: &str = "30000000-0000-4000-8000-000000000004";
const RUST_OS_PROJECT: &str = "10000000-0000-4000-8000-000000000001";
const MEMORY_TASK: &str = "20000000-0000-4000-8000-000000000001";

pub(crate) fn run(root: &Path, bin_dir: &Path, sandbox: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let smoke = Smoke { bin_dir, home };
    smoke.cli(&["init"])?;
    verify_first_run(&smoke)?;
    exercise_packaged_worker_rejects_malformed_prepare(bin_dir)?;
    let paths = smoke.paths();
    let vault = paths.vault_root();
    copy_fixture(&root.join("examples/vault"), &vault)?;
    let rust_os = vault.join("projects/rust-os");
    fs::remove_dir_all(rust_os.join("units"))?;
    smoke.cli(&["validate"])?;
    exercise_packaged_supervisor_rejects_malformed_worker(temp.path(), bin_dir, root)?;
    let (applied_proposal, mcp_workflow_revision) = exercise_planning(&smoke, &rust_os)?;

    let graph = temp.path().join("graph.yaml");
    fs::write(&graph, GRAPH)?;
    let revision = smoke.cli(&["workflow", "publish", path_arg(&graph)?])?;
    let revision = revision.trim();
    ensure!(revision.len() == 64, "publication did not return a SHA-256");
    let mut daemon = smoke.start_daemon()?;
    ensure!(
        smoke.json(&["workflow", "runs"])?["runs"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "approving an MCP workflow proposal unexpectedly enqueued a run"
    );
    let unauthorized = smoke.json(&["workflow", "run", &mcp_workflow_revision])?;
    let unauthorized_id = unauthorized["id"]
        .as_str()
        .context("MCP workflow run has no ID")?;
    let unauthorized = smoke.await_run(unauthorized_id, "failed")?;
    ensure!(
        unauthorized["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("authorization")),
        "MCP workflow publication unexpectedly authorized script execution: {unauthorized}"
    );
    smoke.cli(&["timer", "start", "--unit", MAPPING_UNIT])?;
    let status = smoke.json(&["timer", "status", "--json"])?;
    ensure!(
        status["state"] == "running" && status["session"]["work_unit_id"] == MAPPING_UNIT,
        "timer did not start for the mapping WorkUnit: {status}"
    );
    let report_day = status["session"]["started_at"]
        .as_str()
        .and_then(|stamp| stamp.get(..10))
        .context("focus session has no start date")?;
    let session_id = status["session"]["id"]
        .as_str()
        .context("focus session has no ID")?;
    smoke.drive_tui("UTC", &[("NORMAL", " ti"), (session_id, "q")])?;
    let elapsed_at_exit = smoke.json(&["timer", "status", "--json"])?["session"]["elapsed_seconds"]
        .as_u64()
        .context("focus session has no elapsed time after TUI exit")?;
    poll("focus time to advance after TUI exit", || {
        let current = smoke.json(&["timer", "status", "--json"])?;
        ensure!(
            current["state"] == "running" && current["session"]["id"] == status["session"]["id"],
            "the focus session stopped or changed when the TUI exited: {current}"
        );
        Ok(current["session"]["elapsed_seconds"]
            .as_u64()
            .is_some_and(|elapsed| elapsed > elapsed_at_exit))
    })?;
    smoke.cli(&["timer", "pause"])?;
    let paused = smoke.json(&["timer", "status", "--json"])?;
    ensure!(paused["state"] == "paused", "timer did not pause: {paused}");
    smoke.cli(&["timer", "stop"])?;
    let report = smoke.json(&["timer", "report", "--date", report_day, "--json"])?;
    ensure!(
        report["session_count"] == 1
            && report["by_work_unit"][0]["work_unit_id"] == MAPPING_UNIT
            && report["total_elapsed_seconds"] == paused["session"]["elapsed_seconds"]
            && report["by_work_unit"][0]["elapsed_seconds"] == report["total_elapsed_seconds"],
        "focus report did not preserve the mapping session's paused duration: {report}"
    );
    ensure!(
        smoke
            .cli(&["timer", "report", "--date", report_day])?
            .contains(MAPPING_UNIT),
        "Markdown focus report is missing the WorkUnit"
    );
    let run = smoke.json(&["workflow", "run", revision])?;
    let id = run["id"].as_str().context("enqueued run has no ID")?;
    smoke.await_run(id, "waiting")?;
    daemon.stop()?;
    daemon = smoke.start_daemon()?;
    smoke.await_run(id, "waiting")?;
    smoke.cli(&["workflow", "approve", id, "review"])?;
    smoke.await_run(id, "succeeded")?;
    let step = smoke.json(&["workflow", "trace", id, "2"])?;
    ensure!(
        step["trace"]["node"] == "report",
        "expected the approved report step, got {step}"
    );
    ensure!(
        smoke.cli(&["workflow", "artifact", id, "2"])? == "# Approved",
        "unexpected artifact content"
    );
    exercise_schedules(&smoke, revision)?;
    let downstream = exercise_dependency(temp.path(), &smoke, revision, &mut daemon)?;
    let (completion, source_event) = exercise_completion_trigger(&smoke, &downstream, &mut daemon)?;
    let failed_check = if sandbox {
        let failed_check = exercise_sandbox(root, temp.path(), &smoke)?;
        exercise_interrupted_worker(temp.path(), &smoke, &mut daemon)?;
        Some(failed_check)
    } else {
        exercise_cron(&smoke, revision)?;
        None
    };
    let completion_events = smoke.cli(&["vault", "completion-events"])?;
    let bundle = temp.path().join("backup");
    let restored_home = temp.path().join("restored");
    daemon.stop()?;
    smoke.cli(&["backup", path_arg(&bundle)?])?;
    smoke.cli(&["restore", path_arg(&bundle)?, path_arg(&restored_home)?])?;
    let restored = Smoke {
        bin_dir,
        home: restored_home,
    };
    restored.cli(&["validate"])?;
    ensure!(
        restored.cli(&["vault", "completion-events"])? == completion_events,
        "backup did not preserve WorkUnit completion events"
    );
    let restored_proposal = restored.json(&["proposal", "show", &applied_proposal])?;
    ensure!(
        restored_proposal["status"] == "applied",
        "backup did not preserve the applied proposal"
    );
    restored.cli(&["proposal", "apply", &applied_proposal])?;
    let mut restored_daemon = restored.start_daemon()?;
    restored.await_run(id, "succeeded")?;
    restored.await_run(&downstream, "succeeded")?;
    ensure!(
        restored.await_run(&completion, "succeeded")?["source_event_id"] == source_event,
        "backup did not preserve the completion trigger's source event"
    );
    let notifications = restored.json(&["notifications"])?;
    ensure!(
        notifications["notifications"]
            .as_array()
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| { entry["run_id"] == id && entry["state"] == "succeeded" })),
        "backup did not preserve the terminal workflow notification"
    );
    ensure!(
        restored.cli(&["workflow", "artifact", id, "2"])? == "# Approved",
        "backup did not preserve the artifact"
    );
    if let Some(failed_check) = failed_check {
        assert_failed_check(&restored, &failed_check)?;
    }
    restored_daemon.stop()?;
    println!("Workspace smoke test passed");
    Ok(())
}

fn verify_first_run(smoke: &Smoke<'_>) -> Result<()> {
    let paths = smoke.paths();
    let config = paths.config_path();
    let workspace = paths.data_dir();
    let state = paths.state_dir();
    let vault = paths.vault_root();
    ensure!(config.is_file(), "init did not create the user config");
    let original_config = fs::read(&config)?;

    for path in [&workspace, &state, &vault] {
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o777 == 0o700,
            "{} is not private",
            path.display()
        );
    }
    ensure!(
        fs::metadata(&config)?.permissions().mode() & 0o777 == 0o600,
        "user config is not private"
    );

    smoke.cli(&["init"])?;
    ensure!(
        fs::read(&config)? == original_config,
        "repeated init overwrote the existing config"
    );
    let doctor = smoke.json(&["doctor", "--json"])?;
    ensure!(
        doctor["ok"].as_bool().unwrap_or(false)
            && doctor["checks"].as_array().is_some_and(|checks| checks
                .iter()
                .any(|check| { check["id"] == "daemon" && check["status"] == "offline" })),
        "fresh initialized workspace did not pass doctor: {doctor}"
    );
    Ok(())
}

fn exercise_planning(smoke: &Smoke<'_>, rust_os: &Path) -> Result<(String, String)> {
    let projects = smoke.mcp_call("list_projects", json!({}))?;
    ensure!(
        projects["projects"]
            .as_array()
            .is_some_and(|items| items.iter().any(|project| project["id"] == RUST_OS_PROJECT)),
        "MCP did not expose the Rust OS project"
    );
    let calendar = |day| {
        smoke.json(&[
            "--timezone",
            "Asia/Shanghai",
            "calendar",
            "--date",
            day,
            "--json",
        ])
    };
    let opening = calendar("2026-11-20")?;
    ensure!(
        opening
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| {
                entry["id"] == RUST_OS_PROJECT
                    && entry["kind"] == "active_period"
                    && entry["busy"] == false
            })),
        "the Rust OS project did not appear as a non-busy activity period"
    );
    let deadline = calendar("2027-03-01")?;
    ensure!(
        deadline
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| {
                entry["id"] == RUST_OS_PROJECT
                    && entry["kind"] == "deadline"
                    && entry["busy"] == false
            })),
        "the project deadline did not appear on 2027-03-01"
    );

    let units = json!([
        {"id": READ_SPEC_UNIT, "title": "Read Sv39", "estimated_minutes": 60,
         "body": "- [ ] Record PTE flags and virtual address layout."},
        {"id": MAPPING_UNIT, "title": "Implement page mapping", "estimated_minutes": 120,
         "body": "- [ ] Map a virtual page to the requested physical page.", "depends_on": [READ_SPEC_UNIT]},
        {"id": TRANSLATION_UNIT, "title": "Implement address translation", "estimated_minutes": 90,
         "body": "- [ ] Resolve a mapped address with its page offset intact.", "depends_on": [READ_SPEC_UNIT]},
        {"id": TEST_UNIT, "title": "Test the page table", "estimated_minutes": 45,
         "body": "- [ ] Cover mapped and unmapped addresses and invalid page flags.", "depends_on": [MAPPING_UNIT, TRANSLATION_UNIT]}
    ]);
    let draft = json!({"task_id": MEMORY_TASK, "units": units});
    let diff = smoke.mcp_call("preview_work_unit_graph_diff", draft.clone())?;
    let edges = diff["added_edges"]
        .as_array()
        .context("missing Graph Diff edges")?;
    ensure!(
        diff["added_nodes"]
            .as_array()
            .is_some_and(|units| units.len() == 4)
            && diff["ready"] == json!([READ_SPEC_UNIT])
            && edges.len() == 4
            && [
                (READ_SPEC_UNIT, MAPPING_UNIT),
                (READ_SPEC_UNIT, TRANSLATION_UNIT),
                (MAPPING_UNIT, TEST_UNIT),
                (TRANSLATION_UNIT, TEST_UNIT)
            ]
            .iter()
            .all(|(from, to)| edges
                .iter()
                .any(|edge| edge["prerequisite"] == *from && edge["dependent"] == *to)),
        "Graph Diff did not expose the two parallel branches and their join: {diff}"
    );
    let propose = || -> Result<String> {
        let result = smoke.mcp_call("propose_work_unit_graph_diff", draft.clone())?;
        ensure!(
            result["graph_diff"] == diff,
            "queued Graph Diff differs from the validated preview"
        );
        Ok(result["proposal_id"]
            .as_str()
            .context("MCP did not return a proposal UUID")?
            .to_owned())
    };
    let work_units = || smoke.mcp_call("list_work_units", json!({"task_id": MEMORY_TASK}));

    let rejected = propose()?;
    smoke.cli(&["proposal", "reject", &rejected])?;
    let stale = propose()?;
    let task_path = rust_os.join("tasks/memory.md");
    let mut task = fs::read_to_string(&task_path)?;
    task.push_str("\nReviewed the acceptance plan.\n");
    fs::write(&task_path, task)?;
    let conflict = smoke.approve_proposal(&stale, false)?;
    ensure!(
        conflict.contains(&format!("stale revision for document {MEMORY_TASK}")),
        "proposal did not report the edited task's revision conflict: {conflict}"
    );
    let approved = propose()?;
    ensure!(
        work_units()?["work_units"] == json!([]),
        "pending, rejected, or stale proposals changed the Vault"
    );
    smoke.approve_proposal_in_tui(&approved)?;
    ensure!(
        work_units()?["work_units"]
            .as_array()
            .is_some_and(|units| units.len() == 4),
        "approval did not publish all four WorkUnits"
    );
    exercise_completion_parity(smoke, rust_os)?;

    let workflow_proposal = smoke.mcp_call(
        "propose_workflow_draft",
        json!({
            "graph": MCP_WORKFLOW_GRAPH,
            "sources": {
                "scripts/check.nu": STANDARD.encode(MCP_WORKFLOW_SOURCE.as_bytes())
            }
        }),
    )?;
    ensure!(
        workflow_proposal["status"] == "queued"
            && workflow_proposal["execution_authorized"] == false,
        "MCP workflow proposal was not queued for review only: {workflow_proposal}"
    );
    let workflow_proposal_id = workflow_proposal["proposal_id"]
        .as_str()
        .context("MCP workflow proposal has no ID")?;
    let workflow_revision = workflow_proposal["workflow_revision"]
        .as_str()
        .context("MCP workflow proposal has no revision")?
        .to_owned();
    ensure!(
        workflow_revision.len() == 64
            && !smoke
                .cli(&["workflow", "revisions"])?
                .lines()
                .any(|revision| revision == workflow_revision),
        "queued MCP workflow snapshot was published before human review"
    );
    smoke.approve_workflow_proposal_in_tui(workflow_proposal_id)?;
    ensure!(
        smoke
            .cli(&["workflow", "revisions"])?
            .lines()
            .any(|revision| revision == workflow_revision),
        "TUI approval did not publish the reviewed MCP workflow revision"
    );

    smoke.create_time_block_in_tui()?;
    let planned = calendar("2026-11-20")?;
    ensure!(
        planned
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| {
                entry["id"] == MAPPING_UNIT
                    && entry["kind"] == "time_block"
                    && entry["busy"] == true
                    && entry["occurrence"]["start"] == "2026-11-20T09:00:00+08:00"
                    && entry["occurrence"]["end"] == "2026-11-20T11:00:00+08:00"
            })),
        "the mapping time block did not appear in the calendar: {planned}"
    );
    Ok((approved, workflow_revision))
}

fn exercise_completion_parity(smoke: &Smoke<'_>, project: &Path) -> Result<()> {
    let default_vault = smoke.paths().vault_root();
    let cli_vault = tempfile::tempdir()?;
    copy_fixture(&default_vault, cli_vault.path())?;
    let unit_path = format!("units/{READ_SPEC_UNIT}.md");
    let mcp_document = project.join(&unit_path);
    let cli_document = cli_vault.path().join("projects/rust-os").join(&unit_path);

    let before = fs::read(&mcp_document)?;
    let proposal = smoke.mcp_call(
        "propose_complete_work_unit",
        json!({"work_unit_id": READ_SPEC_UNIT}),
    )?;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .context("MCP did not return a completion proposal UUID")?;
    ensure!(
        fs::read(&mcp_document)? == before
            && smoke.cli(&["vault", "completion-events"])?.is_empty(),
        "pending completion proposal changed the Vault"
    );
    smoke.approve_proposal(proposal_id, true)?;
    smoke.cli(&["proposal", "apply", proposal_id])?;

    let cli_root = path_arg(cli_vault.path())?;
    let complete = [
        "--vault",
        cli_root,
        "vault",
        "complete",
        "--unit",
        READ_SPEC_UNIT,
    ];
    smoke.cli(&complete)?;
    smoke.cli(&complete)?;
    ensure!(
        fs::read(&mcp_document)? == fs::read(cli_document)?,
        "direct and approved completion wrote different Markdown"
    );

    let normalize = |output: String| -> Result<Value> {
        let mut event: Value = serde_json::from_str(&output)
            .context("expected exactly one completion event after a retry")?;
        ensure!(
            event["work_unit_id"] == READ_SPEC_UNIT,
            "unexpected event: {event}"
        );
        let fields = event
            .as_object_mut()
            .context("completion event isn't an object")?;
        ensure!(
            fields.remove("event_id").is_some() && fields.remove("occurred_at").is_some(),
            "missing completion event identity or timestamp"
        );
        Ok(event)
    };
    ensure!(
        normalize(smoke.cli(&["vault", "completion-events"])?)?
            == normalize(smoke.cli(&["--vault", cli_root, "vault", "completion-events"])?)?,
        "direct and approved completion recorded different event data"
    );
    Ok(())
}

fn exercise_schedules(smoke: &Smoke<'_>, revision: &str) -> Result<()> {
    let due = (Utc::now() + TimeDelta::seconds(1)).to_rfc3339();
    let run = smoke.json(&["workflow", "run", revision, "--at", &due])?;
    let id = run["id"].as_str().context("one-time run has no ID")?;
    smoke.await_run(id, "waiting")?;
    smoke.cli(&["workflow", "reject", id, "review"])?;
    smoke.await_run(id, "succeeded")?;
    ensure!(
        smoke.cli(&["workflow", "artifact", id, "1"])? == "# Declined",
        "one-time run did not follow the rejection branch"
    );

    let interval = smoke.json(&[
        "workflow",
        "schedule",
        "interval",
        revision,
        "--from",
        &Utc::now().to_rfc3339(),
        "--seconds",
        "1",
    ])?;
    let schedule_id = interval["id"].as_str().context("interval has no ID")?;
    poll("first interval run", || {
        let page = smoke.json(&["workflow", "runs"])?;
        let runs = page["runs"].as_array().context("missing run page")?;
        Ok(runs.iter().any(|run| run["schedule_id"] == schedule_id))
    })?;
    smoke.cli(&["workflow", "schedule", "disable", schedule_id])?;
    Ok(())
}

fn exercise_cron(smoke: &Smoke<'_>, revision: &str) -> Result<()> {
    let cron = smoke.json(&[
        "workflow",
        "schedule",
        "cron",
        revision,
        "0 23 * * *",
        "--zone",
        "Asia/Tokyo",
        "--missed",
        "run-once",
    ])?;
    let schedule_id = cron["id"].as_str().context("cron schedule has no ID")?;
    let persisted = smoke.json(&["workflow", "schedule", "inspect", schedule_id])?;
    ensure!(
        persisted["revision"] == revision
            && persisted["trigger"]["expression"] == "0 23 * * *"
            && persisted["trigger"]["timezone"] == "Asia/Tokyo"
            && persisted["missed"]["policy"] == "run_once"
            && persisted["next_at"]
                .as_str()
                .is_some_and(|time| time.ends_with("T14:00:00Z")),
        "cron did not persist the nightly 23:00 Asia/Tokyo schedule: {persisted}"
    );
    smoke.cli(&["workflow", "schedule", "disable", schedule_id])?;
    Ok(())
}

fn exercise_dependency(
    temp: &Path,
    smoke: &Smoke<'_>,
    upstream: &str,
    daemon: &mut Daemon,
) -> Result<String> {
    let graph = temp.join("after-review.yaml");
    fs::write(
        &graph,
        r##"
schema: mcbtodo.workflow/v1
id: after-review
name: After review
entry: report
nodes:
  report: {kind: action, action: report.create}
literals:
  - {to: {node: report, port: summary}, value: "# Review finished"}
"##,
    )?;
    let revision = smoke.cli(&["workflow", "publish", path_arg(&graph)?])?;
    let revision = revision.trim();
    let schedule = smoke.json(&["workflow", "schedule", "dependency", revision, upstream])?;
    let schedule_id = schedule["id"].as_str().context("dependency has no ID")?;
    ensure!(
        schedule["trigger"]["upstream_revision"] == upstream
            && schedule["revision"] == revision
            && schedule["next_at"].is_null(),
        "dependency did not retain its pinned revisions: {schedule}"
    );

    let source = smoke.json(&["workflow", "run", upstream])?;
    let source_id = source["id"].as_str().context("upstream run has no ID")?;
    smoke.await_run(source_id, "waiting")?;
    smoke.cli(&["workflow", "approve", source_id, "review"])?;
    smoke.await_run(source_id, "succeeded")?;

    let find_child = || -> Result<String> {
        let page = smoke.json(&["workflow", "runs"])?;
        let runs = page["runs"].as_array().context("missing run page")?;
        let mut children = runs.iter().filter(|run| run["schedule_id"] == schedule_id);
        let child = children.next().context("dependency did not queue a run")?;
        ensure!(
            children.next().is_none()
                && child["source_run_id"] == source_id
                && child["revision"] == revision,
            "unexpected dependency runs: {runs:?}"
        );
        Ok(child["id"]
            .as_str()
            .context("downstream run has no ID")?
            .to_owned())
    };
    let child = find_child()?;
    smoke.await_run(&child, "succeeded")?;
    daemon.stop()?;
    *daemon = smoke.start_daemon()?;
    smoke.cli(&["workflow", "run", upstream, "--run-id", source_id])?;
    ensure!(
        child == find_child()?
            && smoke.cli(&["workflow", "artifact", &child, "0"])? == "# Review finished",
        "dependency receipt or report changed after restart and repeated submission"
    );
    smoke.cli(&["workflow", "schedule", "disable", schedule_id])?;
    Ok(child)
}

fn exercise_completion_trigger(
    smoke: &Smoke<'_>,
    downstream: &str,
    daemon: &mut Daemon,
) -> Result<(String, String)> {
    let run = smoke.json(&["workflow", "inspect", downstream])?;
    let revision = run["revision"].as_str().context("run has no revision")?;
    let rule = smoke.json(&[
        "workflow",
        "schedule",
        "work-unit-completed",
        revision,
        MAPPING_UNIT,
    ])?;
    let rule_id = rule["id"].as_str().context("completion rule has no ID")?;
    daemon.stop()?;
    smoke.cli(&["vault", "complete", "--unit", MAPPING_UNIT])?;
    let events = smoke.cli(&["vault", "completion-events"])?;
    let mut event_id = None;
    for line in events.lines() {
        let event: Value = serde_json::from_str(line)?;
        if event["work_unit_id"] == MAPPING_UNIT {
            ensure!(event_id.is_none(), "duplicate mapping completion");
            event_id = event["event_id"].as_str().map(str::to_owned);
        }
    }
    let event_id = event_id.context("offline completion did not write an event")?;
    *daemon = smoke.start_daemon()?;
    let find_run = || -> Result<Option<String>> {
        let page = smoke.json(&["workflow", "runs"])?;
        let runs = page["runs"].as_array().context("missing run page")?;
        let mut runs = runs.iter().filter(|run| run["schedule_id"] == rule_id);
        let Some(run) = runs.next() else {
            return Ok(None);
        };
        ensure!(
            runs.next().is_none()
                && run["source_event_id"] == event_id
                && run["revision"] == revision
                && run["source_run_id"].is_null(),
            "unexpected WorkUnit completion run: {run}"
        );
        Ok(Some(
            run["id"]
                .as_str()
                .context("completion run has no ID")?
                .to_owned(),
        ))
    };
    let mut completion = None;
    poll("offline WorkUnit completion trigger", || {
        completion = find_run()?;
        Ok(completion.is_some())
    })?;
    let completion = completion.context("completion trigger did not enqueue")?;
    smoke.await_run(&completion, "succeeded")?;
    daemon.stop()?;
    *daemon = smoke.start_daemon()?;
    smoke.cli(&["vault", "complete", "--unit", MAPPING_UNIT])?;
    ensure!(
        find_run()?.as_ref() == Some(&completion),
        "completion trigger lost its run on restart"
    );
    smoke.cli(&["workflow", "schedule", "disable", rule_id])?;
    Ok((completion, event_id))
}

fn exercise_sandbox(root: &Path, temp: &Path, smoke: &Smoke<'_>) -> Result<String> {
    let fixture = temp.join("fixture/examples/workflow");
    fs::create_dir_all(&fixture)?;
    copy_fixture(&root.join("examples/workflow"), &fixture)?;
    let fixture_root = temp.join("fixture");
    let graph = crate::example::build(&fixture_root, &temp.join("nightly"), false)?;
    let revision = smoke.cli(&["workflow", "publish", path_arg(&graph)?])?;
    exercise_worker_failure(temp, smoke)?;
    // The worker must use the published bytes when the draft is unavailable.
    fs::remove_file(graph.with_file_name("page-table-check"))?;
    let revision = revision.trim();
    let refused = smoke.json(&["workflow", "run", revision])?;
    let id = refused["id"]
        .as_str()
        .context("unauthorized run has no ID")?;
    let refused = smoke.await_run(id, "failed")?;
    ensure!(
        refused["detail"]
            .as_str()
            .is_some_and(|s| s.contains("authorization")),
        "unauthorized script did not fail its authorization check"
    );
    smoke.cli(&["workflow", "authorize-script", revision, "test"])?;
    exercise_cron(smoke, revision)?;

    let binding = smoke.cli(&[
        "vault",
        "bind-workflow",
        "--unit",
        TEST_UNIT,
        "--revision",
        revision,
    ])?;
    let test_file = binding
        .strip_prefix(&format!("{TEST_UNIT} "))
        .context("binding did not return the Test WorkUnit's document")?
        .trim_end();
    let failed_check = exercise_agent(&fixture_root, &temp.join("nightly-fail"), smoke, test_file)?;

    let due = (Utc::now() + TimeDelta::seconds(1)).to_rfc3339();
    let run = smoke.json(&["workflow", "run", revision, "--at", &due])?;
    let id = run["id"].as_str().context("authorized run has no ID")?;
    let finished = smoke.await_run(id, "succeeded")?;
    ensure!(
        finished["revision"] == revision
            && finished["due_at"] == run["due_at"]
            && finished["due_at"].is_string(),
        "the scheduled run lost its due time or pinned revision: {finished}"
    );
    let trace = smoke.json(&["workflow", "trace", id, "0"])?;
    ensure!(
        trace["trace"]["node"] == "test" && trace["trace"]["outputs"]["passed"] == true,
        "the scheduled check did not return a passing TestResult: {trace}"
    );
    let logs = smoke.json(&["workflow", "artifact", id, "0"])?;
    ensure!(
        logs["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("ok preserve page offset"))
            && logs["stderr"].is_string(),
        "worker did not save the Rust test output: {logs}"
    );
    smoke.cli(&["vault", "complete", "--unit", TRANSLATION_UNIT])?;
    let before = fs::read(test_file)?;
    let prior_events = smoke.cli(&["vault", "completion-events"])?;
    let wrong_revision = smoke
        .command("mcbtodo")
        .args([
            "vault",
            "complete",
            "--unit",
            TEST_UNIT,
            "--evidence",
            &format!("{failed_check}:0"),
        ])
        .output()?;
    ensure!(
        !wrong_revision.status.success()
            && String::from_utf8_lossy(&wrong_revision.stderr).contains("work unit is bound to")
            && fs::read(test_file)? == before
            && smoke.cli(&["vault", "completion-events"])? == prior_events,
        "completion with the failed check's different revision was not rejected cleanly: {}",
        String::from_utf8_lossy(&wrong_revision.stderr)
    );
    smoke.cli(&[
        "vault",
        "complete",
        "--unit",
        TEST_UNIT,
        "--evidence",
        &format!("{id}:0"),
    ])?;
    let test_work_unit = fs::read_to_string(test_file)?;
    ensure!(
        test_work_unit.contains("status: completed")
            && test_work_unit.contains(id)
            && test_work_unit.contains(revision),
        "completion did not persist the Test WorkUnit's revision and Evidence"
    );
    let events = smoke.cli(&["vault", "completion-events"])?;
    let events: Vec<Value> = events
        .lines()
        .map(serde_json::from_str)
        .collect::<serde_json::Result<_>>()?;
    ensure!(
        events
            .iter()
            .filter(|event| event["work_unit_id"] == TEST_UNIT)
            .count()
            == 1
            && events.iter().any(|event| {
                event["work_unit_id"] == TEST_UNIT
                    && event["evidence"] == json!([{"run_id": id, "step_index": 0}])
            }),
        "expected one completion event containing the Test WorkUnit's Evidence"
    );
    assert_failed_check(smoke, &failed_check)?;
    Ok(failed_check)
}

fn exercise_packaged_worker_rejects_malformed_prepare(bin_dir: &Path) -> Result<()> {
    let worker = bin_dir.join("mcbtodo-worker");
    let mut child = Command::new(&worker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting packaged Worker at {}", worker.display()))?;
    child
        .stdin
        .as_mut()
        .context("packaged Worker stdin is unavailable")?
        .write_all(&[0, 0, 0, 1, b'x'])
        .context("sending malformed Prepare frame to packaged Worker")?;
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .context("waiting for packaged Worker to reject malformed Prepare")?;
    ensure!(
        !output.status.success() && output.stdout.is_empty(),
        "packaged Worker accepted malformed Prepare or emitted readiness"
    );
    ensure!(
        String::from_utf8_lossy(&output.stderr).contains("reading worker preparation"),
        "packaged Worker rejection did not identify the preparation stage: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn exercise_worker_failure(temp: &Path, smoke: &Smoke<'_>) -> Result<()> {
    let graph_dir = temp.join("worker-failure");
    fs::create_dir_all(&graph_dir)?;
    fs::write(graph_dir.join("fail.sh"), b"exit 23\n")?;
    let graph = graph_dir.join("workflow.yaml");
    fs::write(
        &graph,
        r#"schema: mcbtodo.workflow/v1
id: packaged-worker-failure
name: Packaged Worker failure
entry: fail
nodes:
  fail: {kind: script, runner: bash, source: fail.sh, timeout_seconds: 5, capabilities: [], outputs: {}}
"#,
    )?;

    let revision = smoke.cli(&["workflow", "publish", path_arg(&graph)?])?;
    let revision = revision.trim();
    smoke.cli(&["workflow", "authorize-script", revision, "fail"])?;
    let queued = smoke.json(&["workflow", "run", revision])?;
    let id = queued["id"]
        .as_str()
        .context("packaged Worker failure run has no ID")?;
    let failed = smoke.await_run(id, "failed")?;
    ensure!(
        failed["step_count"] == 1,
        "packaged Worker failure was not durably recorded: {failed}"
    );
    let trace = smoke.json(&["workflow", "trace", id, "0"])?;
    ensure!(
        trace["trace"]["node"] == "fail"
            && trace["trace"]["status"] == "failed"
            && trace["trace"]["reason"]
                .as_str()
                .is_some_and(|message| message.contains("23")),
        "packaged Worker failure trace did not preserve the process failure: {trace}"
    );
    Ok(())
}

fn exercise_packaged_supervisor_rejects_malformed_worker(
    temp: &Path,
    bin_dir: &Path,
    root: &Path,
) -> Result<()> {
    let fixture_dir = temp.join("malformed-worker-fixture");
    let home = fixture_dir.join("home");
    let fault_bin = fixture_dir.join("bin");
    fs::create_dir_all(&home)?;
    fs::create_dir(&fault_bin)?;
    for binary in ["mcbtodo", "mcbtodo-daemon"] {
        let source = bin_dir.join(binary);
        let target = fault_bin.join(binary);
        fs::copy(&source, &target).with_context(|| format!("copying {}", source.display()))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
    }

    let pid_file = home.join("worker.pid");
    let worker = fault_bin.join("mcbtodo-worker");
    fs::write(
        &worker,
        b"#!/bin/sh\nprintf '%s' \"$$\" > \"$HOME/worker.pid\"\nprintf '\\000\\000\\000\\001x'\nsleep 30\n",
    )?;
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))?;

    let smoke = Smoke {
        bin_dir: &fault_bin,
        home,
    };
    smoke.cli(&["init"])?;
    copy_fixture(&root.join("examples/vault"), &smoke.paths().vault_root())?;

    let graph_dir = fixture_dir.join("graph");
    fs::create_dir(&graph_dir)?;
    fs::write(graph_dir.join("noop.sh"), b"exit 0\n")?;
    let graph = graph_dir.join("workflow.yaml");
    fs::write(
        &graph,
        r#"schema: mcbtodo.workflow/v1
id: malformed-worker-supervision
name: Malformed Worker supervision
entry: run
nodes:
  run: {kind: script, runner: bash, source: noop.sh, timeout_seconds: 5, capabilities: [], outputs: {}}
"#,
    )?;
    let revision = smoke.cli(&["workflow", "publish", path_arg(&graph)?])?;
    let revision = revision.trim();
    let mut daemon = smoke.start_daemon()?;
    smoke.cli(&["workflow", "authorize-script", revision, "run"])?;
    let queued = smoke.json(&["workflow", "run", revision])?;
    let id = queued["id"]
        .as_str()
        .context("malformed Worker run has no ID")?;
    let failed = smoke.await_run(id, "failed")?;
    ensure!(
        failed["step_count"] == 0
            && failed["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("Worker failed readiness handshake")),
        "packaged Daemon accepted a malformed Worker readiness frame: {failed}"
    );
    ensure!(
        pid_file.exists(),
        "malformed Worker did not start its persistent process"
    );
    let pid: u32 = fs::read_to_string(&pid_file)?.parse()?;
    let proc_entry = Path::new("/proc").join(pid.to_string());
    ensure!(
        !proc_entry.exists(),
        "packaged Daemon did not terminate and reap malformed Worker {pid}"
    );
    daemon.stop()?;
    Ok(())
}

fn exercise_interrupted_worker(temp: &Path, smoke: &Smoke<'_>, daemon: &mut Daemon) -> Result<()> {
    let graph = temp.join("interrupted.yaml");
    fs::write(
        &graph,
        "\
schema: mcbtodo.workflow/v1
id: interrupted
name: Interrupted worker
entry: test
nodes:
  test: {kind: script, runner: bash, source: slow.sh, timeout_seconds: 30, capabilities: []}
",
    )?;
    fs::write(
        temp.join("slow.sh"),
        "(printf '{}' > \"$PROD_OUTPUT_JSON\"; kill -STOP \"$BASHPID\") &\nwait\n",
    )?;
    let revision = smoke.cli(&["workflow", "publish", path_arg(&graph)?])?;
    let revision = revision.trim();
    smoke.cli(&["workflow", "authorize-script", revision, "test"])?;
    let run = smoke.json(&["workflow", "run", revision])?;
    let id = run["id"].as_str().context("interrupted run has no ID")?;
    smoke.await_run(id, "running")?;
    let state = smoke.paths().state_dir();
    poll("script side effect in sandbox", || {
        let run = smoke.json(&["workflow", "inspect", id])?;
        ensure!(
            run["state"] == "running",
            "worker stopped before interruption: {run}"
        );
        for entry in fs::read_dir(&state)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("script-sandbox-")
                && fs::read(entry.path().join("output.json")).is_ok_and(|bytes| bytes == b"{}")
            {
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    let descendants = process_tree(daemon.0.id())?;
    ensure!(
        !descendants.is_empty(),
        "daemon had no observable Worker process tree at interruption"
    );
    daemon.stop()?;
    poll("orphaned Worker process tree to exit", || {
        Ok(descendants.iter().all(|pid| !process_is_running(*pid)))
    })?;
    *daemon = smoke.start_daemon()?;
    let recovered = smoke.await_run(id, "outcome_unknown")?;
    ensure!(
        recovered["step_count"] == 0,
        "interrupted script was recorded as complete"
    );
    let retry = smoke.json(&["workflow", "run", revision, "--run-id", id])?;
    ensure!(
        retry == recovered && smoke.await_run(id, "outcome_unknown")? == recovered,
        "resubmitting the interrupted run changed its recovery state: {retry}"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_tree(root: u32) -> Result<Vec<u32>> {
    let mut pending = vec![root];
    let mut descendants = Vec::new();
    while let Some(parent) = pending.pop() {
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
                continue;
            };
            let is_child = status.lines().any(|line| {
                line.strip_prefix("PPid:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
                    == Some(parent)
            });
            if !is_child {
                continue;
            }
            if !descendants.contains(&pid) {
                descendants.push(pid);
                pending.push(pid);
            }
        }
    }
    Ok(descendants)
}

#[cfg(target_os = "linux")]
fn process_is_running(pid: u32) -> bool {
    let stat = Path::new("/proc").join(pid.to_string()).join("stat");
    let Ok(stat) = fs::read_to_string(stat) else {
        return false;
    };
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state != "Z" && state != "X")
}

const FAILED_CHECK_SUMMARY: &str = "FAIL preserve page offset";

fn assert_failed_check(smoke: &Smoke<'_>, id: &str) -> Result<()> {
    let step = smoke.json(&["workflow", "trace", id, "0"])?;
    let trace = &step["trace"];
    let logs = smoke.json(&["workflow", "artifact", id, "0"])?;
    ensure!(
        trace["node"] == "test"
            && trace["outputs"] == json!({"passed": false, "summary": FAILED_CHECK_SUMMARY})
            && logs["stdout"]
                .as_str()
                .is_some_and(|stdout| stdout.lines().any(|line| line == FAILED_CHECK_SUMMARY))
            && logs["stderr"].is_string(),
        "failed check {id} lost its TestResult or log: {trace}, {logs}"
    );
    Ok(())
}

fn exercise_agent(
    fixture_root: &Path,
    draft: &Path,
    smoke: &Smoke<'_>,
    test_file: &str,
) -> Result<String> {
    const TITLE: &str = "Investigate the page-table mapping test";

    let graph = crate::example::build(fixture_root, draft, true)?;
    let watched = [
        fixture_root.join("examples/workflow/page_table.rs"),
        draft.join("scripts/check.nu"),
        graph.clone(),
        PathBuf::from(test_file),
    ];
    let before = watched
        .iter()
        .map(fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;
    let assert_unchanged = || -> Result<()> {
        for (path, bytes) in watched.iter().zip(&before) {
            ensure!(
                fs::read(path)? == *bytes,
                "Agent workflow changed {}",
                path.display()
            );
        }
        Ok(())
    };
    let revision = smoke.cli(&["workflow", "publish", path_arg(&graph)?])?;
    let revision = revision.trim();
    smoke.cli(&["workflow", "authorize-script", revision, "test"])?;

    let paths = smoke.paths();
    let listener = UnixListener::bind(paths.state_dir().join("agent.sock"))?;
    listener.set_nonblocking(true)?;
    let policy = paths.agent_policy_path();
    fs::write(&policy, "socket = \"state/agent.sock\"\n")?;
    let run = smoke.json(&["workflow", "run", revision])?;
    let id = run["id"].as_str().context("denied Agent run has no ID")?;
    smoke.await_run(id, "failed")?;
    ensure!(
        listener
            .accept()
            .is_err_and(|e| e.kind() == ErrorKind::WouldBlock),
        "an unauthorized run reached the Agent adapter"
    );
    assert_unchanged()?;

    fs::write(
        policy,
        format!(
            "socket = \"state/agent.sock\"\ntimeout_seconds = 5\n\
             [[allow]]\ncapability = \"analyze_test_failure\"\nrevision = \"{revision}\"\n"
        ),
    )?;

    let mut approved = None;
    for approve in [false, true] {
        let run = smoke.json(&["workflow", "run", revision])?;
        let id = run["id"].as_str().context("Agent run has no ID")?;
        mock_agent(&listener, FAILED_CHECK_SUMMARY, TITLE)?;
        let waiting = smoke.await_run(id, "waiting")?;
        ensure!(
            waiting["detail"].as_str().is_some_and(|review| {
                review.contains(TITLE) && review.contains(RUST_OS_PROJECT)
            }),
            "approval review did not include the actual project and title"
        );
        assert_failed_check(smoke, id)?;
        assert_unchanged()?;
        ensure!(
            !smoke.cli(&["outline"])?.contains(TITLE),
            "Agent changed the Vault before approval"
        );
        smoke.decide_workflow_in_tui(id, approve)?;
        smoke.await_run(id, "succeeded")?;
        assert_unchanged()?;
        ensure!(
            smoke.cli(&["outline"])?.contains(TITLE) == approve,
            "Vault contents do not match the workflow approval decision"
        );
        if approve {
            approved = Some(id.to_owned());
        }
    }
    approved.context("Agent follow-up did not reach approval")
}

fn mock_agent(listener: &UnixListener, summary: &str, title: &str) -> Result<()> {
    let mut stream = None;
    poll("workflow Agent connection", || match listener.accept() {
        Ok((client, _)) => {
            stream = Some(client);
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(error.into()),
    })?;
    let mut stream = stream.context("missing Agent connection")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!((1..=16_384).contains(&length), "invalid Agent request size");
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload)?;
    let request: Value = serde_json::from_slice(&payload)?;
    ensure!(
        request
            == json!({
                "schema": "mcbtodo.agent/v1",
                "capability": "analyze_test_failure",
                "inputs": {"summary": summary}
            }),
        "Agent received unexpected input: {request}"
    );
    let reply = serde_json::to_vec(&json!({
        "schema": "mcbtodo.agent/v1",
        "outputs": {"title": title}
    }))?;
    stream.write_all(&(reply.len() as u32).to_be_bytes())?;
    stream.write_all(&reply)?;
    Ok(())
}

struct Smoke<'a> {
    bin_dir: &'a Path,
    home: PathBuf,
}

impl Smoke<'_> {
    fn paths(&self) -> WorkspacePaths {
        WorkspacePaths::from_home(self.home.clone())
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(self.bin_dir.join(binary));
        command.env("HOME", &self.home);
        command
    }

    fn cli(&self, args: &[&str]) -> Result<String> {
        let Output {
            status,
            stdout,
            stderr,
        } = self
            .command("mcbtodo")
            .args(args)
            .output()
            .with_context(|| format!("starting mcbtodo {args:?}"))?;
        ensure!(
            status.success(),
            "mcbtodo {args:?}: {}",
            String::from_utf8_lossy(&stderr)
        );
        Ok(String::from_utf8(stdout)?)
    }

    fn json(&self, args: &[&str]) -> Result<Value> {
        let output = self.cli(args)?;
        serde_json::from_str(&output).with_context(|| format!("reading mcbtodo {args:?} JSON"))
    }

    fn mcp_call(&self, name: &str, arguments: Value) -> Result<Value> {
        let mut child = self
            .command("mcbtodo")
            .args(["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = child.stdin.take().context("MCP server has no stdin")?;
        serde_json::to_writer(
            &mut input,
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {}
                    },
                    "name": name, "arguments": arguments
                }
            }),
        )?;
        input.write_all(b"\n")?;
        drop(input);
        let output = child.wait_with_output()?;
        ensure!(
            output.status.success(),
            "MCP server failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut reply: Value = serde_json::from_slice(&output.stdout)?;
        ensure!(
            reply["id"] == 1 && reply["result"]["structuredContent"].is_object(),
            "MCP tool {name} failed: {reply}"
        );
        Ok(reply["result"]["structuredContent"].take())
    }

    fn approve_proposal(&self, id: &str, expect_success: bool) -> Result<String> {
        let (mut child, mut master) = self.terminal(&["proposal", "apply", id])?;
        let mut transcript = Vec::new();
        let mut sent = false;
        poll("interactive proposal approval", || {
            read_terminal(&mut master, &mut transcript)?;
            if !sent && transcript.ends_with(b"Type 'apply' to confirm: ") {
                master.write_all(b"apply\n")?;
                sent = true;
            }
            let Some(status) = child.0.try_wait()? else {
                return Ok(false);
            };
            read_terminal(&mut master, &mut transcript)?;
            ensure!(
                sent && status.success() == expect_success,
                "unexpected approval result {status}: {}",
                String::from_utf8_lossy(&transcript)
            );
            Ok(true)
        })?;
        Ok(String::from_utf8(transcript)?)
    }

    fn approve_proposal_in_tui(&self, id: &str) -> Result<()> {
        let proposals = self.json(&["proposal", "list"])?;
        let row = proposals
            .as_array()
            .and_then(|rows| rows.iter().position(|record| record["id"] == id))
            .context("approved proposal is absent from the queue")?;
        let select = format!("{}\r", "j".repeat(row));
        self.drive_tui(
            "UTC",
            &[
                ("\x1b[?1049h", " a"),
                ("Enter review", select.as_str()),
                ("Graph Diff", "a"),
                ("Type 'apply' to confirm: ", "apply\n"),
                ("Enter review", "\x1b"),
                ("NORMAL", "q"),
            ],
        )?;
        let record = self.json(&["proposal", "show", id])?;
        ensure!(record["status"] == "applied", "TUI did not apply {id}");
        Ok(())
    }

    fn approve_workflow_proposal_in_tui(&self, id: &str) -> Result<()> {
        let proposals = self.json(&["proposal", "list"])?;
        let row = proposals
            .as_array()
            .and_then(|rows| rows.iter().position(|record| record["id"] == id))
            .context("workflow proposal is absent from the queue")?;
        let select = format!("{}\r", "j".repeat(row));
        self.drive_tui(
            "UTC",
            &[
                ("\x1b[?1049h", " a"),
                ("Enter review", select.as_str()),
                // Ratatui may split adjacent words with cursor updates on a PTY.
                ("snapshot", "a"),
                ("Type 'apply' to confirm: ", "apply\n"),
                ("Enter review", "\x1b"),
                ("NORMAL", "q"),
            ],
        )?;
        let record = self.json(&["proposal", "show", id])?;
        ensure!(
            record["status"] == "applied",
            "TUI did not publish workflow proposal {id}"
        );
        Ok(())
    }

    fn decide_workflow_in_tui(&self, id: &str, approve: bool) -> Result<()> {
        let page = self.json(&["workflow", "runs"])?;
        let row = page["runs"]
            .as_array()
            .and_then(|rows| rows.iter().position(|record| record["id"] == id))
            .context("the workflow run is absent from the first page")?;
        let open_trace = format!("{}\r", "j".repeat(row));
        let decision = if approve { "a" } else { "d" };
        self.drive_tui(
            "Asia/Shanghai",
            &[
                ("NORMAL", " ud"),
                ("09:00–11:00  Implement page mapping", " ug"),
                ("page table [blocked]", " uf"),
                ("Selected", &open_trace),
                // Ratatui can split adjacent words across incremental cursor updates.
                ("\"FAIL", "\x1b"),
                ("Selected", decision),
                ("Confirm decision", "yq"),
            ],
        )
    }

    fn drive_tui(&self, timezone: &str, steps: &[(&str, &str)]) -> Result<()> {
        let (mut child, mut master) =
            self.terminal(&["--timezone", timezone, "tui", "--date", "2026-11-20"])?;
        for &(marker, keys) in steps {
            let mut transcript = Vec::new();
            poll(marker, || {
                read_terminal(&mut master, &mut transcript)?;
                let screen = String::from_utf8_lossy(&transcript);
                ensure!(child.0.try_wait()?.is_none(), "TUI exited early");
                Ok(screen.contains(marker))
            })
            .with_context(|| {
                format!(
                    "TUI screen waiting for {marker}: {}",
                    String::from_utf8_lossy(&transcript)
                )
            })?;
            master.write_all(keys.as_bytes())?;
        }
        poll("TUI exit", || {
            let Some(status) = child.0.try_wait()? else {
                return Ok(false);
            };
            ensure!(status.success(), "TUI exited with {status}");
            Ok(true)
        })
    }

    fn create_time_block_in_tui(&self) -> Result<()> {
        let (mut child, mut master) =
            self.terminal(&["--timezone", "Asia/Shanghai", "tui", "--date", "2026-11-20"])?;
        let mut transcript = Vec::new();
        for (marker, keys) in [
            ("\x1b[?1049h", " tb"),
            ("Find WorkUnit", "Implement page mapping\r09:00\r11:00\rq"),
        ] {
            poll(marker, || {
                read_terminal(&mut master, &mut transcript)?;
                ensure!(
                    child.0.try_wait()?.is_none(),
                    "TUI exited early: {}",
                    String::from_utf8_lossy(&transcript)
                );
                Ok(String::from_utf8_lossy(&transcript).contains(marker))
            })?;
            master.write_all(keys.as_bytes())?;
        }
        poll("TUI exit after creating a time block", || {
            read_terminal(&mut master, &mut transcript)?;
            let Some(status) = child.0.try_wait()? else {
                return Ok(false);
            };
            ensure!(status.success(), "TUI exited with {status}");
            Ok(true)
        })
    }

    fn terminal(&self, args: &[&str]) -> Result<(Daemon, fs::File)> {
        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let master = openpt(flags).context("opening a pseudoterminal for the smoke test")?;
        unlockpt(&master)?;
        let terminal = fs::File::from(ioctl_tiocgptpeer(&master, flags)?);
        tcsetwinsize(
            &terminal,
            Winsize {
                ws_col: 100,
                ws_row: 30,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        fcntl_setfl(&master, OFlags::NONBLOCK)?;
        let child = self
            .command("mcbtodo")
            .args(args)
            .env("TERM", "xterm-256color")
            .stdin(terminal.try_clone()?)
            .stdout(terminal.try_clone()?)
            .stderr(terminal)
            .spawn()?;
        Ok((Daemon(child), fs::File::from(master)))
    }

    fn start_daemon(&self) -> Result<Daemon> {
        let mut daemon = Daemon(
            self.command("mcbtodo-daemon")
                .spawn()
                .context("starting mcbtodo-daemon")?,
        );
        poll("daemon socket", || {
            if let Some(status) = daemon.0.try_wait()? {
                bail!("mcbtodo-daemon quit with {status}");
            }
            Ok(self.cli(&["timer", "status"]).is_ok())
        })?;
        Ok(daemon)
    }

    fn await_run(&self, id: &str, state: &str) -> Result<Value> {
        let mut result = Value::Null;
        poll(&format!("run {id} to become {state}"), || {
            let run = self.json(&["workflow", "inspect", id])?;
            if run["state"] == state {
                result = run;
                return Ok(true);
            }
            if matches!(
                run["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "outcome_unknown")
            ) {
                bail!("unexpected terminal state for {id}: {run}");
            }
            Ok(false)
        })?;
        Ok(result)
    }
}

struct Daemon(Child);

impl Daemon {
    fn stop(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_none() {
            self.0.kill()?;
            self.0.wait()?;
        }
        Ok(())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn poll(label: &str, mut predicate: impl FnMut() -> Result<bool>) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if predicate()? {
            return Ok(());
        }
        ensure!(Instant::now() < end, "timed out waiting for {label}");
        thread::sleep(Duration::from_millis(25));
    }
}

fn read_terminal(master: &mut fs::File, transcript: &mut Vec<u8>) -> Result<()> {
    const MAX_TRANSCRIPT: u64 = 128 * 1024;
    let remaining = MAX_TRANSCRIPT.saturating_sub(transcript.len() as u64);
    match master.take(remaining).read_to_end(transcript) {
        Ok(_) => (),
        Err(error)
            if error.kind() == ErrorKind::WouldBlock
                || error.raw_os_error() == Some(Errno::IO.raw_os_error()) => {}
        Err(error) => return Err(error.into()),
    }
    ensure!(
        (transcript.len() as u64) < MAX_TRANSCRIPT,
        "terminal transcript exceeded the smoke test limit"
    );
    Ok(())
}

fn path_arg(path: &Path) -> Result<&str> {
    path.to_str().context("expected a UTF-8 smoke test path")
}

fn copy_fixture(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir(&target)?;
            copy_fixture(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
