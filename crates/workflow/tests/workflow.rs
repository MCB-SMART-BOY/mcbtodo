use std::collections::BTreeMap;

use mcbtodo_workflow::{
    JobKind, NodeOutcome, NodeStatus, Progress, Scalar, TraversalError, WorkflowStatus, advance,
    compile, parse_definition, parse_fixture, simulate,
};
use serde_json::json;

const GRAPH: &str = include_str!("../../../examples/workflow/graph.yaml");
const SOURCE: &[u8] = include_bytes!("../../../examples/workflow/scripts/check.nu");
const PASSED: &str = include_str!("../../../examples/workflow/fixtures/passed.json");
const FAILED: &str = include_str!("../../../examples/workflow/fixtures/failed.json");
const SCRIPT_ERROR: &str = include_str!("../../../examples/workflow/fixtures/script-error.json");

fn sources() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("scripts/check.nu".into(), SOURCE.into()),
        (
            "page-table-check".into(),
            b"simulated executable input".to_vec(),
        ),
    ])
}

#[test]
fn agent_paths_to_vault_writes_require_a_subsequent_approved_edge() {
    for edge in [
        "  - {from: triage, to: followup}\n",
        "  - {from: rejected, to: followup}\n",
    ] {
        let graph = GRAPH.replace("bindings:\n", &format!("{edge}bindings:\n"));
        let definition = parse_definition(&graph).unwrap();
        let error = compile(&definition, &sources()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires approval after an agent"),
            "{error}"
        );
    }
}

#[test]
fn approval_must_directly_guard_an_agent_descended_write() {
    let graph = GRAPH
        .replace("  followup:\n", "  middle: {kind: approval}\n  followup:\n")
        .replace(
            "  - {from: approve, to: followup, when: approved}",
            "  - {from: approve, to: middle, when: approved}\n  - {from: middle, to: followup, when: rejected}\n  - {from: middle, to: report, when: approved}",
        );
    let error = compile(&parse_definition(&graph).unwrap(), &sources()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("through direct `approved` edges only"),
        "{error}"
    );
}

const REVIEW_GRAPH: &str = r#"
schema: mcbtodo.workflow/v1
id: review-target
name: Review exact targets
entry: agent
capabilities:
  - {kind: agent.invoke, scope: propose}
nodes:
  agent: {kind: agent, capability: propose, outputs: {title: string, project: string}}
  review: {kind: approval}
  create: {kind: action, action: task.create}
  decline: {kind: action, action: report.create}
edges:
  - {from: agent, to: review}
  - {from: review, to: create, when: approved}
  - {from: review, to: decline, when: rejected}
bindings:
  - {from: {node: agent, port: title}, to: {node: create, port: title}}
  - {from: {node: agent, port: project}, to: {node: create, port: project_id}}
literals:
  - {to: {node: decline, port: summary}, value: declined}
"#;
const REVIEW_PROJECT: &str = "10000000-0000-4000-8000-000000000001";

#[test]
fn approval_shows_the_exact_bound_task_and_escapes_untrusted_text() {
    let plan = compile(&parse_definition(REVIEW_GRAPH).unwrap(), &BTreeMap::new()).unwrap();
    let Progress::Next { job, .. } = advance(&plan, &[]).unwrap() else {
        panic!("expected agent job");
    };
    let title = "  Fix \"demo\" \\ \u{202e}check ";
    let mut history = vec![
        job.complete(NodeOutcome::Succeeded {
            outputs: BTreeMap::from([
                ("title".into(), Scalar::String(title.into())),
                ("project".into(), Scalar::String(REVIEW_PROJECT.into())),
            ]),
        })
        .unwrap(),
    ];
    let Progress::Next {
        job: approval,
        steps,
    } = advance(&plan, &history).unwrap()
    else {
        panic!("expected approval job");
    };
    assert!(steps.is_empty());
    assert_eq!(
        approval.kind,
        JobKind::Approval {
            prompt: format!(
                "Approve task creation?\n`create` in project {REVIEW_PROJECT} with title \"Fix \\\"demo\\\" \\\\ \\u{{202e}}check\""
            ),
        },
    );
    let approved = approval.complete(NodeOutcome::Approved).unwrap();
    let mut incomplete_step = serde_json::to_value(&approved).unwrap();
    incomplete_step
        .as_object_mut()
        .unwrap()
        .remove("approval_review");
    history.push(serde_json::from_value(incomplete_step).unwrap());
    assert!(
        advance(&plan, &history)
            .unwrap_err()
            .to_string()
            .contains("does not record the required task review")
    );
    let incomplete_step = std::mem::replace(history.last_mut().unwrap(), approved);
    let Progress::Next { job: create, .. } = advance(&plan, &history).unwrap() else {
        panic!("expected task creation job");
    };
    assert_eq!(create.inputs["title"], Scalar::String(title.into()));
    assert_eq!(
        create.inputs["project_id"],
        Scalar::String(REVIEW_PROJECT.into())
    );
    history.push(
        create
            .complete(NodeOutcome::Succeeded {
                outputs: BTreeMap::new(),
            })
            .unwrap(),
    );
    assert!(matches!(
        advance(&plan, &history).unwrap(),
        Progress::Complete(_)
    ));
    history[1] = incomplete_step;
    assert!(matches!(
        advance(&plan, &history),
        Err(TraversalError::InvalidHistory(_))
    ));
}

