use std::io::{self, Write};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand, ValueEnum};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::workflow::{
    Fold, Missed, ObservedOutcome, Overlap, Trigger, WorkflowRequest, WorkflowResponse,
};
use uuid::Uuid;

use crate::daemon_client;

#[derive(Subcommand)]
pub enum Command {
    /// Queue an immutable revision for immediate or one-time scheduled execution.
    Run {
        revision: String,
        /// RFC 3339 timestamp with an explicit UTC offset.
        #[arg(long)]
        at: Option<DateTime<Utc>>,
        /// Reuse the same ID to retry a request without queuing a second execution.
        #[arg(long)]
        run_id: Option<Uuid>,
    },
    /// Show a workflow run's state and the position of its latest step.
    Inspect { run_id: Uuid },
    /// Record a one-time operator observation for an interrupted, uncertain run.
    Resolve {
        run_id: Uuid,
        #[arg(long, value_enum)]
        outcome: ResolutionOutcome,
        #[arg(long)]
        note: String,
    },
    /// List recent workflow runs, with an optional pagination cursor.
    Runs {
        #[arg(long)]
        before: Option<Uuid>,
    },
    /// Read a recorded step and its artifact using a zero-based index.
    Trace { run_id: Uuid, index: u32 },
    /// Write the recorded step's artifact bytes to stdout.
    Artifact { run_id: Uuid, index: u32 },
    /// Continue a waiting run through an explicit approval node.
    Approve {
        run_id: Uuid,
        node: String,
        /// The approval_review value from the reviewed workflow inspect output.
        #[arg(long)]
        review: String,
    },
    /// Continue a waiting run through its rejection branch.
    Reject { run_id: Uuid, node: String },
    /// Cancel a pending or active run.
    Cancel { run_id: Uuid },
    /// Grant a specific published script revision permission to run in the sandbox.
    AuthorizeScript { revision: String, node: String },
    /// Revoke permission for future invocations of this script.
    RevokeScript { revision: String, node: String },
    /// Manage durable schedules and event triggers.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    /// Trigger on a five-field cron expression with an explicit IANA timezone.
    Cron {
        revision: String,
        expression: String,
        #[arg(long)]
        zone: String,
        #[arg(long, value_enum, default_value_t = FoldArg::Earlier)]
        fold: FoldArg,
        #[command(flatten)]
        policy: ScheduleOptions,
    },
    /// Trigger on a fixed rate using an explicit RFC 3339 anchor.
    Interval {
        revision: String,
        #[arg(long)]
        from: DateTime<Utc>,
        #[arg(long)]
        seconds: u32,
        #[command(flatten)]
        policy: ScheduleOptions,
    },
    /// Queue a pinned revision whenever another pinned revision succeeds.
    Dependency {
        /// Published downstream revision.
        revision: String,
        /// Published upstream revision to watch for future successes.
        upstream_revision: String,
        /// Reuse the ID to retry registration without creating a duplicate.
        #[arg(long)]
        id: Option<Uuid>,
    },
    /// Queue a pinned revision on future committed completions in the default Vault.
    WorkUnitCompleted {
        /// Published revision to queue.
        revision: String,
        /// Exact WorkUnit UUID to watch.
        work_unit_id: Uuid,
        /// Reuse the ID to retry registration with the original event cutoff.
        #[arg(long)]
        id: Option<Uuid>,
    },
    /// Inspect an existing schedule and its skipped occurrence counters.
    Inspect { id: Uuid },
    /// Stop new occurrences. Existing workflow runs keep their state.
    Disable { id: Uuid },
}

