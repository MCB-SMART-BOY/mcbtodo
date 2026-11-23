use std::{
    io::{self, BufRead, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use chrono::{DateTime, FixedOffset, NaiveDate};
use clap::{Args, Subcommand, ValueEnum};
use mcbtodo_application::{
    CompletionExportEntry, VaultCommands, WorkflowUseCases, document_revision_in_snapshot,
    project_inbox, work_unit_completion_context,
};
use mcbtodo_domain::{
    Availability, EvidenceRef, Id, InboxEntry, Priority, Project, Task, WorkUnit,
    WorkflowRevisionHash,
    recurrence::{CalendarRecurrence, ReminderSchedule},
    time::{DateSpan, Deadline, Span},
};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_vault::{Revision, Vault, VaultStore};

use crate::{daemon_client, parse_day};

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Capture an unclassified item in the Inbox.
    CaptureInbox {
        #[command(flatten)]
        document: NewDocument,
    },
    /// Print Inbox entries as JSON Lines.
    ListInbox,
    /// Assign an Inbox entry to an existing Project without losing its notes.
    TriageInbox {
        #[arg(long)]
        id: Id,
        #[arg(long)]
        project: Id,
    },
    /// Capture a task in an existing project.
    Capture {
        #[arg(long)]
        project: Id,
        #[command(flatten)]
        document: NewDocument,
        #[arg(long, value_enum, default_value = "normal")]
        priority: PriorityArg,
        /// CalendarRecurrence JSON for a recurring Task template.
        #[arg(long)]
        recurrence: Option<String>,
    },
    /// Materialize a recurring Task into independent Markdown occurrences.
    MaterializeTaskOccurrences {
        #[arg(long)]
        template: Id,
        #[arg(long, value_parser = parse_day)]
        from: NaiveDate,
        #[arg(long, value_parser = parse_day)]
        through: NaiveDate,
    },
    /// Complete a work unit whose prerequisites are complete.
    Complete {
        #[arg(long)]
        unit: Id,
        /// Workflow run UUID and zero-based step position (RUN_UUID:STEP). Repeat to attach more.
        #[arg(long, value_parser = parse_evidence, value_name = "RUN_UUID:STEP")]
        evidence: Vec<EvidenceRef>,
    },
    /// Set the date deadline of a project, task or work unit.
    SetDeadline {
        #[arg(long)]
        id: Id,
        #[arg(long, value_parser = parse_day)]
        date: NaiveDate,
    },
    /// Clear the deadline of a project, task or work unit.
    ClearDeadline {
        #[arg(long)]
        id: Id,
    },
    /// Reconcile interrupted completions and export recorded events as JSON Lines.
    CompletionEvents {
        /// Export up to 64 records after this journal-local sequence (0 starts at the beginning).
        #[arg(long, value_parser = clap::value_parser!(i64).range(0..))]
        after: Option<i64>,
        /// Export each fact in a versioned logical event envelope.
        #[arg(long, requires = "after")]
        logical: bool,
    },
    /// Replay trusted logical completion events from standard input.
    ImportCompletionEvents,
    /// Bind a work unit to a published, immutable workflow revision.
    BindWorkflow {
        #[arg(long)]
        unit: Id,
        #[arg(long)]
        revision: WorkflowRevisionHash,
    },
    /// Append a scheduled time block to a work unit.
    CreateTimeBlock {
        #[arg(long)]
        unit: Id,
        /// Start in RFC 3339 with an offset matching --timezone.
        #[arg(long, value_parser = parse_timestamp)]
        start: DateTime<FixedOffset>,
        /// Exclusive end in RFC 3339 with an offset matching --timezone.
        #[arg(long, value_parser = parse_timestamp)]
        end: DateTime<FixedOffset>,
        #[arg(long, value_enum, default_value = "busy")]
        availability: AvailabilityArg,
    },
    /// Create a project in a new directory.
    CreateProject {
        /// Project directory name. Defaults to the project UUID.
        #[arg(long)]
        slug: Option<String>,
        #[command(flatten)]
        document: NewDocument,
        /// First day of the project activity period (YYYY-MM-DD).
        #[arg(long, value_parser = parse_day, requires = "active_through")]
        active_from: Option<NaiveDate>,
        /// Last day of the project activity period (YYYY-MM-DD, inclusive).
        #[arg(long, value_parser = parse_day, requires = "active_from")]
        active_through: Option<NaiveDate>,
    },
    /// Create a work unit under an existing task and validate its prerequisites.
    CreateWorkUnit {
        #[arg(long)]
        project: Id,
        #[arg(long)]
        task: Id,
        #[command(flatten)]
        document: NewDocument,
        #[arg(long, value_enum, default_value = "normal")]
        priority: PriorityArg,
        #[arg(long)]
        estimated_minutes: Option<u32>,
        /// Prerequisite work unit UUID. Repeat to add multiple prerequisites.
        #[arg(long)]
        depends_on: Vec<Id>,
    },
    /// Create a one-time or recurring reminder from a JSON schedule object.
    CreateReminder {
        #[arg(long)]
        id: Option<Id>,
        #[arg(long)]
        target: Option<Id>,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        /// ReminderSchedule JSON, such as {"kind":"once","at":{"value":"2026-11-20T09:00:00+00:00","timezone":"UTC"}}.
        #[arg(long)]
        schedule: String,
    },
}

#[derive(Args)]
pub(crate) struct NewDocument {
    /// UUID to assign. A random UUID is generated when omitted.
    #[arg(long)]
    id: Option<Id>,
    #[arg(long)]
    title: String,
    /// Markdown content after the title.
    #[arg(long, default_value = "")]
    body: String,
    /// Due date (YYYY-MM-DD).
    #[arg(long, value_parser = parse_day)]
    deadline: Option<NaiveDate>,
    /// Repeat to add multiple tags.
    #[arg(long = "tag")]
    tags: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum PriorityArg {
    Low,
    Normal,
    High,
    Urgent,
}

impl From<PriorityArg> for Priority {
    fn from(value: PriorityArg) -> Self {
        match value {
            PriorityArg::Low => Self::Low,
            PriorityArg::Normal => Self::Normal,
            PriorityArg::High => Self::High,
            PriorityArg::Urgent => Self::Urgent,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum AvailabilityArg {
    Busy,
    Free,
}

pub(crate) fn requires_snapshot(command: &Command) -> bool {
    !matches!(
        command,
        Command::CaptureInbox { .. }
            | Command::CompletionEvents { .. }
            | Command::ImportCompletionEvents
    )
}

pub(crate) fn run_without_snapshot(
    command: Command,
    root: &Path,
    timezone: chrono_tz::Tz,
) -> Result<()> {
    match command {
        Command::CaptureInbox { document } => {
            let written = VaultCommands::new(root, timezone).capture_inbox_entry(
                InboxEntry {
                    id: document.id.unwrap_or_else(Id::new_v4),
                    captured_at: Utc::now(),
                    project_id: None,
                    tags: document.tags,
                },
                document.title,
                document.body,
            )?;
            println!("{} {}", written.id, written.path.display());
        }
        Command::CompletionEvents { after, logical } => {
            let store = VaultStore::new(root, timezone);
            let mut output = io::stdout().lock();
            if let Some(after) = after {
                for record in store.work_unit_completion_export_page(after)? {
                    if logical {
                        serde_json::to_writer(&mut output, &CompletionExportEntry::from(record))?;
                    } else {
                        serde_json::to_writer(&mut output, &record)?;
                    }
                    writeln!(output)?;
                }
            } else {
                for event in store.work_unit_completed_events()? {
                    serde_json::to_writer(&mut output, &event)?;
                    writeln!(output)?;
                }
            }
            output.flush()?;
        }
        Command::ImportCompletionEvents => {
            let store = VaultStore::new(root, timezone);
            let mut applied = 0;
            let mut repeated = 0;
            for (index, raw) in io::stdin().lock().lines().enumerate() {
                let line = index + 1;
                let input =
                    |message| format!("line {line}: {message}. Earlier lines remain committed");
                let raw = raw.with_context(|| input("could not read completion event"))?;
                let entry: CompletionExportEntry = serde_json::from_str(&raw)
                    .with_context(|| input("invalid logical completion event"))?;
                if entry
                    .import_into(&store)
                    .with_context(|| input("could not import completion event"))?
                {
                    applied += 1;
                } else {
                    repeated += 1;
                }
            }
            println!("Imported {applied}; already recorded {repeated}");
        }
        _ => unreachable!("command requires a Vault snapshot"),
    }
    Ok(())
}

pub(crate) fn run(
    command: Command,
    root: &Path,
    vault: &Vault,
    paths: &WorkspacePaths,
) -> Result<()> {
    let written =
        match command {
            Command::CaptureInbox { .. }
            | Command::CompletionEvents { .. }
            | Command::ImportCompletionEvents => {
                unreachable!("snapshot-independent command used snapshot path")
            }
            Command::ListInbox => {
                let entries = project_inbox(vault);
                let mut output = io::stdout().lock();
                for entry in entries {
                    serde_json::to_writer(&mut output, &entry)?;
                    writeln!(output)?;
                }
                output.flush()?;
                return Ok(());
            }
            Command::TriageInbox { id, project } => VaultCommands::new(root, vault.timezone())
                .triage_inbox_entry(id, project, revision(vault, id)?)?,
            Command::Capture {
                project,
                document,
                priority,
                recurrence,
            } => VaultCommands::new(root, vault.timezone()).capture_task(
                Task {
                    id: document.id.unwrap_or_else(Id::new_v4),
                    project_id: project,
                    status: Default::default(),
                    priority: priority.into(),
                    active_period: None,
                    deadline: document.deadline.map(date_deadline),
                    recurrence: recurrence
                        .map(|value| {
                            serde_json::from_str::<CalendarRecurrence>(&value)
                                .context("invalid task recurrence JSON")
                        })
                        .transpose()?,
                    occurrence: None,
                    tags: document.tags,
                },
                document.title,
                document.body,
                revision(vault, project)?,
            )?,
            Command::MaterializeTaskOccurrences {
                template,
                from,
                through,
            } => {
                let window = DateSpan::new(from, through)?;
                let ids = VaultCommands::new(root, vault.timezone()).materialize_task_occurrences(
                    template,
                    window,
                    revision(vault, template)?,
                )?;
                println!(
                    "{}",
                    ids.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                return Ok(());
            }
            Command::Complete { unit, evidence } => {
                let context = work_unit_completion_context(vault, unit)
                    .with_context(|| format!("no work unit with ID {unit} in the vault"))?;
                let origin = paths.load_or_create_device_identity()?;
                VaultCommands::new(root, vault.timezone())
                    .with_completion_origin(origin)
                    .complete_work_unit(context, &evidence, |binding, evidence| {
                        validate_evidence(paths, binding, evidence)
                    })?
            }
            Command::SetDeadline { id, date } => VaultCommands::new(root, vault.timezone())
                .set_deadline(id, Some(date_deadline(date)), revision(vault, id)?)?,
            Command::ClearDeadline { id } => VaultCommands::new(root, vault.timezone())
                .set_deadline(id, None, revision(vault, id)?)?,
            Command::BindWorkflow {
                unit,
                revision: workflow_revision,
            } => {
                let expected_revision = revision(vault, unit)?;
                WorkflowUseCases::new(paths.state_dir())
                    .load_revision(workflow_revision.as_str())
                    .with_context(|| {
                        format!("loading published workflow revision {workflow_revision}")
                    })?;
                VaultCommands::new(root, vault.timezone()).bind_workflow(
                    unit,
                    workflow_revision,
                    expected_revision,
                )?
            }
            Command::CreateTimeBlock {
                unit,
                start,
                end,
                availability,
            } => VaultCommands::new(root, vault.timezone()).create_time_block(
                unit,
                start,
                end,
                match availability {
                    AvailabilityArg::Busy => Availability::Busy,
                    AvailabilityArg::Free => Availability::Free,
                },
                revision(vault, unit)?,
            )?,
            Command::CreateProject {
                slug,
                document,
                active_from,
                active_through,
            } => {
                let active_period = active_from
                    .zip(active_through)
                    .map(|(start, end)| DateSpan::new(start, end).map(Span::DateSpan))
                    .transpose()?;
                VaultCommands::new(root, vault.timezone()).create_project(
                    Project {
                        id: document.id.unwrap_or_else(Id::new_v4),
                        status: Default::default(),
                        active_period,
                        deadline: document.deadline.map(date_deadline),
                        tags: document.tags,
                    },
                    slug,
                    document.title,
                    document.body,
                )?
            }
            Command::CreateWorkUnit {
                project,
                task,
                document,
                priority,
                estimated_minutes,
                depends_on,
            } => VaultCommands::new(root, vault.timezone()).create_work_unit(
                WorkUnit {
                    id: document.id.unwrap_or_else(Id::new_v4),
                    project_id: project,
                    parent_task_id: task,
                    status: Default::default(),
                    priority: priority.into(),
                    depends_on,
                    estimated_minutes,
                    deadline: document.deadline.map(date_deadline),
                    time_blocks: Vec::new(),
                    workflow_revision: None,
                    evidence: Vec::new(),
                    tags: document.tags,
                },
                document.title,
                document.body,
                revision(vault, task)?,
            )?,
            Command::CreateReminder {
                id,
                target,
                title,
                body,
                schedule,
            } => {
                let schedule: ReminderSchedule =
                    serde_json::from_str(&schedule).context("invalid reminder schedule JSON")?;
                ensure!(
                    paths.is_default_vault(root)?,
                    "background reminders are only supported for the default Vault ({})",
                    paths.vault_root().display()
                );
                VaultCommands::new(root, vault.timezone()).create_reminder(
                    id.unwrap_or_else(Id::new_v4),
                    target,
                    schedule,
                    title,
                    body,
                )?
            }
        };
    println!("{} {}", written.id, written.path.display());
    Ok(())
}

fn validate_evidence(
    paths: &WorkspacePaths,
    binding: Option<&WorkflowRevisionHash>,
    references: &[EvidenceRef],
) -> Result<()> {
    for &EvidenceRef { run_id, step_index } in references {
        if let Some(binding) = binding {
            let run = daemon_client::workflow_run(paths, run_id)
                .with_context(|| format!("inspecting workflow run {run_id}"))?;
            ensure!(
                run.revision == binding.as_str(),
                "workflow run {run_id} uses revision {}, but the work unit is bound to {binding}",
                run.revision
            );
        }
        let step = daemon_client::workflow_step(paths, run_id, step_index)
            .with_context(|| format!("checking workflow run {run_id}, step {step_index}"))?;
        ensure!(
            successful_step_evidence(&step),
            "workflow run {run_id}, step {step_index} is not successful evidence"
        );
    }
    Ok(())
}

fn successful_step_evidence(step: &serde_json::Value) -> bool {
    step.pointer("/trace/status")
        .and_then(serde_json::Value::as_str)
        == Some("succeeded")
}

fn parse_evidence(value: &str) -> Result<EvidenceRef> {
    let (run, step) = value.split_once(':').context("expected RUN_UUID:STEP")?;
    Ok(EvidenceRef {
        run_id: run.parse().context("invalid workflow run UUID")?,
        step_index: step.parse().context("invalid workflow step position")?,
    })
}

fn revision(vault: &Vault, id: Id) -> Result<Revision> {
    document_revision_in_snapshot(vault, id)
        .with_context(|| format!("no document with ID {id} in the vault"))
}

fn date_deadline(value: NaiveDate) -> Deadline {
    Deadline::Date { value }
}

fn parse_timestamp(value: &str) -> Result<DateTime<FixedOffset>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value)
}

#[cfg(test)]
mod evidence_tests {
    use serde_json::json;

    use super::successful_step_evidence;

    #[test]
    fn completion_evidence_requires_a_well_formed_successful_step_trace() {
        assert!(successful_step_evidence(
            &json!({"trace": {"status": "succeeded"}})
        ));
        assert!(!successful_step_evidence(
            &json!({"trace": {"status": "failed"}})
        ));
        assert!(!successful_step_evidence(
            &json!({"trace": {"status": "skipped"}})
        ));
        assert!(!successful_step_evidence(&json!({"trace": {}})));
        assert!(!successful_step_evidence(&json!({"trace": "succeeded"})));
        assert!(!successful_step_evidence(&json!({})));
    }
}
