use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use anyhow::Result;
use mcbtodo_domain::Id;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_platform::agent_policy::MAX_POLICY_BYTES;
use mcbtodo_ports::workflow::{
    WorkflowArtifact, WorkflowProgressStore, WorkflowRunCatalog, WorkflowScheduleStore,
};
use mcbtodo_storage_sqlite::{focus::FocusStore, run_store::RunStore};
use mcbtodo_vault::{CompleteWorkUnit, Vault, VaultStore};
use mcbtodo_workflow::{
    RevisionStore, ScriptSources, WorkflowDraft,
    schedule::{Missed, Overlap, ScheduleSummary, Trigger},
};
use rusqlite::Connection;
use tempfile::tempdir;
use uuid::Uuid;

use super::{
    Destination, DirectoryIdentity, MANIFEST, PAYLOAD, backup, capture, inventory, restore,
    validate,
};
use crate::proposal::{self, Operation, Queue};

fn initialize(root: &Path) -> Result<WorkspacePaths> {
    fs::create_dir(root)?;
    let paths = WorkspacePaths::from_home(root.to_owned());
    paths.initialize()?;
    Ok(paths)
}

#[test]
fn round_trip_captures_committed_wal_data_and_published_workflows() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    fs::write(paths.config_path(), "timezone = 'Asia/Shanghai'\n")?;
    fs::create_dir(paths.vault_root().join("inbox"))?;
    let inbox_path = paths
        .vault_root()
        .join("inbox/90000000-0000-4000-8000-000000000001.md");
    let inbox_source = "---\nschema: mcbtodo.inbox-entry/v1\nid: 90000000-0000-4000-8000-000000000001\ncaptured_at: '2026-11-20T12:00:00Z'\n---\n\n# Backup design\n\nReview hot-journal recovery.\n";
    fs::write(&inbox_path, inbox_source)?;
    let draft = paths.vault_root().join("graph.yaml");
    fs::write(
        &draft,
        include_str!("../../../examples/workflow/graph.yaml"),
    )?;
    fs::create_dir(paths.vault_root().join("scripts"))?;
    fs::write(
        paths.vault_root().join("scripts/check.nu"),
        include_str!("../../../examples/workflow/scripts/check.nu"),
    )?;
    let executable = b"\x7fELF snapshot test bytes";
    fs::write(paths.vault_root().join("page-table-check"), executable)?;
    let revision = RevisionStore::new(paths.state_dir()).publish(&draft)?;
    fs::create_dir(paths.state_dir().join("scratch"))?;
    let _socket = std::os::unix::net::UnixListener::bind(paths.socket_path())?;
    fs::write(paths.prepare_cache()?.join("cache.txt"), "runtime cache")?;

    let database = paths.state_dir().join("focus.sqlite3");
    FocusStore::open(&database)?;
    let db = Connection::open(&database)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")?;
    db.execute(
        "INSERT INTO focus_sessions (id, state, started_at, elapsed_ns)
         VALUES (?1, 'running', '2027-01-15T08:00:00Z', 42)",
        [uuid::Uuid::from_u128(1).to_string()],
    )?;
    assert!(database.with_extension("sqlite3-wal").is_file());
    let workflow = paths.state_dir().join("workflow.sqlite3");
    let store = RunStore::open(&workflow)?;
    let run = store.enqueue_if_absent(Uuid::from_u128(1), revision.hash(), None)?;
    store.append_host_step(
        &run,
        r#"{"node":"report","status":"succeeded","outcome":null,"inputs":{},"outputs":{},"reason":"完成"}"#,
        Some(&WorkflowArtifact {
            bytes: vec![1, 2, 3],
            media_type: "application/octet-stream",
        }),
    )?;
    drop(store);
    Connection::open(workflow)?.pragma_update(None, "user_version", 0)?;

    let archive = root.path().join("archive");
    let destination = root.path().join("restored");
    backup(&paths, &archive)?;
    let captured = WorkspacePaths::from_home(archive.join(PAYLOAD));
    fs::create_dir(&destination)?;
    restore(&archive, &destination)?;

    let restored = WorkspacePaths::from_home(destination);
    assert_eq!(restored.load_config()?.timezone.name(), "Asia/Shanghai");
    assert_eq!(
        fs::read(
            restored
                .vault_root()
                .join("inbox")
                .join(inbox_path.file_name().unwrap())
        )?,
        inbox_source.as_bytes()
    );
    assert_eq!(
        fs::read(restored.vault_root().join("graph.yaml"))?,
        fs::read(draft)?
    );
    let restored_revision = RevisionStore::new(restored.state_dir()).load(revision.hash())?;
    assert_eq!(
        restored_revision.script_sources()["page-table-check"],
        executable
    );
    let restored_db = restored.state_dir().join("focus.sqlite3");
    assert_eq!(
        fs::read(&restored_db)?,
        fs::read(captured.focus_database_path())?
    );
    assert_eq!(
        fs::metadata(&restored_db)?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        Connection::open(restored_db)?.query_row(
            "SELECT elapsed_ns FROM focus_sessions WHERE state = 'running'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        42
    );
    let restored_workflow = restored.state_dir().join("workflow.sqlite3");
    assert_eq!(
        fs::read(&restored_workflow)?,
        fs::read(captured.workflow_database_path())?
    );
    assert_eq!(
        fs::metadata(&restored_workflow)?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        Connection::open(&restored_workflow)?.query_row(
            "SELECT data FROM workflow_artifacts",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )?,
        [1, 2, 3]
    );
    let reopened = RunStore::open(&restored_workflow)?;
    assert_eq!(reopened.get(run.id)?.unwrap().step_count, 1);
    for path in [
        restored.socket_path(),
        restored.cache_dir(),
        restored.vault_root().join(".mcbtodo-vault.lock"),
        restored.state_dir().join("scratch"),
        restored.state_dir().join("mcbtodo-daemon.lock"),
        restored.state_dir().join("focus.sqlite3-wal"),
    ] {
        assert!(!path.exists(), "{}", path.display());
    }
    Ok(())
}

