use super::*;
use crate::proposal;
use mcbtodo_vault::Vault;
use std::os::unix::fs::PermissionsExt;

const PROJECT: &str = "10000000-0000-4000-8000-000000000001";
const TASK: &str = "20000000-0000-4000-8000-000000000001";
const BLOCKED_UNIT: &str = "30000000-0000-4000-8000-000000000003";

fn fixture() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/vault"))
}

fn exchange(messages: &[Value]) -> Vec<Value> {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    exchange_in(fixture(), &paths, messages)
}

fn exchange_in(root: &Path, paths: &WorkspacePaths, messages: &[Value]) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut input, message).unwrap();
        input.push(b'\n');
    }
    let mut output = Vec::new();
    serve_io(
        root,
        chrono_tz::Asia::Shanghai,
        paths,
        &input[..],
        &mut output,
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn modern(id: Value, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({PROTOCOL_VERSION: VERSION, CLIENT_CAPABILITIES: {}});
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn call(id: i32, name: &str, arguments: Value) -> Value {
    modern(
        json!(id),
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

#[test]
fn modern_stdio_reads_fixture_projections_without_a_handshake() {
    let responses = exchange(&[
        call(1, "list_projects", json!({})),
        call(2, "list_tasks", json!({"project_id": PROJECT})),
        call(3, "list_work_units", json!({"task_id": TASK})),
        call(4, "calendar_day", json!({"date": "2026-11-25"})),
        modern(json!("discovery"), "server/discover", json!({})),
        modern(json!(6), "tools/list", json!({})),
    ]);
    assert_eq!(responses.len(), 6);
    let result = |i: usize| &responses[i]["result"];
    let data = |i| &result(i)["structuredContent"];
    let projects = data(0)["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["id"], PROJECT);
    assert_eq!(projects[0]["title"], "Rust OS");
    assert_eq!(projects[0]["revision"].as_str().unwrap().len(), 64);
    let task = data(1)["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == TASK)
        .unwrap();
    assert_eq!(task["project_id"], PROJECT);
    assert_eq!(task["revision"].as_str().unwrap().len(), 64);
    let units = data(2)["work_units"].as_array().unwrap();
    assert_eq!(units.len(), 3);
    assert!(units.iter().all(|unit| unit["task_id"] == TASK));
    assert!(
        units
            .iter()
            .any(|unit| unit["id"] == BLOCKED_UNIT && unit["blocked"] == true)
    );
    assert_eq!(data(3)["timezone"], "Asia/Shanghai");
    let expected: Vec<Value> = Vault::load(fixture(), chrono_tz::Asia::Shanghai)
        .unwrap()
        .calendar()
        .into_iter()
        .filter(|entry| {
            entry.on_day(
                chrono::NaiveDate::from_ymd_opt(2026, 11, 25).unwrap(),
                chrono_tz::Asia::Shanghai,
            )
        })
        .map(|entry| serde_json::to_value(entry).unwrap())
        .collect();
    assert_eq!(data(3)["entries"], json!(expected));
    let text: Value =
        serde_json::from_str(result(3)["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, *data(3));
    assert_eq!(responses[4]["id"], "discovery");
    assert_eq!(result(4)["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(result(4)["resultType"], "complete");
    assert_eq!(result(4)["_meta"][SERVER_INFO]["name"], "mcbtodo");
    let tools = result(5)["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15);
    for tool in tools {
        let posts_proposal = tool["name"].as_str().unwrap().starts_with("propose_");
        assert_eq!(tool["annotations"]["readOnlyHint"], !posts_proposal);
    }
    let schema =
        |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"].clone();
    assert_eq!(
        schema("propose_set_deadline")["required"],
        json!(["target_id", "expected_revision", "date"])
    );
    assert_eq!(
        schema("propose_set_deadline")["properties"]["date"]["format"],
        "date"
    );
    assert_eq!(
        schema("propose_clear_deadline")["required"],
        json!(["target_id", "expected_revision"])
    );
    assert_eq!(
        schema("propose_clear_deadline")["additionalProperties"],
        false
    );
    assert_eq!(
        schema("propose_create_reminder")["required"],
        json!(["target_id", "expected_revision", "schedule", "title"])
    );
}

#[test]
fn list_inbox_returns_revision_and_metadata_without_mutating_markdown() {
    let vault = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let inbox = vault.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let id = "60000000-0000-4000-8000-000000000001";
    let path = inbox.join(format!("{id}.md"));
    let source = format!(
        "---\nschema: mcbtodo.inbox-entry/v1\nid: {id}\ncaptured_at: '2026-11-20T12:00:00Z'\ntags: [follow-up]\n---\n\n# Review recovery\n\nCheck the journal.\n"
    );
    std::fs::write(&path, &source).unwrap();

    let responses = exchange_in(
        vault.path(),
        &paths,
        &[
            call(1, "list_inbox", json!({})),
            modern(json!(2), "tools/list", json!({})),
        ],
    );
    let entries = responses[0]["result"]["structuredContent"]["inbox"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], id);
    assert_eq!(entries[0]["title"], "Review recovery");
    assert_eq!(entries[0]["tags"], json!(["follow-up"]));
    assert_eq!(entries[0]["revision"].as_str().unwrap().len(), 64);
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15);
    let inbox_tool = tools
        .iter()
        .find(|tool| tool["name"] == "list_inbox")
        .unwrap();
    assert_eq!(inbox_tool["annotations"]["readOnlyHint"], true);
    assert_eq!(inbox_tool["inputSchema"]["additionalProperties"], false);
}

#[test]
fn workflow_draft_tool_captures_exact_base64_sources_idempotently_for_review() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let graph = r#"schema: mcbtodo.workflow/v1
id: mcp-reviewed-workflow
name: MCP reviewed workflow
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
"#;
    let source = b"print 'captured by MCP'\n";
    let request_id = "70000000-0000-4000-8000-000000000001";
    let arguments = json!({
        "graph": graph,
        "sources": {
            "scripts/check.nu": base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                source
            )
        },
        "request_id": request_id
    });
    let responses = exchange_in(
        fixture(),
        &paths,
        &[
            call(1, "propose_workflow_draft", arguments.clone()),
            call(2, "propose_workflow_draft", arguments),
            modern(json!(3), "tools/list", json!({})),
        ],
    );
    let first = &responses[0]["result"]["structuredContent"];
    let second = &responses[1]["result"]["structuredContent"];
    assert_eq!(first["status"], "queued");
    assert_eq!(first["execution_authorized"], false);
    assert_eq!(first["proposal_id"], second["proposal_id"]);
    assert_eq!(first["workflow_revision"].as_str().unwrap().len(), 64);

    let queue = proposal::Queue::open(fixture(), chrono_tz::Asia::Shanghai, &paths).unwrap();
    let id = first["proposal_id"].as_str().unwrap().parse().unwrap();
    let proposal = queue.review(id).unwrap();
    assert_eq!(proposal.status, proposal::Status::Pending);
    assert!(
        mcbtodo_workflow::RevisionStore::new(paths.state_dir())
            .list()
            .unwrap()
            .is_empty()
    );
    let proposal::Operation::PublishWorkflow { draft } = proposal.operation else {
        panic!("workflow tool must queue a workflow snapshot");
    };
    assert_eq!(draft.graph(), graph);
    assert_eq!(draft.script_sources()["scripts/check.nu"], source);

    let tools = responses[2]["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "propose_workflow_draft")
        .unwrap();
    assert_eq!(tool["inputSchema"]["required"], json!(["graph", "sources"]));
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
}

#[test]
fn inbox_triage_proposal_is_revision_bound_idempotent_and_review_only() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    for relative in [
        "projects/rust-os/project.md",
        "projects/rust-os/tasks/memory.md",
        "projects/rust-os/units/read-spec.md",
        "projects/rust-os/units/mapping.md",
        "projects/rust-os/units/translation.md",
    ] {
        let destination = root.join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(fixture().join(relative), destination).unwrap();
    }
    let inbox_id = "60000000-0000-4000-8000-000000000011";
    let inbox_path = root.join("inbox").join(format!("{inbox_id}.md"));
    std::fs::create_dir_all(inbox_path.parent().unwrap()).unwrap();
    let source = format!(
        "---\nschema: mcbtodo.inbox-entry/v1\nid: {inbox_id}\ncaptured_at: '2026-11-20T12:00:00Z'\ntags: [follow-up]\n---\n\n# Review recovery\n\nKeep this body.\n"
    );
    std::fs::write(&inbox_path, &source).unwrap();
    let vault = Vault::load(&root, chrono_tz::Asia::Shanghai).unwrap();
    let revision = vault.revision(inbox_id.parse().unwrap()).unwrap().digest();
    drop(vault);

    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let request_id = "60000000-0000-4000-8000-000000000012";
    let arguments = json!({
        "inbox_id": inbox_id,
        "project_id": PROJECT,
        "expected_revision": revision,
        "request_id": request_id
    });
    let responses = exchange_in(
        &root,
        &paths,
        &[
            call(1, "propose_triage_inbox", arguments.clone()),
            call(2, "propose_triage_inbox", arguments),
            modern(json!(3), "tools/list", json!({})),
        ],
    );
    let queued = &responses[0]["result"]["structuredContent"];
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued, &responses[1]["result"]["structuredContent"]);
    assert_eq!(std::fs::read_to_string(&inbox_path).unwrap(), source);
    assert_eq!(std::fs::read_dir(paths.proposals_dir()).unwrap().count(), 2);
    let tools = responses[2]["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "propose_triage_inbox")
        .unwrap();
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["inbox_id", "project_id", "expected_revision"])
    );
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
}

#[test]
fn old_handshake_does_not_enable_tool_calls_without_request_metadata() {
    let mut unsupported = modern(json!(4), "tools/list", json!({}));
    unsupported["params"]["_meta"][PROTOCOL_VERSION] = json!("2025-11-25");
    let responses = exchange(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}
        }}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
        unsupported,
    ]);
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["error"]["code"], -32602);
    assert_eq!(responses[1]["error"]["code"], -32602);
    assert_eq!(responses[2]["error"]["code"], -32022);
    assert_eq!(
        responses[2]["error"]["data"]["supported"],
        json!(["2026-07-28"])
    );
}