#[derive(Args)]
pub struct ScheduleOptions {
    #[arg(long)]
    id: Option<Uuid>,
    #[arg(long, value_enum, default_value_t = MissedArg::RunOnce)]
    missed: MissedArg,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=16))]
    replay_limit: u8,
    #[arg(long, value_enum, default_value_t = OverlapArg::Skip)]
    overlap: OverlapArg,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum FoldArg {
    Earlier,
    Later,
    Both,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ResolutionOutcome {
    Completed,
    Failed,
    NotStarted,
}

#[derive(Clone, Copy, ValueEnum)]
enum MissedArg {
    Skip,
    RunOnce,
    ReplayLimited,
}

#[derive(Clone, Copy, ValueEnum)]
enum OverlapArg {
    Skip,
    Queue,
}

impl ScheduleOptions {
    fn create(self, revision: String, trigger: Trigger) -> WorkflowRequest {
        WorkflowRequest::AddSchedule {
            id: self.id.unwrap_or_else(Uuid::new_v4),
            revision,
            trigger,
            missed: match self.missed {
                MissedArg::Skip => Missed::Skip,
                MissedArg::RunOnce => Missed::RunOnce,
                MissedArg::ReplayLimited => Missed::ReplayLimited {
                    limit: self.replay_limit,
                },
            },
            overlap: match self.overlap {
                OverlapArg::Skip => Overlap::Skip,
                OverlapArg::Queue => Overlap::Queue,
            },
        }
    }
}

pub fn run(command: Command, paths: &WorkspacePaths) -> Result<()> {
    let command = match command {
        Command::Run {
            revision,
            at,
            run_id,
        } => WorkflowRequest::Enqueue {
            run_id: run_id.unwrap_or_else(Uuid::new_v4),
            revision,
            due_at: at,
        },
        Command::Inspect { run_id } => {
            let run = daemon_client::workflow_run(paths, run_id)?;
            if let Some(review) = run.approval_review() {
                let mut output = serde_json::to_value(&run)?;
                output["approval_review"] = review.into();
                return print_json(&output);
            }
            return print_json(&run);
        }
        Command::Resolve {
            run_id,
            outcome,
            note,
        } => WorkflowRequest::ResolveUnknown {
            run_id,
            outcome: match outcome {
                ResolutionOutcome::Completed => ObservedOutcome::Completed,
                ResolutionOutcome::Failed => ObservedOutcome::Failed,
                ResolutionOutcome::NotStarted => ObservedOutcome::NotStarted,
            },
            note,
        },
        Command::Runs { before } => {
            let page = daemon_client::workflow_runs(paths, before)?;
            return print_json(&WorkflowResponse::Runs {
                runs: page.runs,
                next_before: page.next_before,
            });
        }
        Command::Trace { run_id, index } => {
            return print_json(&daemon_client::workflow_step(paths, run_id, index)?);
        }
        Command::Artifact { run_id, index } => return read_artifact(paths, run_id, index),
        Command::Approve {
            run_id,
            node,
            review,
        } => {
            return print_json(&daemon_client::decide_workflow(
                paths,
                run_id,
                node,
                true,
                Some(review),
            )?);
        }
        Command::Reject { run_id, node } => {
            return print_json(&daemon_client::decide_workflow(
                paths, run_id, node, false, None,
            )?);
        }
        Command::Cancel { run_id } => WorkflowRequest::Cancel { run_id },
        Command::AuthorizeScript { revision, node } => WorkflowRequest::AuthorizeScript {
            revision,
            node,
            granted: true,
        },
        Command::RevokeScript { revision, node } => WorkflowRequest::AuthorizeScript {
            revision,
            node,
            granted: false,
        },
        Command::Schedule { command } => match command {
            ScheduleCommand::Cron {
                revision,
                expression,
                zone,
                fold,
                policy,
            } => {
                let fold = match fold {
                    FoldArg::Earlier => Fold::Earlier,
                    FoldArg::Later => Fold::Later,
                    FoldArg::Both => Fold::Both,
                };
                policy.create(
                    revision,
                    Trigger::Cron {
                        expression,
                        timezone: zone,
                        fold,
                    },
                )
            }
            ScheduleCommand::Interval {
                revision,
                from,
                seconds,
                policy,
            } => policy.create(
                revision,
                Trigger::Interval {
                    anchor: from,
                    seconds,
                },
            ),
            ScheduleCommand::Dependency {
                revision,
                upstream_revision,
                id,
            } => WorkflowRequest::AddSchedule {
                id: id.unwrap_or_else(Uuid::new_v4),
                revision,
                trigger: Trigger::Dependency { upstream_revision },
                missed: Missed::Skip,
                overlap: Overlap::Queue,
            },
            ScheduleCommand::WorkUnitCompleted {
                revision,
                work_unit_id,
                id,
            } => WorkflowRequest::AddSchedule {
                id: id.unwrap_or_else(Uuid::new_v4),
                revision,
                trigger: Trigger::WorkUnitCompleted { work_unit_id },
                missed: Missed::Skip,
                overlap: Overlap::Queue,
            },
            ScheduleCommand::Inspect { id } => WorkflowRequest::InspectSchedule { id },
            ScheduleCommand::Disable { id } => WorkflowRequest::DisableSchedule { id },
        },
    };
    match daemon_client::workflow(paths, command)? {
        WorkflowResponse::Run(run) => print_json(&run),
        WorkflowResponse::Schedule(schedule) => print_json(&schedule),
        WorkflowResponse::Resolution(resolution) => print_json(&resolution),
        summary @ WorkflowResponse::ScriptAuthorization { .. } => print_json(&summary),
        _ => bail!("unexpected workflow response"),
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn read_artifact(paths: &WorkspacePaths, run_id: Uuid, index: u32) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let mut offset = 0;
    loop {
        let WorkflowResponse::Artifact { bytes, next_offset } = daemon_client::workflow(
            paths,
            WorkflowRequest::Artifact {
                run_id,
                index,
                offset,
            },
        )?
        else {
            bail!("unexpected response to artifact request");
        };
        stdout.write_all(&bytes)?;
        match next_offset {
            Some(next) if next > offset => offset = next,
            Some(_) => bail!("daemon returned an invalid artifact offset"),
            None => return Ok(stdout.flush()?),
        }
    }
}
