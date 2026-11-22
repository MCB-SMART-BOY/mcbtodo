use anyhow::{Result, ensure};
use rusqlite::Connection;

pub(crate) const FOCUS: u32 = 1;
pub(crate) const WORKFLOW: u32 = 1;

pub(crate) fn check_focus(db: &Connection, allow_uninitialized: bool) -> Result<u32> {
    let version = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        version == FOCUS || (allow_uninitialized && version == 0),
        "unsupported focus database schema version {version}"
    );
    Ok(version)
}

pub(crate) fn check_workflow(db: &Connection) -> Result<u32> {
    let version = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        version == WORKFLOW || version == 0,
        "unsupported workflow database schema version {version}"
    );
    Ok(version)
}
