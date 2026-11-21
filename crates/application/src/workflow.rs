use std::path::Path;

use mcbtodo_workflow::{
    Fixture, RevisionStore, SimulationTrace, WorkflowDraft, WorkflowRevision, load_draft,
    parse_fixture, simulate,
};

/// Shared workflow authoring and immutable revision use cases.
pub struct WorkflowUseCases {
    revisions: RevisionStore,
}

impl WorkflowUseCases {
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            revisions: RevisionStore::new(state_dir),
        }
    }

    /// Loads and compiles a workflow draft from its graph and adjacent sources.
    pub fn validate_draft(&self, graph_path: impl AsRef<Path>) -> anyhow::Result<WorkflowRevision> {
        Ok(load_draft(graph_path)?)
    }

    /// Simulates a draft with caller-provided fixture JSON.
    ///
    /// Reading and bounding the fixture file remains the transport's responsibility.
    pub fn simulate_draft(
        &self,
        graph_path: impl AsRef<Path>,
        fixture_json: &str,
    ) -> anyhow::Result<SimulationTrace> {
        let revision = load_draft(graph_path)?;
        let fixture: Fixture = parse_fixture(fixture_json)?;
        let mut trace = simulate(revision.plan(), &fixture)?;
        trace.revision_hash = revision.hash().to_owned();
        Ok(trace)
    }

    /// Publishes a graph and the exact source bytes captured from its directory.
    pub fn publish_graph(&self, graph_path: impl AsRef<Path>) -> anyhow::Result<WorkflowRevision> {
        Ok(self.revisions.publish(graph_path)?)
    }

    /// Publishes an immutable snapshot after revalidating its contents.
    pub fn publish_snapshot(&self, draft: &WorkflowDraft) -> anyhow::Result<WorkflowRevision> {
        Ok(self.revisions.publish_draft(draft)?)
    }

    pub fn list_revisions(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.revisions.list()?)
    }

    pub fn load_revision(&self, hash: &str) -> anyhow::Result<WorkflowRevision> {
        Ok(self.revisions.load(hash)?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn validation_publication_and_revision_access_share_one_use_case() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let graph = temp.path().join("workflow.yaml");
        fs::write(
            &graph,
            r#"schema: mcbtodo.workflow/v1
id: application-test
name: Application test
entry: report
nodes:
  report: {kind: action, action: report.create}
literals:
  - {to: {node: report, port: summary}, value: test}
"#,
        )
        .unwrap();
        let workflows = WorkflowUseCases::new(&state);

        let validated = workflows.validate_draft(&graph).unwrap();
        let published = workflows.publish_graph(&graph).unwrap();
        assert_eq!(published.hash(), validated.hash());
        assert_eq!(
            workflows.list_revisions().unwrap(),
            [published.hash().to_owned()]
        );

        let loaded = workflows.load_revision(published.hash()).unwrap();
        assert_eq!(loaded.graph_bytes(), published.graph_bytes());
        assert_eq!(
            workflows.publish_graph(&graph).unwrap().hash(),
            published.hash()
        );
    }
}
