use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use mcbtodo_platform::WorkspacePaths;
use mcbtodo_ports::workflow::WorkflowRunCatalog;
use mcbtodo_protocol::{TimerState, request, wire};

use super::*;

fn status(server: &mut Server, path: PathBuf, command: Request) -> Response {
    thread::scope(|scope| {
        let client = scope.spawn(move || request(path, command));
        while !client.is_finished() {
            server.step().unwrap();
        }
        client.join().unwrap().unwrap()
    })
}

#[test]
fn document_source_follows_moves_and_preserves_unix_path_bytes() -> Result<()> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let id = Uuid::from_u128(1);
    let folder = paths.vault_root().join("projects/test");
    fs::create_dir_all(&folder)?;
    let original = folder.join("project.md");
    fs::write(
        &original,
        format!("---\nschema: mcbtodo.project/v1\nid: {id}\n---\n# Test\n"),
    )?;
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let mut locate = |id| status(&mut server, socket.clone(), Request::DocumentSource { id });
    assert_eq!(
        locate(id),
        Response::DocumentSource {
            unix_path: original.as_os_str().as_bytes().to_vec()
        }
    );
    let moved_folder = folder.with_file_name(OsStr::from_bytes(b"moved-\xff"));
    fs::rename(&folder, &moved_folder)?;
    let moved = moved_folder.join("project.md");
    assert_eq!(
        locate(id),
        Response::DocumentSource {
            unix_path: moved.as_os_str().as_bytes().to_vec()
        }
    );
    assert!(matches!(
        locate(Uuid::from_u128(2)),
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    Ok(())
}

#[test]
fn deadline_edit_checks_observed_revision_and_writer_lock_over_ipc() -> Result<()> {
    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let id = Uuid::from_u128(1);
    let document = root.join("projects/test/project.md");
    fs::create_dir_all(document.parent().unwrap())?;
    let original = format!("---\nschema: mcbtodo.project/v1\nid: {id}\n---\n# Test\n");
    fs::write(&document, &original)?;
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let mut send = |json| -> Result<serde_json::Value> {
        Ok(serde_json::to_value(status(
            &mut server,
            socket.clone(),
            serde_json::from_value(json)?,
        ))?)
    };
    let context = serde_json::json!({"command": "deadline_context", "id": id});
    let observed = send(context.clone())?;
    assert_eq!(observed["result"], "deadline_context");
    assert!(observed["deadline"].is_null());
    let date = serde_json::json!({"kind": "date", "value": "2026-12-10"});
    let update = |observed: &serde_json::Value, deadline| {
        serde_json::json!({
            "command": "set_deadline", "id": id, "revision": observed["revision"], "deadline": deadline
        })
    };
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert_eq!(send(update(&observed, date.clone()))?["code"], "conflict");
    drop(lock);
    let edited = original.replace("# Test", "# Edited elsewhere");
    fs::write(&document, &edited)?;
    assert_eq!(
        send(update(&observed, date.clone()))?["code"],
        "stale_revision"
    );
    assert_eq!(fs::read_to_string(&document)?, edited);

    let refreshed = send(context.clone())?;
    assert_eq!(
        send(update(&refreshed, date.clone()))?,
        serde_json::json!({"result": "deadline_set", "synced": true})
    );
    let saved = send(context.clone())?;
    assert_eq!(saved["deadline"], date);
    assert_ne!(saved["revision"], refreshed["revision"]);
    assert_eq!(
        send(update(&saved, serde_json::Value::Null))?["result"],
        "deadline_set"
    );
    assert!(send(context)?["deadline"].is_null());
    assert!(fs::read_to_string(&document)?.contains("# Edited elsewhere"));
    Ok(())
}

#[test]
fn inbox_triage_is_shared_and_checks_the_observed_revision_and_lock() -> Result<()> {
    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let inbox = uuid::Uuid::from_u128(1);
    let project = uuid::Uuid::from_u128(2);
    fs::create_dir_all(root.join("projects/inbox-test"))?;
    fs::write(
        root.join("projects/inbox-test/project.md"),
        format!("---\nschema: mcbtodo.project/v1\nid: {project}\n---\n# Inbox test\n"),
    )?;
    fs::create_dir_all(root.join("inbox"))?;
    let note = root.join(format!("inbox/{inbox}.md"));
    let original = format!(
        "---\nschema: mcbtodo.inbox-entry/v1\nid: {inbox}\ncaptured_at: \"2026-11-20T08:00:00Z\"\n---\n# Research notes\n\nKeep these notes.\n"
    );
    fs::write(&note, &original)?;
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let mut send = |command| -> Result<serde_json::Value> {
        let response = status(
            &mut server,
            socket.clone(),
            serde_json::from_value(command)?,
        );
        Ok(serde_json::to_value(response)?)
    };
    let query = serde_json::json!({"command": "inbox"});
    let observed = send(query.clone())?;
    assert_eq!(observed["items"][0]["title"], "Research notes");
    let triage = |revision| {
        serde_json::json!({
            "command": "triage_inbox", "inbox_id": inbox,
            "project_id": project, "inbox_revision": revision
        })
    };
    let old = triage(observed["items"][0]["revision"].clone());
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert_eq!(send(old.clone())?["code"], "conflict");
    drop(lock);
    let edited = original.replace("Research notes", "Updated notes");
    fs::write(&note, &edited)?;
    assert_eq!(send(old.clone())?["code"], "stale_revision");
    assert!(!root.join("projects/inbox-test/tasks").exists());
    let latest = send(query.clone())?;
    let result = send(triage(latest["items"][0]["revision"].clone()))?;
    assert_eq!(result["result"], "inbox_triaged", "{result}");
    let next = send(query)?;
    assert_eq!(next["items"][0]["project_id"], project.to_string());
    assert_eq!(send(old)?["code"], "stale_revision");
    let outline = send(serde_json::json!({"command": "outline", "timezone": "UTC"}))?;
    assert_eq!(outline["items"][1]["title"], "Updated notes");
    assert_eq!(outline["items"].as_array().unwrap().len(), 2);
    assert!(fs::read_to_string(&note)?.contains("Keep these notes."));

    // Restore only the source in the fixture to model a stop just after Task publication.
    fs::write(&note, edited)?;
    let retry = triage(latest["items"][0]["revision"].clone());
    assert_eq!(send(retry)?["result"], "inbox_triaged");
    assert_eq!(
        fs::read_dir(root.join("projects/inbox-test/tasks"))?.count(),
        1
    );
    Ok(())
}

#[test]
fn task_capture_checks_the_project_revision_and_writer_lock_over_ipc() -> Result<()> {
    use mcbtodo_domain::{Id, Project};
    use mcbtodo_vault::{CreateProject, Vault, VaultStore};

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let timezone = paths.load_config()?.timezone;
    let project_id = Id::new_v4();
    let task_id = Id::new_v4();
    let project = VaultStore::new(&root, timezone).create_project(CreateProject {
        project: Project {
            id: project_id,
            status: Default::default(),
            active_period: None,
            deadline: None,
            tags: Vec::new(),
        },
        slug: Some("capture".into()),
        title: "Capture test".into(),
        body: String::new(),
    })?;
    let request = |project_revision| Request::CaptureTask {
        task_id,
        project_id,
        project_revision,
        title: "Call the team".into(),
    };
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let lock = fs::File::options()
        .write(true)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert!(matches!(
        status(
            &mut server,
            socket.clone(),
            request(project.revision.digest())
        ),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    drop(lock);

    let mut edited = fs::read_to_string(&project.path)?;
    edited.push_str("\nExternal edit\n");
    fs::write(&project.path, &edited)?;
    assert!(matches!(
        status(
            &mut server,
            socket.clone(),
            request(project.revision.digest())
        ),
        Response::Error {
            code: ErrorCode::StaleRevision,
            ..
        }
    ));
    assert!(!root.join("projects/capture/tasks").exists());
    let current = Vault::load(&root, timezone)?;
    let command = request(current.revision(project_id).unwrap().digest());
    assert_eq!(
        status(&mut server, socket.clone(), command.clone()),
        Response::TaskCaptured {
            task_id,
            synced: true
        }
    );
    assert!(matches!(
        status(&mut server, socket, command),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert_eq!(fs::read_to_string(project.path)?, edited);
    let saved = Vault::load(&root, timezone)?;
    let rows = saved.outline();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.id == task_id && row.title == "Call the team")
    );
    Ok(())
}

#[test]
fn all_day_event_creation_validates_dates_and_refuses_duplicate_ids_over_ipc() -> Result<()> {
    use chrono::NaiveDate;
    use mcbtodo_domain::{CalendarKind, Occurrence, time::Span};
    use mcbtodo_vault::Vault;

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let timezone = paths.load_config()?.timezone;
    let event_id = Uuid::new_v4();
    let start = NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
    let through = start.succ_opt().unwrap();
    let command = |start, through| Request::CreateAllDayEvent {
        event_id,
        title: "Campus event".into(),
        start,
        through,
    };
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    assert!(matches!(
        status(&mut server, socket.clone(), command(through, start)),
        Response::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
    assert!(Vault::load(&root, timezone)?.calendar().is_empty());

    let lock = fs::File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert!(matches!(
        status(&mut server, socket.clone(), command(start, through)),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    drop(lock);

    assert_eq!(
        status(&mut server, socket.clone(), command(start, through)),
        Response::AllDayEventCreated {
            event_id,
            synced: true,
        }
    );
    assert_eq!(
        status(
            &mut server,
            socket.clone(),
            Request::Calendar {
                start: through,
                through,
                timezone: "UTC".into(),
            }
        ),
        Response::Calendar {
            items: vec![mcbtodo_domain::CalendarItem::from(
                mcbtodo_domain::CalendarEntry::event(
                    event_id,
                    "Campus event",
                    Span::DateSpan(DateSpan::new(start, through)?),
                    mcbtodo_domain::Availability::Busy,
                )
            )],
        }
    );
    assert!(matches!(
        status(&mut server, socket, command(start, through)),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    let saved = Vault::load(&root, timezone)?;
    let entries = saved.calendar();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, event_id);
    assert_eq!(entries[0].kind, CalendarKind::Event);
    assert!(entries[0].busy);
    assert!(matches!(
        entries[0].occurrence,
        Occurrence::Span(Span::DateSpan(span))
            if span.start() == start && span.through() == through
    ));
    Ok(())
}

#[test]
fn calendar_uses_the_requested_zone_and_rejects_unusable_projections() -> Result<()> {
    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let calendar = paths.vault_root().join("calendar");
    fs::create_dir(&calendar)?;
    let template = calendar.join("template.md");
    let event_id = Uuid::new_v4();
    let write_event = |title: &str| {
        fs::write(
            &template,
            format!(
                "---\nschema: mcbtodo.calendar-event/v1\nid: {event_id}\n\
                 when: {{kind: date_time_span, start: '2026-11-27T01:00:00Z', end: '2026-11-27T02:00:00Z', timezone: UTC}}\n\
                 recurrence: {{start: 2026-11-27, frequency: daily, interval: 1, dst: {{gap: skip, fold: earlier}}}}\n\
                 availability: busy\n---\n\n# {title}\n"
            ),
        )
    };
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    write_event("晨间例会")?;
    let mut query = |start: &str, through: &str, timezone: &str| {
        status(
            &mut server,
            socket.clone(),
            Request::Calendar {
                start: start.parse().unwrap(),
                through: through.parse().unwrap(),
                timezone: timezone.into(),
            },
        )
    };
    let response = query("2026-11-26", "2026-11-27", "America/Los_Angeles");
    let Response::Calendar { items } = response else {
        panic!("expected two recurring occurrences, got {response:?}");
    };
    assert_eq!(items.len(), 2);
    for (entry, day) in items.iter().zip(["2026-11-26", "2026-11-27"]) {
        assert_eq!(entry.id, event_id);
        assert_eq!(entry.title, "晨间例会");
        assert!(entry.on_day(day.parse()?, chrono_tz::America::Los_Angeles));
    }
    assert_eq!(
        query("2026-11-26", "2026-11-26", "UTC"),
        Response::Calendar { items: vec![] }
    );
    for response in [
        query("2026-11-27", "2026-11-26", "UTC"),
        query("2026-11-26", "2026-11-26", "no-such-zone"),
        query("2026-11-26", "2028-11-26", "UTC"),
    ] {
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::InvalidRequest,
                ..
            }
        ));
    }

    write_event(&"日".repeat(wire::MAX_FRAME_BYTES as usize / 2))?;
    assert!(matches!(
        query("2026-11-27", "2026-11-27", "UTC"),
        Response::Error { code: ErrorCode::InvalidRequest, message }
            if message.contains("IPC size limit")
    ));
    fs::write(template, "broken markdown")?;
    assert!(matches!(
        query("2026-11-27", "2026-11-27", "UTC"),
        Response::Error {
            code: ErrorCode::Internal,
            ..
        }
    ));
    Ok(())
}

#[test]
fn outline_returns_one_snapshot_even_when_rows_exceed_one_frame() -> Result<()> {
    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let project = root.join("projects/outline");
    let project_id = Uuid::new_v4();
    fs::create_dir_all(project.join("tasks"))?;
    fs::write(
        project.join("project.md"),
        format!("---\nschema: mcbtodo.project/v1\nid: {project_id}\n---\n\n# 任务规划\n"),
    )?;
    for index in 0..100 {
        let id = Uuid::new_v4();
        fs::write(
            project.join(format!("tasks/{id}.md")),
            format!(
                "---\nschema: mcbtodo.task/v1\nid: {id}\nproject_id: {project_id}\n---\n\n# Task {index:03} {}\n",
                "日".repeat(199),
            ),
        )?;
    }
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let items = thread::scope(|scope| -> Result<Vec<serde_json::Value>> {
        let peer = scope.spawn(|| -> Result<Vec<serde_json::Value>> {
            let mut stream = UnixStream::connect(&socket)?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            wire::write(
                &mut stream,
                &serde_json::json!({"command": "outline", "timezone": "UTC"}),
            )?;
            let mut items = Vec::new();
            let mut frames = 0;
            loop {
                let response: serde_json::Value = wire::read(&mut stream)?;
                assert_eq!(response["result"], "outline", "{response}");
                frames += 1;
                items.extend(response["items"].as_array().unwrap().iter().cloned());
                if response["more"] == false {
                    break;
                }
                assert_eq!(response["more"], true);
            }
            assert!(frames > 1);
            Ok(items)
        });
        while !peer.is_finished() {
            server.step()?;
        }
        peer.join().unwrap()
    })?;
    let expected = VaultQueries::new(root, chrono_tz::UTC).outline()?;
    assert_eq!(
        status(
            &mut server,
            socket,
            Request::Outline {
                timezone: "UTC".into(),
            },
        ),
        Response::Outline {
            items: expected.clone(),
            more: false,
        },
    );
    assert_eq!(
        items,
        serde_json::to_value(expected)?
            .as_array()
            .unwrap()
            .as_slice()
    );
    assert_eq!(items.len(), 101);
    assert_eq!(items[0]["title"], "任务规划");
    assert_eq!(items[100]["project_id"], project_id.to_string());
    Ok(())
}

#[test]
fn graph_clients_receive_all_nodes_and_see_subsequent_vault_edits() -> Result<()> {
    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let project = root.join("projects/graph");
    let project_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    fs::create_dir_all(project.join("tasks"))?;
    fs::create_dir_all(project.join("units"))?;
    fs::write(
        project.join("project.md"),
        format!("---\nschema: mcbtodo.project/v1\nid: {project_id}\n---\n# Project\n"),
    )?;
    fs::write(
        project.join("tasks/task.md"),
        format!(
            "---\nschema: mcbtodo.task/v1\nid: {task_id}\nproject_id: {project_id}\n---\n# Task\n"
        ),
    )?;
    let unit_ids: Vec<_> = (0..100).map(|_| Uuid::new_v4()).collect();
    for (index, id) in unit_ids.iter().enumerate() {
        let dependency = index
            .checked_sub(1)
            .map(|index| unit_ids[index].to_string())
            .unwrap_or_default();
        fs::write(
            project.join(format!("units/{index}.md")),
            format!(
                "---\nschema: mcbtodo.work-unit/v1\nid: {id}\nproject_id: {project_id}\nparent_task_id: {task_id}\nstatus: todo\ndepends_on: [{dependency}]\n---\n# Unit {index:03} {}\n",
                "日".repeat(199),
            ),
        )?;
    }

    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let query = Request::WorkUnitGraph {
        timezone: "UTC".into(),
    };
    let first = status(&mut server, socket.clone(), query.clone());
    assert!(matches!(
        wire::write(&mut Vec::new(), &first),
        Err(mcbtodo_protocol::ProtocolError::FrameSize(_)),
    ));
    assert_eq!(status(&mut server, socket.clone(), query.clone()), first);
    let Response::WorkUnitGraph { items, more: false } = first else {
        panic!("expected a complete graph");
    };
    assert_eq!(items.len(), unit_ids.len());
    assert!(
        items
            .iter()
            .map(|node| node.id)
            .eq(unit_ids.iter().copied())
    );
    assert_eq!(items[49].dependents, vec![unit_ids[50]]);
    assert_eq!(items[50].prerequisites, vec![unit_ids[49]]);
    assert!(items[50].blocked);

    let source = project.join("units/99.md");
    fs::write(
        &source,
        fs::read_to_string(&source)?.replace("Unit 099", "Edited unit"),
    )?;
    let Response::WorkUnitGraph { items, more: false } = status(&mut server, socket, query) else {
        panic!("expected an updated graph");
    };
    assert!(items[99].title.starts_with("Edited unit"));
    Ok(())
}

#[test]
fn graph_dependency_edits_are_previewed_and_committed_by_the_daemon() -> Result<()> {
    use mcbtodo_domain::Id;
    use mcbtodo_vault::Vault;

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let project = root.join("projects/dependency-edit");
    let project_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let prerequisite = Id::new_v4();
    let dependent = Id::new_v4();
    fs::create_dir_all(project.join("tasks"))?;
    fs::create_dir_all(project.join("units"))?;
    fs::write(
        project.join("project.md"),
        format!("---\nschema: mcbtodo.project/v1\nid: {project_id}\n---\n# Graph edit\n"),
    )?;
    fs::write(
        project.join("tasks/task.md"),
        format!(
            "---\nschema: mcbtodo.task/v1\nid: {task_id}\nproject_id: {project_id}\n---\n# Task\n"
        ),
    )?;
    let prerequisite_path = project.join(format!("units/{prerequisite}.md"));
    let dependent_path = project.join(format!("units/{dependent}.md"));
    fs::write(
        &prerequisite_path,
        format!(
            "---\nschema: mcbtodo.work-unit/v1\nid: {prerequisite}\nproject_id: {project_id}\nparent_task_id: {task_id}\nstatus: todo\ndepends_on: []\n---\n# Prerequisite\n"
        ),
    )?;
    fs::write(
        &dependent_path,
        format!(
            "---\nschema: mcbtodo.work-unit/v1\nid: {dependent}\nproject_id: {project_id}\nparent_task_id: {task_id}\nstatus: todo\ndepends_on: [{prerequisite}]\n---\n# Dependent\n"
        ),
    )?;

    let socket = paths.socket_path();
    let timezone = paths.load_config()?.timezone;
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let preview = |server: &mut Server, target, sources, operation| {
        status(
            server,
            socket.clone(),
            Request::PreviewWorkUnitDependencyChange {
                target,
                sources,
                operation,
                timezone: timezone.name().into(),
            },
        )
    };
    let Response::WorkUnitDependencyPreview(edit) = preview(
        &mut server,
        dependent,
        [prerequisite].into(),
        mcbtodo_domain::DependencyOperation::Remove,
    ) else {
        panic!("expected a dependency preview");
    };
    assert_eq!(edit.prerequisites, Vec::<Id>::new());
    assert_eq!(edit.removed, [prerequisite]);
    assert_eq!(edit.titles[&dependent], "Dependent");
    assert_eq!(edit.titles[&prerequisite], "Prerequisite");

    let submit = || Request::SetWorkUnitDependencies {
        target: dependent,
        prerequisites: edit.prerequisites.clone(),
        revision: edit.revision.clone(),
    };
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert!(matches!(
        status(&mut server, socket.clone(), submit()),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    drop(lock);

    fs::write(
        &dependent_path,
        fs::read_to_string(&dependent_path)?.replace("# Dependent", "# Edited dependent"),
    )?;
    assert!(matches!(
        status(&mut server, socket.clone(), submit()),
        Response::Error {
            code: ErrorCode::StaleRevision,
            ..
        }
    ));

    let Response::WorkUnitDependencyPreview(current) = preview(
        &mut server,
        dependent,
        [prerequisite].into(),
        mcbtodo_domain::DependencyOperation::Remove,
    ) else {
        panic!("expected a current dependency preview");
    };
    let before_cycle = Vault::load(&root, timezone)?;
    let cycle = status(
        &mut server,
        socket.clone(),
        Request::SetWorkUnitDependencies {
            target: prerequisite,
            prerequisites: vec![dependent],
            revision: before_cycle.revision(prerequisite).unwrap().digest(),
        },
    );
    assert!(matches!(
        cycle,
        Response::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));

    assert_eq!(
        status(
            &mut server,
            socket.clone(),
            Request::SetWorkUnitDependencies {
                target: dependent,
                prerequisites: current.prerequisites,
                revision: current.revision,
            },
        ),
        Response::WorkUnitDependenciesSet { synced: true }
    );
    let Response::WorkUnitGraph { items, more: false } = status(
        &mut server,
        socket,
        Request::WorkUnitGraph {
            timezone: timezone.name().into(),
        },
    ) else {
        panic!("expected a graph read from the second client request");
    };
    assert!(
        items
            .iter()
            .find(|item| item.id == dependent)
            .unwrap()
            .prerequisites
            .is_empty()
    );
    assert!(fs::read_to_string(&dependent_path)?.contains("# Edited dependent"));
    assert!(
        Vault::load(&root, timezone)?
            .work_unit(dependent)
            .unwrap()
            .data
            .depends_on
            .is_empty()
    );
    Ok(())
}

#[test]
fn planned_project_creation_preserves_calendar_kind_and_checks_the_writer_lock() -> Result<()> {
    use chrono::NaiveDate;
    use mcbtodo_domain::{
        CalendarKind, Occurrence,
        time::{Deadline, Span},
    };
    use mcbtodo_vault::Vault;

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let timezone = paths.load_config()?.timezone;
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let start = NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
    let through = start.succ_opt().unwrap();
    let active_id = Uuid::new_v4();
    let deadline_id = Uuid::new_v4();
    let create = |project_id, dates| Request::CreatePlannedProject {
        project_id,
        title: "Research".into(),
        dates,
    };
    assert!(matches!(
        status(
            &mut server,
            socket.clone(),
            create(
                active_id,
                ProjectDates::ActivePeriod {
                    start: through,
                    through: start,
                }
            )
        ),
        Response::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
    let active = create(active_id, ProjectDates::ActivePeriod { start, through });
    let lock = fs::File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert!(matches!(
        status(&mut server, socket.clone(), active.clone()),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    drop(lock);
    assert!(Vault::load(&root, timezone)?.outline().is_empty());
    assert_eq!(
        status(&mut server, socket.clone(), active.clone()),
        Response::PlannedProjectCreated {
            project_id: active_id,
            synced: true
        }
    );
    assert!(matches!(
        status(&mut server, socket.clone(), active),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert_eq!(
        status(
            &mut server,
            socket,
            create(deadline_id, ProjectDates::Deadline { due: through })
        ),
        Response::PlannedProjectCreated {
            project_id: deadline_id,
            synced: true
        }
    );
    let saved = Vault::load(&root, timezone)?;
    let entries = saved.calendar();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|item| item.id == active_id
        && !item.busy
        && item.kind == CalendarKind::ActivePeriod
        && matches!(item.occurrence, Occurrence::Span(Span::DateSpan(span))
            if span.start() == start && span.through() == through)));
    assert!(entries.iter().any(|item| item.id == deadline_id
        && !item.busy
        && item.kind == CalendarKind::Deadline
        && matches!(item.occurrence, Occurrence::Deadline(Deadline::Date { value })
            if value == through)));
    Ok(())
}

#[test]
fn reminder_creation_validates_the_offset_target_and_writer_lock_over_ipc() -> Result<()> {
    use mcbtodo_domain::recurrence::ReminderSchedule;
    use mcbtodo_vault::Vault;

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let timezone = paths.load_config()?.timezone;
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    let reminder_id = Uuid::new_v4();
    let command = |at: &str, target_id| Request::CreateReminder {
        reminder_id,
        target_id,
        title: "Review notes".into(),
        at: at.parse().unwrap(),
    };
    let valid_time = "2026-11-30T09:00:00+00:00";
    for (request, expected) in [
        (
            command("2026-11-30T09:00:00+08:00", None),
            ErrorCode::InvalidRequest,
        ),
        (
            command(valid_time, Some(Uuid::new_v4())),
            ErrorCode::NotFound,
        ),
    ] {
        assert!(matches!(
            status(&mut server, socket.clone(), request),
            Response::Error { code, .. } if code == expected
        ));
    }
    let valid = command(valid_time, None);
    let lock = fs::File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert!(matches!(
        status(&mut server, socket.clone(), valid.clone()),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    drop(lock);
    assert_eq!(Vault::load(&root, timezone)?.reminders().count(), 0);
    assert_eq!(
        status(&mut server, socket.clone(), valid.clone()),
        Response::ReminderCreated {
            reminder_id,
            synced: true
        }
    );
    assert!(matches!(
        status(&mut server, socket, valid),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    let vault = Vault::load(&root, timezone)?;
    let reminders: Vec<_> = vault.reminders().collect();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].data.id, reminder_id);
    assert_eq!(reminders[0].title, "Review notes");
    let ReminderSchedule::Once { at } = reminders[0].data.schedule else {
        panic!("expected a one-time reminder");
    };
    assert_eq!(at.fixed_offset().to_rfc3339(), valid_time);
    Ok(())
}

#[test]
fn time_block_creation_checks_offsets_revision_and_writer_lock_over_ipc() -> Result<()> {
    use mcbtodo_domain::{Occurrence, time::Span};
    use mcbtodo_vault::Vault;

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let timezone = paths.load_config()?.timezone;
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/vault/projects/rust-os");
    let project = root.join("projects/rust-os");
    for file in [
        "project.md",
        "tasks/memory.md",
        "units/mapping.md",
        "units/read-spec.md",
    ] {
        let target = project.join(file);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(source.join(file), target)?;
    }
    let work_unit_id = Uuid::parse_str("30000000-0000-4000-8000-000000000002")?;
    let before = Vault::load(&root, timezone)?;
    let path = before.source(work_unit_id).unwrap();
    let old_revision = before.revision(work_unit_id).unwrap().digest();
    let edited = format!("{}\nConcurrent edit.\n", fs::read_to_string(path)?);
    fs::write(path, &edited)?;
    let revision = Vault::load(&root, timezone)?
        .revision(work_unit_id)
        .unwrap()
        .digest();
    let start = "2026-11-20T13:30:00+00:00";
    let end = "2026-11-20T15:00:00+00:00";
    let command = |revision: &str, start: &str, end: &str| Request::CreateTimeBlock {
        work_unit_id,
        work_unit_revision: revision.to_owned(),
        start: start.parse().unwrap(),
        end: end.parse().unwrap(),
    };
    let valid = command(&revision, start, end);
    let socket = paths.socket_path();
    let mut server = Server::open(paths, crate::DEFAULT_WORKER_SLOTS)?;
    for (request, expected) in [
        (command(&old_revision, start, end), ErrorCode::StaleRevision),
        (
            command(&revision, "2026-11-20T13:30:00+08:00", end),
            ErrorCode::InvalidRequest,
        ),
        (command(&revision, end, start), ErrorCode::InvalidRequest),
    ] {
        assert!(matches!(
            status(&mut server, socket.clone(), request),
            Response::Error { code, .. } if code == expected
        ));
    }
    let lock = fs::File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".mcbtodo-vault.lock"))?;
    lock.lock()?;
    assert!(matches!(
        status(&mut server, socket.clone(), valid.clone()),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    drop(lock);
    assert_eq!(fs::read_to_string(path)?, edited);
    assert_eq!(
        status(&mut server, socket.clone(), valid.clone()),
        Response::TimeBlockCreated {
            work_unit_id,
            synced: true,
        }
    );
    let saved = fs::read_to_string(path)?;
    assert!(saved.ends_with("Concurrent edit.\n"));
    let day = "2026-11-20".parse()?;
    let Response::Calendar { items } = status(
        &mut server,
        socket.clone(),
        Request::Calendar {
            start: day,
            through: day,
            timezone: "UTC".into(),
        },
    ) else {
        anyhow::bail!("a second client could not query the calendar after publication");
    };
    assert!(items.iter().any(|item| {
        item.id == work_unit_id
            && item.busy
            && matches!(
                item.occurrence,
                Occurrence::Span(Span::DateTimeSpan(span))
                    if span.start().to_rfc3339() == start && span.end().to_rfc3339() == end
            )
    }));
    assert!(matches!(
        status(&mut server, socket, valid),
        Response::Error {
            code: ErrorCode::StaleRevision,
            ..
        }
    ));
    assert_eq!(fs::read_to_string(path)?, saved);
    let after = Vault::load(&root, timezone)?;
    let blocks = &after.work_unit(work_unit_id).unwrap().data.time_blocks;
    assert_eq!(blocks.len(), 2);
    let original = &before.work_unit(work_unit_id).unwrap().data.time_blocks[0];
    assert_eq!(blocks[0].when, original.when);
    assert_eq!(blocks[0].availability, original.availability);
    assert_eq!(blocks[1].when.start().to_rfc3339(), start);
    assert_eq!(blocks[1].when.end().to_rfc3339(), end);
    assert_eq!(blocks[1].availability, mcbtodo_domain::Availability::Busy);
    Ok(())
}

#[test]
fn daemon_answers_commands_and_recovers_running_timer_over_ipc() -> anyhow::Result<()> {
    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let mut server = open_for_test(dir.path(), &socket)?;
    assert_eq!(fs::metadata(&socket)?.permissions().mode() & 0o777, 0o600);
    assert!(
        matches!(status(&mut server, socket.clone(), Request::Status), Response::Status(state) if state.state == TimerState::Idle && state.session.is_none())
    );
    let started = status(
        &mut server,
        socket.clone(),
        Request::Start { work_unit_id: None },
    );
    let Response::Status(started) = started else {
        panic!("start failed: {started:?}");
    };
    assert_eq!(started.state, TimerState::Running);
    drop(server);

    let mut server = open_for_test(dir.path(), &socket)?;
    let recovered = status(&mut server, socket.clone(), Request::Status);
    let Response::Status(recovered) = recovered else {
        panic!("status failed: {recovered:?}");
    };
    assert_eq!(recovered.state, TimerState::NeedsReview);
    assert_eq!(recovered.session.unwrap().id, started.session.unwrap().id);
    Ok(())
}

#[test]
fn reminders_publish_without_connected_clients_and_survive_daemon_restart() -> Result<()> {
    use chrono::{SecondsFormat, TimeDelta, Utc};
    use uuid::Uuid;

    let dir = private_tempdir()?;
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    paths.initialize()?;
    let state = paths.state_dir();
    let socket = state.join("s");
    let mut server = Server::open_with_paths(&state, &socket, paths.clone())?;
    server.desktop = None;
    let reminders = paths.vault_root().join("reminders");
    fs::create_dir(&reminders)?;
    let now = Utc::now();
    let write_reminder = |name: &str, id, offset_seconds| -> Result<()> {
        let scheduled_at =
            (now + TimeDelta::seconds(offset_seconds)).to_rfc3339_opts(SecondsFormat::Nanos, true);
        fs::write(
            reminders.join(format!("{name}.md")),
            format!(
                "---\nschema: mcbtodo.reminder/v1\nid: {}\nschedule:\n  kind: once\n  at:\n    value: '{scheduled_at}'\n    timezone: UTC\n---\n\n# {name}\n",
                Uuid::from_u128(id),
            ),
        )?;
        Ok(())
    };
    write_reminder("due", 1, -60)?;
    // The catch-up window includes recent occurrences but excludes anything older than 24h.
    write_reminder("within-horizon", 5, -86_000)?;
    write_reminder("expired", 2, -90_000)?;
    write_reminder("future", 3, 3_600)?;
    wait_for_reminders(&mut server, 2)?;
    let request = Request::ListReminderNotifications { before: None };
    let Response::ReminderNotifications(page) =
        status(&mut server, socket.clone(), request.clone())
    else {
        panic!("expected reminder inbox");
    };
    assert_eq!(page.notifications.len(), 2);
    assert_eq!(page.notifications[0].reminder_id, Uuid::from_u128(1));
    assert_eq!(
        page.notifications[0].scheduled_at,
        now - TimeDelta::seconds(60)
    );
    assert_eq!(page.notifications[1].reminder_id, Uuid::from_u128(5));
    assert_eq!(
        page.notifications[1].scheduled_at,
        now - TimeDelta::seconds(86_000)
    );
    drop(server);

    // An independent due item proves the restarted daemon rescanned the first.
    write_reminder("second-due", 4, -30)?;
    let mut server = Server::open_with_paths(&state, &socket, paths)?;
    server.desktop = None;
    wait_for_reminders(&mut server, 3)?;
    let Response::ReminderNotifications(restarted) = status(&mut server, socket, request) else {
        panic!("expected reminder inbox");
    };
    assert_eq!(restarted.notifications.len(), 3);
    assert_eq!(restarted.notifications[0].reminder_id, Uuid::from_u128(4));
    assert_eq!(restarted.notifications[1..], page.notifications);
    Ok(())
}

fn wait_for_reminders(server: &mut Server, count: usize) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while server
        .runtime
        .notifications()
        .reminder_page(None)?
        .notifications
        .len()
        < count
    {
        assert!(Instant::now() < deadline, "timed out waiting for reminders");
        server.step()?;
    }
    Ok(())
}

#[test]
fn a_second_daemon_cannot_change_the_live_socket() -> anyhow::Result<()> {
    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let mut server = open_for_test(dir.path(), &socket)?;
    let inode = fs::symlink_metadata(&socket)?.ino();
    assert!(open_for_test(dir.path(), &socket).is_err());
    assert_eq!(fs::symlink_metadata(&socket)?.ino(), inode);
    assert!(matches!(
        status(&mut server, socket.clone(), Request::Status),
        Response::Status(_)
    ));
    Ok(())
}

#[test]
fn daemon_keeps_a_live_socket_even_when_its_lock_is_available() -> anyhow::Result<()> {
    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let external = UnixListener::bind(&socket)?;
    let inode = fs::symlink_metadata(&socket)?.ino();
    let Err(error) = open_for_test(dir.path(), &socket) else {
        panic!("unexpectedly replaced a live listener");
    };
    assert!(error.to_string().contains("listener is already active"));
    assert_eq!(fs::symlink_metadata(&socket)?.ino(), inode);
    drop(external);
    // A subprocess forked by a parallel test can still hold the listener
    // descriptor until exec closes it. Wait only for that listener to close.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut server = loop {
        match open_for_test(dir.path(), &socket) {
            Ok(server) => break server,
            Err(error)
                if error.to_string().contains("listener is already active")
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    };
    assert!(matches!(
        status(&mut server, socket.clone(), Request::Status),
        Response::Status(_)
    ));
    Ok(())
}

#[test]
fn daemon_does_not_remove_a_non_socket_at_the_socket_path() -> anyhow::Result<()> {
    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    fs::write(&socket, "preserve me")?;
    assert!(open_for_test(dir.path(), &socket).is_err());
    assert_eq!(fs::read_to_string(socket)?, "preserve me");
    Ok(())
}

#[test]
fn daemon_requires_a_private_preinitialized_state_directory() -> anyhow::Result<()> {
    let dir = private_tempdir()?;
    let state = dir.path().join("state");
    let socket = state.join("s");
    assert!(open_for_test(&state, &socket).is_err());
    assert!(!state.exists());
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755))?;
    assert!(open_for_test(&state, &socket).is_err());
    assert_eq!(fs::read_dir(&state)?.count(), 0);
    Ok(())
}

#[test]
fn partial_ipc_frame_does_not_hold_the_daemon_step_open() -> anyhow::Result<()> {
    use std::io::Write;

    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let mut server = open_for_test(dir.path(), &socket)?;
    let mut client = UnixStream::connect(&socket)?;
    let mut frame = Vec::new();
    wire::write(&mut frame, &Request::Status)?;
    client.write_all(&frame[..1])?;

    let started = Instant::now();
    server.step()?;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the Daemon step waited on the incomplete IPC frame"
    );

    client.write_all(&frame[1..])?;
    assert!(matches!(
        read_with_server::<Response>(&mut server, client)?,
        Response::Status(_)
    ));
    Ok(())
}

#[test]
fn expired_partial_request_releases_admission_without_dispatch() -> anyhow::Result<()> {
    use std::io::{Read, Write};

    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let mut server = open_for_test(dir.path(), &socket)?;
    let mut stalled = UnixStream::connect(&socket)?;
    let mut frame = Vec::new();
    wire::write(&mut frame, &Request::Start { work_unit_id: None })?;
    stalled.write_all(&frame[..frame.len() - 1])?;
    server.step()?;

    let Response::Status(state) = status(&mut server, socket, Request::Status) else {
        panic!("expected the next client to be admitted after the read deadline");
    };
    assert_eq!(state.state, TimerState::Idle);
    stalled.set_read_timeout(Some(Duration::from_secs(1)))?;
    assert_eq!(stalled.read(&mut [0])?, 0);
    Ok(())
}

fn read_with_server<T: serde::de::DeserializeOwned + Send>(
    server: &mut Server,
    mut client: UnixStream,
) -> Result<T> {
    client.set_read_timeout(Some(Duration::from_secs(3)))?;
    thread::scope(|scope| {
        let reader = scope.spawn(move || wire::read(&mut client));
        while !reader.is_finished() {
            server.step()?;
        }
        Ok(reader.join().unwrap()?)
    })
}

#[test]
fn malformed_request_receives_a_typed_error_and_does_not_stop_daemon() -> anyhow::Result<()> {
    use std::io::Write;

    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let mut server = open_for_test(dir.path(), &socket)?;
    let mut client = UnixStream::connect(&socket)?;
    client.write_all(&u32::MAX.to_be_bytes())?;
    assert!(matches!(
        read_with_server::<Response>(&mut server, client)?,
        Response::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
    assert!(matches!(
        status(&mut server, socket.clone(), Request::Status),
        Response::Status(_)
    ));
    Ok(())
}

fn private_tempdir() -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
}

fn open_for_test(state_dir: &std::path::Path, socket_path: &std::path::Path) -> Result<Server> {
    Server::open_with_paths(
        state_dir,
        socket_path,
        WorkspacePaths::from_home(state_dir.to_owned()),
    )
}

#[test]
fn production_server_uses_paths_from_one_workspace() -> anyhow::Result<()> {
    let home = private_tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let socket = paths.socket_path();
    let server = Server::open(paths.clone(), crate::DEFAULT_WORKER_SLOTS)?;

    assert!(socket.exists());
    assert!(paths.state_dir().join("focus.sqlite3").exists());
    assert!(paths.state_dir().join("workflow.sqlite3").exists());
    drop(server);
    Ok(())
}

#[test]
fn daemon_checkpoints_without_any_connected_clients() -> anyhow::Result<()> {
    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let mut server = open_for_test(dir.path(), &socket)?;
    let now = Instant::now();
    let started = now - Duration::from_secs(6);
    let result = server
        .timer
        .apply(Request::Start { work_unit_id: None }, started)?;
    assert!(result.is_ok());
    server.step()?;
    drop(server);

    let mut server = open_for_test(dir.path(), &socket)?;
    let recovered = status(&mut server, socket, Request::Status);
    let Response::Status(recovered) = recovered else {
        panic!("status failed: {recovered:?}");
    };
    assert_eq!(recovered.state, TimerState::NeedsReview);
    assert!(recovered.session.unwrap().elapsed_seconds >= 5);
    Ok(())
}

#[test]
fn notification_inbox_keeps_terminal_runs_and_recovery_across_restarts() -> anyhow::Result<()> {
    use chrono::Utc;
    use mcbtodo_ports::workflow::WorkflowProgressStore;
    use mcbtodo_protocol::workflow::{RunState, WorkflowRequest};
    use mcbtodo_storage_sqlite::run_store::RunStore;
    use serde_json::{Value, json};
    use uuid::Uuid;

    let dir = private_tempdir()?;
    let socket = dir.path().join("s");
    let db = dir.path().join("workflow.sqlite3");
    let succeeded = Uuid::from_u128(1);
    let failed = Uuid::from_u128(2);
    let interrupted = Uuid::from_u128(3);
    let cancelled = Uuid::from_u128(4);
    let pending = Uuid::from_u128(5);
    let due_at = Utc::now() + chrono::TimeDelta::days(1);
    {
        let store = RunStore::open(&db)?;
        for id in [succeeded, failed, interrupted, cancelled, pending] {
            store.enqueue_if_absent(id, "revision", Some(due_at))?;
        }
        store.transition(
            &store.get(succeeded)?.unwrap(),
            RunState::Succeeded,
            None,
            None,
        )?;
        store.transition(
            &store.get(failed)?.unwrap(),
            RunState::Failed,
            None,
            Some("approval denied"),
        )?;
        store.transition(
            &store.get(interrupted)?.unwrap(),
            RunState::Running,
            Some("worker"),
            None,
        )?;
    }

    let list = |server: &mut Server| -> anyhow::Result<Value> {
        let mut client = UnixStream::connect(&socket)?;
        wire::write(
            &mut client,
            &json!({"command":"list_notifications","before":null}),
        )?;
        read_with_server(server, client)
    };
    let mut server = open_for_test(dir.path(), &socket)?;
    for _ in 0..2 {
        let reply = status(
            &mut server,
            socket.clone(),
            Request::Workflow(WorkflowRequest::Cancel { run_id: cancelled }),
        );
        assert!(matches!(reply, Response::Workflow(_)));
    }
    let inbox = list(&mut server)?;
    assert_eq!(inbox["result"], "notifications", "{inbox}");
    let entries = inbox["notifications"].as_array().unwrap();
    assert_eq!(entries.len(), 4);
    for (entry, (id, state)) in entries.iter().zip([
        (cancelled, "cancelled"),
        (interrupted, "outcome_unknown"),
        (failed, "failed"),
        (succeeded, "succeeded"),
    ]) {
        assert_eq!(entry["run_id"], id.to_string());
        assert_eq!(entry["state"], state);
        assert!(
            entry["finished_at"]
                .as_str()
                .unwrap()
                .parse::<chrono::DateTime<Utc>>()
                .is_ok()
        );
    }
    assert_eq!(entries[2]["detail"], "approval denied");
    assert!(inbox["next_before"].is_null());
    drop(server);

    let mut restarted = open_for_test(dir.path(), &socket)?;
    assert_eq!(list(&mut restarted)?, inbox);
    Ok(())
}
