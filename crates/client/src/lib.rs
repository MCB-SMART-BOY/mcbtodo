//! Shared workspace access for the mcbtodo clients.

pub mod agent_discovery;
pub mod calendar_format;
pub mod daemon_client;
pub mod proposal;

use chrono::NaiveDate;
use mcbtodo_domain::time::parse_date;
use mcbtodo_protocol::{TimerState, TimerStatus};

pub fn parse_day(value: &str) -> Result<NaiveDate, String> {
    parse_date(value).map_err(|error| error.to_string())
}

pub fn timer_label(status: &TimerStatus) -> String {
    let state = match status.state {
        TimerState::Idle => "idle",
        TimerState::Running => "running",
        TimerState::Paused => "paused",
        TimerState::NeedsReview => "needs review",
    };
    match &status.session {
        Some(session) => format!(
            "Timer {state} · {}s · {}",
            session.elapsed_seconds, session.id
        ),
        None => format!("Timer {state}"),
    }
}