#[test]
fn rejects_invalid_arguments_and_unsupported_requests() {
    let mut unsupported = modern(json!(4), "server/discover", json!({}));
    unsupported["params"]["_meta"][PROTOCOL_VERSION] = json!("1900-01-01");
    let responses = exchange(&[
        call(1, "calendar_day", json!({"date": "2026-02-30"})),
        call(2, "list_projects", json!({"path": "/tmp"})),
        call(3, "approve", json!({})),
        unsupported,
        json!({"jsonrpc": "2.0", "id": 5, "method": "server/discover"}),
        json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "list_projects"}}),
        call(6, "list_tasks", json!({"project_id": null})),
        call(7, "calendar_day", json!({"date": "0000-01-01"})),
    ]);
    assert_eq!(responses.len(), 7);
    for index in [0, 1, 5, 6] {
        assert_eq!(responses[index]["result"]["isError"], true);
    }
    assert_eq!(responses[2]["error"]["code"], -32602);
    assert_eq!(responses[3]["error"]["code"], -32022);
    assert_eq!(responses[4]["error"]["code"], -32602);
}

#[test]
fn stdio_rejects_batched_and_oversized_messages() {
    let mut output = Vec::new();
    let oversized = vec![b'x'; MAX_LINE_BYTES + 1];
    let input = b"not JSON\n[]\n"
        .iter()
        .chain(&oversized)
        .copied()
        .collect::<Vec<_>>();
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let error = serve_io(
        fixture(),
        chrono_tz::Asia::Shanghai,
        &paths,
        &input[..],
        &mut output,
    )
    .unwrap_err();
    assert!(error.to_string().contains("stdio limit"));
    let output = String::from_utf8(output).unwrap();
    let responses = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["error"]["code"], -32600);
}

