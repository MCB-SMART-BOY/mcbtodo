use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::{
    WorkflowDefinition, WorkflowError,
    definition::{
        Action, Capability, CapabilityKind, Definition, Edge, EdgeOutcome, Node, Port, PortType,
        Scalar, relative_path,
    },
};

/// Workflow source bytes keyed by the graph-relative path from [`WorkflowDefinition::source_paths`].
pub type ScriptSources = BTreeMap<String, Vec<u8>>;

/// An immutable in-memory plan identified by its normalized graph and script digests.
#[derive(Debug)]
pub struct ExecutionPlan {
    workflow_id: String,
    revision_hash: String,
    script_hashes: BTreeMap<String, String>,
    pub(crate) order: Vec<String>,
    pub(crate) nodes: BTreeMap<String, PlannedNode>,
}

impl ExecutionPlan {
    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    /// Lowercase SHA-256 of the normalized v1 definition and referenced script digests.
    pub fn revision_hash(&self) -> &str {
        &self.revision_hash
    }

    pub fn node_order(&self) -> &[String] {
        &self.order
    }

    /// Graph-relative path to lowercase SHA-256 for each referenced script.
    pub fn script_hashes(&self) -> &BTreeMap<String, String> {
        &self.script_hashes
    }

    pub fn is_script(&self, node: &str) -> bool {
        self.nodes
            .get(node)
            .is_some_and(|planned| matches!(planned.node, Node::Script { .. }))
    }
}

#[derive(Debug)]
pub(crate) struct PlannedNode {
    pub(crate) node: Node,
    pub(crate) incoming: Vec<Edge>,
    pub(crate) handles_failure: bool,
    pub(crate) inputs: BTreeMap<String, Input>,
    pub(crate) approval_targets: Vec<String>,
}

// Four complete reviews fit comfortably in each row of the daemon's run-list page.
const MAX_TASKS_PER_APPROVAL: usize = 4;

#[derive(Debug)]
pub(crate) enum Input {
    Literal(Scalar),
    Output(Port),
}

pub fn compile(
    definition: &WorkflowDefinition,
    sources: &ScriptSources,
) -> Result<ExecutionPlan, WorkflowError> {
    let definition_data = &definition.0;
    if !identifier(&definition_data.id) || definition_data.name.trim().is_empty() {
        return Err(invalid(
            "expected a workflow identifier and a non-empty name",
        ));
    }
    for (id, node) in &definition_data.nodes {
        if !identifier(id) {
            return Err(invalid(format!("invalid node identifier `{id}`")));
        }
        for port in node
            .inputs()
            .keys()
            .chain(node.outputs().into_iter().flatten().map(|(port, _)| port))
        {
            if !identifier(port) {
                return Err(invalid(format!("invalid port identifier `{id}.{port}`")));
            }
        }
    }
    let control = Control::resolve(definition_data)?;
    let order = control.topological_order(&definition_data.entry)?;
    control.check_branches(&definition_data.nodes)?;
    let mut inputs = resolve_inputs(definition_data, &control)?;
    check_capabilities(definition_data)?;
    let mut approval_targets = control.reviewed_writes(&definition_data.nodes, &order, &inputs)?;

    let script_hashes = definition
        .source_paths()
        .map(|path| {
            let bytes = sources
                .get(path)
                .ok_or_else(|| invalid(format!("missing script source `{path}`")))?;
            Ok((path.to_owned(), format!("{:x}", Sha256::digest(bytes))))
        })
        .collect::<Result<BTreeMap<_, _>, WorkflowError>>()?;
    let revision_hash = revision_hash(definition_data.clone(), &script_hashes)?;
    let nodes = definition_data
        .nodes
        .iter()
        .map(|(id, node)| {
            let incoming = control.incoming[id.as_str()]
                .iter()
                .map(|edge| (*edge).clone())
                .collect();
            let handles_failure = control.outgoing[id.as_str()]
                .iter()
                .any(|edge| edge.when == EdgeOutcome::Failure);
            (
                id.clone(),
                PlannedNode {
                    node: node.clone(),
                    incoming,
                    handles_failure,
                    inputs: inputs.remove(id).unwrap_or_default(),
                    approval_targets: approval_targets.remove(id).unwrap_or_default(),
                },
            )
        })
        .collect();
    Ok(ExecutionPlan {
        workflow_id: definition_data.id.clone(),
        revision_hash,
        script_hashes,
        order,
        nodes,
    })
}

