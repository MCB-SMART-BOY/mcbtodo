use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use mcbtodo_domain::WorkUnitCompleted;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

// Keep the rollback-mode database beside the Markdown so backup captures both
// under the Vault writer lock, including a hot rollback journal after a crash.
const DATABASE: &str = ".mcbtodo-events.sqlite3";
const APPLICATION_ID: i32 = 0x4d434245; // MCBE

pub(crate) struct CompletionJournal(Connection);

pub(crate) struct PendingCompletion {
    pub event: WorkUnitCompleted,
    pub before: String,
    pub after: String,
}

impl CompletionJournal {
    /// The caller holds the Vault writer lock for the lifetime of the connection.
    pub fn open(root: &Path, create: bool) -> Result<Option<Self>> {
        let path = root.join(DATABASE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => check_database_file(&path, &metadata)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                options.open(&path)?;
            }
            Err(err) => return Err(err.into()),
        }
        check_sidecars(root)?;
        let mut db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| format!("opening {}", path.display()))?;
        db.pragma_update(None, "trusted_schema", false)?;
        db.pragma_update(None, "synchronous", "EXTRA")?;
        let mode: String = db.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        ensure!(
            mode == "delete",
            "completion store must use DELETE journal mode"
        );
        match (
            db.pragma_query_value(None, "application_id", |row| row.get::<_, i32>(0))?,
            db.pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))?,
        ) {
            (APPLICATION_ID, 1) => (),
            (0, 0) => {
                let tx = db.transaction()?;
                let objects: i64 =
                    tx.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
                ensure!(objects == 0, "completion store has an unexpected schema");
                tx.execute_batch(
                    "CREATE TABLE work_unit_completions (
                        sequence INTEGER PRIMARY KEY,
                        event TEXT NOT NULL,
                        before_digest TEXT NOT NULL,
                        after_digest TEXT NOT NULL,
                        pending INTEGER NOT NULL CHECK (pending IN (0, 1))
                    ) STRICT;
                    CREATE UNIQUE INDEX one_pending_completion
                        ON work_unit_completions(pending) WHERE pending = 1;",
                )?;
                tx.pragma_update(None, "application_id", APPLICATION_ID)?;
                tx.pragma_update(None, "user_version", 1)?;
                tx.commit()?;
            }
            _ => anyhow::bail!("unsupported completion store format"),
        }
        Ok(Some(Self(db)))
    }

    pub fn validate_snapshot(root: &Path) -> Result<Option<PendingCompletion>> {
        ensure!(fs::metadata(root)?.is_dir(), "expected a Vault directory");
        let database = root.join(DATABASE);
        let rollback = check_sidecars(root)?;
        if !database_file_exists(&database)? {
            ensure!(
                rollback.is_none(),
                "rollback journal has no completion database"
            );
            return Ok(None);
        }

        // Opening SQLite can recover a hot journal or initialize an empty store.
        // Even a file staged with read-only permissions can be recovered here.
        let copy = tempfile::tempdir()?;
        copy_database_file(&database, copy.path())?;
        if let Some(rollback) = rollback {
            let copied = copy_database_file(&rollback, copy.path())?;
            reject_super_journal_footer(&copied)?;
        }
        let journal =
            Self::open(copy.path(), false)?.context("missing copied completion database")?;
        let integrity: String = journal
            .0
            .pragma_query_value(None, "integrity_check", |row| row.get(0))?;
        ensure!(
            integrity == "ok",
            "completion database integrity check failed: {integrity}"
        );
        let mut query = journal.0.prepare(
            "SELECT event, before_digest, after_digest, pending
             FROM work_unit_completions ORDER BY sequence",
        )?;
        let mut rows = query.query([])?;
        let mut pending = None;
        while let Some(row) = rows.next()? {
            let event = serde_json::from_str(row.get_ref(0)?.as_str()?)
                .context("decoding WorkUnit completion event")?;
            let before: String = row.get(1)?;
            let after: String = row.get(2)?;
            ensure!(
                is_revision_digest(&before) && is_revision_digest(&after) && before != after,
                "invalid completion revision digests"
            );
            match row.get::<_, i64>(3)? {
                0 => (),
                1 if pending.is_none() => {
                    pending = Some(PendingCompletion {
                        event,
                        before,
                        after,
                    })
                }
                1 => anyhow::bail!("multiple pending completions"),
                _ => anyhow::bail!("invalid completion publication state"),
            }
        }
        Ok(pending)
    }

    pub fn prepare(&self, event: &WorkUnitCompleted, before: &str, after: &str) -> Result<()> {
        self.0.execute(
            "INSERT INTO work_unit_completions(event, before_digest, after_digest, pending)
             VALUES (?1, ?2, ?3, 1)",
            params![serde_json::to_string(event)?, before, after],
        )?;
        Ok(())
    }

    pub fn pending(&self) -> Result<Option<PendingCompletion>> {
        self.0
            .query_row(
                "SELECT event, before_digest, after_digest FROM work_unit_completions WHERE pending = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .map(|(event, before, after)| {
                Ok(PendingCompletion {
                    event: serde_json::from_str(&event)?,
                    before,
                    after,
                })
            })
            .transpose()
    }

    pub fn commit(&self) -> Result<()> {
        let changed = self.0.execute(
            "UPDATE work_unit_completions SET pending = 0 WHERE pending = 1",
            [],
        )?;
        ensure!(changed == 1, "missing prepared completion");
        Ok(())
    }

    pub fn abort(&self) -> Result<()> {
        self.0
            .execute("DELETE FROM work_unit_completions WHERE pending = 1", [])?;
        Ok(())
    }

    pub fn watermark(&self) -> Result<i64> {
        Ok(self.0.query_row(
            "SELECT coalesce(max(sequence), 0) FROM work_unit_completions WHERE pending = 0",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn page(&self, after: i64) -> Result<Vec<(i64, WorkUnitCompleted)>> {
        let mut query = self.0.prepare(
            "SELECT sequence, event FROM work_unit_completions
             WHERE sequence > ?1 AND pending = 0 ORDER BY sequence LIMIT 64",
        )?;
        let mut rows = query.query([after])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            events.push((
                row.get(0)?,
                serde_json::from_str(row.get_ref(1)?.as_str()?)?,
            ));
        }
        Ok(events)
    }

    pub fn events(&self) -> Result<Vec<WorkUnitCompleted>> {
        let mut events = self.0.prepare(
            "SELECT event FROM work_unit_completions WHERE pending = 0 ORDER BY sequence",
        )?;
        let mut rows = events.query([])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            result.push(serde_json::from_str(row.get_ref(0)?.as_str()?)?);
        }
        Ok(result)
    }
}