#[test]
fn agent_policy_round_trips_without_live_socket() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let socket = paths.vault_root().join("adapter.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket)?;
    let policy = format!(
        "[providers]\nunix_socket = 'vault/adapter.sock'\n[[allow]]\ncapability = 'test'\nrevision = '{}'\n",
        "a".repeat(64)
    );
    fs::write(paths.agent_policy_path(), &policy)?;
    fs::set_permissions(paths.agent_policy_path(), fs::Permissions::from_mode(0o644))?;

    let archive = root.path().join("archive");
    let destination = root.path().join("restored");
    backup(&paths, &archive)?;
    restore(&archive, &destination)?;

    for home in [archive.join("data"), destination] {
        let paths = WorkspacePaths::from_home(home);
        let path = paths.agent_policy_path();
        assert_eq!(fs::read_to_string(&path)?, policy);
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
        assert!(!paths.vault_root().join("adapter.sock").exists());
    }
    assert!(socket.exists());
    Ok(())
}

#[test]
fn backup_refuses_to_make_an_untrusted_agent_policy_active() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let policy = paths.agent_policy_path();
    fs::write(&policy, "[providers]\nunix_socket = 'state/agent.sock'\n")?;
    fs::set_permissions(&policy, fs::Permissions::from_mode(0o660))?;

    let archive = root.path().join("archive");
    let error = backup(&paths, &archive).unwrap_err();
    assert!(format!("{error:#}").contains("protected from other writers"));
    assert!(!archive.exists());

    fs::remove_file(&policy)?;
    symlink(paths.config_path(), policy)?;
    let error = backup(&paths, &archive).unwrap_err();
    assert!(format!("{error:#}").contains("opening agent policy"));
    assert!(!archive.exists());
    Ok(())
}

