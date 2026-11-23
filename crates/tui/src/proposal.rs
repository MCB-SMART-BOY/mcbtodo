//! Terminal presentation and confirmation shared by the CLI and TUI.

use std::io::{self, BufRead, IsTerminal, Write};

use anyhow::{Result, ensure};
use mcbtodo_client::proposal::Queue;
use mcbtodo_domain::Id;
use serde::Serialize;

pub fn apply_interactively(queue: &Queue, id: Id) -> Result<()> {
    let reviewed = queue.review(id)?;
    if reviewed.requires_approval()? {
        let stdin = io::stdin();
        let stdout = io::stdout();
        ensure!(
            stdin.is_terminal() && stdout.is_terminal(),
            "proposal apply requires an interactive terminal"
        );
        print_review(&reviewed)?;
        print!("Type 'apply' to confirm: ");
        stdout.lock().flush()?;
        let mut response = String::new();
        stdin.lock().read_line(&mut response)?;
        ensure!(response.trim() == "apply", "proposal was not confirmed");
    }
    let ids = queue.apply(&reviewed)?;
    if let Some(hash) = reviewed.workflow_revision() {
        println!("Published workflow revision {hash}");
    } else if let [id] = ids.as_slice() {
        println!("Applied proposal {} to document {id}", reviewed.id);
    } else {
        println!(
            "Applied proposal {} to {} work units",
            reviewed.id,
            ids.len()
        );
    }
    Ok(())
}

// The JSON serializer escapes ASCII controls. Escape Unicode control and
// formatting characters that could also interfere with terminal review.
pub fn print_review(value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    let mut out = io::stdout().lock();
    for ch in json.chars() {
        match ch {
            '\u{7f}'..='\u{9f}'
            | '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => {
                write!(out, "\\u{:04x}", u32::from(ch))?;
            }
            ch => write!(out, "{ch}")?,
        }
    }
    writeln!(out)?;
    Ok(())
}