#[test]
fn proposals_are_idempotent_and_do_not_change_the_vault() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let args = json!({
        "project_id": PROJECT,
        "title": "Review the next change",
        "body": "Review the tests.",
        "request_id": "40000000-0000-4000-8000-000000000001"
    });
    let mut changed_args = args.clone();
    changed_args["title"] = json!("Different title");
    let unit_path = fixture().join("projects/rust-os/units/translation.md");
    let before = std::fs::read(&unit_path).unwrap();
    let responses = exchange_in(
        fixture(),
        &paths,
        &[
            call(1, "propose_capture_task", args.clone()),
            call(2, "propose_capture_task", args),
            call(3, "propose_capture_task", changed_args),
            call(
                4,
                "propose_complete_work_unit",
                json!({"work_unit_id": BLOCKED_UNIT}),
            ),
            call(5, "apply_proposal", json!({})),
        ],
    );
    let first = &responses[0]["result"]["structuredContent"];
    assert_eq!(first["status"], "queued");
    assert_eq!(first, &responses[1]["result"]["structuredContent"]);
    assert_eq!(responses[2]["result"]["isError"], true);
    assert_eq!(
        responses[3]["result"]["structuredContent"]["status"],
        "queued"
    );
    assert_eq!(responses[4]["error"]["code"], -32602);
    let vault = Vault::load(fixture(), chrono_tz::Asia::Shanghai).unwrap();
    assert!(
        !vault
            .outline()
            .iter()
            .any(|row| row.title == "Review the next change")
    );
    assert_eq!(std::fs::read(unit_path).unwrap(), before);
}