#[test]
fn backup_and_restore_reject_invalid_or_oversized_agent_policy() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let policy_path = paths.agent_policy_path();
    let valid = b"[providers]\nunix_socket = 'state/agent.sock'\n";
    fs::write(&policy_path, valid)?;
    fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o600))?;
    let archive = root.path().join("archive");
    backup(&paths, &archive)?;

    let payload = archive.join(PAYLOAD);
    let bundled = WorkspacePaths::from_home(payload.clone());
    let failed_backup = root.path().join("failed-backup");
    let failed_restore = root.path().join("failed-restore");
    fs::create_dir(&failed_restore)?;
    let mut oversized = valid.to_vec();
    oversized.resize(MAX_POLICY_BYTES + 1, b' ');
    for (bytes, message) in [
        (
            b"[providers]\nunix_socket = '/tmp/agent.sock'\n".as_slice(),
            "Agent socket must be a relative path",
        ),
        (&oversized, "agents.toml exceeds the size limit"),
    ] {
        fs::write(&policy_path, bytes)?;
        let error = backup(&paths, &failed_backup).unwrap_err();
        assert!(format!("{error:#}").contains(message), "{error:#}");
        assert!(!failed_backup.exists());

        fs::write(bundled.agent_policy_path(), bytes)?;
        fs::write(
            archive.join(MANIFEST),
            serde_json::to_vec(&inventory(&payload)?)?,
        )?;
        let error = restore(&archive, &failed_restore).unwrap_err();
        assert!(format!("{error:#}").contains(message), "{error:#}");
        assert!(fs::read_dir(&failed_restore)?.next().is_none());
    }
    Ok(())
}

#[test]
fn proposal_queue_restores_across_homes_with_its_idempotency_key() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let project = paths.vault_root().join("projects/rust-os");
    fs::create_dir_all(&project)?;
    fs::write(
        project.join("project.md"),
        include_str!("../../../tests/fixtures/vault/projects/rust-os/project.md"),
    )?;
    let request_id = Id::new_v4();
    let timezone = paths.load_config()?.timezone;
    let operation = || Operation::CaptureTask {
        project_id: Id::from_u128(0x10000000000040008000000000000001),
        title: "Portable request".to_owned(),
        body: String::new(),
    };
    let id = proposal::enqueue(
        &paths.vault_root(),
        timezone,
        &paths,
        operation(),
        Some(request_id),
    )?;
    assert_eq!(id, request_id);
    let mut sources = ScriptSources::new();
    sources.insert(
        "scripts/review.nu".to_owned(),
        b"print 'portable workflow'\n".to_vec(),
    );
    let draft = WorkflowDraft::new(
        r#"schema: mcbtodo.workflow/v1
id: portable-workflow
name: Portable workflow
entry: check
nodes:
  check:
    kind: script
    runner: nushell
    source: scripts/review.nu
    timeout_seconds: 30
    capabilities: []
    outputs: {summary: string}
  report: {kind: action, action: report.create}
edges:
  - {from: check, to: report}
bindings:
  - {from: {node: check, port: summary}, to: {node: report, port: summary}}
"#
        .to_owned(),
        sources,
    )?;
    let workflow_request_id = Id::new_v4();
    let workflow_id = proposal::enqueue(
        &paths.vault_root(),
        timezone,
        &paths,
        Operation::PublishWorkflow { draft },
        Some(workflow_request_id),
    )?;
    assert_eq!(workflow_id, workflow_request_id);
    let queue = paths.proposals_dir();
    fs::write(queue.join(".tmp-interrupted"), b"partial proposal")?;
    let filename = format!("{id}.json");
    let expected = fs::read(queue.join(&filename))?;
    let workflow_filename = format!("{workflow_id}.json");
    let expected_workflow = fs::read(queue.join(&workflow_filename))?;

    let archive = root.path().join("archive");
    let destination = root.path().join("restored");
    backup(&paths, &archive)?;
    restore(&archive, &destination)?;

    let restored = WorkspacePaths::from_home(destination);
    let queue = restored.proposals_dir();
    assert_eq!(fs::read_dir(&queue)?.count(), 2);
    assert_eq!(fs::read(queue.join(&filename))?, expected);
    assert_eq!(fs::read(queue.join(&workflow_filename))?, expected_workflow);
    assert_eq!(
        proposal::enqueue(
            &restored.vault_root(),
            timezone,
            &restored,
            operation(),
            Some(request_id),
        )?,
        request_id
    );
    assert_eq!(fs::read(queue.join(filename))?, expected);
    let restored_queue = proposal::Queue::open(&restored.vault_root(), timezone, &restored)?;
    let restored_workflow = restored_queue.review(workflow_id)?;
    assert_eq!(restored_workflow.status, proposal::Status::Pending);
    let Operation::PublishWorkflow { draft } = restored_workflow.operation else {
        anyhow::bail!("restored proposal did not retain its workflow snapshot");
    };
    assert_eq!(
        draft.script_sources()["scripts/review.nu"],
        b"print 'portable workflow'\n"
    );
    assert!(RevisionStore::new(restored.state_dir()).list()?.is_empty());
    Ok(())
}

