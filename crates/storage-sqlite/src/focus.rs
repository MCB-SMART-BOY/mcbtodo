use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use mcbtodo_ports::focus::{FocusCheckpoint, FocusEntry, FocusSessionStore, TimerState};
use rusqlite::{Connection, OpenFlags, Row, params};

use crate::schema_version;

const SESSION_COLUMNS: &str = "id, work_unit_id, state, started_at, stopped_at, elapsed_ns";

pub struct FocusStore(Connection);

impl FocusStore {
    /// Open the focus database and atomically mark interrupted sessions for review.
    pub fn open(path: &Path) -> Result<(Self, Option<FocusCheckpoint>)> {
        mcbtodo_platform::private_file::open_private_file(path)?;
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.pragma_update(None, "synchronous", "FULL")?;
        let tx = db.transaction()?;
        if schema_version::check_focus(&tx, true)? == 0 {
            tx.execute_batch(
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
                    WHERE state <> 'stopped';",
            )?;
            tx.pragma_update(None, "user_version", schema_version::FOCUS)?;
        }
        // The elapsed value of a running session is already its durable checkpoint.
        tx.execute(
            "UPDATE focus_sessions SET state = 'needs_review' WHERE state = 'running'",
            [],
        )?;
        let session = load_latest(&tx)?;
        tx.commit()?;
        Ok((Self(db), session))
    }
}

impl FocusSessionStore for FocusStore {
    fn save(&self, session: &FocusCheckpoint) -> Result<()> {
        save_checkpoint(&self.0, session)
    }

    fn for_each_session_on(
        &self,
        date: NaiveDate,
        timezone: Tz,
        visit: &mut dyn FnMut(FocusEntry),
    ) -> Result<()> {
        let mut query = self
            .0
            .prepare("SELECT work_unit_id, started_at, elapsed_ns, state FROM focus_sessions")?;
        let mut sessions = query.query([])?;
        while let Some(row) = sessions.next()? {
            let started_at: DateTime<Utc> = row.get_ref(1)?.as_str()?.parse()?;
            if started_at.with_timezone(&timezone).date_naive() != date {
                continue;
            }
            visit(FocusEntry {
                work_unit_id: row
                    .get_ref(0)?
                    .as_str_or_null()?
                    .map(str::parse)
                    .transpose()?,
                elapsed: Duration::from_nanos(row.get::<_, i64>(2)?.try_into()?),
                state: decode_state(row.get_ref(3)?.as_str()?)?,
            });
        }
        Ok(())
    }
}

fn decode_state(state: &str) -> Result<TimerState> {
    Ok(match state {
        "stopped" => TimerState::Idle,
        "running" => TimerState::Running,
        "paused" => TimerState::Paused,
        "needs_review" => TimerState::NeedsReview,
        _ => bail!("unexpected saved focus state {state:?}"),
    })
}

fn load_latest(db: &Connection) -> Result<Option<FocusCheckpoint>> {
    let mut query = db.prepare(&format!(
        "SELECT {SESSION_COLUMNS} FROM focus_sessions ORDER BY rowid DESC LIMIT 1"
    ))?;
    let mut rows = query.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let session = decode_checkpoint(row).context("decoding latest focus session")?;
    ensure!(
        session.state != TimerState::Running,
        "focus restart recovery left a running session"
    );
    Ok(Some(session))
}

/// Verify historical sessions as well as the latest checkpoint. A running
/// checkpoint remains valid here, before startup recovery marks it for review.
pub(crate) fn validate_sessions(db: &Connection) -> Result<()> {
    let mut query = db
        .prepare(&format!("SELECT {SESSION_COLUMNS} FROM focus_sessions"))
        .context("reading focus session columns")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        decode_checkpoint(row).context("decoding saved focus session")?;
    }
    Ok(())
}

fn decode_checkpoint(row: &Row<'_>) -> Result<FocusCheckpoint> {
    let state = decode_state(row.get_ref(2)?.as_str()?)?;
    let stopped_at = row
        .get_ref(4)?
        .as_str_or_null()?
        .map(str::parse)
        .transpose()
        .context("invalid stopped_at timestamp")?;
    ensure!(
        (state == TimerState::Idle) == stopped_at.is_some(),
        "stopped_at must be present exactly when the session is stopped"
    );
    Ok(FocusCheckpoint {
        id: row.get_ref(0)?.as_str()?.parse().context("invalid id")?,
        work_unit_id: row
            .get_ref(1)?
            .as_str_or_null()?
            .map(str::parse)
            .transpose()
            .context("invalid work_unit_id")?,
        state,
        started_at: row
            .get_ref(3)?
            .as_str()?
            .parse()
            .context("invalid started_at timestamp")?,
        stopped_at,
        elapsed: Duration::from_nanos(
            row.get::<_, i64>(5)?
                .try_into()
                .context("negative saved elapsed time")?,
        ),
    })
}

fn save_checkpoint(db: &Connection, session: &FocusCheckpoint) -> Result<()> {
    let state = match session.state {
        TimerState::Idle => "stopped",
        TimerState::Running => "running",
        TimerState::Paused => "paused",
        TimerState::NeedsReview => "needs_review",
    };
    let elapsed_ns =
        i64::try_from(session.elapsed.as_nanos()).context("focus elapsed time overflow")?;
    let changed = db.execute(
        "INSERT INTO focus_sessions (id, work_unit_id, state, started_at, stopped_at, elapsed_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET state = excluded.state,
                 stopped_at = excluded.stopped_at, elapsed_ns = excluded.elapsed_ns",
        params![
            session.id.to_string(),
            session.work_unit_id.map(|id| id.to_string()),
            state,
            session.started_at.to_rfc3339(),
            session.stopped_at.map(|time| time.to_rfc3339()),
            elapsed_ns
        ],
    )?;
    ensure!(changed == 1, "saving focus session affected {changed} rows");
    Ok(())
}