#[test]
fn a_parallel_branch_cannot_supply_task_data_without_preceding_each_approval() {
    let graph = REVIEW_GRAPH
        .replace(
            "  review: {kind: approval}",
            "  review: {kind: approval}\n  review_other: {kind: approval}\n  a_producer: {kind: agent, capability: propose, outputs: {title: string}}",
        )
        .replace(
            "  - {from: agent, to: review}",
            "  - {from: agent, to: review}\n  - {from: agent, to: a_producer}\n  - {from: a_producer, to: review_other}\n  - {from: review_other, to: create, when: approved}\n  - {from: review_other, to: decline, when: rejected}",
        )
        .replace("node: agent, port: title", "node: a_producer, port: title");
    let error = compile(&parse_definition(&graph).unwrap(), &BTreeMap::new()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("output `a_producer.title` is not guaranteed"),
        "{error}"
    );
}

#[test]
fn invalid_task_targets_fail_before_approval() {
    let plan = compile(&parse_definition(REVIEW_GRAPH).unwrap(), &BTreeMap::new()).unwrap();
    let Progress::Next { job, .. } = advance(&plan, &[]).unwrap() else {
        panic!("expected agent job");
    };
    for (title, project, diagnostic) in [
        (" \u{2003}".into(), REVIEW_PROJECT, "non-empty title"),
        ("new\nline".into(), REVIEW_PROJECT, "no control characters"),
        ("字".repeat(86), REVIEW_PROJECT, "at most 256 bytes"),
        ("\\".repeat(129), REVIEW_PROJECT, "escaped title limit"),
        ("valid title".into(), "not-a-uuid", "invalid project UUID"),
    ] {
        let step = job
            .complete(NodeOutcome::Succeeded {
                outputs: BTreeMap::from([
                    ("title".into(), Scalar::String(title)),
                    ("project".into(), Scalar::String(project.into())),
                ]),
            })
            .unwrap();
        let error = advance(&plan, &[step]).unwrap_err();
        assert!(error.to_string().contains(diagnostic), "{error}");
    }
}