#[test]
fn symlinked_default_vault_is_backed_up_from_its_canonical_directory() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let vault = paths.vault_root();
    let actual_vault = root.path().join("actual-vault");
    fs::rename(&vault, &actual_vault)?;
    let project = actual_vault.join("projects/rust-os/project.md");
    fs::create_dir_all(project.parent().unwrap())?;
    fs::write(
        &project,
        include_str!("../../../tests/fixtures/vault/projects/rust-os/project.md"),
    )?;
    fs::write(actual_vault.join("note.md"), "default Vault data")?;
    symlink(&actual_vault, &vault)?;
    let proposal_id = proposal::submit(
        &vault,
        chrono_tz::UTC,
        &paths,
        Operation::CaptureTask {
            project_id: Id::parse_str("10000000-0000-4000-8000-000000000001")?,
            title: "Portable proposal".to_owned(),
            body: String::new(),
        },
        None,
    )?
    .proposal_id;

    let archive = root.path().join("archive");
    backup(&paths, &archive)?;

    let captured = WorkspacePaths::from_home(archive.join(PAYLOAD));
    assert_eq!(
        fs::read_to_string(captured.vault_root().join("note.md"))?,
        "default Vault data"
    );
    assert!(!captured.vault_root().is_symlink());
    let restored = root.path().join("restored");
    restore(&archive, &restored)?;
    let restored_paths = WorkspacePaths::from_home(restored);
    let restored_queue = Queue::open(
        &restored_paths.vault_root(),
        chrono_tz::UTC,
        &restored_paths,
    )?;
    let restored_proposal = restored_queue
        .list()?
        .into_iter()
        .find(|proposal| proposal.id == proposal_id)
        .expect("default proposal was included in the backup");
    assert_eq!(restored_proposal.target_title, "Rust OS");
    Ok(())
}

#[test]
fn capture_rejects_a_default_vault_replaced_after_identity_resolution() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let canonical_vault = paths.vault_root().canonicalize()?;
    let expected = DirectoryIdentity::of(&fs::metadata(paths.vault_root())?);
    let displaced = root.path().join("displaced-vault");
    fs::rename(paths.vault_root(), &displaced)?;
    fs::create_dir(paths.vault_root())?;
    let destination = WorkspacePaths::from_home(root.path().join("staged"));

    let error = capture(&paths, &destination, &canonical_vault, expected).unwrap_err();
    assert!(format!("{error:#}").contains("default Vault directory changed"));
    Ok(())
}

#[test]
fn destination_must_be_absent_or_an_empty_real_directory() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let archive = root.path().join("archive");
    fs::create_dir(&archive)?;
    backup(&paths, &archive)?;

    let occupied = root.path().join("occupied");
    fs::create_dir(&occupied)?;
    fs::write(occupied.join("keep"), "keep")?;
    assert!(backup(&paths, &occupied).is_err());
    assert!(restore(&archive, &occupied).is_err());
    assert_eq!(fs::read_to_string(occupied.join("keep"))?, "keep");

    let empty = root.path().join("empty");
    let link = root.path().join("link");
    fs::create_dir(&empty)?;
    symlink(&empty, &link)?;
    assert!(backup(&paths, &link).is_err());
    assert!(restore(&archive, &link).is_err());
    assert!(link.is_symlink());
    assert!(fs::read_dir(empty)?.next().is_none());
    Ok(())
}

#[test]
fn a_held_daemon_lock_prevents_backup() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let lock = fs::File::create_new(paths.state_dir().join("mcbtodo-daemon.lock"))?;
    lock.set_permissions(fs::Permissions::from_mode(0o600))?;
    lock.lock()?;
    let archive = root.path().join("archive");
    let error = backup(&paths, &archive).unwrap_err();
    assert!(format!("{error:#}").contains("daemon state lock"));
    assert!(!archive.exists());
    Ok(())
}

#[test]
fn a_held_vault_write_lock_prevents_backup() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let lock = fs::File::create_new(paths.vault_root().join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    let archive = root.path().join("archive");
    let error = backup(&paths, &archive).unwrap_err();
    assert!(format!("{error:#}").contains("Vault write lock is held"));
    assert!(!archive.exists());
    Ok(())
}

