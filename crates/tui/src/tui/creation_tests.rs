#[cfg(target_os = "linux")]
use std::time::Duration;
use std::{fs, path::Path};

use mcbtodo_domain::{Id, Project, Task};
use mcbtodo_vault::{CaptureTask, CreateProject, VaultStore};

use super::{calendar_tests::screen, creation::Schedule, *};

const ZONE: chrono_tz::Tz = chrono_tz::Asia::Shanghai;

fn day(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

fn project_vault(root: &Path) -> (Workspace, Id) {
    let id = Id::new_v4();
    VaultStore::new(root, ZONE)
        .create_project(CreateProject {
            project: Project {
                id,
                status: Default::default(),
                active_period: None,
                deadline: None,
                tags: Vec::new(),
            },
            slug: Some("capture-test".into()),
            title: "Capture test".into(),
            body: String::new(),
        })
        .unwrap();
    (Workspace::load(root, ZONE).unwrap(), id)
}

pub(super) fn press(state: &mut State, vault: &Workspace, input: &str) -> Action {
    let mut action = Action::Continue;
    for character in input.chars() {
        assert_eq!(action, Action::Continue);
        let code = match character {
            '\n' => KeyCode::Enter,
            '\t' => KeyCode::Tab,
            '\x1b' => KeyCode::Esc,
            '\u{7f}' => KeyCode::Delete,
            character => KeyCode::Char(character),
        };
        action = state.handle(KeyEvent::new(code, KeyModifiers::NONE), vault);
    }
    action
}

#[test]
fn calendar_create_menu_advertises_each_keyboard_action() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    press(&mut state, &vault, " c");
    let rendered = screen(&state, &vault, 44, 14);
    for option in [
        "p  Project",
        "e  All-day event",
        "d  Deadline",
        "r  Reminder",
        "b  Time block",
    ] {
        assert!(rendered.contains(option), "{rendered}");
    }
}

#[test]
fn calendar_create_commands_use_the_selected_dates() {
    use mcbtodo_domain::time::DateSpan;

    let vault = tests::fixture();
    let start = day("2026-11-24");
    let through = day("2026-11-25");
    let span = DateSpan::new(start, through).unwrap();
    for (typed, menu, fields, expected) in [
        (
            ":create project\n",
            " cp",
            "Plan\t\t\n",
            Schedule::Project(span),
        ),
        (
            ":create event\n",
            " ce",
            "Plan\t\t\n",
            Schedule::Event(span),
        ),
        (
            ":create deadline\n",
            " cd",
            "Plan\t\n",
            Schedule::Deadline(through),
        ),
    ] {
        for entry in [typed, menu] {
            let mut state = State::new(through);
            state.anchor = Some(start);
            assert_eq!(press(&mut state, &vault, entry), Action::Continue);
            assert!(matches!(state.overlay, Overlay::CreateForm(_)), "{entry:?}");
            let Action::CreateCalendar(request) = press(&mut state, &vault, fields) else {
                panic!("{entry:?} did not submit: {}", state.message);
            };
            assert_eq!(request.title, "Plan");
            assert_eq!(request.schedule, expected);
        }
    }
}

#[test]
fn task_create_command_binds_the_selected_project_and_revision() {
    let root = tempfile::tempdir().unwrap();
    let (vault, project_id) = project_vault(root.path());
    let expected_revision = vault.local().revision(project_id).unwrap().clone();

    for entry in [":create task\n", " ct"] {
        let mut state = State::new(day("2026-11-25"));
        state.switch(View::Outline, &vault);
        assert_eq!(press(&mut state, &vault, entry), Action::Continue);
        assert!(matches!(state.overlay, Overlay::CreateForm(_)), "{entry:?}");
        let Action::CreateTask(request) = press(&mut state, &vault, "Write tests\n") else {
            panic!("{entry:?} did not submit: {}", state.message);
        };
        assert_eq!(request.title, "Write tests");
        assert_eq!(
            request.schedule,
            Schedule::Task {
                project_id,
                expected_revision: creation::ProjectRevision::Local(expected_revision.clone()),
            },
        );
    }

    let mut state = State::new(day("2026-11-25"));
    assert_eq!(
        press(&mut state, &vault, ":create task\n"),
        Action::Continue
    );
    assert!(matches!(state.overlay, Overlay::None));
    assert!(state.message.contains("Select a Project in Outline"));
}

