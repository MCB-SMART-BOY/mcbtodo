use super::{calendar_tests::screen, creation_tests::press, *};

use mcbtodo_domain::time::Deadline;

fn open_deadline(state: &mut State, vault: &Workspace, input: &str) {
    let Action::OpenDeadline(id) = press(state, vault, input) else {
        panic!("expected deadline open action");
    };
    assert!(matches!(state.overlay, Overlay::Space(_)));
    state.open_deadline(vault, id);
}

#[cfg(target_os = "linux")]
use std::{os::unix::net::UnixListener, thread, time::Duration};

#[cfg(target_os = "linux")]
use mcbtodo_protocol::{ErrorCode, Response, wire};

#[cfg(target_os = "linux")]
fn listen(paths: &WorkspacePaths) -> Result<UnixListener> {
    use std::{fs, os::unix::fs::PermissionsExt};

    paths.initialize()?;
    let socket = paths.socket_path();
    fs::create_dir_all(socket.parent().unwrap())?;
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(target_os = "linux")]
fn outline(id: Id, title: &str) -> Response {
    Response::Outline {
        items: vec![OutlineItem {
            id,
            title: title.into(),
            depth: 0,
            project_id: Some(id),
            task_id: None,
            source_path: "projects/test/project.md".into(),
            revision: "outline revision".into(),
            blocked: false,
        }],
        more: false,
    }
}

#[cfg(target_os = "linux")]
fn serve(
    listener: UnixListener,
    exchanges: Vec<(Request, Option<Response>)>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        for (request, response) in exchanges {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            assert_eq!(wire::read::<Request>(&mut stream)?, request);
            if let Some(response) = response {
                wire::write(&mut stream, &response)?;
            }
        }
        Ok(())
    })
}

#[cfg(target_os = "linux")]
#[test]
fn deadline_editor_uses_daemon_context_for_a_document_absent_locally() -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let listener = listen(&paths)?;
    let id = Id::new_v4();
    let outline_request = Request::Outline {
        timezone: "UTC".into(),
    };
    let peer = serve(
        listener,
        vec![
            (outline_request.clone(), Some(outline(id, "Remote project"))),
            (
                Request::DeadlineContext { id },
                Some(Response::DeadlineContext {
                    revision: "deadline revision".into(),
                    deadline: Some(Deadline::Date {
                        value: "2026-12-10".parse()?,
                    }),
                }),
            ),
            (
                Request::SetDeadline {
                    id,
                    revision: "deadline revision".into(),
                    deadline: None,
                },
                Some(Response::Error {
                    code: ErrorCode::StaleRevision,
                    message: "Document changed".into(),
                }),
            ),
            (
                Request::DeadlineContext { id },
                Some(Response::DeadlineContext {
                    revision: "fresh revision".into(),
                    deadline: Some(Deadline::Date {
                        value: "2026-12-11".parse()?,
                    }),
                }),
            ),
            (
                Request::SetDeadline {
                    id,
                    revision: "fresh revision".into(),
                    deadline: None,
                },
                Some(Response::DeadlineSet { synced: true }),
            ),
            (outline_request, Some(outline(id, "Refreshed project"))),
        ],
    );
    let root = paths.vault_root();
    let vault = Workspace::load(&root, chrono_tz::UTC)?;
    assert!(vault.local().deadline(id).is_none());
    let mut state = State::new("2026-11-25".parse()?);
    state.view = View::Outline;
    let mut vault = Workspace::daemon(vault.timezone(), paths.clone());
    open_deadline(&mut state, &vault, " d");
    assert!(
        matches!(&state.overlay, Overlay::Deadline(form) if form.value == "2026-12-10"),
        "{}",
        state.message
    );
    let Action::SetDeadline(submission) = press(&mut state, &vault, "\u{7f}\n") else {
        panic!("expected deadline clear action");
    };
    state.set_deadline(&root, &mut vault, submission);
    assert!(state.message.contains("press r"), "{}", state.message);
    assert_eq!(press(&mut state, &vault, "\n"), Action::Continue);
    assert_eq!(press(&mut state, &vault, "r"), Action::RefreshDeadline);
    assert!(matches!(&state.overlay, Overlay::Deadline(form) if form.needs_refresh));
    state.refresh_deadline(&root, &vault);
    assert!(
        matches!(&state.overlay, Overlay::Deadline(form) if form.cleared && form.value.is_empty()),
        "{}",
        state.message
    );
    let Action::SetDeadline(submission) = press(&mut state, &vault, "\n") else {
        panic!("expected deadline clear action after refresh");
    };
    state.set_deadline(&root, &mut vault, submission);
    assert!(matches!(state.overlay, Overlay::None));
    assert_eq!(state.message, "Deadline saved");
    assert_eq!(state.outline_rows(&vault)?[0].title, "Refreshed project");
    peer.join().unwrap()?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn daemon_deadline_keeps_timestamp_and_invalidates_views_after_an_uncertain_write() -> Result<()> {
    use mcbtodo_domain::time::ZonedMoment;

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let listener = listen(&paths)?;
    let id = Id::new_v4();
    let timestamp = Deadline::DateTime(ZonedMoment::new(
        "2026-12-10T23:30:00-08:00".parse()?,
        chrono_tz::America::Los_Angeles,
    )?);
    let outline_request = Request::Outline {
        timezone: "UTC".into(),
    };
    let peer = serve(
        listener,
        vec![
            (outline_request.clone(), Some(outline(id, "Remote project"))),
            (
                Request::DeadlineContext { id },
                Some(Response::DeadlineContext {
                    revision: "deadline revision".into(),
                    deadline: Some(timestamp),
                }),
            ),
            (
                Request::SetDeadline {
                    id,
                    revision: "deadline revision".into(),
                    deadline: Some(timestamp),
                },
                None,
            ),
            (outline_request, Some(outline(id, "Updated project"))),
        ],
    );
    let root = paths.vault_root();
    let vault = Workspace::load(&root, chrono_tz::UTC)?;
    let mut state = State::new("2026-11-25".parse()?);
    state.view = View::Outline;
    let mut vault = Workspace::daemon(vault.timezone(), paths);
    open_deadline(&mut state, &vault, " d");
    assert!(matches!(&state.overlay, Overlay::Deadline(form) if form.value == "2026-12-11"));
    let Action::SetDeadline(submission) = press(&mut state, &vault, "\n") else {
        panic!("expected deadline submission");
    };
    state.set_deadline(&root, &mut vault, submission);
    assert!(matches!(state.overlay, Overlay::None));
    assert!(
        state.message.contains("outcome unknown"),
        "{}",
        state.message
    );
    assert_eq!(state.outline_rows(&vault)?[0].title, "Updated project");
    peer.join().unwrap()?;
    Ok(())
}