#[test]
fn sample_traces_pass_failed_check_and_technical_failure() {
    let plan = compile(&parse_definition(GRAPH).unwrap(), &sources()).unwrap();
    let run = |fixture| simulate(&plan, &parse_fixture(fixture).unwrap()).unwrap();
    let passed = run(PASSED);
    assert_eq!(passed, run(PASSED));
    assert_eq!(passed.status, WorkflowStatus::Succeeded);
    assert_eq!(
        passed
            .steps
            .iter()
            .map(|step| step.node.as_str())
            .collect::<Vec<_>>(),
        plan.node_order()
    );
    let passed = serde_json::to_value(passed).unwrap();
    let step = |trace: &serde_json::Value, id: &str| {
        trace["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["node"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        step(&passed, "report")["inputs"],
        json!({"summary": "All checks passed"})
    );
    assert_eq!(step(&passed, "triage")["status"], "skipped");
    let failed = serde_json::to_value(run(FAILED)).unwrap();
    assert_eq!(step(&failed, "judge")["outcome"], "false");
    assert_eq!(step(&failed, "approve")["outcome"], "approved");
    assert_eq!(
        step(&failed, "followup")["inputs"],
        json!({
            "project_id": "10000000-0000-4000-8000-000000000001",
            "title": "Investigate the failed check",
        })
    );
    let recovered = run(SCRIPT_ERROR);
    assert_eq!(recovered.status, WorkflowStatus::Succeeded);
    assert_eq!(recovered.steps[0].status, NodeStatus::Failed);
    let recovered = serde_json::to_value(recovered).unwrap();
    assert_eq!(step(&recovered, "judge")["status"], "skipped");
    assert_eq!(step(&recovered, "script_error")["status"], "succeeded");
}

#[test]
fn joins_ignore_unchosen_branches_but_block_on_unhandled_failure() {
    let yaml = r#"
schema: mcbtodo.workflow/v1
id: join-check
name: Join check
entry: choose
nodes:
  choose: {kind: condition, equals: true}
  a: {kind: action, action: report.create}
  b: {kind: action, action: report.create}
  c: {kind: action, action: report.create}
  join: {kind: action, action: report.create}
edges:
  - {from: choose, to: a, when: "true"}
  - {from: choose, to: b, when: "true"}
  - {from: choose, to: c, when: "false"}
  - {from: a, to: join}
  - {from: b, to: join}
  - {from: c, to: join}
literals:
  - {to: {node: choose, port: value}, value: true}
  - {to: {node: a, port: summary}, value: "First"}
  - {to: {node: b, port: summary}, value: "Second"}
  - {to: {node: c, port: summary}, value: "Alternative"}
  - {to: {node: join, port: summary}, value: "Finished"}
"#;
    let plan = compile(&parse_definition(yaml).unwrap(), &BTreeMap::new()).unwrap();
    let run = |nodes| {
        let fixture = json!({"schema": "mcbtodo.workflow-fixture/v1", "nodes": nodes});
        simulate(&plan, &parse_fixture(&fixture.to_string()).unwrap()).unwrap()
    };
    let passed = run(json!({
        "a": {"status": "succeeded"},
        "b": {"status": "succeeded"},
        "join": {"status": "succeeded"}
    }));
    assert_eq!(passed.status, WorkflowStatus::Succeeded);
    assert_eq!(passed.steps[3].node, "c");
    assert_eq!(passed.steps[3].status, NodeStatus::Skipped);
    assert_eq!(passed.steps[4].node, "join");
    assert_eq!(passed.steps[4].status, NodeStatus::Succeeded);

    let failed = run(json!({
        "a": {"status": "succeeded"},
        "b": {"status": "failed", "message": "fixture failed"}
    }));
    assert_eq!(failed.status, WorkflowStatus::Failed);
    assert_eq!(failed.steps[4].status, NodeStatus::Skipped);
    assert!(failed.steps[4].reason.contains("blocked by `b`"));
}

#[test]
fn compiler_rejects_bad_references_cycles_branches_types_and_capabilities() {
    for (from, to, expected) in [
        ("to: judge}", "to: missing}", "unknown node"),
        ("port: passed}", "port: missing}", "unknown output port"),
        ("port: value}", "port: missing}", "unknown input port"),
        (
            "  - {from: judge, to: report, when: \"true\"}",
            "  - {from: judge, to: report}",
            "invalid outcome",
        ),
        (
            "  - {from: judge, to: report, when: \"true\"}",
            "  - {from: judge, to: report, when: \"false\"}",
            "missing branch",
        ),
        (
            "  - {from: approve, to: followup, when: approved}",
            "  - {from: approve, to: triage, when: approved}",
            "cycle",
        ),
        ("equals: true", "equals: 1", "type mismatch"),
        (
            "capability: analyze_test_failure",
            "capability: other",
            "undeclared capability",
        ),
        (
            "  - {kind: agent.invoke, scope: analyze_test_failure}",
            "  - {kind: agent.invoke, scope: analyze_test_failure}\n  - {kind: process.exec, scope: cargo}",
            "unused workflow capability",
        ),
        (
            "capabilities: [{kind: process.exec, scope: page-table-check}]",
            "capabilities: [{kind: agent.invoke, scope: analyze_test_failure}]",
            "cannot declare",
        ),
        (
            "scope: analyze_test_failure",
            "scope: \"*\"",
            "invalid scope",
        ),
        (
            "port: value}}",
            "port: value}}\n  - {from: {node: test, port: passed}, to: {node: judge, port: value}}",
            "multiple values",
        ),
        (
            "  - {from: test, to: judge}\n",
            "  - {from: script_error, to: judge}\n",
            "not guaranteed",
        ),
        (
            "  - {from: test, to: judge}",
            "  - {from: test, to: judge}\n  - {from: test, to: judge, when: failure}",
            "not guaranteed",
        ),
        (
            "  - {from: test, to: script_error, when: failure}\n",
            "",
            "unreachable",
        ),
    ] {
        let yaml = GRAPH.replace(from, to);
        assert_ne!(GRAPH, yaml);
        let error = compile(&parse_definition(&yaml).unwrap(), &sources()).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "{from} → {to}: {error}"
        );
    }
}

