//! Standalone snapshots of the workspace's focus and workflow databases.

use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension,
    backup::{Backup, StepResult},
};

use crate::{focus, run_store, schema_version};

/// Identifies the business store expected in a workspace snapshot.
#[derive(Clone, Copy)]
pub enum SnapshotKind {
    Focus,
    Workflow,
}

/// Copy a live database, including committed WAL data, into a standalone file.
///
/// The backup host owns the workspace lock and filesystem checks. It must
/// pre-create an empty destination file in a private staging directory.
pub fn capture(source: &Path, destination: &Path) -> Result<()> {
    let from = open(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut to = open(destination, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    ensure!(
        Backup::new(&from, &mut to)?.step(-1)? == StepResult::Done,
        "SQLite snapshot could not acquire its locks"
    );
    to.pragma_update(None, "journal_mode", "DELETE")?;
    to.close().map_err(|(_, error)| error)?;
    File::open(destination)?.sync_all()?;
    Ok(())
}

impl SnapshotKind {
    /// Inspect a private staged file whose directory has been checked for
    /// unexpected entries, including SQLite sidecars.
    pub fn validate(self, path: &Path) -> Result<()> {
        ensure!(
            path.is_file(),
            "expected database file at {}",
            path.display()
        );
        // SQLite may create WAL and shared-memory files even on read-only open.
        // SQLite's file format puts the write and read versions at offsets
        // 18 and 19. Both must be 1 for a standalone rollback-mode snapshot.
        let mut header = [0; 20];
        File::open(path)?.read_exact(&mut header)?;
        ensure!(
            &header[..16] == b"SQLite format 3\0",
            "invalid SQLite snapshot header"
        );
        ensure!(header[18..] == [1, 1], "SQLite snapshot is not standalone");

        let db = open(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let result: String = db.pragma_query_value(None, "integrity_check", |row| row.get(0))?;
        ensure!(result == "ok", "SQLite integrity check failed: {result}");
        let broken_reference = db
            .pragma_query_value(None, "foreign_key_check", |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(2)?))
            })
            .optional()?;
        if let Some((child, parent)) = broken_reference {
            anyhow::bail!("SQLite foreign key check failed in {child}: missing {parent} record");
        }
        let table = match self {
            Self::Focus => "focus_sessions",
            Self::Workflow => "workflow_runs",
        };
        let matching: bool = db.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )?;
        ensure!(matching, "SQLite snapshot is missing the {table} table");
        match self {
            Self::Focus => {
                schema_version::check_focus(&db, false)?;
                focus::validate_sessions(&db)?;
            }
            Self::Workflow => {
                schema_version::check_workflow(&db)?;
                run_store::validate_history(&db)?;
                run_store::validate_schedules(&db)?;
            }
        }
        Ok(())
    }
}

fn open(path: &Path, access: OpenFlags) -> Result<Connection> {
    let db = Connection::open_with_flags(path, access | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .with_context(|| format!("opening SQLite database {}", path.display()))?;
    db.pragma_update(None, "trusted_schema", false)?;
    Ok(db)
}