fn write_project(root: &Path, id: Id) -> Result<std::path::PathBuf> {
    let document = root.join("projects/test/project.md");
    std::fs::create_dir_all(document.parent().unwrap())?;
    std::fs::write(
        &document,
        format!("---\nschema: mcbtodo.project/v1\nid: {id}\n---\n# Test\n"),
    )?;
    Ok(document)
}

#[cfg(target_os = "linux")]
#[test]
fn default_deadline_editor_never_falls_back_to_a_local_document() -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    let listener = listen(&paths)?;
    let root = paths.vault_root();
    let id = Id::new_v4();
    let document = write_project(&root, id)?;
    let before = std::fs::read(&document)?;
    let vault = Workspace::load(&root, chrono_tz::UTC)?;
    let peer = serve(
        listener,
        vec![
            (
                Request::Outline {
                    timezone: "UTC".into(),
                },
                Some(outline(id, "Test")),
            ),
            (
                Request::DeadlineContext { id },
                Some(Response::DeadlineContext {
                    revision: vault.local().revision(id).unwrap().digest(),
                    deadline: None,
                }),
            ),
        ],
    );
    let mut state = State::new("2026-11-25".parse()?);
    assert!(mcbtodo_client::daemon_client::owns_vault(&paths, &root)?);
    let mut vault = Workspace::daemon(vault.timezone(), paths);
    state.view = View::Outline;
    open_deadline(&mut state, &vault, " d");
    peer.join().unwrap()?;

    let Action::SetDeadline(submission) = press(&mut state, &vault, "2026-12-10\n") else {
        panic!("expected deadline submission");
    };
    state.set_deadline(&root, &mut vault, submission);
    assert!(matches!(state.overlay, Overlay::Deadline(_)));
    assert!(
        state.message.contains("Start mcbtodo-daemon"),
        "{}",
        state.message
    );
    assert_eq!(std::fs::read(document)?, before);

    open_deadline(&mut state, &vault, "\x1b d");
    assert!(matches!(state.overlay, Overlay::Space(_)));
    assert!(
        state.message.contains("Start mcbtodo-daemon"),
        "{}",
        state.message
    );
    Ok(())
}

#[test]
fn external_deadline_editor_persists_locally() -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let external = tempfile::tempdir()?;
    let root = external.path();
    let id = Id::new_v4();
    write_project(root, id)?;
    assert!(!mcbtodo_client::daemon_client::owns_vault(&paths, root)?);
    let mut vault = Workspace::load(root, chrono_tz::UTC)?;
    let mut state = State::new("2026-11-25".parse()?);
    state.view = View::Outline;

    open_deadline(&mut state, &vault, " d");
    let Action::SetDeadline(submission) = press(&mut state, &vault, "2026-12-10\n") else {
        panic!("expected deadline submission");
    };
    state.set_deadline(root, &mut vault, submission);
    assert!(matches!(state.overlay, Overlay::None));
    assert_eq!(state.message, "Deadline saved");
    assert_eq!(
        Vault::load(root, chrono_tz::UTC)?.deadline(id),
        Some(Some(Deadline::Date {
            value: "2026-12-10".parse()?
        }))
    );
    Ok(())
}

