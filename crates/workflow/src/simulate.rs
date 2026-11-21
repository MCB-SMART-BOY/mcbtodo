use std::{collections::BTreeMap, fmt, marker::PhantomData};

use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, Visitor},
};

use crate::{
    ExecutionPlan, WorkflowError,
    definition::{EdgeOutcome, Values},
    traverse::{
        NodeJob, NodeOutcome, Traversal, TraversalError, validate_failure_message, validate_outputs,
    },
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    #[serde(rename = "schema")]
    _schema: Schema,
    #[serde(deserialize_with = "unique_map")]
    nodes: BTreeMap<String, FixtureResult>,
}

#[derive(Debug, Deserialize)]
enum Schema {
    #[serde(rename = "mcbtodo.workflow-fixture/v1")]
    V1,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum FixtureResult {
    Succeeded {
        #[serde(default, deserialize_with = "unique_map")]
        output: Values,
    },
    Failed {
        message: String,
    },
    Approved {},
    Rejected {},
}

impl FixtureResult {
    fn outcome(&self) -> NodeOutcome {
        match self {
            Self::Succeeded { output } => NodeOutcome::Succeeded {
                outputs: output.clone(),
            },
            Self::Failed { message } => NodeOutcome::Failed {
                message: message.clone(),
            },
            Self::Approved {} => NodeOutcome::Approved,
            Self::Rejected {} => NodeOutcome::Rejected,
        }
    }