#[test]
fn tampered_bundle_and_symlinked_vault_fail_before_install() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let note = paths.vault_root().join("note.txt");
    symlink(paths.config_path(), &note)?;
    let archive = root.path().join("archive");
    assert!(backup(&paths, &archive).is_err());
    assert!(!archive.exists());
    fs::remove_file(&note)?;
    fs::write(note, "before")?;
    backup(&paths, &archive)?;

    let payload = WorkspacePaths::from_home(archive.join("data"));
    fs::write(payload.vault_root().join("note.txt"), "after")?;
    let destination = root.path().join("restored");
    fs::create_dir(&destination)?;
    let error = restore(&archive, &destination).unwrap_err();
    assert!(format!("{error:#}").contains("manifest"));
    assert!(fs::read_dir(destination)?.next().is_none());
    Ok(())
}

#[test]
fn publish_refuses_destinations_created_or_populated_during_staging() -> Result<()> {
    let root = tempdir()?;
    let staged = root.path().join("staged");
    fs::create_dir(&staged)?;
    fs::write(staged.join("data"), "finished")?;

    let target = root.path().join("target");
    let absent = Destination::check(&target)?;
    fs::create_dir(&target)?;
    assert!(absent.publish(&staged).is_err());
    assert!(fs::read_dir(&target)?.next().is_none());

    let empty = Destination::check(&target)?;
    fs::write(target.join("keep"), "keep")?;
    assert!(empty.publish(&staged).is_err());
    assert_eq!(fs::read_to_string(target.join("keep"))?, "keep");
    assert_eq!(fs::read_to_string(staged.join("data"))?, "finished");
    Ok(())
}

#[test]
fn corrupt_sqlite_does_not_publish_or_replace() -> Result<()> {
    use std::io::Write;

    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let db = paths.state_dir().join("workflow.sqlite3");
    let mut file = fs::File::create_new(&db)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(b"corrupt")?;
    drop(file);
    let archive = root.path().join("archive");
    fs::create_dir(&archive)?;
    assert!(backup(&paths, &archive).is_err());
    assert!(fs::read_dir(archive)?.next().is_none());
    assert_eq!(fs::read(db)?, b"corrupt");
    Ok(())
}

#[test]
fn wal_mode_is_rejected_without_creating_staged_sidecars() -> Result<()> {
    let root = tempdir()?;
    let staged = initialize(&root.path().join("staged"))?;
    let path = staged.focus_database_path();
    fs::File::create_new(&path)?.set_permissions(fs::Permissions::from_mode(0o600))?;
    Connection::open(&path)?
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE focus_sessions (elapsed INTEGER);")?;

    let original = fs::read(&path)?;
    assert!(!path.with_extension("sqlite3-wal").exists());
    assert!(!path.with_extension("sqlite3-shm").exists());
    let error = validate(&staged).unwrap_err();
    assert!(format!("{error:#}").contains("SQLite snapshot is not standalone"));
    assert_eq!(fs::read(&path)?, original);
    assert!(!path.with_extension("sqlite3-wal").exists());
    assert!(!path.with_extension("sqlite3-shm").exists());
    Ok(())
}