#[test]
fn graph_diff_previews_parallel_units_and_rejects_cycles_without_writes() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let first = "50000000-0000-4000-8000-000000000001";
    let second = "50000000-0000-4000-8000-000000000002";
    let join = "50000000-0000-4000-8000-000000000003";
    let args = json!({
        "task_id": TASK,
        "units": [
            {"id": join, "title": "Integration", "depends_on": [first, second]},
            {"id": first, "title": "Branch one", "estimated_minutes": 30},
            {"id": second, "title": "Branch two"}
        ]
    });
    let mut cycle = args.clone();
    cycle["units"][1]["depends_on"] = json!([join]);
    let mut missing = args.clone();
    missing["units"][2]["depends_on"] = json!([PROJECT]);
    let mut duplicate = args.clone();
    duplicate["units"][2]["id"] = json!("40000000-0000-4000-8000-000000000002");
    let tool = "preview_work_unit_graph_diff";
    let responses = exchange_in(
        fixture(),
        &paths,
        &[
            call(1, tool, args),
            call(2, tool, cycle),
            call(3, tool, missing),
            call(4, tool, duplicate),
        ],
    );
    let graph = &responses[0]["result"]["structuredContent"];
    assert_eq!(graph["project_id"], PROJECT);
    assert_eq!(graph["task_id"], TASK);
    assert_eq!(graph["ready"], json!([first, second]));
    assert_eq!(
        graph["added_edges"],
        json!([
            {"prerequisite": first, "dependent": join},
            {"prerequisite": second, "dependent": join}
        ])
    );
    assert_eq!(graph["added_nodes"][0]["id"], first);
    assert_eq!(graph["added_nodes"][0]["estimated_minutes"], 30);
    assert!(
        responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cycle")
    );
    assert!(
        responses[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing work unit")
    );
    assert!(
        responses[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );
    assert!(!paths.state_dir().exists());
    let vault = Vault::load(fixture(), chrono_tz::Asia::Shanghai).unwrap();
    assert_eq!(vault.task_graph().order().len(), 3);
}

#[test]
fn graph_diff_submission_replays_the_review_and_rejects_reused_request_ids() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let unit = "50000000-0000-4000-8000-000000000001";
    let request_id = "60000000-0000-4000-8000-000000000001";
    let draft = json!({"task_id": TASK, "units": [{"id": unit, "title": "New branch"}]});
    let mut submission = draft.clone();
    submission["request_id"] = json!(request_id);
    let mut changed = submission.clone();
    changed["units"][0]["title"] = json!("Changed branch");
    let mut invalid = draft.clone();
    invalid["units"][0]["depends_on"] = json!([unit]);
    let tool = "propose_work_unit_graph_diff";
    let responses = exchange_in(
        fixture(),
        &paths,
        &[
            call(1, "preview_work_unit_graph_diff", draft),
            call(2, tool, submission.clone()),
            call(3, tool, submission),
            call(4, tool, changed),
            call(5, tool, invalid),
        ],
    );
    let data = |index: usize| &responses[index]["result"]["structuredContent"];
    assert_eq!(
        data(1),
        &json!({"proposal_id": request_id, "status": "queued", "graph_diff": data(0)})
    );
    assert_eq!(data(1), data(2));
    assert_eq!(responses[3]["result"]["isError"], true);
    assert_eq!(responses[4]["result"]["isError"], true);
    assert_eq!(std::fs::read_dir(paths.proposals_dir()).unwrap().count(), 2);
    assert!(
        Vault::load(fixture(), chrono_tz::Asia::Shanghai)
            .unwrap()
            .source(unit.parse().unwrap())
            .is_none()
    );
}

