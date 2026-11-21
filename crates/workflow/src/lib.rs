//! Compilation, fixture simulation, immutable publication and trigger calculation for workflows.
//!
//! Script digests identify the supplied bytes, and capability declarations describe intended access.

mod compile;
mod definition;
pub mod publication;
pub mod schedule;
mod simulate;
mod traverse;

pub use compile::{ExecutionPlan, ScriptSources, compile};
pub use definition::{EdgeOutcome, Scalar, WorkflowDefinition, parse_definition};
pub use publication::{
    PublicationError, RevisionStore, WorkflowDraft, WorkflowRevision, load_draft,
};
pub use simulate::{
    Fixture, NodeStatus, SimulationTrace, StepTrace, WorkflowStatus, parse_fixture, simulate,
};
pub use traverse::{
    JobKind, NodeJob, NodeOutcome, Progress, ScriptCapability, ScriptRunner, TraversalError,
    advance,
};

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("workflow YAML: {0}")]
    DefinitionSyntax(String),
    #[error("workflow definition: {0}")]
    InvalidDefinition(String),
    #[error("fixture JSON: {0}")]
    FixtureSyntax(serde_json::Error),
    #[error("workflow fixture: {0}")]
    InvalidFixture(String),
    #[error("encoding workflow revision: {0}")]
    EncodeRevision(#[from] serde_json::Error),
}