#[test]
fn restore_rejects_wrong_store_even_with_matching_manifest() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let archive = root.path().join("archive");
    backup(&paths, &archive)?;
    let payload = archive.join(PAYLOAD);
    let captured = WorkspacePaths::from_home(payload.clone());
    let path = captured.workflow_database_path();
    Connection::open(&path)?.execute_batch("CREATE TABLE focus_sessions (elapsed INTEGER);")?;
    serde_json::to_writer(
        fs::File::create(archive.join(MANIFEST))?,
        &inventory(&payload)?,
    )?;

    let destination = root.path().join("restored");
    let error = restore(&archive, &destination).unwrap_err();
    assert!(format!("{error:#}").contains("SQLite snapshot is missing the workflow_runs table"));
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn backup_and_restore_reject_unsupported_database_versions() -> Result<()> {
    for (path, table, name) in [
        (
            WorkspacePaths::focus_database_path as fn(&WorkspacePaths) -> _,
            "focus_sessions",
            "focus",
        ),
        (
            WorkspacePaths::workflow_database_path,
            "workflow_runs",
            "workflow",
        ),
    ] {
        let root = tempdir()?;
        let paths = initialize(&root.path().join("live"))?;
        let archive = root.path().join("archive");
        backup(&paths, &archive)?;
        let payload = archive.join(PAYLOAD);
        let captured = WorkspacePaths::from_home(payload.clone());
        for database in [path(&paths), path(&captured)] {
            fs::File::create_new(&database)?.set_permissions(fs::Permissions::from_mode(0o600))?;
            Connection::open(&database)?.execute_batch(&format!(
                "CREATE TABLE {table} (id TEXT); PRAGMA user_version = 2147483647;"
            ))?;
        }
        serde_json::to_writer(
            fs::File::create(archive.join(MANIFEST))?,
            &inventory(&payload)?,
        )?;

        let diagnostic = format!("unsupported {name} database schema version 2147483647");
        let rejected_archive = root.path().join("rejected-archive");
        let error = backup(&paths, &rejected_archive).unwrap_err();
        assert!(format!("{error:#}").contains(&diagnostic), "{error:#}");
        assert!(!rejected_archive.exists());
        let destination = root.path().join("restored");
        let error = restore(&archive, &destination).unwrap_err();
        assert!(format!("{error:#}").contains(&diagnostic), "{error:#}");
        assert!(!destination.exists());
    }
    Ok(())
}

#[test]
fn backup_and_restore_reject_malformed_older_focus_session() -> Result<()> {
    let root = tempdir()?;
    let live = initialize(&root.path().join("live"))?;
    let database = live.focus_database_path();
    FocusStore::open(&database)?;
    Connection::open(&database)?.execute_batch(
        "INSERT INTO focus_sessions (id, state, started_at, stopped_at, elapsed_ns) VALUES
            ('00000000-0000-0000-0000-000000000001', 'stopped',
             '2027-01-15T08:00:00Z', '2027-01-15T08:01:00Z', 42),
            ('00000000-0000-0000-0000-000000000002', 'paused',
             '2027-01-16T08:00:00Z', NULL, 42);",
    )?;

    let archive = root.path().join("archive");
    backup(&live, &archive)?;
    let payload = archive.join(PAYLOAD);
    let captured = WorkspacePaths::from_home(payload.clone());
    for path in [&database, &captured.focus_database_path()] {
        let db = Connection::open(path)?;
        assert_eq!(
            db.execute(
                "UPDATE focus_sessions SET started_at = 'invalid' WHERE state = 'stopped'",
                [],
            )?,
            1
        );
        let integrity: String = db.pragma_query_value(None, "integrity_check", |row| row.get(0))?;
        assert_eq!(integrity, "ok");
    }
    serde_json::to_writer(
        fs::File::create(archive.join(MANIFEST))?,
        &inventory(&payload)?,
    )?;

    let rejected_archive = root.path().join("rejected-archive");
    let destination = root.path().join("restored");
    let original_db = fs::read(&database)?;
    let staged_db = fs::read(captured.focus_database_path())?;
    for error in [
        backup(&live, &rejected_archive).unwrap_err(),
        restore(&archive, &destination).unwrap_err(),
    ] {
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("focus session"), "{diagnostic}");
        assert!(diagnostic.contains("started_at"), "{diagnostic}");
    }
    assert!(!rejected_archive.exists());
    assert!(!destination.exists());
    assert_eq!(fs::read(database)?, original_db);
    assert_eq!(fs::read(captured.focus_database_path())?, staged_db);
    Ok(())
}

