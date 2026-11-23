use chrono::NaiveDate;

use super::View;

/// An intentional terminal action, independent of the key or text that invokes it.
pub(super) enum Command {
    Quit,
    Reload,
    Today,
    Date(NaiveDate),
    Help,
    Settings,
    Search(String),
    Proposals,
    View(View),
    Timer(TimerCommand),
    TimeBlock,
}

impl Command {
    pub(super) fn parse(input: &str) -> Result<Self, String> {
        let parts = input.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["q" | "quit"] => Ok(Self::Quit),
            ["reload"] => Ok(Self::Reload),
            ["today"] => Ok(Self::Today),
            ["date", value] => mcbtodo_client::parse_day(value).map(Self::Date),
            ["help" | "?"] => Ok(Self::Help),
            ["settings"] => Ok(Self::Settings),
            ["search", query @ ..] if !query.is_empty() => Ok(Self::Search(query.join(" "))),
            ["proposals" | "proposal"] => Ok(Self::Proposals),
            ["time-block"] => Ok(Self::TimeBlock),
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