#[test]
fn short_create_menu_reaches_the_last_action_and_keeps_its_shortcut() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    press(&mut state, &vault, " c");
    let rendered = screen(&state, &vault, 24, 5);
    assert!(rendered.contains("p  Project"), "{rendered}");
    assert!(rendered.contains("Esc back"), "{rendered}");

    state.handle(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &vault);
    let rendered = screen(&state, &vault, 24, 5);
    assert!(rendered.contains("b  Time block"), "{rendered}");
    assert!(
        rendered.lines().nth(2).unwrap().ends_with('│'),
        "{rendered}"
    );

    state.handle(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), &vault);
    let rendered = screen(&state, &vault, 24, 5);
    assert!(rendered.contains("p  Project"), "{rendered}");
    press(&mut state, &vault, "\x1b");
    assert!(matches!(state.overlay, Overlay::Space(_)));
    press(&mut state, &vault, "cb");
    assert!(matches!(state.overlay, Overlay::TimeBlock(_)));
}

#[test]
fn reminder_form_requires_an_explicit_matching_timezone_offset() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));

    assert_eq!(
        press(
            &mut state,
            &vault,
            " crReview notes\t2026-11-25T09:00:00+00:00\n",
        ),
        Action::Continue
    );
    assert!(state.message.contains("does not match Asia/Shanghai"));
    assert!(matches!(state.overlay, Overlay::CreateForm(_)));
}

#[test]
fn keyboard_submits_a_one_time_reminder_with_an_explicit_local_offset() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    let Action::CreateReminder(request) = press(
        &mut state,
        &vault,
        " crReview notes\t2026-11-25T09:30:00+08:00\n",
    ) else {
        panic!("expected reminder creation, got {}", state.message);
    };
    assert_eq!(request.title, "Review notes");
    let Schedule::Reminder { at, target_id } = request.schedule else {
        panic!("expected reminder schedule");
    };
    assert_eq!(at.instant().to_rfc3339(), "2026-11-25T01:30:00+00:00");
    assert!(target_id.is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn default_reminder_keeps_the_form_and_does_not_write_when_the_daemon_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let paths = mcbtodo_platform::WorkspacePaths::from_home(root.path().to_owned());
    paths.initialize().unwrap();
    let vault_root = paths.vault_root();
    let mut vault = Workspace::load(&vault_root, ZONE).unwrap();
    let mut state = State::new(day("2026-11-25"));
    let Action::CreateReminder(request) = press(
        &mut state,
        &vault,
        " crReview notes\t2026-11-25T09:30:00+08:00\n",
    ) else {
        panic!("expected reminder creation, got {}", state.message);
    };

    state.create_reminder(&vault_root, &mut vault, &paths, request);

    assert!(state.message.contains("Start mcbtodo-daemon"));
    assert!(matches!(state.overlay, Overlay::CreateForm(_)));
    assert_eq!(
        Vault::load(&vault_root, ZONE).unwrap().reminders().count(),
        0
    );
}

#[test]
#[cfg(target_os = "linux")]
fn reminder_response_loss_reports_uuid_without_replay_or_local_write() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let mut vault = Workspace::load(&root, ZONE).unwrap();
    let mut state = State::new(day("2026-11-25"));
    let Action::CreateReminder(request) = press(
        &mut state,
        &vault,
        " crReview notes\t2026-11-25T09:30:00+08:00\n",
    ) else {
        panic!("expected reminder creation");
    };
    let sent = disconnect_after_request(&paths, || {
        state.create_reminder(&root, &mut vault, &paths, request);
    });
    let mcbtodo_protocol::Request::CreateReminder {
        reminder_id,
        title,
        at,
        ..
    } = sent
    else {
        panic!("expected a reminder request");
    };
    assert_eq!(title, "Review notes");
    assert_eq!(at.to_rfc3339(), "2026-11-25T09:30:00+08:00");
    assert!(matches!(state.overlay, Overlay::None));
    assert!(state.message.contains("outcome unknown"));
    assert!(state.message.contains(&reminder_id.to_string()));
    assert_eq!(Vault::load(&root, ZONE).unwrap().reminders().count(), 0);
}

