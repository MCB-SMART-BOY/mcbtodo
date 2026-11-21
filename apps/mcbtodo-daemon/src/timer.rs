use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use mcbtodo_protocol::focus::{DailyFocusReport, WorkUnitFocus};
use mcbtodo_protocol::{FocusSession, Request, TimerState, TimerStatus};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use uuid::Uuid;

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);
type CommandResult = std::result::Result<TimerStatus, &'static str>;

pub struct Timer {
    db: Connection,
    session: Option<Session>,
    running_since: Option<Instant>,
}

#[derive(Clone)]
struct Session {
    id: Uuid,
    work_unit_id: Option<Uuid>,
    state: TimerState,
    started_at: DateTime<Utc>,
    stopped_at: Option<DateTime<Utc>>,
    elapsed: Duration,
}

impl Timer {
    pub fn open(path: &Path) -> Result<Self> {
        crate::endpoint::open_private_file(path)?;
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.pragma_update(None, "synchronous", "FULL")?;
        let tx = db.transaction()?;
        match tx.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))? {
            0 => tx.execute_batch(
                "CREATE TABLE focus_sessions (
                    id TEXT NOT NULL UNIQUE,
                    work_unit_id TEXT,
                    state TEXT NOT NULL CHECK (state IN ('running', 'paused', 'needs_review', 'stopped')),
                    started_at TEXT NOT NULL,
                    stopped_at TEXT,
                    elapsed_ns INTEGER NOT NULL CHECK (elapsed_ns >= 0),
                    CHECK ((state = 'stopped') = (stopped_at IS NOT NULL))
                ) STRICT;
                CREATE UNIQUE INDEX one_active_focus_session ON focus_sessions ((1))
                    WHERE state <> 'stopped';
                PRAGMA user_version = 1;",
            )?,
            1 => (),
            version => bail!("unsupported focus database schema version {version}"),
        }
        // The elapsed value of a running session is already its durable checkpoint.
        tx.execute(
            "UPDATE focus_sessions SET state = 'needs_review' WHERE state = 'running'",
            [],
        )?;
        let session = Session::load_latest(&tx)?;
        tx.commit()?;
        Ok(Self {
            db,
            session,
            running_since: None,
        })
    }

    pub fn apply(&mut self, command: Request, now: Instant) -> Result<CommandResult> {
        let state = self
            .session
            .as_ref()
            .map_or(TimerState::Idle, |session| session.state);
        match (command, state) {
            (Request::Start { work_unit_id }, TimerState::Idle) => {
                let session = Session {
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
                | Request::ListNotifications { .. }
                | Request::ListReminderNotifications { .. },
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
        let mut query = self
            .db
            .prepare("SELECT work_unit_id, started_at, elapsed_ns, state FROM focus_sessions")?;
        let mut sessions = query.query([])?;
        let mut total = FocusTotals::default();
        let mut by_work_unit = BTreeMap::<Option<Uuid>, FocusTotals>::new();
        while let Some(row) = sessions.next()? {
            let started_at: DateTime<Utc> = row.get_ref(1)?.as_str()?.parse()?;
            if started_at.with_timezone(&timezone).date_naive() != date {
                continue;
            }
            let work_unit_id = row
                .get_ref(0)?
                .as_str_or_null()?
                .map(str::parse)
                .transpose()?;
            let mut elapsed = Duration::from_nanos(row.get::<_, i64>(2)?.try_into()?);
            let state = row.get_ref(3)?.as_str()?;
            if state == "running" {
                elapsed += self.uncommitted_time(now);
            }
            let needs_review = state == "needs_review";
            total.add(elapsed, needs_review);
            by_work_unit
                .entry(work_unit_id)
                .or_default()
                .add(elapsed, needs_review);
        }
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

    fn current(&self, now: Instant) -> Result<Session> {
        let mut session = self
            .session
            .clone()
            .context("active focus session missing")?;
        session.elapsed += self.uncommitted_time(now);
        Ok(session)
    }

    fn persist(&mut self, session: Session, now: Instant) -> Result<()> {
        session.save(&self.db)?;
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

impl Session {
    fn load_latest(db: &Connection) -> Result<Option<Self>> {
        let record = db
            .query_row(
                "SELECT id, work_unit_id, state, started_at, stopped_at, elapsed_ns
             FROM focus_sessions ORDER BY rowid DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, work_unit_id, state, started_at, stopped_at, elapsed_ns)) = record else {
            return Ok(None);
        };
        let state = match state.as_str() {
            "stopped" => TimerState::Idle,
            "paused" => TimerState::Paused,
            "needs_review" => TimerState::NeedsReview,
            _ => bail!("unexpected saved focus state {state:?}"),
        };
        Ok(Some(Self {
            id: id.parse()?,
            work_unit_id: work_unit_id.as_deref().map(str::parse).transpose()?,
            state,
            started_at: started_at.parse()?,
            stopped_at: stopped_at.as_deref().map(str::parse).transpose()?,
            elapsed: Duration::from_nanos(
                elapsed_ns
                    .try_into()
                    .context("negative saved elapsed time")?,
            ),
        }))
    }

    fn save(&self, db: &Connection) -> Result<()> {
        let state = match self.state {
            TimerState::Idle => "stopped",
            TimerState::Running => "running",
            TimerState::Paused => "paused",
            TimerState::NeedsReview => "needs_review",
        };
        let elapsed_ns =
            i64::try_from(self.elapsed.as_nanos()).context("focus elapsed time overflow")?;
        let changed = db.execute(
            "INSERT INTO focus_sessions (id, work_unit_id, state, started_at, stopped_at, elapsed_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET state = excluded.state,
                 stopped_at = excluded.stopped_at, elapsed_ns = excluded.elapsed_ns",
            params![self.id.to_string(), self.work_unit_id.map(|id| id.to_string()), state,
                self.started_at.to_rfc3339(), self.stopped_at.map(|time| time.to_rfc3339()), elapsed_ns],
        )?;
        ensure!(changed == 1, "saving focus session affected {changed} rows");
        Ok(())
    }
}

#[cfg(test)]
#[path = "timer_tests.rs"]
mod tests;
