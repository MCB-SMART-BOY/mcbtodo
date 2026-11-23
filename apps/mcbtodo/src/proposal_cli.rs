use std::path::Path;

use anyhow::Result;
use chrono_tz::Tz;
use clap::Subcommand;
use mcbtodo_client::proposal::Queue;
use mcbtodo_domain::Id;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_tui::proposal::{apply_interactively, print_review};

#[derive(Subcommand)]
pub(crate) enum Command {
    /// List proposals for the configured Vault.
    List,
    /// Show the exact operation and its expected revision.
    Show { id: Id },
    /// Review and confirm a proposal in an interactive terminal.
    Apply { id: Id },
    /// Reject a pending proposal.
    Reject { id: Id },
}

pub(crate) fn run(
    command: Command,
    root: &Path,
    timezone: Tz,
    paths: &WorkspacePaths,
) -> Result<()> {
    let queue = Queue::open(root, timezone, paths)?;
    match command {
        Command::List => print_review(&queue.list()?),
        Command::Show { id } => print_review(&queue.review(id)?),
        Command::Apply { id } => apply_interactively(&queue, id),
        Command::Reject { id } => {
            queue.reject(id)?;
            println!("Rejected proposal {id}");
            Ok(())
        }
    }
}