#[test]
fn local_deadline_refresh_uses_current_revision_without_discarding_the_draft() -> Result<()> {
    let external = tempfile::tempdir()?;
    let root = external.path();
    let id = Id::new_v4();
    write_project(root, id)?;
    let mut vault = Workspace::load(root, chrono_tz::UTC)?;
    let mut state = State::new("2026-11-25".parse()?);
    state.view = View::Outline;
    open_deadline(&mut state, &vault, " d");

    let new_deadline = Deadline::Date {
        value: "2026-12-09".parse()?,
    };
    VaultCommands::new(root, vault.timezone()).set_deadline(
        id,
        Some(new_deadline),
        vault.local().revision(id).unwrap().clone(),
    )?;
    let Action::SetDeadline(submission) = press(&mut state, &vault, "2026-12-10\n") else {
        panic!("expected deadline submission");
    };
    state.set_deadline(root, &mut vault, submission);
    assert!(state.message.contains("press r"), "{}", state.message);
    assert_eq!(press(&mut state, &vault, "\n"), Action::Continue);

    assert_eq!(press(&mut state, &vault, "r"), Action::RefreshDeadline);
    state.refresh_deadline(root, &vault);
    assert!(
        matches!(&state.overlay, Overlay::Deadline(form) if !form.needs_refresh && form.value == "2026-12-10"),
        "{}",
        state.message
    );
    let Action::SetDeadline(submission) = press(&mut state, &vault, "\n") else {
        panic!("expected deadline submission after refreshing");
    };
    state.set_deadline(root, &mut vault, submission);
    assert_eq!(
        vault.local().deadline(id),
        Some(Some(Deadline::Date {
            value: "2026-12-10".parse()?
        }))
    );
    Ok(())
}

#[test]
fn selected_outline_document_can_clear_its_deadline() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " ul");
    let id = state.selected_document(&vault).unwrap();
    assert!(vault.local().deadline(id).is_some());

    open_deadline(&mut state, &vault, " d");
    let rendered = screen(&state, &vault, 80, 14);
    assert!(rendered.contains("Deadline ·"), "{rendered}");
    assert!(rendered.contains("Delete clears"), "{rendered}");

    let Action::SetDeadline(submission) = press(&mut state, &vault, "\u{7f}\n") else {
        panic!("expected deadline update action");
    };
    assert_eq!(submission.id, id);
    assert_eq!(submission.deadline, None);
}

#[test]
fn selected_outline_document_keeps_invalid_deadline_input_open() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open_deadline(&mut state, &vault, " ul d");
    press(&mut state, &vault, "\u{7f}2026-02-30\n");
    assert!(matches!(state.overlay, Overlay::Deadline(_)));
    assert!(state.message.contains("expected a date"));
}

#[test]
fn deadline_editor_keeps_input_feedback_and_actions_visible_at_28_by_5() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open_deadline(&mut state, &vault, " ul d");
    press(&mut state, &vault, "\u{7f}2026-02-30\n");

    let rendered = screen(&state, &vault, 28, 5);
    let lines: Vec<_> = rendered.lines().collect();
    assert!(lines[0].contains("INSERT"), "{rendered}");
    assert!(lines[1].contains("> Due  2026-02-30▏"), "{rendered}");
    assert!(lines[3].contains("expected a date"), "{rendered}");
    assert!(
        lines[4].contains("Enter save  Del clear  Esc"),
        "{rendered}"
    );
}

#[test]
fn selected_outline_document_can_set_a_new_date_deadline() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open_deadline(&mut state, &vault, " ul d");
    let id = state.selected_document(&vault).unwrap();

    let Action::SetDeadline(submission) = press(&mut state, &vault, "\u{7f}2026-12-10\n") else {
        panic!("expected deadline update action");
    };
    assert_eq!(submission.id, id);
    assert_eq!(
        submission.deadline,
        Some(Deadline::Date {
            value: "2026-12-10".parse().unwrap()
        })
    );
}

#[test]
fn selected_outline_document_without_a_deadline_can_add_one() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.switch(View::Outline, &vault);
    let (row, id) = mcbtodo_application::project_outline(vault.local())
        .into_iter()
        .enumerate()
        .find_map(|(row, item)| {
            (vault.local().deadline(item.id) == Some(None)).then_some((row, item.id))
        })
        .expect("fixture has a deadline-capable document without a deadline");
    state.row = row;
    assert_eq!(state.selected_document(&vault), Some(id));
    assert_eq!(vault.local().deadline(id), Some(None));

    open_deadline(&mut state, &vault, " d");
    assert!(matches!(state.overlay, Overlay::Deadline(_)));

    let Action::SetDeadline(submission) = press(&mut state, &vault, "2026-12-10\n") else {
        panic!("expected deadline update action");
    };
    assert_eq!(submission.id, id);
    assert_eq!(
        submission.deadline,
        Some(Deadline::Date {
            value: "2026-12-10".parse().unwrap()
        })
    );
}