fn check_sidecars(root: &Path) -> Result<Option<PathBuf>> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = root.join(format!("{DATABASE}{suffix}"));
        ensure!(
            !database_file_exists(&sidecar)?,
            "unexpected SQLite sidecar at {}",
            sidecar.display()
        );
    }
    let rollback = root.join(format!("{DATABASE}-journal"));
    Ok(database_file_exists(&rollback)?.then_some(rollback))
}

fn database_file_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            check_database_file(path, &metadata)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn copy_database_file(source: &Path, directory: &Path) -> Result<PathBuf> {
    let destination = directory.join(source.file_name().context("missing database filename")?);
    io::copy(&mut File::open(source)?, &mut File::create(&destination)?)?;
    Ok(destination)
}

fn reject_super_journal_footer(path: &Path) -> Result<()> {
    const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    let mut journal = File::open(path)?;
    // SQLite only looks for a super-journal name when this magic ends a
    // footer of at least 16 bytes. Reject the framing without reading any name.
    // Vault transactions use a single database, so they need no super-journal.
    if journal.metadata()?.len() >= 16 {
        journal.seek(SeekFrom::End(-8))?;
        let mut magic = [0; 8];
        journal.read_exact(&mut magic)?;
        ensure!(
            magic != JOURNAL_MAGIC,
            "unexpected SQLite super-journal footer"
        );
    }
    Ok(())
}

fn is_revision_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn check_database_file(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    ensure!(
        metadata.is_file(),
        "expected a regular file at {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.nlink() == 1,
            "refusing a hard-linked database file at {}",
            path.display()
        );
    }
    Ok(())
}