#[test]
fn reminder_from_selected_outline_task_keeps_its_target_binding() {
    let root = tempfile::tempdir().unwrap();
    let (mut vault, project_id) = project_vault(root.path());
    let task_id = Id::new_v4();
    VaultStore::new(root.path(), ZONE)
        .capture_task(CaptureTask {
            task: Task {
                id: task_id,
                project_id,
                status: Default::default(),
                priority: Default::default(),
                active_period: None,
                deadline: None,
                recurrence: None,
                occurrence: None,
                tags: Vec::new(),
            },
            title: "Prepare review".into(),
            body: String::new(),
            expected_revision: vault.local().revision(project_id).unwrap().clone(),
        })
        .unwrap();
    vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-25"));
    state.switch(View::Outline, &vault);
    state.vertical(true, &vault);

    let Action::CreateReminder(request) = press(
        &mut state,
        &vault,
        " crReview notes\t2026-11-25T09:30:00+08:00\n",
    ) else {
        panic!("expected reminder creation");
    };
    let Schedule::Reminder { target_id, .. } = request.schedule else {
        panic!("expected reminder schedule");
    };
    assert_eq!(target_id, Some(task_id));
}

#[cfg(target_os = "linux")]
#[test]
fn timer_and_reminder_use_the_selected_daemon_outline_target() -> Result<()> {
    use std::{os::unix::net::UnixListener, thread};

    use mcbtodo_protocol::wire;

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let vault = Workspace::load(&paths.vault_root(), ZONE)?;
    let rows = mcbtodo_application::project_outline(tests::fixture().local());
    let task = rows.iter().position(|row| row.depth == 1).unwrap();
    let unit = rows.iter().position(|row| row.depth == 2).unwrap();
    let (task_id, unit_id) = (rows[task].id, rows[unit].id);
    assert!(vault.local().revision(task_id).is_none() && vault.local().revision(unit_id).is_none());

    let listener = UnixListener::bind(paths.socket_path())?;
    let peer = thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        assert!(matches!(wire::read(&mut stream)?, Request::Outline { .. }));
        wire::write_outline(&mut stream, rows)?;
        Ok(())
    });
    let mut state = State::new(day("2026-11-25"));
    let vault = Workspace::daemon(vault.timezone(), paths);
    state.view = View::Outline;
    state.row = unit;
    assert_eq!(
        press(&mut state, &vault, " ts"),
        Action::Timer(Request::Start {
            work_unit_id: Some(unit_id),
        })
    );
    state.row = task;
    assert_eq!(
        press(&mut state, &vault, ":timer start\n"),
        Action::Timer(Request::Start { work_unit_id: None })
    );
    for entry in [" cr", ":reminder\n", ":create reminder\n"] {
        press(&mut state, &vault, entry);
        let Action::CreateReminder(request) = press(
            &mut state,
            &vault,
            "Review notes\t2026-11-25T09:30:00+08:00\n",
        ) else {
            panic!("expected reminder creation");
        };
        assert!(matches!(
            request.schedule,
            Schedule::Reminder { target_id: Some(id), .. } if id == task_id
        ));
        press(&mut state, &vault, "\x1b");
    }
    peer.join().unwrap()?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn unavailable_daemon_outline_does_not_drop_the_timer_or_reminder_binding() -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    state.view = View::Outline;
    state.row = vault
        .local()
        .outline()
        .iter()
        .position(|row| row.depth == 2)
        .unwrap();
    let vault = Workspace::daemon(vault.timezone(), paths);

    for view in [View::Outline, View::Day] {
        state.view = view;
        assert_eq!(press(&mut state, &vault, " ts"), Action::Continue);
        assert!(state.message.contains("Cannot load timer target"));
        assert_eq!(press(&mut state, &vault, " cr"), Action::Continue);
        assert!(state.message.contains("Cannot load reminder target"));
        assert!(matches!(state.overlay, Overlay::None));
    }
    Ok(())
}