struct Control<'a> {
    incoming: BTreeMap<&'a str, Vec<&'a Edge>>,
    outgoing: BTreeMap<&'a str, Vec<&'a Edge>>,
}

impl<'a> Control<'a> {
    fn resolve(definition: &'a Definition) -> Result<Self, WorkflowError> {
        let empty: BTreeMap<_, Vec<_>> = definition
            .nodes
            .keys()
            .map(|id| (id.as_str(), Vec::new()))
            .collect();
        let mut incoming = empty.clone();
        let mut outgoing = empty;
        let mut unique = BTreeSet::new();
        for edge in &definition.edges {
            let source = definition
                .nodes
                .get(&edge.from)
                .ok_or_else(|| invalid(format!("edge from unknown node `{}`", edge.from)))?;
            let target = incoming
                .get_mut(edge.to.as_str())
                .ok_or_else(|| invalid(format!("edge to unknown node `{}`", edge.to)))?;
            if !source.outcomes().contains(&edge.when) {
                return Err(invalid(format!(
                    "invalid outcome `{}` on edge `{} → {}`",
                    edge.when, edge.from, edge.to
                )));
            }
            if !unique.insert(edge) {
                return Err(invalid(format!(
                    "duplicate edge `{} → {}` ({})",
                    edge.from, edge.to, edge.when
                )));
            }
            outgoing.get_mut(edge.from.as_str()).unwrap().push(edge);
            target.push(edge);
        }
        for edges in incoming.values_mut() {
            edges.sort_unstable();
        }
        Ok(Self { incoming, outgoing })
    }

    fn topological_order(&self, entry: &str) -> Result<Vec<String>, WorkflowError> {
        let Some(entry_incoming) = self.incoming.get(entry) else {
            return Err(invalid(format!("entry is an unknown node `{entry}`")));
        };
        let mut remaining: BTreeMap<_, _> = self
            .incoming
            .iter()
            .map(|(id, edges)| (*id, edges.len()))
            .collect();
        let mut ready: BTreeSet<_> = remaining
            .iter()
            .filter_map(|(&id, &count)| (count == 0).then_some(id))
            .collect();
        let mut order = Vec::new();
        let mut reachable = BTreeSet::from([entry]);
        while let Some(id) = ready.pop_first() {
            order.push(id.to_owned());
            for edge in &self.outgoing[id] {
                let count = remaining.get_mut(edge.to.as_str()).unwrap();
                *count -= 1;
                if *count == 0 {
                    ready.insert(edge.to.as_str());
                }
                if reachable.contains(id) {
                    reachable.insert(edge.to.as_str());
                }
            }
        }
        if order.len() != self.incoming.len() {
            let blocked = remaining
                .iter()
                .filter_map(|(&id, &count)| (count > 0).then_some(id))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(invalid(format!(
                "control cycle prevents ordering: {blocked}"
            )));
        }
        if !entry_incoming.is_empty() {
            return Err(invalid(format!(
                "entry `{entry}` has incoming control edges"
            )));
        }
        if let Some(unreachable) = order.iter().find(|id| !reachable.contains(id.as_str())) {
            return Err(invalid(format!(
                "node `{unreachable}` is unreachable from `{entry}`"
            )));
        }
        Ok(order)
    }

