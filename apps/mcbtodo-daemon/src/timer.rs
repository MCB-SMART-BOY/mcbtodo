use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use mcbtodo_ports::focus::{FocusCheckpoint, FocusSessionStore, TimerState};
use mcbtodo_protocol::focus::{DailyFocusReport, WorkUnitFocus};
use mcbtodo_protocol::{FocusSession, Request, TimerStatus};
use uuid::Uuid;

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);
type CommandResult = std::result::Result<TimerStatus, &'static str>;

pub struct Timer {
    store: Box<dyn FocusSessionStore>,
    session: Option<FocusCheckpoint>,
    running_since: Option<Instant>,
}

impl Timer {
    pub fn new(store: impl FocusSessionStore + 'static, session: Option<FocusCheckpoint>) -> Self {
        Self {
            store: Box::new(store),
            session,
            running_since: None,
        }
    }

    pub fn apply(&mut self, command: Request, now: Instant) -> Result<CommandResult> {
        let state = self
            .session
            .as_ref()
            .map_or(TimerState::Idle, |session| session.state);
        match (command, state) {
            (Request::Start { work_unit_id }, TimerState::Idle) => {
                let session = FocusCheckpoint {
                    id: Uuid::new_v4(),
                    work_unit_id,
                    state: TimerState::Running,
                    started_at: Utc::now(),
                    stopped_at: None,
                    elapsed: Duration::ZERO,
                };
                self.persist(session, now)?;
            }
            (Request::Start { .. } | Request::Pause, TimerState::NeedsReview) => {
                return Ok(Err(
                    "timer needs review after an interruption; stop it to keep the saved time",
                ));
            }
            (Request::Start { work_unit_id }, TimerState::Running | TimerState::Paused) => {
                let mut session = self.current(now)?;
                if work_unit_id.is_some() && work_unit_id != session.work_unit_id {
                    return Ok(Err(
                        "another focus session is active; stop it before starting a new one",
                    ));
                }
                if state == TimerState::Paused {
                    session.state = TimerState::Running;
                    self.persist(session, now)?;
                }
            }
            (Request::Pause, TimerState::Running) => {
                let mut session = self.current(now)?;
                session.state = TimerState::Paused;
                self.persist(session, now)?;
            }
            (Request::Stop, TimerState::Running | TimerState::Paused | TimerState::NeedsReview) => {
                let mut session = self.current(now)?;
                session.state = TimerState::Idle;
                session.stopped_at = Some(Utc::now());
                self.persist(session, now)?;
            }
            (Request::Status, _)
            | (Request::Pause, TimerState::Idle | TimerState::Paused)
            | (Request::Stop, TimerState::Idle) => (),
            (
                Request::Workflow(_)
                | Request::FocusReport { .. }
                | Request::Calendar { .. }
                | Request::Outline { .. }
                | Request::Inbox
                | Request::WorkUnitGraph { .. }
                | Request::PreviewWorkUnitDependencyChange { .. }
                | Request::SetWorkUnitDependencies { .. }
                | Request::TriageInbox { .. }
                | Request::ListNotifications { .. }
                | Request::ListReminderNotifications { .. }
                | Request::CaptureTask { .. }
                | Request::CreateAllDayEvent { .. }
                | Request::CreatePlannedProject { .. }
                | Request::CreateReminder { .. }
                | Request::CreateTimeBlock { .. },
                _,
            ) => {
                return Ok(Err("expected a timer command"));
            }
        }
        Ok(Ok(self.status(now)))
    }

    pub fn status(&self, now: Instant) -> TimerStatus {
        let session = self.session.as_ref();
        TimerStatus {
            state: session.map_or(TimerState::Idle, |value| value.state),
            session: session.map(|value| FocusSession {
                id: value.id,
                work_unit_id: value.work_unit_id,
                started_at: value.started_at,
                stopped_at: value.stopped_at,
                elapsed_seconds: (value.elapsed + self.uncommitted_time(now)).as_secs(),
            }),
        }
    }

    pub fn daily_report(
        &self,
        date: NaiveDate,
        timezone: Tz,
        now: Instant,
    ) -> Result<DailyFocusReport> {
        let mut total = FocusTotals::default();
        let mut by_work_unit = BTreeMap::<Option<Uuid>, FocusTotals>::new();
        self.store
            .for_each_session_on(date, timezone, &mut |session| {
                let mut elapsed = session.elapsed;
                if session.state == TimerState::Running {
                    elapsed += self.uncommitted_time(now);
                }
                let needs_review = session.state == TimerState::NeedsReview;
                total.add(elapsed, needs_review);
                by_work_unit
                    .entry(session.work_unit_id)
                    .or_default()
                    .add(elapsed, needs_review);
            })?;
        Ok(DailyFocusReport {
            date,
            timezone: timezone.to_string(),
            total_elapsed_seconds: total.elapsed.as_secs(),
            session_count: total.sessions,
            needs_review_count: total.needs_review,
            by_work_unit: by_work_unit
                .into_iter()
                .map(|(work_unit_id, totals)| WorkUnitFocus {
                    work_unit_id,
                    elapsed_seconds: totals.elapsed.as_secs(),
                    session_count: totals.sessions,
                    needs_review_count: totals.needs_review,
                })
                .collect(),
        })
    }

    pub fn checkpoint_due(&self) -> Option<Instant> {
        self.running_since.map(|since| since + CHECKPOINT_INTERVAL)
    }

    pub fn checkpoint(&mut self, now: Instant) -> Result<()> {
        if self.checkpoint_due().is_some_and(|due| now >= due) {
            self.persist(self.current(now)?, now)?;
        }
        Ok(())
    }

    fn uncommitted_time(&self, now: Instant) -> Duration {
        self.running_since
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }

    fn current(&self, now: Instant) -> Result<FocusCheckpoint> {
        let mut session = self
            .session
            .clone()
            .context("active focus session missing")?;
        session.elapsed += self.uncommitted_time(now);
        Ok(session)
    }

    fn persist(&mut self, session: FocusCheckpoint, now: Instant) -> Result<()> {
        self.store.save(&session)?;
        self.running_since = (session.state == TimerState::Running).then_some(now);
        self.session = Some(session);
        Ok(())
    }
}

#[derive(Default)]
struct FocusTotals {
    elapsed: Duration,
    sessions: u64,
    needs_review: u64,
}

impl FocusTotals {
    fn add(&mut self, elapsed: Duration, needs_review: bool) {
        self.elapsed += elapsed;
        self.sessions += 1;
        self.needs_review += u64::from(needs_review);
    }
}

#[cfg(test)]
#[path = "timer_tests.rs"]
mod tests;