#[test]
fn outline_project_menu_exposes_task_capture() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    press(&mut state, &vault, " ul c");
    assert!(state.view == View::Outline);
    let rendered = screen(&state, &vault, 60, 18);
    assert!(
        rendered.contains("t  Task in selected Project"),
        "{rendered}"
    );
}

#[test]
fn task_capture_publishes_under_selected_project_and_focuses_the_task() {
    let root = tempfile::tempdir().unwrap();
    let (mut vault, project_id) = project_vault(root.path());
    let mut state = State::new(day("2026-11-25"));
    state.switch(View::Outline, &vault);

    let Action::CreateTask(request) = press(&mut state, &vault, " ctWrite tests\n") else {
        panic!("expected task capture, got {}", state.message);
    };
    let paths = mcbtodo_platform::WorkspacePaths::from_home(root.path().to_owned());
    state.create_task(root.path(), &mut vault, &paths, request);

    assert!(state.message.contains("Task saved"), "{}", state.message);
    let task_id = state.selected_document(&vault).expect("new task focused");
    let row = vault
        .local()
        .outline()
        .into_iter()
        .find(|row| row.id == task_id)
        .expect("captured task in outline");
    assert_eq!(row.depth, 1);
    assert_eq!(row.title, "Write tests");
    let task_file = root
        .path()
        .join("projects/capture-test/tasks")
        .join(format!("{task_id}.md"));
    assert!(task_file.is_file());
    assert!(
        vault
            .local()
            .outline()
            .iter()
            .any(|row| row.id == project_id && row.depth == 0)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn default_task_capture_keeps_the_form_when_the_daemon_is_absent() {
    let home = tempfile::tempdir().unwrap();
    let paths = mcbtodo_platform::WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let (mut vault, _) = project_vault(&root);
    let mut state = State::new(day("2026-11-25"));
    state.switch(View::Outline, &vault);
    let Action::CreateTask(request) = press(&mut state, &vault, " ctWrite tests\n") else {
        panic!("expected task capture");
    };
    state.create_task(&root, &mut vault, &paths, request);
    assert!(matches!(state.overlay, Overlay::CreateForm(_)));
    assert!(state.message.contains("Start mcbtodo-daemon"));
    assert_eq!(Vault::load(&root, ZONE).unwrap().outline().len(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn task_capture_uses_the_revision_displayed_by_the_daemon_outline() -> Result<()> {
    use std::{os::unix::net::UnixListener, thread};

    use mcbtodo_domain::OutlineItem;
    use mcbtodo_protocol::{Request, Response, wire};

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let (vault, project_id) = project_vault(&root);
    let remote_revision = "a".repeat(64);
    assert_ne!(
        vault.local().revision(project_id).unwrap().digest(),
        remote_revision
    );

    let listener = UnixListener::bind(paths.socket_path())?;
    let row = OutlineItem {
        id: project_id,
        depth: 0,
        title: "Edited in another client".into(),
        source_path: "projects/project/project.md".into(),
        project_id: Some(project_id),
        task_id: None,
        blocked: false,
        revision: remote_revision.clone(),
    };
    let peer = thread::spawn(move || -> Result<Request> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        assert!(matches!(wire::read(&mut stream)?, Request::Outline { .. }));
        wire::write_outline(&mut stream, vec![row])?;

        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let sent = wire::read(&mut stream)?;
        wire::write(
            &mut stream,
            &Response::Error {
                code: mcbtodo_protocol::ErrorCode::StaleRevision,
                message: "the Project has changed since it was selected".into(),
            },
        )?;
        Ok(sent)
    });
    let mut state = State::new(day("2026-11-25"));
    let mut vault = Workspace::daemon(vault.timezone(), paths.clone());
    state.switch(View::Outline, &vault);
    let displayed = super::calendar_tests::screen(&state, &vault, 80, 16);
    assert!(
        displayed.contains("Edited in another client"),
        "{displayed}"
    );
    let Action::CreateTask(request) = press(&mut state, &vault, " ctWrite tests\n") else {
        panic!("expected Task capture");
    };
    state.create_task(&root, &mut vault, &paths, request);
    assert!(matches!(
        peer.join().unwrap()?,
        Request::CaptureTask { project_id: observed, project_revision, .. }
            if observed == project_id && project_revision == remote_revision
    ));
    assert!(matches!(state.overlay, Overlay::CreateForm(_)));
    assert!(state.message.contains("Project changed"));
    assert_eq!(Vault::load(&root, ZONE)?.outline().len(), 1);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn connected_task_capture_focuses_the_new_daemon_document() -> Result<()> {
    use std::{os::unix::net::UnixListener, thread};

    use mcbtodo_protocol::{Request, Response, wire};

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let root = paths.vault_root();
    let (vault, project_id) = project_vault(&root);
    let rows = mcbtodo_application::project_outline(vault.local());
    let listener = UnixListener::bind(paths.socket_path())?;
    let peer = thread::spawn(move || -> Result<Id> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        assert!(matches!(wire::read(&mut stream)?, Request::Outline { .. }));
        wire::write_outline(&mut stream, rows.clone())?;

        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let Request::CaptureTask { task_id, .. } = wire::read(&mut stream)? else {
            panic!("expected Task capture");
        };
        wire::write(
            &mut stream,
            &Response::TaskCaptured {
                task_id,
                synced: true,
            },
        )?;

        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        assert!(matches!(wire::read(&mut stream)?, Request::Outline { .. }));
        let mut task = rows[0].clone();
        task.id = task_id;
        task.depth = 1;
        task.task_id = Some(task_id);
        task.title = "Write tests".into();
        wire::write_outline(&mut stream, vec![rows[0].clone(), task])?;
        Ok(task_id)
    });

    let mut state = State::new(day("2026-11-25"));
    let mut vault = Workspace::daemon(vault.timezone(), paths.clone());
    state.switch(View::Outline, &vault);
    assert_eq!(state.selected_document(&vault), Some(project_id));
    let Action::CreateTask(request) = press(&mut state, &vault, " ctWrite tests\n") else {
        panic!("expected Task capture");
    };
    state.create_task(&root, &mut vault, &paths, request);
    let task_id = peer.join().unwrap()?;
    assert!(matches!(state.overlay, Overlay::None));
    assert_eq!(state.selected_document(&vault), Some(task_id));
    assert_eq!(state.message, "Task saved");
    assert!(Vault::load(&root, ZONE)?.source(task_id).is_none());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn default_task_capture_does_not_repeat_a_request_without_a_response() {
    let home = tempfile::tempdir().unwrap();
    let paths = mcbtodo_platform::WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let (mut vault, project_id) = project_vault(&root);
    let observed = vault.local().revision(project_id).unwrap().digest();
    let mut state = State::new(day("2026-11-25"));
    state.switch(View::Outline, &vault);
    let Action::CreateTask(request) = press(&mut state, &vault, " ctWrite tests\n") else {
        panic!("expected task capture");
    };

    let sent = disconnect_after_request(&paths, || {
        state.create_task(&root, &mut vault, &paths, request);
    });
    let mcbtodo_protocol::Request::CaptureTask {
        task_id,
        project_id: sent_project,
        project_revision,
        title,
    } = sent
    else {
        panic!("expected a Task request");
    };
    assert_eq!(sent_project, project_id);
    assert_eq!(project_revision, observed);
    assert_eq!(title, "Write tests");
    assert!(matches!(state.overlay, Overlay::None));
    assert!(state.message.contains("outcome unknown"));
    assert!(state.message.contains(&task_id.to_string()));
    assert_eq!(Vault::load(&root, ZONE).unwrap().outline().len(), 1);
}

#[cfg(target_os = "linux")]
pub(super) fn disconnect_after_request(
    paths: &WorkspacePaths,
    submit: impl FnOnce(),
) -> mcbtodo_protocol::Request {
    use std::{io::ErrorKind, os::unix::net::UnixListener, thread, time::Instant};

    let listener = UnixListener::bind(paths.socket_path()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut connection = loop {
            match listener.accept() {
                Ok((connection, _)) => break connection,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no creation request was sent");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accepting creation request: {error}"),
            }
        };
        connection
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let sent = mcbtodo_protocol::wire::read(&mut connection).unwrap();
        (sent, listener)
    });
    submit();
    let (sent, listener) = server.join().unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    sent
}

#[test]
fn task_capture_keeps_form_open_and_preserves_a_concurrent_project_edit() {
    let root = tempfile::tempdir().unwrap();
    let (vault, project_id) = project_vault(root.path());
    let mut state = State::new(day("2026-11-25"));
    state.switch(View::Outline, &vault);
    let Action::CreateTask(request) = press(&mut state, &vault, " ctTask title\n") else {
        panic!("expected task capture");
    };

    let project_path = vault.local().source(project_id).unwrap().to_path_buf();
    let before = fs::read(&project_path).unwrap();
    let mut edited = before.clone();
    edited.extend_from_slice(b"\nExternal edit\n");
    fs::write(&project_path, &edited).unwrap();

    let mut observed = vault;
    let paths = mcbtodo_platform::WorkspacePaths::from_home(root.path().to_owned());
    state.create_task(root.path(), &mut observed, &paths, request);
    assert!(matches!(state.overlay, Overlay::CreateForm(_)));
    assert!(
        state.message.contains("Project changed"),
        "{}",
        state.message
    );
    assert_eq!(fs::read(&project_path).unwrap(), edited);
    assert!(fs::read_dir(root.path().join("projects/capture-test/tasks")).is_err());
}

#[test]
fn project_form_defaults_to_the_inclusive_range_in_reverse_selection() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    press(&mut state, &vault, "vh cp");
    let rendered = screen(&state, &vault, 44, 14);
    assert!(rendered.contains("New project"), "{rendered}");
    assert!(rendered.contains("Start  2026-11-24"), "{rendered}");
    assert!(rendered.contains("Through  2026-11-25"), "{rendered}");
}

#[test]
fn invalid_project_range_keeps_fields_available_and_escape_cancels() {
    let root = tempfile::tempdir().unwrap();
    let vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-25"));
    assert_eq!(
        press(&mut state, &vault, " cpResearch\n\n2026-11-24\n"),
        Action::Continue
    );
    let rendered = screen(&state, &vault, 80, 14);
    assert!(rendered.contains("invalid endpoints"), "{rendered}");
    assert!(rendered.contains("Research"), "{rendered}");
    assert!(rendered.contains("Through  2026-11-24"), "{rendered}");
    press(&mut state, &vault, "\x1b");
    assert!(matches!(state.overlay, Overlay::None));
    assert!(Vault::load(root.path(), ZONE).unwrap().outline().is_empty());
}

#[test]
fn malformed_deadline_date_is_reported_instead_of_using_the_default() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    assert_eq!(
        press(&mut state, &vault, " cdSubmit notes\ninvalid\n"),
        Action::Continue
    );
    let rendered = screen(&state, &vault, 80, 14);
    assert!(rendered.contains("Due · expected a date"), "{rendered}");
    assert!(rendered.contains("Due  invalid"), "{rendered}");
}

fn save(state: &mut State, root: &Path, vault: &mut Workspace, input: &str) -> Id {
    let before = Vault::load(root, vault.timezone()).unwrap();
    let paths = WorkspacePaths::from_home(root.to_owned());
    let Action::CreateCalendar(submission) = press(state, vault, input) else {
        panic!("expected a calendar submission, got {}", state.message);
    };
    state.create_calendar(root, vault, &paths, submission);
    assert!(matches!(state.overlay, Overlay::None), "{}", state.message);
    let id = state.selected_document(vault).expect("saved item focused");
    assert!(before.revision(id).is_none());
    id
}

#[test]
fn keyboard_creates_a_long_term_project_as_a_nonbusy_active_period() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-20"));
    let id = save(
        &mut state,
        root.path(),
        &mut vault,
        " cpLong-term research\n\n2027-03-01\n",
    );
    assert_eq!(state.day, day("2026-11-20"));
    let persisted = Workspace::load(root.path(), ZONE).unwrap();
    assert!(persisted.local().outline().iter().any(|row| row.id == id));
    let entries = persisted.local().calendar();
    let entry = entries.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(entry.title, "Long-term research");
    assert_eq!(entry.kind, CalendarKind::ActivePeriod);
    assert!(!entry.busy);
    assert!(
        matches!(entry.occurrence, Occurrence::Span(Span::DateSpan(span))
        if span.start() == day("2026-11-20") && span.through() == day("2027-03-01"))
    );

    state.set_day(day("2027-03-01"));
    assert!(
        state
            .agenda(&persisted)
            .unwrap()
            .iter()
            .any(|entry| entry.id == id)
    );
    press(&mut state, &persisted, " um");
    assert!(screen(&state, &persisted, 80, 24).contains(" 1."));
    state.set_day(day("2027-03-02"));
    assert!(state.agenda(&persisted).unwrap().is_empty());
}

