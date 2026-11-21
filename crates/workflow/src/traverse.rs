use std::{borrow::Cow, collections::BTreeMap, fmt::Write};

use uuid::Uuid;

use crate::{
    ExecutionPlan, NodeStatus, SimulationTrace, StepTrace, WorkflowStatus,
    compile::{Input, PlannedNode},
    definition::{
        Action, Capability, CapabilityKind, EdgeOutcome, Node, Ports, Runner, Scalar, Values,
    },
};

// Bound both the title and its escaped representation before exposing it in run summaries.
const MAX_REVIEW_TITLE_BYTES: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    /// Append these condition and skip steps to the supplied prefix before
    /// appending the result of the job.
    Next { job: NodeJob, steps: Vec<StepTrace> },
    /// The complete trace, including the supplied prefix.
    Complete(SimulationTrace),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeJob {
    pub node: String,
    pub kind: JobKind,
    pub inputs: Values,
    outcomes: &'static [EdgeOutcome],
    outputs: Option<Ports>,
    approval_review: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    Action {
        action: &'static str,
    },
    Script {
        source: String,
        runner: ScriptRunner,
        timeout_seconds: u32,
        capabilities: Vec<ScriptCapability>,
    },
    Approval {
        prompt: String,
    },
    Agent {
        capability: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRunner {
    Nushell,
    Bash,
    Python,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCapability {
    pub kind: &'static str,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOutcome {
    Succeeded { outputs: Values },
    Failed { message: String },
    Approved,
    Rejected,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TraversalError {
    #[error("workflow step history: {0}")]
    InvalidHistory(String),
    #[error("workflow node result: {0}")]
    InvalidOutcome(String),
    #[error("workflow approval review: {0}")]
    InvalidReview(String),
}

impl ExecutionPlan {
    /// Identifies Agent steps in persisted traces for this revision.
    pub fn is_agent(&self, node: &str) -> bool {
        self.nodes
            .get(node)
            .is_some_and(|planned| matches!(planned.node, Node::Agent { .. }))
    }
}

/// Replays a contiguous prefix for this plan's revision in topological order.
/// Conditions and skips are evaluated here. Only external work is returned as
/// a job. Calling again with the same prefix yields the same progress.
pub fn advance(plan: &ExecutionPlan, history: &[StepTrace]) -> Result<Progress, TraversalError> {
    let mut traversal = Traversal::new(plan);
    for step in history {
        traversal.replay(step)?;
    }
    if let Some(job) = traversal.next_job()? {
        let steps = traversal
            .steps
            .into_iter()
            .skip(history.len())
            .map(Cow::into_owned)
            .collect();
        Ok(Progress::Next { job, steps })
    } else {
        Ok(Progress::Complete(traversal.finish()))
    }
}

impl NodeJob {
    /// Validates an external result against this node's outcome and output ports.
    pub fn complete(&self, outcome: NodeOutcome) -> Result<StepTrace, TraversalError> {
        let approved = matches!(outcome, NodeOutcome::Approved);
        let mut step = outcome.into_step(self.node.clone(), self.inputs.clone());
        validate_result(self.outcomes, self.outputs.as_ref(), &step)
            .map_err(TraversalError::InvalidOutcome)?;
        if approved {
            step.approval_review = self.approval_review.clone();
        }
        Ok(step)
    }

    fn new(id: &str, node: &Node, inputs: Values) -> Self {
        let kind = match node {
            Node::Action { action } => JobKind::Action {
                action: match action {
                    Action::ReportCreate => "report.create",
                    Action::TaskCreate => "task.create",
                },
            },
            Node::Script {
                source,
                runner,
                timeout_seconds,
                capabilities,
                ..
            } => JobKind::Script {
                source: source.clone(),
                runner: match runner {
                    Runner::Nushell => ScriptRunner::Nushell,
                    Runner::Bash => ScriptRunner::Bash,
                    Runner::Python => ScriptRunner::Python,
                },
                timeout_seconds: *timeout_seconds,
                capabilities: capabilities
                    .iter()
                    .map(ScriptCapability::from_definition)
                    .collect(),
            },
            Node::Approval {} => JobKind::Approval {
                prompt: format!("Approve workflow node `{id}`?"),
            },
            Node::Agent { capability, .. } => JobKind::Agent {
                capability: capability.clone(),
            },
            Node::Condition { .. } => unreachable!("conditions are evaluated by traversal"),
        };
        Self {
            node: id.into(),
            kind,
            inputs,
            outcomes: node.outcomes(),
            outputs: node.outputs().cloned(),
            approval_review: None,
        }
    }
}

impl ScriptCapability {
    fn from_definition(capability: &Capability) -> Self {
        let kind = match capability.kind {
            CapabilityKind::FilesystemRead => "filesystem.read",
            CapabilityKind::FilesystemWrite => "filesystem.write",
            CapabilityKind::ProcessExec => "process.exec",
            CapabilityKind::NetworkAccess => "network.access",
            CapabilityKind::SecretsRead => "secrets.read",
            CapabilityKind::AgentInvoke => "agent.invoke",
        };
        Self {
            kind,
            scope: capability.scope.clone(),
        }
    }
}

impl NodeOutcome {
    fn into_step(self, node: String, inputs: Values) -> StepTrace {
        use EdgeOutcome::*;
        let (outcome, outputs, reason) = match self {
            Self::Succeeded { outputs } => (Success, outputs, "succeeded".into()),
            Self::Failed { message } => (Failure, Values::new(), message),
            Self::Approved => (Approved, Values::new(), "approved".into()),
            Self::Rejected => (Rejected, Values::new(), "rejected".into()),
        };
        StepTrace {
            node,
            status: outcome_status(outcome),
            outcome: Some(outcome),
            inputs,
            outputs,
            reason,
            approval_review: None,
        }
    }
}

fn validate_result(
    allowed: &[EdgeOutcome],
    ports: Option<&Ports>,
    step: &StepTrace,
) -> Result<(), String> {
    let id = &step.node;
    let outcome = step
        .outcome
        .ok_or_else(|| format!("missing outcome for node `{id}`"))?;
    if !allowed.contains(&outcome) {
        return Err(format!("invalid outcome `{outcome}` for node `{id}`"));
    }
    if step.status != outcome_status(outcome) {
        return Err(format!("status does not match outcome for node `{id}`"));
    }
    if outcome == EdgeOutcome::Failure {
        validate_failure_message(id, &step.reason)?;
    }
    let ports = if outcome == EdgeOutcome::Success {
        ports
    } else {
        None
    };
    validate_outputs(id, ports, &step.outputs)
}

pub(super) fn validate_failure_message(id: &str, message: &str) -> Result<(), String> {
    if message.trim().is_empty() {
        Err(format!("missing failure message for node `{id}`"))
    } else {
        Ok(())
    }
}

pub(super) fn validate_outputs(
    id: &str,
    ports: Option<&Ports>,
    outputs: &Values,
) -> Result<(), String> {
    for (port, value) in outputs {
        let Some(expected) = ports.and_then(|ports| ports.get(port)) else {
            return Err(format!("unknown output `{id}.{port}`"));
        };
        let actual = value.port_type();
        if actual != *expected {
            return Err(format!(
                "type mismatch for output `{id}.{port}`: expected {expected}, got {actual}"
            ));
        }
    }
    if let Some(ports) = ports {
        for port in ports.keys() {
            if !outputs.contains_key(port) {
                return Err(format!("missing output `{id}.{port}`"));
            }
        }
    }
    Ok(())
}

fn outcome_status(outcome: EdgeOutcome) -> NodeStatus {
    if outcome == EdgeOutcome::Failure {
        NodeStatus::Failed
    } else {
        NodeStatus::Succeeded
    }
}

pub(super) struct Traversal<'plan, 'step> {
    plan: &'plan ExecutionPlan,
    steps: Vec<Cow<'step, StepTrace>>,
    visits: BTreeMap<&'plan str, Visit>,
    status: WorkflowStatus,
}

struct Visit {
    index: usize,
    blocked: bool,
}

enum NextStep {
    Automatic { step: StepTrace, blocked: bool },
    Job(NodeJob),
}

impl<'plan, 'step> Traversal<'plan, 'step> {
    pub(super) fn new(plan: &'plan ExecutionPlan) -> Self {
        Self {
            plan,
            steps: Vec::new(),
            visits: BTreeMap::new(),
            status: WorkflowStatus::Succeeded,
        }
    }

    fn replay(&mut self, step: &'step StepTrace) -> Result<(), TraversalError> {
        let Some(id) = self.plan.order.get(self.steps.len()) else {
            return Err(TraversalError::InvalidHistory(format!(
                "unexpected step `{}` after completion",
                step.node
            )));
        };
        if &step.node != id {
            return Err(TraversalError::InvalidHistory(format!(
                "expected node `{id}`, got `{}`",
                step.node
            )));
        }
        let blocked = match self.prepare(id)? {
            NextStep::Automatic {
                step: expected,
                blocked,
            } => {
                if *step != expected {
                    return Err(TraversalError::InvalidHistory(format!(
                        "step `{id}` differs from the evaluated condition or skip"
                    )));
                }
                blocked
            }
            NextStep::Job(job) => {
                if step.inputs != job.inputs {
                    return Err(TraversalError::InvalidHistory(format!(
                        "inputs for node `{id}` differ from resolved inputs"
                    )));
                }
                validate_result(job.outcomes, job.outputs.as_ref(), step)
                    .map_err(TraversalError::InvalidHistory)?;
                let expected_review = if step.outcome == Some(EdgeOutcome::Approved) {
                    job.approval_review.as_deref()
                } else {
                    None
                };
                if step.approval_review.as_deref() != expected_review {
                    return Err(TraversalError::InvalidHistory(format!(
                        "step `{id}` does not record the required task review"
                    )));
                }
                false
            }
        };
        self.record(Cow::Borrowed(step), blocked);
        Ok(())
    }

    pub(super) fn next_job(&mut self) -> Result<Option<NodeJob>, TraversalError> {
        while let Some(id) = self.plan.order.get(self.steps.len()) {
            match self.prepare(id)? {
                NextStep::Automatic { step, blocked } => self.record(Cow::Owned(step), blocked),
                NextStep::Job(job) => return Ok(Some(job)),
            }
        }
        Ok(None)
    }

    pub(super) fn record_job(&mut self, step: StepTrace) {
        self.record(Cow::Owned(step), false);
    }

    pub(super) fn finish(self) -> SimulationTrace {
        SimulationTrace {
            workflow_id: self.plan.workflow_id().into(),
            revision_hash: self.plan.revision_hash().into(),
            status: self.status,
            steps: self.steps.into_iter().map(Cow::into_owned).collect(),
        }
    }

    fn record(&mut self, step: Cow<'step, StepTrace>, blocked: bool) {
        let index = self.steps.len();
        let id = &self.plan.order[index];
        if step.status == NodeStatus::Failed && !self.plan.nodes[id].handles_failure {
            self.status = WorkflowStatus::Failed;
        }
        self.visits.insert(id, Visit { index, blocked });
        self.steps.push(step);
    }

    fn prepare(&self, id: &str) -> Result<NextStep, TraversalError> {
        let node = &self.plan.nodes[id];
        let mut has_taken_edge = node.incoming.is_empty();
        let mut blocked_by = None;
        for edge in &node.incoming {
            let visit = &self.visits[edge.from.as_str()];
            let predecessor = &self.steps[visit.index];
            if predecessor.outcome == Some(edge.when) {
                has_taken_edge = true;
            } else if visit.blocked
                || (predecessor.status == NodeStatus::Failed
                    && !self.plan.nodes[&edge.from].handles_failure)
            {
                blocked_by.get_or_insert(&edge.from);
            }
        }
        if let Some(predecessor) = blocked_by {
            return Ok(NextStep::Automatic {
                step: skipped(id, format!("blocked by `{predecessor}`")),
                blocked: true,
            });
        }
        if !has_taken_edge {
            return Ok(NextStep::Automatic {
                step: skipped(id, "no active incoming edge".into()),
                blocked: false,
            });
        }
        let inputs = self.resolve_inputs(id, node)?;
        if let Node::Condition { equals } = &node.node {
            let outcome = if inputs["value"] == *equals {
                EdgeOutcome::True
            } else {
                EdgeOutcome::False
            };
            return Ok(NextStep::Automatic {
                step: StepTrace {
                    node: id.into(),
                    status: NodeStatus::Succeeded,
                    outcome: Some(outcome),
                    inputs,
                    outputs: Values::new(),
                    reason: "evaluated scalar equality".into(),
                    approval_review: None,
                },
                blocked: false,
            });
        }
        let mut job = NodeJob::new(id, &node.node, inputs);
        if let JobKind::Approval { prompt } = &mut job.kind
            && !node.approval_targets.is_empty()
        {
            *prompt = self.review_task_creations(&node.approval_targets)?;
            job.approval_review = Some(prompt.clone());
        }
        Ok(NextStep::Job(job))
    }

    fn review_task_creations(&self, targets: &[String]) -> Result<String, TraversalError> {
        let mut prompt = String::from("Approve task creation?");
        for id in targets {
            let inputs = self.resolve_inputs(id, &self.plan.nodes[id])?;
            let (Some(Scalar::String(title)), Some(Scalar::String(project_id))) =
                (inputs.get("title"), inputs.get("project_id"))
            else {
                return Err(TraversalError::InvalidReview(format!(
                    "task `{id}` requires a title and project UUID"
                )));
            };
            let project: Uuid = project_id.parse().map_err(|_| {
                TraversalError::InvalidReview(format!("task `{id}` has an invalid project UUID"))
            })?;
            if title.len() > MAX_REVIEW_TITLE_BYTES
                || title.trim().is_empty()
                || title.chars().any(char::is_control)
            {
                return Err(TraversalError::InvalidReview(format!(
                    "task `{id}` requires a non-empty title with no control characters and at most {MAX_REVIEW_TITLE_BYTES} bytes"
                )));
            }
            // The Vault parser trims the Markdown heading to obtain the saved title.
            let title = format!("{:?}", title.trim());
            if title.len() > MAX_REVIEW_TITLE_BYTES + 2 {
                return Err(TraversalError::InvalidReview(format!(
                    "task `{id}` exceeds the {MAX_REVIEW_TITLE_BYTES}-byte escaped title limit"
                )));
            }
            write!(prompt, "\n`{id}` in project {project} with title {title}")
                .expect("writing to a String cannot fail");
        }
        Ok(prompt)
    }

    fn resolve_inputs(&self, id: &str, node: &PlannedNode) -> Result<Values, TraversalError> {
        node.inputs
            .iter()
            .map(|(port, input)| {
                let value = match input {
                    Input::Literal(value) => value,
                    Input::Output(from) => self
                        .visits
                        .get(from.node.as_str())
                        .and_then(|visit| self.steps[visit.index].outputs.get(&from.port))
                        .ok_or_else(|| {
                            TraversalError::InvalidHistory(format!(
                                "missing output `{from}` for input `{id}.{port}`"
                            ))
                        })?,
                };
                Ok((port.clone(), value.clone()))
            })
            .collect()
    }
}

fn skipped(id: &str, reason: String) -> StepTrace {
    StepTrace {
        node: id.into(),
        status: NodeStatus::Skipped,
        outcome: None,
        inputs: Values::new(),
        outputs: Values::new(),
        reason,
        approval_review: None,
    }
}
