use chrono::NaiveDate;
use mcbtodo_domain::DependencyOperation;

use super::{View, creation::Kind};

/// An intentional terminal action, independent of the key or text that invokes it.
pub(super) enum Command {
    Quit,
    Reload,
    Edit,
    EditDeadline,
    Today,
    Date(NaiveDate),
    Help,
    Settings,
    Search(String),
    Proposals,
    Proposal(ProposalCommand),
    View(View),
    Timer(TimerCommand),
    Create(Kind),
    TimeBlock,
    GraphDependencies(DependencyOperation),
    Workflow(WorkflowCommand),
}

impl Command {
    pub(super) fn parse(input: &str) -> Result<Self, String> {
        let parts = input.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["q" | "quit"] => Ok(Self::Quit),
            ["reload"] => Ok(Self::Reload),
            ["edit"] => Ok(Self::Edit),
            ["deadline"] => Ok(Self::EditDeadline),
            ["today"] => Ok(Self::Today),
            ["date", value] => mcbtodo_client::parse_day(value).map(Self::Date),
            ["help" | "?"] => Ok(Self::Help),
            ["settings"] => Ok(Self::Settings),
            ["search", query @ ..] if !query.is_empty() => Ok(Self::Search(query.join(" "))),
            ["proposals" | "proposal"] => Ok(Self::Proposals),
            ["proposal", "refresh"] => Ok(Self::Proposal(ProposalCommand::Refresh)),
            ["proposal", "review"] => Ok(Self::Proposal(ProposalCommand::Review)),
            ["proposal", ..] => Err("Usage: proposal refresh|review".into()),
            ["graph", "add-dependency"] => Ok(Self::GraphDependencies(DependencyOperation::Add)),
            ["graph", "remove-dependency"] => {
                Ok(Self::GraphDependencies(DependencyOperation::Remove))
            }
            ["graph", ..] if parts.len() > 1 => {
                Err("Usage: graph add-dependency|remove-dependency".into())
            }
            ["workflow", "refresh"] => Ok(Self::Workflow(WorkflowCommand::Refresh)),
            ["workflow", "older"] => Ok(Self::Workflow(WorkflowCommand::Older)),
            ["workflow", "trace"] => Ok(Self::Workflow(WorkflowCommand::Trace)),
            ["workflow", "approve"] => {
                Ok(Self::Workflow(WorkflowCommand::Review { approved: true }))
            }
            ["workflow", "reject"] => {
                Ok(Self::Workflow(WorkflowCommand::Review { approved: false }))
            }
            ["workflow", "step", direction] => {
                let step = match *direction {
                    "previous" => TraceStep::Previous,
                    "next" => TraceStep::Next,
                    "refresh" => TraceStep::Refresh,
                    _ => return Err("Usage: workflow step previous|next|refresh".into()),
                };
                Ok(Self::Workflow(WorkflowCommand::Step(step)))
            }
            ["workflow", ..] if parts.len() > 1 => {
                Err("Usage: workflow refresh|older|trace|approve|reject|step".into())
            }
            ["time-block"] | ["create", "time-block"] => Ok(Self::TimeBlock),
            ["reminder"] | ["create", "reminder"] => Ok(Self::Create(Kind::Reminder)),
            ["create", "project"] => Ok(Self::Create(Kind::Project)),
            ["create", "task"] => Ok(Self::Create(Kind::Task)),
            ["create", "event"] => Ok(Self::Create(Kind::Event)),
            ["create", "deadline"] => Ok(Self::Create(Kind::Deadline)),
            ["create", ..] => {
                Err("Usage: create project|task|event|deadline|reminder|time-block".into())
            }
            ["timer", operation] => match *operation {
                "status" => Ok(Self::Timer(TimerCommand::Status)),
                "start" => Ok(Self::Timer(TimerCommand::Start)),
                "pause" => Ok(Self::Timer(TimerCommand::Pause)),
                "stop" => Ok(Self::Timer(TimerCommand::Stop)),
                _ => Err("Usage: timer status|start|pause|stop".into()),
            },
            ["view", view] | [view] => View::from_name(view).map(Self::View).ok_or_else(|| {
                let names = View::ALL.map(View::saved_name).join("|");
                format!("Usage: view {names}")
            }),
            _ => Err("Type :help for available commands".into()),
        }
    }
}

pub(super) enum TimerCommand {
    Status,
    Start,
    Pause,
    Stop,
}

pub(super) enum ProposalCommand {
    Refresh,
    Review,
}

pub(super) enum WorkflowCommand {
    Refresh,
    Older,
    Trace,
    Review { approved: bool },
    Step(TraceStep),
}

pub(super) enum TraceStep {
    Previous,
    Next,
    Refresh,
}