#[test]
fn keyboard_creates_a_busy_all_day_event_with_inclusive_date_precision() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-24"));
    let id = save(
        &mut state,
        root.path(),
        &mut vault,
        "vl ceCampus event\n\n\n",
    );
    let persisted = Workspace::load(root.path(), ZONE).unwrap();
    let entries = State::new(day("2026-11-25"))
        .day_entries(&persisted)
        .unwrap();
    let entry = entries.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(entry.title, "Campus event");
    assert_eq!(entry.kind, CalendarKind::Event);
    assert!(entry.busy);
    assert!(
        matches!(entry.occurrence, Occurrence::Span(Span::DateSpan(span))
        if span.start() == day("2026-11-24") && span.through() == day("2026-11-25"))
    );
    assert!(
        State::new(day("2026-11-26"))
            .day_entries(&persisted)
            .unwrap()
            .is_empty()
    );
    let rendered = screen(&state, &persisted, 80, 18);
    assert!(
        rendered.contains("All-day") && rendered.contains("Campus event"),
        "{rendered}"
    );
}

#[test]
fn month_selection_creates_an_inclusive_all_day_event_across_month_boundary() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-30"));

    press(&mut state, &vault, "vl ce");
    let Action::CreateCalendar(request) = press(&mut state, &vault, "Year-end event\n\n\n") else {
        panic!("expected all-day event creation");
    };

    let paths = WorkspacePaths::from_home(root.path().to_owned());
    state.create_calendar(root.path(), &mut vault, &paths, request);
    assert_eq!(state.message, "All-day event saved");

    let persisted = Workspace::load(root.path(), ZONE).unwrap();
    for selected_day in ["2026-11-30", "2026-12-01"] {
        assert!(
            State::new(day(selected_day))
                .day_entries(&persisted)
                .unwrap()
                .iter()
                .any(|entry| entry.title == "Year-end event" && entry.busy),
            "event should appear on {selected_day}"
        );
    }
    assert!(
        State::new(day("2026-12-02"))
            .day_entries(&persisted)
            .unwrap()
            .iter()
            .all(|entry| entry.title != "Year-end event"),
        "inclusive event range must not extend beyond December 1"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn default_calendar_creation_keeps_its_draft_without_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let mut vault = Workspace::load(&root, ZONE).unwrap();
    for (input, title) in [
        ("vl ceCampus event\n\n\n", "Campus event"),
        (" cpResearch project\n\n\n", "Research project"),
        (" cdSubmit report\n\n", "Submit report"),
    ] {
        let mut state = State::new(day("2026-11-30"));
        let Action::CreateCalendar(request) = press(&mut state, &vault, input) else {
            panic!("expected calendar creation for {title}");
        };
        state.create_calendar(&root, &mut vault, &paths, request);

        assert!(matches!(state.overlay, Overlay::CreateForm(_)));
        assert!(state.message.contains("Start mcbtodo-daemon"));
        assert!(screen(&state, &vault, 80, 24).contains(title));
    }
    let saved = Vault::load(&root, ZONE).unwrap();
    assert!(saved.calendar().is_empty());
    assert!(saved.outline().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn default_all_day_event_reports_its_identity_without_replaying_a_lost_response() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let mut vault = Workspace::load(&root, ZONE).unwrap();
    let mut state = State::new(day("2026-11-30"));
    let Action::CreateCalendar(request) = press(&mut state, &vault, "vl ceCampus event\n\n\n")
    else {
        panic!("expected all-day event creation");
    };

    let sent = disconnect_after_request(&paths, || {
        state.create_calendar(&root, &mut vault, &paths, request);
    });

    let mcbtodo_protocol::Request::CreateAllDayEvent {
        event_id,
        title,
        start,
        through,
    } = sent
    else {
        panic!("expected an all-day event request");
    };
    assert_eq!(title, "Campus event");
    assert_eq!(start, day("2026-11-30"));
    assert_eq!(through, day("2026-12-01"));
    assert!(matches!(state.overlay, Overlay::None));
    assert!(state.message.contains("outcome unknown"));
    assert!(state.message.contains(&event_id.to_string()));
    assert!(Vault::load(&root, ZONE).unwrap().calendar().is_empty());
}

#[test]
fn deadline_creation_publishes_a_project_with_no_busy_period() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-24"));
    let id = save(&mut state, root.path(), &mut vault, "vl cdSend report\n\n");
    let persisted = Workspace::load(root.path(), ZONE).unwrap();
    assert_eq!(persisted.local().outline().len(), 1);
    assert!(persisted.local().source(id).unwrap().is_file());
    let entries = state.agenda(&persisted).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
    assert_eq!(entries[0].title, "Send report");
    assert_eq!(entries[0].kind, CalendarKind::Deadline);
    assert!(!entries[0].busy);
    assert!(matches!(entries[0].occurrence,
        Occurrence::Deadline(mcbtodo_domain::time::Deadline::Date { value }) if value == day("2026-11-25")));
    assert!(
        State::new(day("2026-11-24"))
            .day_entries(&persisted)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failed_vault_write_leaves_form_open_for_retry() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-25"));
    let Action::CreateCalendar(request) = press(&mut state, &vault, " ceRetryable event\t\t\n")
    else {
        panic!("expected a calendar submission");
    };
    let paths = WorkspacePaths::from_home(root.path().to_owned());
    state.create_calendar(&root.path().join("missing"), &mut vault, &paths, request);
    let rendered = screen(&state, &vault, 80, 14);
    assert!(rendered.contains("failed"), "{rendered}");
    assert!(rendered.contains("Retryable event"), "{rendered}");
    assert!(vault.local().calendar().is_empty());

    let id = save(&mut state, root.path(), &mut vault, "\n");
    assert_eq!(vault.local().calendar()[0].id, id);
}

#[test]
fn a_project_deadline_added_outside_the_tui_refreshes_an_open_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let mut observer_vault = Workspace::load(root.path(), ZONE).unwrap();
    let mut writer_vault = Workspace::load(root.path(), ZONE).unwrap();
    let date = day("2026-11-25");
    let mut observer = State::new(date);
    let id = save(
        &mut State::new(date),
        root.path(),
        &mut writer_vault,
        " cdSend report\n\n",
    );

    assert!(observer.refresh_external(root.path(), &mut observer_vault));
    assert_eq!(observer.agenda(&observer_vault).unwrap()[0].id, id);
    assert!(!observer.refresh_external(root.path(), &mut observer_vault));
}

#[test]
fn both_menus_open_the_precise_time_block_picker() {
    let vault = tests::fixture();
    let mut state = State::new(day("2026-11-25"));
    for input in [" cbl映射", " tbl映射"] {
        press(&mut state, &vault, input);
        let rendered = screen(&state, &vault, 80, 14);
        assert!(rendered.contains("Time block · 2026-11-25"), "{rendered}");
        press(&mut state, &vault, "\x1b");
        assert!(matches!(state.overlay, Overlay::None));
    }
}