#[test]
fn scheduling_proposal_requires_a_current_revision_and_matching_local_offsets() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let outline = exchange_in(
        fixture(),
        &paths,
        &[call(1, "list_work_units", json!({"task_id": TASK}))],
    );
    let unit = outline[0]["result"]["structuredContent"]["work_units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|unit| unit["id"] == BLOCKED_UNIT)
        .unwrap();
    let revision = unit["revision"].as_str().unwrap();
    let request_id = "40000000-0000-4000-8000-000000000005";
    let draft = json!({
        "work_unit_id": BLOCKED_UNIT,
        "expected_revision": revision,
        "start": "2026-11-21T09:00:00+08:00",
        "end": "2026-11-21T11:00:00+08:00",
        "availability": "free",
        "request_id": request_id
    });
    let mut stale = draft.clone();
    stale["expected_revision"] = json!("0".repeat(64));
    let mut wrong_offset = draft.clone();
    wrong_offset["start"] = json!("2026-11-21T01:00:00Z");
    let mut backwards = draft.clone();
    backwards["end"] = json!("2026-11-21T08:00:00+08:00");
    let tool = "propose_create_time_block";
    let document = fixture().join("projects/rust-os/units/translation.md");
    let before = std::fs::read(&document).unwrap();
    let responses = exchange_in(
        fixture(),
        &paths,
        &[
            call(2, tool, stale),
            call(3, tool, wrong_offset),
            call(4, tool, backwards),
            call(5, tool, draft.clone()),
            call(6, tool, draft),
        ],
    );
    for response in &responses[..3] {
        assert_eq!(response["result"]["isError"], true);
    }
    assert_eq!(
        responses[3]["result"]["structuredContent"],
        json!({"proposal_id": request_id, "status": "queued"})
    );
    assert_eq!(responses[3]["result"], responses[4]["result"]);
    assert_eq!(std::fs::read_dir(paths.proposals_dir()).unwrap().count(), 2);
    assert_eq!(std::fs::read(&document).unwrap(), before);
}

#[test]
fn reminder_proposals_validate_schedule_target_revision_and_are_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    std::fs::create_dir_all(paths.data_dir()).unwrap();
    std::fs::set_permissions(paths.data_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(fixture(), paths.vault_root()).unwrap();
    let listing = exchange_in(
        fixture(),
        &paths,
        &[call(1, "list_work_units", json!({"task_id": TASK}))],
    );
    let unit = listing[0]["result"]["structuredContent"]["work_units"][0].clone();
    let request_id = "40000000-0000-4000-8000-000000000006";
    let draft = json!({
        "target_id": unit["id"],
        "expected_revision": unit["revision"],
        "schedule": {
            "kind": "once",
            "at": {"value": "2026-11-21T09:00:00+08:00", "timezone": "Asia/Shanghai"}
        },
        "title": "Review implementation",
        "body": "Check the test results.",
        "request_id": request_id
    });
    let mut stale = draft.clone();
    stale["expected_revision"] = json!("0".repeat(64));
    let mut malformed = draft.clone();
    malformed["schedule"]["kind"] = json!("hourly");
    let responses = exchange_in(
        fixture(),
        &paths,
        &[
            call(2, "propose_create_reminder", stale),
            call(3, "propose_create_reminder", malformed),
            call(4, "propose_create_reminder", draft.clone()),
            call(5, "propose_create_reminder", draft),
        ],
    );
    assert_eq!(responses[0]["result"]["isError"], true);
    assert_eq!(responses[1]["result"]["isError"], true);
    assert_eq!(
        responses[2]["result"]["structuredContent"],
        json!({"proposal_id": request_id, "status": "queued"})
    );
    assert_eq!(responses[2]["result"], responses[3]["result"]);
    assert_eq!(std::fs::read_dir(paths.proposals_dir()).unwrap().count(), 2);
    assert!(
        Vault::load(fixture(), chrono_tz::Asia::Shanghai)
            .unwrap()
            .reminders()
            .next()
            .is_none()
    );
}

