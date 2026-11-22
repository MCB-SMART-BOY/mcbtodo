use std::num::NonZeroUsize;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{DstResolution, MissedPolicy, Schedule, ScheduleError};

const MAX_REPLAY: u8 = 16;

/// Persisted workflow trigger definition shared by registration and delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Interval {
        anchor: DateTime<Utc>,
        seconds: u32,
    },
    Cron {
        expression: String,
        timezone: String,
        fold: Fold,
    },
    /// Queue on future successes of this exact published revision.
    Dependency {
        upstream_revision: String,
    },
    /// Queue on future committed completion events for this WorkUnit in the default Vault.
    WorkUnitCompleted {
        work_unit_id: Uuid,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fold {
    Earlier,
    Later,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case", deny_unknown_fields)]
pub enum Missed {
    Skip,
    RunOnce,
    ReplayLimited { limit: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    Skip,
    Queue,
}

/// A registered trigger and its durable delivery state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleSummary {
    pub id: Uuid,
    pub revision: String,
    pub trigger: Trigger,
    pub missed: Missed,
    pub overlap: Overlap,
    pub next_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub missed_occurrences: u64,
    pub overlap_skipped: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cascade_skipped: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, thiserror::Error)]
pub enum ClockScheduleError {
    #[error(transparent)]
    Schedule(#[from] ScheduleError),
    #[error("Cron expression or timezone is too long")]
    CronSpecTooLong,
    #[error("Unknown IANA timezone `{0}`")]
    UnknownTimezone(String),
    #[error("Dependency triggers have no clock schedule")]
    DependencyTrigger,
    #[error("WorkUnit completion triggers have no clock schedule")]
    CompletionTrigger,
    #[error("Replay limit must be between 1 and {MAX_REPLAY}")]
    ReplayLimit,
}

impl Trigger {
    /// Compile a clock trigger and validate its bounded missed-occurrence policy.
    pub fn clock_schedule(
        &self,
        missed: Missed,
    ) -> Result<(Schedule, MissedPolicy), ClockScheduleError> {
        let schedule = match self {
            Self::Interval { anchor, seconds } => {
                Schedule::interval(*anchor, TimeDelta::seconds(i64::from(*seconds)))?
            }
            Self::Cron {
                expression,
                timezone,
                fold,
            } => {
                if expression.len() > 256 || timezone.len() > 96 {
                    return Err(ClockScheduleError::CronSpecTooLong);
                }
                let zone = timezone
                    .parse()
                    .map_err(|_| ClockScheduleError::UnknownTimezone(timezone.clone()))?;
                let dst = match fold {
                    Fold::Earlier => DstResolution::Earlier,
                    Fold::Later => DstResolution::Later,
                    Fold::Both => DstResolution::Both,
                };
                Schedule::cron(expression, zone, dst)?
            }
            Self::Dependency { .. } => return Err(ClockScheduleError::DependencyTrigger),
            Self::WorkUnitCompleted { .. } => return Err(ClockScheduleError::CompletionTrigger),
        };
        let policy = match missed {
            Missed::Skip => MissedPolicy::Skip,
            Missed::RunOnce => MissedPolicy::RunOnce,
            Missed::ReplayLimited { limit } => {
                let limit = NonZeroUsize::new(limit.into())
                    .filter(|limit| limit.get() <= usize::from(MAX_REPLAY))
                    .ok_or(ClockScheduleError::ReplayLimit)?;
                MissedPolicy::ReplayLimited { limit }
            }
        };
        Ok((schedule, policy))
    }
}