#[test]
fn revision_is_stable_under_reordering_and_depends_on_script_bytes() {
    let definition = parse_definition(GRAPH).unwrap();
    assert_eq!(
        definition.source_paths().collect::<Vec<_>>(),
        ["page-table-check", "scripts/check.nu"]
    );
    assert!(
        compile(&definition, &BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("page-table-check")
    );
    let first = compile(&definition, &sources()).unwrap();
    assert_eq!(first.workflow_id(), "nightly-check");
    let reordered = GRAPH.replace(
        "  - {from: test, to: judge}\n  - {from: test, to: script_error, when: failure}",
        "  - {from: test, to: script_error, when: failure}\n  - {from: test, to: judge}",
    );
    let reordered = compile(&parse_definition(&reordered).unwrap(), &sources()).unwrap();
    assert_eq!(first.revision_hash(), reordered.revision_hash());
    let mut changed_sources = sources();
    changed_sources
        .get_mut("scripts/check.nu")
        .unwrap()
        .push(b'\n');
    let changed = compile(&definition, &changed_sources).unwrap();
    assert_ne!(first.revision_hash(), changed.revision_hash());
}

#[test]
fn fixture_rejects_bad_outputs_and_missing_active_results() {
    let plan = compile(&parse_definition(GRAPH).unwrap(), &sources()).unwrap();
    for (fixture, expected) in [
        (
            PASSED.replace("\"passed\": true", "\"passed\": \"true\""),
            "type mismatch",
        ),
        (PASSED.replace("\"passed\": true, ", ""), "missing output"),
        (
            PASSED.replace("\"passed\": true", "\"passed\": true, \"unknown\": 1"),
            "unknown output",
        ),
        (
            PASSED.replace(
                "\"report\": {\"status\": \"succeeded\"}",
                "\"typo\": {\"status\": \"succeeded\"}",
            ),
            "unknown node",
        ),
        (
            PASSED.replace(",\n    \"report\": {\"status\": \"succeeded\"}", ""),
            "missing fixture",
        ),
    ] {
        let error = simulate(&plan, &parse_fixture(&fixture).unwrap()).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn parsing_rejects_unknown_fields_versions_duplicate_ids_and_unsafe_paths() {
    for yaml in [
        GRAPH.replace("mcbtodo.workflow/v1", "mcbtodo.workflow/v2"),
        GRAPH.replace("runner: nushell", "runner: nushell\n    typo: true"),
        GRAPH.replace(
            "  report:\n",
            "  judge: {kind: condition, equals: true}\n  report:\n",
        ),
        GRAPH.replace("scripts/check.nu", "../outside.nu"),
        GRAPH.replace("scripts/check.nu", "scripts/./check.nu"),
        GRAPH.replace("scripts/check.nu", "C:\\check.nu"),
    ] {
        assert!(
            parse_definition(&yaml).is_err(),
            "accepted malformed definition {yaml}"
        );
    }
    assert!(
        parse_fixture(&PASSED.replace("\"passed\": true", "\"passed\": true, \"passed\": false"))
            .is_err()
    );
}