#[test]
fn reminder_proposals_reject_external_vaults() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let listing = exchange_in(
        fixture(),
        &paths,
        &[call(1, "list_work_units", json!({"task_id": TASK}))],
    );
    let unit = listing[0]["result"]["structuredContent"]["work_units"][0].clone();
    let response = exchange_in(
        fixture(),
        &paths,
        &[call(
            2,
            "propose_create_reminder",
            json!({
                "target_id": unit["id"],
                "expected_revision": unit["revision"],
                "schedule": {
                    "kind": "once",
                    "at": {"value": "2026-11-21T09:00:00+08:00", "timezone": "Asia/Shanghai"}
                },
                "title": "Review implementation",
                "body": "",
                "request_id": "40000000-0000-4000-8000-000000000008"
            }),
        )],
    );

    assert_eq!(response[0]["result"]["isError"], true);
}

#[test]
fn deadline_proposals_are_idempotent_revision_checked_and_leave_vault_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let projection = exchange_in(
        fixture(),
        &paths,
        &[
            call(1, "list_projects", json!({})),
            call(2, "list_tasks", json!({"project_id": PROJECT})),
            call(3, "list_work_units", json!({"task_id": TASK})),
        ],
    );
    let project_revision = projection[0]["result"]["structuredContent"]["projects"][0]["revision"]
        .as_str()
        .unwrap();
    let task_revision = projection[1]["result"]["structuredContent"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == TASK)
        .unwrap()["revision"]
        .as_str()
        .unwrap();
    let unit_revision = projection[2]["result"]["structuredContent"]["work_units"][0]["revision"]
        .as_str()
        .unwrap();
    for revision in [project_revision, task_revision, unit_revision] {
        assert_eq!(revision.len(), 64);
        assert!(
            revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }

    let project_file = fixture().join("projects/rust-os/project.md");
    let task_file = fixture().join("projects/rust-os/tasks/memory.md");
    let before_project = std::fs::read(&project_file).unwrap();
    let before_task = std::fs::read(&task_file).unwrap();
    let before_deadline = Vault::load(fixture(), chrono_tz::Asia::Shanghai)
        .unwrap()
        .deadline(PROJECT.parse().unwrap());
    let set_args = json!({
        "target_id": PROJECT,
        "expected_revision": project_revision,
        "date": "2026-12-10",
        "request_id": "40000000-0000-4000-8000-000000000011"
    });
    let mut stale = set_args.clone();
    stale["expected_revision"] = json!("0".repeat(64));
    let mut malformed = set_args.clone();
    malformed["date"] = json!("2026-2-30");
    let clear_args = json!({
        "target_id": TASK,
        "expected_revision": task_revision,
        "request_id": "40000000-0000-4000-8000-000000000012"
    });
    let mut clear_stale = clear_args.clone();
    clear_stale["expected_revision"] = json!("f".repeat(64));
    let responses = exchange_in(
        fixture(),
        &paths,
        &[
            call(4, "propose_set_deadline", stale),
            call(5, "propose_set_deadline", malformed),
            call(6, "propose_set_deadline", set_args.clone()),
            call(7, "propose_set_deadline", set_args),
            call(8, "propose_clear_deadline", clear_stale),
            call(9, "propose_clear_deadline", clear_args.clone()),
            call(10, "propose_clear_deadline", clear_args),
        ],
    );
    assert_eq!(responses[0]["result"]["isError"], true);
    assert_eq!(responses[1]["result"]["isError"], true);
    assert_eq!(
        responses[2]["result"]["structuredContent"]["status"],
        "queued"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"],
        responses[3]["result"]["structuredContent"]
    );
    assert_eq!(responses[4]["result"]["isError"], true);
    assert_eq!(
        responses[5]["result"]["structuredContent"]["status"],
        "queued"
    );
    assert_eq!(
        responses[5]["result"]["structuredContent"],
        responses[6]["result"]["structuredContent"]
    );
    let proposal_count = std::fs::read_dir(paths.proposals_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .count();
    assert_eq!(proposal_count, 2);
    assert_eq!(std::fs::read(project_file).unwrap(), before_project);
    assert_eq!(std::fs::read(task_file).unwrap(), before_task);
    assert_eq!(
        Vault::load(fixture(), chrono_tz::Asia::Shanghai)
            .unwrap()
            .deadline(PROJECT.parse().unwrap()),
        before_deadline
    );
}