#[test]
fn backup_and_restore_reject_malformed_workflow_state() -> Result<()> {
    let id = Uuid::from_u128(1);
    let run_id = id.to_string();
    for (corruption, expected_parts) in [
        (
            "UPDATE workflow_runs SET due_at = 'invalid'",
            ["decoding saved workflow run", &run_id],
        ),
        (
            "UPDATE workflow_runs SET due_ms = due_ms + 1",
            ["inconsistent due time", &run_id],
        ),
        (
            "UPDATE workflow_steps SET trace = '{}'",
            ["Invalid trace for run", &run_id],
        ),
        (
            "UPDATE workflow_schedules SET summary = '{}'",
            ["decoding saved workflow schedule", &run_id],
        ),
        (
            "UPDATE workflow_schedules SET next_ms = next_ms + 1",
            ["inconsistent next occurrence", &run_id],
        ),
        (
            "UPDATE workflow_schedules SET summary = json_set(summary, '$.trigger.seconds', 0)",
            ["schedule interval must be positive", &run_id],
        ),
        (
            "DELETE FROM workflow_artifacts",
            [
                "SQLite foreign key check failed in workflow_steps",
                "missing workflow_artifacts record",
            ],
        ),
    ] {
        let root = tempdir()?;
        let live = initialize(&root.path().join("live"))?;
        let database = live.workflow_database_path();
        let store = RunStore::open(&database)?;
        let revision = "a".repeat(64);
        let due = "2027-01-15T08:00:00.000001Z".parse()?;
        let run = store.enqueue_if_absent(id, &revision, Some(due))?;
        store.append_host_step(
            &run,
            r#"{"node":"report","status":"succeeded","outcome":null,"inputs":{},"outputs":{},"reason":"done"}"#,
            Some(&WorkflowArtifact {
                bytes: vec![1, 2, 3],
                media_type: "application/octet-stream",
            }),
        )?;
        store.insert_schedule(
            &ScheduleSummary {
                id,
                revision,
                trigger: Trigger::Interval {
                    anchor: due,
                    seconds: 60,
                },
                missed: Missed::RunOnce,
                overlap: Overlap::Queue,
                next_at: Some(due),
                enabled: true,
                missed_occurrences: 0,
                overlap_skipped: 0,
                cascade_skipped: 0,
            },
            None,
        )?;
        drop(store);

        let archive = root.path().join("archive");
        backup(&live, &archive)?;
        let payload = archive.join(PAYLOAD);
        let captured_db = WorkspacePaths::from_home(payload.clone()).workflow_database_path();
        for path in [&database, &captured_db] {
            let db = Connection::open(path)?;
            db.pragma_update(None, "foreign_keys", false)?;
            assert_eq!(db.execute(corruption, [])?, 1);
            let integrity: String =
                db.pragma_query_value(None, "integrity_check", |row| row.get(0))?;
            assert_eq!(integrity, "ok");
        }
        serde_json::to_writer(
            fs::File::create(archive.join(MANIFEST))?,
            &inventory(&payload)?,
        )?;

        let original = fs::read(&database)?;
        let archived = fs::read(&captured_db)?;
        let rejected_archive = root.path().join("rejected-archive");
        let destination = root.path().join("restored");
        for error in [
            backup(&live, &rejected_archive).unwrap_err(),
            restore(&archive, &destination).unwrap_err(),
        ] {
            let diagnostic = format!("{error:#}");
            for expected in expected_parts {
                assert!(diagnostic.contains(expected), "{diagnostic}");
            }
        }
        assert!(!rejected_archive.exists());
        assert!(!destination.exists());
        assert_eq!(fs::read(&database)?, original);
        assert_eq!(fs::read(&captured_db)?, archived);
    }
    Ok(())
}

