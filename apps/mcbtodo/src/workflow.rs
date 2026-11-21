use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use mcbtodo_application::WorkflowUseCases;
use mcbtodo_platform::WorkspacePaths;

const MAX_FIXTURE_BYTES: u64 = 1024 * 1024;

#[derive(Subcommand)]
pub enum Command {
    /// Compile a YAML graph and identify the revision of its inputs.
    Validate { graph: PathBuf },
    /// Evaluate a graph with fixture data and print a JSON trace.
    Simulate {
        graph: PathBuf,
        #[arg(long)]
        fixture: PathBuf,
    },
    /// Publish an immutable graph and its scripts, then print its revision hash.
    Publish { graph: PathBuf },
    /// List published workflow revision hashes.
    Revisions,
    #[command(flatten)]
    Runtime(crate::workflow_runtime_cli::Command),
}

pub fn run(command: Command, paths: &WorkspacePaths) -> Result<()> {
    match command {
        Command::Validate { graph } => {
            let revision = WorkflowUseCases::new(paths.state_dir()).validate_draft(graph)?;
            let plan = revision.plan();
            println!(
                "{} · {} nodes · {}",
                plan.workflow_id(),
                plan.node_order().len(),
                revision.hash()
            );
        }
        Command::Simulate { graph, fixture } => {
            let fixture_source = read_fixture(&fixture)?;
            let trace = WorkflowUseCases::new(paths.state_dir())
                .simulate_draft(graph, &fixture_source)
                .with_context(|| format!("simulating with fixture {}", fixture.display()))?;
            serde_json::to_writer_pretty(io::stdout().lock(), &trace)?;
            println!();
        }
        Command::Publish { graph } => {
            let revision = WorkflowUseCases::new(paths.state_dir()).publish_graph(graph)?;
            println!("{}", revision.hash());
        }
        Command::Revisions => {
            let revisions = WorkflowUseCases::new(paths.state_dir()).list_revisions()?;
            for hash in revisions {
                println!("{hash}");
            }
        }
        Command::Runtime(command) => crate::workflow_runtime_cli::run(command, paths)?,
    }
    Ok(())
}

fn read_fixture(path: &Path) -> Result<String> {
    ensure!(
        fs::metadata(path)
            .with_context(|| format!("opening {}", path.display()))?
            .is_file(),
        "expected a regular file at {}",
        path.display()
    );
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_FIXTURE_BYTES + 1).read_to_end(&mut bytes))
        .with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= MAX_FIXTURE_BYTES,
        "workflow input size limit exceeded while reading {}",
        path.display()
    );
    String::from_utf8(bytes).with_context(|| format!("expected UTF-8 in {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn workflow_commands_use_the_supplied_workspace_state_directory() {
        let home = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::from_home(home.path().to_path_buf());
        let graph = home.path().join("workflow.yaml");
        fs::write(
            &graph,
            r#"schema: mcbtodo.workflow/v1
id: workspace-path-test
name: Workspace path test
entry: report
nodes:
  report: {kind: action, action: report.create}
literals:
  - {to: {node: report, port: summary}, value: test}
"#,
        )
        .unwrap();

        run(
            Command::Publish {
                graph: graph.clone(),
            },
            &paths,
        )
        .unwrap();

        let revisions = WorkflowUseCases::new(paths.state_dir())
            .list_revisions()
            .unwrap();
        assert_eq!(revisions.len(), 1);
        assert!(
            home.path()
                .join(".mcbtodo/state/workflows/revisions")
                .join(format!("{}.json", revisions[0]))
                .is_file()
        );

        run(Command::Revisions, &paths).unwrap();
    }
}
