use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Serialize};

use crate::WorkflowError;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Definition {
    schema: Schema,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) entry: String,
    #[serde(default)]
    pub(crate) capabilities: Vec<Capability>,
    pub(crate) nodes: BTreeMap<String, Node>,
    #[serde(default)]
    pub(crate) edges: Vec<Edge>,
    #[serde(default)]
    pub(crate) bindings: Vec<Binding>,
    #[serde(default)]
    pub(crate) literals: Vec<Literal>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
enum Schema {
    #[serde(rename = "mcbtodo.workflow/v1")]
    V1,
}

#[derive(Debug, Clone)]
pub struct WorkflowDefinition(pub(crate) Definition);

impl WorkflowDefinition {
    /// Lists unique graph-relative scripts and executable inputs.
    pub fn source_paths(&self) -> impl Iterator<Item = &str> {
        let executables = self
            .0
            .nodes
            .values()
            .filter_map(|node| match node {
                Node::Script { capabilities, .. } => Some(capabilities),
                _ => None,
            })
            .flatten()
            .filter(|capability| capability.kind == CapabilityKind::ProcessExec)
            .map(|capability| capability.scope.as_str());
        self.script_paths()
            .chain(executables)
            .collect::<BTreeSet<_>>()
            .into_iter()
    }

    pub(crate) fn script_paths(&self) -> impl Iterator<Item = &str> {
        self.0.nodes.values().filter_map(|node| match node {
            Node::Script { source, .. } => Some(source.as_str()),
            _ => None,
        })
    }
}

pub fn parse_definition(yaml: &str) -> Result<WorkflowDefinition, WorkflowError> {
    let mut options = serde_saphyr::Options::default();
    options.duplicate_keys = serde_saphyr::DuplicateKeyPolicy::Error;
    options.merge_keys = serde_saphyr::MergeKeyPolicy::Error;
    let definition: Definition = serde_saphyr::from_str_with_options(yaml, options)
        .map_err(|error| WorkflowError::DefinitionSyntax(error.to_string()))?;
    let definition = WorkflowDefinition(definition);
    // Check before exposing any path to an I/O adapter.
    for path in definition.source_paths() {
        if !relative_path(path) {
            return Err(WorkflowError::InvalidDefinition(format!(
                "invalid workflow source path `{path}`"
            )));
        }
    }
    Ok(definition)
}

pub(crate) fn relative_path(path: &str) -> bool {
    path.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Node {
    Action {
        action: Action,
    },
    Script {
        runner: Runner,
        source: String,
        timeout_seconds: u32,
        capabilities: Vec<Capability>,
        #[serde(default)]
        inputs: Ports,
        #[serde(default)]
        outputs: Ports,
    },
    Condition {
        equals: Scalar,
    },
    Approval {},
    Agent {
        capability: String,
        #[serde(default)]
        inputs: Ports,
        #[serde(default)]
        outputs: Ports,
    },
}

pub(crate) type Ports = BTreeMap<String, PortType>;
pub(crate) type Values = BTreeMap<String, Scalar>;

impl Node {
    pub(crate) fn inputs(&self) -> Ports {
        match self {
            Self::Action {
                action: Action::ReportCreate,
            } => Ports::from([("summary".into(), PortType::String)]),
            Self::Action {
                action: Action::TaskCreate,
            } => Ports::from([
                ("project_id".into(), PortType::String),
                ("title".into(), PortType::String),
            ]),
            Self::Script { inputs, .. } | Self::Agent { inputs, .. } => inputs.clone(),
            Self::Condition { equals } => Ports::from([("value".into(), equals.port_type())]),
            Self::Approval {} => Ports::new(),
        }
    }

    pub(crate) fn outputs(&self) -> Option<&Ports> {
        match self {
            Self::Script { outputs, .. } | Self::Agent { outputs, .. } => Some(outputs),
            _ => None,
        }
    }

    pub(crate) fn outcomes(&self) -> &'static [EdgeOutcome] {
        use EdgeOutcome::*;
        match self {
            Self::Action { .. } | Self::Script { .. } | Self::Agent { .. } => &[Success, Failure],
            Self::Condition { .. } => &[True, False],
            Self::Approval {} => &[Approved, Rejected],
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub(crate) enum Action {
    #[serde(rename = "report.create")]
    ReportCreate,
    #[serde(rename = "task.create")]
    TaskCreate,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Runner {
    Nushell,
    Bash,
    Python,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortType {
    Boolean,
    Integer,
    String,
}

impl fmt::Display for PortType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::String => "string",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Scalar {
    Boolean(bool),
    Integer(i64),
    String(String),
}

impl Scalar {
    pub(crate) fn port_type(&self) -> PortType {
        match self {
            Self::Boolean(_) => PortType::Boolean,
            Self::Integer(_) => PortType::Integer,
            Self::String(_) => PortType::String,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeOutcome {
    #[default]
    Success,
    Failure,
    True,
    False,
    Approved,
    Rejected,
}

impl fmt::Display for EdgeOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::True => "true",
            Self::False => "false",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Edge {
    pub(crate) from: String,
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) when: EdgeOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Port {
    pub(crate) node: String,
    pub(crate) port: String,
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node, self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub(crate) from: Port,
    pub(crate) to: Port,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Literal {
    pub(crate) to: Port,
    pub(crate) value: Scalar,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Capability {
    pub(crate) kind: CapabilityKind,
    pub(crate) scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub(crate) enum CapabilityKind {
    #[serde(rename = "filesystem.read")]
    FilesystemRead,
    #[serde(rename = "filesystem.write")]
    FilesystemWrite,
    #[serde(rename = "process.exec")]
    ProcessExec,
    #[serde(rename = "network.access")]
    NetworkAccess,
    #[serde(rename = "secrets.read")]
    SecretsRead,
    #[serde(rename = "agent.invoke")]
    AgentInvoke,
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            CapabilityKind::FilesystemRead => "filesystem.read",
            CapabilityKind::FilesystemWrite => "filesystem.write",
            CapabilityKind::ProcessExec => "process.exec",
            CapabilityKind::NetworkAccess => "network.access",
            CapabilityKind::SecretsRead => "secrets.read",
            CapabilityKind::AgentInvoke => "agent.invoke",
        };
        write!(f, "{kind} `{}`", self.scope)
    }
}