    fn check_branches(&self, nodes: &BTreeMap<String, Node>) -> Result<(), WorkflowError> {
        for (id, node) in nodes {
            if matches!(node, Node::Condition { .. } | Node::Approval {}) {
                for outcome in node.outcomes() {
                    if !self.outgoing[id.as_str()]
                        .iter()
                        .any(|edge| edge.when == *outcome)
                    {
                        return Err(invalid(format!(
                            "node `{id}` is missing branch `{outcome}`"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn reviewed_writes(
        &self,
        nodes: &BTreeMap<String, Node>,
        order: &[String],
        inputs: &BTreeMap<String, BTreeMap<String, Input>>,
    ) -> Result<BTreeMap<String, Vec<String>>, WorkflowError> {
        let mut agent_descendants = BTreeSet::new();
        let mut approval_targets = BTreeMap::<String, Vec<String>>::new();
        for id in order {
            let incoming = &self.incoming[id.as_str()];
            let follows_agent = incoming
                .iter()
                .any(|edge| agent_descendants.contains(edge.from.as_str()));
            if follows_agent || matches!(nodes[id], Node::Agent { .. }) {
                agent_descendants.insert(id.as_str());
            }
            if follows_agent
                && matches!(
                    nodes[id],
                    Node::Action {
                        action: Action::TaskCreate
                    }
                )
            {
                // Only an Approval can emit Approved. No other edge may activate this write.
                if incoming
                    .iter()
                    .any(|edge| edge.when != EdgeOutcome::Approved)
                {
                    return Err(invalid(format!(
                        "Vault write `{id}` requires approval after an agent through direct `approved` edges only"
                    )));
                }
                for edge in incoming {
                    for input in inputs[id].values() {
                        if let Input::Output(from) = input
                            && !self.guarantees_output(&from.node, &edge.from)
                        {
                            return Err(invalid(format!(
                                "output `{from}` for Vault write `{id}` is not guaranteed before approval `{}`",
                                edge.from
                            )));
                        }
                    }
                    let targets = approval_targets.entry(edge.from.clone()).or_default();
                    if targets.len() == MAX_TASKS_PER_APPROVAL {
                        return Err(invalid(format!(
                            "approval `{}` can review at most {MAX_TASKS_PER_APPROVAL} task creations",
                            edge.from
                        )));
                    }
                    targets.push(id.clone());
                }
            }
        }
        Ok(approval_targets)
    }

    // The conservative v1 rule requires every control path to pass a successful
    // source. A direct failure edge never makes the source's outputs available.
    fn guarantees_output(&self, source: &str, target: &str) -> bool {
        let mut pending = vec![target];
        let mut visited = BTreeSet::new();
        while let Some(node) = pending.pop() {
            if !visited.insert(node) {
                continue;
            }
            let incoming = &self.incoming[node];
            if incoming.is_empty() {
                return false;
            }
            for edge in incoming {
                if edge.from == source {
                    if edge.when == EdgeOutcome::Failure {
                        return false;
                    }
                } else {
                    pending.push(edge.from.as_str());
                }
            }
        }
        true
    }
}

fn resolve_inputs(
    definition: &Definition,
    control: &Control<'_>,
) -> Result<BTreeMap<String, BTreeMap<String, Input>>, WorkflowError> {
    let ports: BTreeMap<_, _> = definition
        .nodes
        .iter()
        .map(|(id, node)| (id.as_str(), node.inputs()))
        .collect();
    let mut assigned = BTreeMap::<String, BTreeMap<String, Input>>::new();
    let mut guarantees = BTreeMap::new();
    let mut assign = |to: &Port, input: Input, actual: PortType| {
        let expected = ports
            .get(to.node.as_str())
            .ok_or_else(|| invalid(format!("input references unknown node `{}`", to.node)))?
            .get(&to.port)
            .ok_or_else(|| invalid(format!("unknown input port `{to}`")))?;
        if actual != *expected {
            return Err(invalid(format!(
                "type mismatch for `{to}`: expected {expected}, got {actual}"
            )));
        }
        if assigned
            .entry(to.node.clone())
            .or_default()
            .insert(to.port.clone(), input)
            .is_some()
        {
            return Err(invalid(format!("multiple values supplied to `{to}`")));
        }
        Ok(())
    };
    for binding in &definition.bindings {
        let from = &binding.from;
        let to = &binding.to;
        let source = definition
            .nodes
            .get(&from.node)
            .ok_or_else(|| invalid(format!("binding references unknown node `{}`", from.node)))?;
        let output_type = source
            .outputs()
            .and_then(|outputs| outputs.get(&from.port))
            .ok_or_else(|| invalid(format!("unknown output port `{from}`")))?;
        assign(to, Input::Output(from.clone()), *output_type)?;
        if !*guarantees
            .entry((&from.node, &to.node))
            .or_insert_with(|| control.guarantees_output(&from.node, &to.node))
        {
            return Err(invalid(format!(
                "output `{from}` is not guaranteed before input `{to}`"
            )));
        }
    }
    for literal in &definition.literals {
        assign(
            &literal.to,
            Input::Literal(literal.value.clone()),
            literal.value.port_type(),
        )?;
    }
    for (node, inputs) in ports {
        for port in inputs.keys() {
            if !assigned
                .get(node)
                .is_some_and(|values| values.contains_key(port))
            {
                return Err(invalid(format!("missing required input `{node}.{port}`")));
            }
        }
    }
    Ok(assigned)
}

fn check_capabilities(definition: &Definition) -> Result<(), WorkflowError> {
    let mut declared = BTreeSet::new();
    for capability in &definition.capabilities {
        if !valid_scope(capability) {
            return Err(invalid(format!(
                "invalid scope for capability {capability}"
            )));
        }
        if !declared.insert(capability.clone()) {
            return Err(invalid(format!(
                "duplicate workflow capability {capability}"
            )));
        }
    }
    let mut used = BTreeSet::new();
    for (id, node) in &definition.nodes {
        let required = match node {
            Node::Script {
                source,
                capabilities,
                timeout_seconds,
                ..
            } => {
                if *timeout_seconds == 0 {
                    return Err(invalid(format!(
                        "script `{id}` requires a positive timeout"
                    )));
                }
                if capabilities
                    .iter()
                    .any(|capability| capability.kind == CapabilityKind::AgentInvoke)
                {
                    return Err(invalid(format!(
                        "script `{id}` cannot declare `agent.invoke`"
                    )));
                }
                let mut executables = capabilities
                    .iter()
                    .filter(|capability| capability.kind == CapabilityKind::ProcessExec);
                if let Some(executable) = executables.next()
                    && (executable.scope == *source || executables.next().is_some())
                {
                    return Err(invalid(format!(
                        "script `{id}` may declare one executable input distinct from its source"
                    )));
                }
                capabilities.clone()
            }
            Node::Agent { capability, .. } => vec![Capability {
                kind: CapabilityKind::AgentInvoke,
                scope: capability.clone(),
            }],
            _ => continue,
        };
        let mut unique = BTreeSet::new();
        for capability in required {
            if !declared.contains(&capability) {
                return Err(invalid(format!(
                    "node `{id}` requires undeclared capability {capability}"
                )));
            }
            if !unique.insert(capability.clone()) {
                return Err(invalid(format!(
                    "node `{id}` repeats capability {capability}"
                )));
            }
            used.insert(capability);
        }
    }
    if let Some(capability) = declared.difference(&used).next() {
        return Err(invalid(format!("unused workflow capability {capability}")));
    }
    Ok(())
}

fn valid_scope(capability: &Capability) -> bool {
    match capability.kind {
        CapabilityKind::FilesystemRead
        | CapabilityKind::FilesystemWrite
        | CapabilityKind::ProcessExec => relative_path(&capability.scope),
        CapabilityKind::SecretsRead | CapabilityKind::AgentInvoke => identifier(&capability.scope),
        CapabilityKind::NetworkAccess => capability.scope.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        }),
    }
}

fn revision_hash(
    mut definition: Definition,
    scripts: &BTreeMap<String, String>,
) -> Result<String, WorkflowError> {
    definition.capabilities.sort_unstable();
    definition.edges.sort_unstable();
    definition.bindings.sort_unstable();
    definition.literals.sort_unstable_by(|a, b| a.to.cmp(&b.to));
    for node in definition.nodes.values_mut() {
        if let Node::Script { capabilities, .. } = node {
            capabilities.sort_unstable();
        }
    }
    let canonical = serde_json::to_vec(&(definition, scripts))?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

fn identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 96
        && bytes.next().is_some_and(|c| c.is_ascii_alphabetic())
        && bytes.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

fn invalid(message: impl Into<String>) -> WorkflowError {
    WorkflowError::InvalidDefinition(message.into())
}