#[test]
fn completion_event_round_trips_and_malformed_event_blocks_restore() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let vault_root = paths.vault_root();
    let project = vault_root.join("projects/rust-os");
    fs::create_dir_all(project.join("tasks"))?;
    fs::create_dir_all(project.join("units"))?;
    for (name, contents) in [
        (
            "project.md",
            include_str!("../../../tests/fixtures/vault/projects/rust-os/project.md"),
        ),
        (
            "tasks/memory.md",
            include_str!("../../../tests/fixtures/vault/projects/rust-os/tasks/memory.md"),
        ),
        (
            "units/read-spec.md",
            include_str!("../../../tests/fixtures/vault/projects/rust-os/units/read-spec.md"),
        ),
        (
            "units/mapping.md",
            include_str!("../../../tests/fixtures/vault/projects/rust-os/units/mapping.md"),
        ),
    ] {
        fs::write(project.join(name), contents)?;
    }
    let timezone = paths.load_config()?.timezone;
    let origin = paths.load_or_create_device_identity()?;
    let store = VaultStore::new(&vault_root, timezone).with_completion_origin(origin);
    let vault = Vault::load(&vault_root, timezone)?;
    let id = Id::from_u128(0x30000000000040008000000000000002);
    store.complete_work_unit(CompleteWorkUnit {
        id,
        expected_revision: vault.revision(id).unwrap().clone(),
    })?;
    let events = store.work_unit_completed_events()?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].work_unit_id, id);

    let archive = root.path().join("archive");
    let destination = root.path().join("restored");
    backup(&paths, &archive)?;
    restore(&archive, &destination)?;
    let restored = WorkspacePaths::from_home(destination);
    let database = ".mcbtodo-events.sqlite3";
    let expected = fs::read(vault_root.join(database))?;
    assert_eq!(fs::read(restored.vault_root().join(database))?, expected);
    assert!(!restored.device_identity_path().exists());
    let restored_origin = restored.load_or_create_device_identity()?;
    assert_ne!(origin, restored_origin);
    let restored_root = restored.vault_root();
    let restored_store =
        VaultStore::new(&restored_root, timezone).with_completion_origin(restored_origin);
    assert_eq!(restored_store.work_unit_completed_events()?, events);
    assert_eq!(
        restored_store.work_unit_completion_export_page(0)?[0].origin_device_id,
        Some(origin)
    );
    let first_unit = restored_root.join("projects/rust-os/units/read-spec.md");
    let source = fs::read_to_string(&first_unit)?.replace("status: completed", "status: todo");
    fs::write(first_unit, source)?;
    let vault = Vault::load(&restored_root, timezone)?;
    let id = Id::from_u128(0x30000000000040008000000000000001);
    restored_store.complete_work_unit(CompleteWorkUnit {
        id,
        expected_revision: vault.revision(id).unwrap().clone(),
    })?;
    let page = restored_store.work_unit_completion_export_page(0)?;
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].event, events[0]);
    assert_eq!(page[0].origin_device_id, Some(origin));
    assert_eq!(page[1].event.work_unit_id, id);
    assert_eq!(page[1].origin_device_id, Some(restored_origin));

    // Give the archive a valid checksummed SQLite file with invalid event contents.
    let payload = archive.join(PAYLOAD);
    let bundle = WorkspacePaths::from_home(payload.clone());
    let database = bundle.vault_root().join(database);
    assert_eq!(fs::read(&database)?, expected);
    assert_eq!(
        Connection::open(&database)?
            .execute("UPDATE work_unit_completions SET event = '{}'", [])?,
        1,
    );
    fs::write(
        archive.join(MANIFEST),
        serde_json::to_vec(&inventory(&payload)?)?,
    )?;

    let destination = root.path().join("empty");
    fs::create_dir(&destination)?;
    let error = restore(&archive, &destination).unwrap_err();
    assert!(format!("{error:#}").contains("validating WorkUnit completion journal"));
    assert!(fs::read_dir(destination)?.next().is_none());
    Ok(())
}

#[test]
fn backup_excludes_only_graph_staging_directories_under_projects() -> Result<()> {
    let root = tempdir()?;
    let paths = initialize(&root.path().join("live"))?;
    let vault = paths.vault_root();
    let prefix = mcbtodo_vault::GRAPH_STAGE_PREFIX;
    let stage = format!("projects/rust-os/{prefix}old-units");
    let preserved = [
        "projects/rust-os/notes/reference.txt".to_owned(),
        format!("projects/rust-os/{prefix}notes.txt"),
        format!("projects/rust-os/notes/{prefix}examples/reference.txt"),
        format!("{prefix}archive/reference.txt"),
    ];
    for path in preserved
        .iter()
        .chain(std::iter::once(&format!("{stage}/displaced.md")))
    {
        let file = vault.join(path);
        fs::create_dir_all(file.parent().unwrap())?;
        fs::write(file, path)?;
    }
    fs::write(
        vault.join("projects/rust-os/project.md"),
        include_str!("../../../tests/fixtures/vault/projects/rust-os/project.md"),
    )?;

    let archive = root.path().join("archive");
    let destination = root.path().join("restored");
    backup(&paths, &archive)?;
    restore(&archive, &destination)?;

    for home in [archive.join(PAYLOAD), destination] {
        let copy = WorkspacePaths::from_home(home).vault_root();
        assert!(!copy.join(&stage).exists());
        for path in &preserved {
            assert_eq!(fs::read_to_string(copy.join(path))?, *path);
        }
    }
    assert!(vault.join(stage).join("displaced.md").is_file());
    Ok(())
}