    fn complete(&self, job: &NodeJob) -> Result<StepTrace, TraversalError> {
        let mut step = job.complete(self.outcome())?;
        let reason = match self {
            Self::Succeeded { .. } => "fixture succeeded",
            Self::Approved {} => "fixture approved",
            Self::Rejected {} => "fixture rejected",
            Self::Failed { .. } => return Ok(step),
        };
        step.reason = reason.into();
        Ok(step)
    }
}

pub fn parse_fixture(json: &str) -> Result<Fixture, WorkflowError> {
    serde_json::from_str(json).map_err(WorkflowError::FixtureSyntax)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SimulationTrace {
    pub workflow_id: String,
    pub revision_hash: String,
    pub status: WorkflowStatus,
    pub steps: Vec<StepTrace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct StepTrace {
    pub node: String,
    pub status: NodeStatus,
    pub outcome: Option<EdgeOutcome>,
    pub inputs: Values,
    pub outputs: Values,
    pub reason: String,
    /// The exact review shown for an approved Agent-descended Vault write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_review: Option<String>,
}

/// Simulates a validated plan in deterministic topological order using fixture results.
/// Conditions evaluate scalar equality. Every other active node requires a fixture.
pub fn simulate(plan: &ExecutionPlan, fixture: &Fixture) -> Result<SimulationTrace, WorkflowError> {
    validate_fixture(plan, fixture)?;
    let mut traversal = Traversal::new(plan);
    while let Some(job) = traversal.next_job().map_err(fixture_error)? {
        let result = fixture
            .nodes
            .get(&job.node)
            .ok_or_else(|| invalid(format!("missing fixture for active node `{}`", job.node)))?;
        traversal.record_job(result.complete(&job).map_err(fixture_error)?);
    }
    Ok(traversal.finish())
}

fn validate_fixture(plan: &ExecutionPlan, fixture: &Fixture) -> Result<(), WorkflowError> {
    for (id, result) in &fixture.nodes {
        let node = &plan
            .nodes
            .get(id)
            .ok_or_else(|| invalid(format!("unknown node `{id}`")))?
            .node;
        let outcome = match result {
            FixtureResult::Succeeded { .. } => EdgeOutcome::Success,
            FixtureResult::Failed { .. } => EdgeOutcome::Failure,
            FixtureResult::Approved {} => EdgeOutcome::Approved,
            FixtureResult::Rejected {} => EdgeOutcome::Rejected,
        };
        if !node.outcomes().contains(&outcome) {
            return Err(invalid(format!(
                "invalid fixture outcome `{outcome}` for node `{id}`"
            )));
        }
        match result {
            FixtureResult::Succeeded { output } => validate_outputs(id, node.outputs(), output),
            FixtureResult::Failed { message } => validate_failure_message(id, message),
            _ => Ok(()),
        }
        .map_err(invalid)?;
    }
    Ok(())
}

fn fixture_error(error: TraversalError) -> WorkflowError {
    match error {
        TraversalError::InvalidHistory(message)
        | TraversalError::InvalidOutcome(message)
        | TraversalError::InvalidReview(message) => invalid(message),
    }
}

fn invalid(message: impl Into<String>) -> WorkflowError {
    WorkflowError::InvalidFixture(message.into())
}

// serde_json's normal map deserializer silently keeps the last duplicate value.
fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct UniqueMap<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for UniqueMap<T> {
        type Value = BTreeMap<String, T>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object with unique keys")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
            let mut map = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, T>()? {
                match map.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        return Err(de::Error::custom(format_args!(
                            "duplicate key `{}`",
                            entry.key()
                        )));
                    }
                }
            }
            Ok(map)
        }
    }

    deserializer.deserialize_map(UniqueMap(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Scalar, compile, parse_definition,
        traverse::{JobKind, NodeOutcome, Progress, ScriptCapability, ScriptRunner, advance},
    };

    const GRAPH: &str = include_str!("../../../examples/workflow/graph.yaml");
    const SOURCE: &[u8] = include_bytes!("../../../examples/workflow/scripts/check.nu");
    const PASSED: &str = include_str!("../../../examples/workflow/fixtures/passed.json");
    const FAILED: &str = include_str!("../../../examples/workflow/fixtures/failed.json");
    const SCRIPT_ERROR: &str =
        include_str!("../../../examples/workflow/fixtures/script-error.json");

    fn sample_plan() -> ExecutionPlan {
        compile(
            &parse_definition(GRAPH).unwrap(),
            &BTreeMap::from([
                ("scripts/check.nu".into(), SOURCE.into()),
                (
                    "page-table-check".into(),
                    b"simulated executable input".to_vec(),
                ),
            ]),
        )
        .unwrap()
    }

    #[test]
    fn resuming_from_serialized_steps_matches_fixture_simulation() {
        let plan = sample_plan();
        let rejected = FAILED
            .replace("\"approved\"", "\"rejected\"")
            .replace("\"followup\"", "\"rejected\"");
        for (json, expected_jobs) in [
            (PASSED, &["test", "report"][..]),
            (FAILED, &["test", "triage", "approve", "followup"][..]),
            (&rejected, &["test", "triage", "approve", "rejected"][..]),
            (SCRIPT_ERROR, &["test", "script_error"][..]),
        ] {
            let fixture = parse_fixture(json).unwrap();
            let expected = simulate(&plan, &fixture).unwrap();
            let mut persisted = "[]".to_owned();
            let mut jobs = Vec::new();
            loop {
                let mut history: Vec<StepTrace> = serde_json::from_str(&persisted).unwrap();
                match advance(&plan, &history).unwrap() {
                    Progress::Next { job, steps } => {
                        assert_eq!(job.node, expected_jobs[jobs.len()]);
                        match job.node.as_str() {
                            "approve" => assert_eq!(
                                job.kind,
                                JobKind::Approval {
                                    prompt: "Approve task creation?\n`followup` in project 10000000-0000-4000-8000-000000000001 with title \"Investigate the failed check\"".into(),
                                }
                            ),
                            "report" => {
                                assert_eq!(
                                    job.kind,
                                    JobKind::Action {
                                        action: "report.create"
                                    }
                                );
                                assert_eq!(
                                    job.inputs["summary"],
                                    Scalar::String("All checks passed".into())
                                );
                                assert_eq!(steps[0].node, "judge");
                                assert_eq!(steps[0].outcome, Some(EdgeOutcome::True));
                            }
                            _ => {}
                        }
                        jobs.push(job.node.clone());
                        history.extend(steps);
                        let step = fixture.nodes[&job.node].complete(&job).unwrap();
                        history.push(step);
                        persisted = serde_json::to_string(&history).unwrap();
                    }
                    Progress::Complete(trace) => {
                        assert_eq!(jobs, expected_jobs);
                        assert_eq!(trace, expected);
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn resumed_join_still_blocks_on_a_persisted_blocked_predecessor() {
        let yaml = r#"
schema: mcbtodo.workflow/v1
id: blocked-join
name: Blocked join
entry: choose
nodes:
  choose: {kind: condition, equals: true}
  fail: {kind: action, action: report.create}
  pass: {kind: action, action: report.create}
  pending: {kind: action, action: report.create}
  blocked: {kind: action, action: report.create}
  join: {kind: action, action: report.create}
edges:
  - {from: choose, to: fail, when: "true"}
  - {from: choose, to: pass, when: "true"}
  - {from: choose, to: pass, when: "false"}
  - {from: fail, to: blocked}
  - {from: pass, to: pending}
  - {from: blocked, to: join}
  - {from: pending, to: join}
literals:
  - {to: {node: choose, port: value}, value: true}
  - {to: {node: fail, port: summary}, value: fail}
  - {to: {node: pass, port: summary}, value: pass}
  - {to: {node: pending, port: summary}, value: pending}
  - {to: {node: blocked, port: summary}, value: blocked}
  - {to: {node: join, port: summary}, value: join}
"#;
        let plan = compile(&parse_definition(yaml).unwrap(), &BTreeMap::new()).unwrap();
        let mut history = Vec::new();
        for (node, outcome) in [
            (
                "fail",
                NodeOutcome::Failed {
                    message: "broken".into(),
                },
            ),
            (
                "pass",
                NodeOutcome::Succeeded {
                    outputs: Values::new(),
                },
            ),
            (
                "pending",
                NodeOutcome::Succeeded {
                    outputs: Values::new(),
                },
            ),
        ] {
            let Progress::Next { job, steps } = advance(&plan, &history).unwrap() else {
                panic!("expected job for {node}");
            };
            assert_eq!(job.node, node);
            history.extend(steps);
            history.push(job.complete(outcome).unwrap());
        }
        assert_eq!(history[2].node, "blocked");
        assert_eq!(history[2].status, NodeStatus::Skipped);
        let Progress::Complete(trace) = advance(&plan, &history).unwrap() else {
            panic!("expected to skip join despite successful pending step");
        };
        assert_eq!(trace.status, WorkflowStatus::Failed);
        let join = trace.steps.last().unwrap();
        assert_eq!(join.node, "join");
        assert_eq!(join.status, NodeStatus::Skipped);
        assert_eq!(join.reason, "blocked by `blocked`");
    }

    #[test]
    fn job_reports_script_contract_and_checks_typed_outcomes() {
        let plan = sample_plan();
        let Progress::Next { job, steps } = advance(&plan, &[]).unwrap() else {
            panic!("expected script job");
        };
        assert!(steps.is_empty());
        assert!(job.inputs.is_empty());
        assert_eq!(
            job.kind,
            JobKind::Script {
                source: "scripts/check.nu".into(),
                runner: ScriptRunner::Nushell,
                timeout_seconds: 30,
                capabilities: vec![ScriptCapability {
                    kind: "process.exec",
                    scope: "page-table-check".into(),
                }],
            }
        );
        for (result, expected) in [
            (NodeOutcome::Approved, "invalid outcome"),
            (
                NodeOutcome::Failed {
                    message: " ".into(),
                },
                "missing failure message",
            ),
            (
                NodeOutcome::Succeeded {
                    outputs: BTreeMap::new(),
                },
                "missing output",
            ),
            (
                NodeOutcome::Succeeded {
                    outputs: BTreeMap::from([("passed".into(), Scalar::Integer(1))]),
                },
                "type mismatch",
            ),
            (
                NodeOutcome::Succeeded {
                    outputs: BTreeMap::from([("extra".into(), Scalar::Boolean(true))]),
                },
                "unknown output",
            ),
        ] {
            let error = job.complete(result).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn resume_rejects_tampered_history_before_dispatch() {
        let plan = sample_plan();
        let trace = simulate(&plan, &parse_fixture(PASSED).unwrap()).unwrap();
        let mut history = trace.steps[..2].to_vec();
        history[1].outcome = Some(EdgeOutcome::False);
        assert!(advance(&plan, &history).is_err());

        let mut history = trace.steps[..1].to_vec();
        history[0].outputs.remove("passed");
        assert!(advance(&plan, &history).is_err());

        let mut history = trace.steps[..3].to_vec();
        history[2].inputs.clear();
        assert!(advance(&plan, &history).is_err());
        assert!(advance(&plan, &trace.steps[1..]).is_err());
    }
}
