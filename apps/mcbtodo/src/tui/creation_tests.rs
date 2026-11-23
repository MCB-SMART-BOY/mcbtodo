use std::{fs, path::Path};

use mcbtodo_domain::{Id, Project, Task};
use mcbtodo_vault::{CaptureTask, CreateProject, VaultStore};

use super::{calendar_tests::screen, creation::Schedule, *};

const ZONE: chrono_tz::Tz = chrono_tz::Asia::Shanghai;

fn day(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

fn project_vault(root: &Path) -> (Vault, Id) {
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
    (Vault::load(root, ZONE).unwrap(), id)
}

pub(super) fn press(state: &mut State, vault: &Vault, input: &str) -> Action {
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
fn keyboard_creates_a_one_time_reminder_with_an_explicit_local_offset() {
    let root = tempfile::tempdir().unwrap();
    let paths = mcbtodo_platform::WorkspacePaths::from_home(root.path().to_owned());
    paths.initialize().unwrap();
    let vault_root = paths.vault_root();
    let mut vault = Vault::load(&vault_root, ZONE).unwrap();
    let mut state = State::new(day("2026-11-25"));
    let Action::CreateReminder(request) = press(
        &mut state,
        &vault,
        " crReview notes\t2026-11-25T09:30:00+08:00\n",
    ) else {
        panic!("expected reminder creation, got {}", state.message);
    };

    state.create_reminder(&vault_root, &mut vault, &paths, request);

    assert_eq!(state.message, "Reminder saved");
    assert!(matches!(state.overlay, Overlay::None));
    let reminders: Vec<_> = vault.reminders().collect();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].title, "Review notes");
    assert_eq!(
        reminders[0].data.schedule,
        mcbtodo_domain::recurrence::ReminderSchedule::Once {
            at: mcbtodo_domain::time::ZonedMoment::new(
                "2026-11-25T09:30:00+08:00".parse().unwrap(),
                ZONE,
            )
            .unwrap(),
        }
    );
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
            expected_revision: vault.revision(project_id).unwrap().clone(),
        })
        .unwrap();
    vault = Vault::load(root.path(), ZONE).unwrap();
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
fn default_task_capture_does_not_repeat_a_request_without_a_response() {
    let home = tempfile::tempdir().unwrap();
    let paths = mcbtodo_platform::WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let (mut vault, project_id) = project_vault(&root);
    let observed = vault.revision(project_id).unwrap().digest();
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
fn disconnect_after_request(
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

    let project_path = vault.source(project_id).unwrap().to_path_buf();
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
    let vault = Vault::load(root.path(), ZONE).unwrap();
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

fn save(state: &mut State, root: &Path, vault: &mut Vault, input: &str) -> Id {
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
    let mut vault = Vault::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-20"));
    let id = save(
        &mut state,
        root.path(),
        &mut vault,
        " cpLong-term research\n\n2027-03-01\n",
    );
    assert_eq!(state.day, day("2026-11-20"));
    let persisted = Vault::load(root.path(), ZONE).unwrap();
    assert!(persisted.outline().iter().any(|row| row.id == id));
    let entries = persisted.calendar();
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
    let mut vault = Vault::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-24"));
    let id = save(
        &mut state,
        root.path(),
        &mut vault,
        "vl ceCampus event\n\n\n",
    );
    let persisted = Vault::load(root.path(), ZONE).unwrap();
    let entries = calendar::day(&persisted, day("2026-11-25")).unwrap();
    let entry = entries.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(entry.title, "Campus event");
    assert_eq!(entry.kind, CalendarKind::Event);
    assert!(entry.busy);
    assert!(
        matches!(entry.occurrence, Occurrence::Span(Span::DateSpan(span))
        if span.start() == day("2026-11-24") && span.through() == day("2026-11-25"))
    );
    assert!(
        calendar::day(&persisted, day("2026-11-26"))
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
    let mut vault = Vault::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-30"));

    press(&mut state, &vault, "vl ce");
    let Action::CreateCalendar(request) = press(&mut state, &vault, "Year-end event\n\n\n") else {
        panic!("expected all-day event creation");
    };

    let paths = WorkspacePaths::from_home(root.path().to_owned());
    state.create_calendar(root.path(), &mut vault, &paths, request);
    assert_eq!(state.message, "All-day event saved");

    let persisted = Vault::load(root.path(), ZONE).unwrap();
    for selected_day in ["2026-11-30", "2026-12-01"] {
        assert!(
            calendar::day(&persisted, day(selected_day))
                .unwrap()
                .iter()
                .any(|entry| entry.title == "Year-end event" && entry.busy),
            "event should appear on {selected_day}"
        );
    }
    assert!(
        calendar::day(&persisted, day("2026-12-02"))
            .unwrap()
            .iter()
            .all(|entry| entry.title != "Year-end event"),
        "inclusive event range must not extend beyond December 1"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn default_all_day_event_keeps_its_draft_without_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let mut vault = Vault::load(&root, ZONE).unwrap();
    let mut state = State::new(day("2026-11-30"));
    let Action::CreateCalendar(request) = press(&mut state, &vault, "vl ceCampus event\n\n\n")
    else {
        panic!("expected all-day event creation");
    };

    state.create_calendar(&root, &mut vault, &paths, request);

    assert!(matches!(state.overlay, Overlay::CreateForm(_)));
    assert!(state.message.contains("Start mcbtodo-daemon"));
    assert!(screen(&state, &vault, 80, 24).contains("Campus event"));
    assert!(Vault::load(&root, ZONE).unwrap().calendar().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn default_all_day_event_reports_its_identity_without_replaying_a_lost_response() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let mut vault = Vault::load(&root, ZONE).unwrap();
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
    let mut vault = Vault::load(root.path(), ZONE).unwrap();
    let mut state = State::new(day("2026-11-24"));
    let id = save(&mut state, root.path(), &mut vault, "vl cdSend report\n\n");
    let persisted = Vault::load(root.path(), ZONE).unwrap();
    assert_eq!(persisted.outline().len(), 1);
    assert!(persisted.source(id).unwrap().is_file());
    let entries = state.agenda(&persisted).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
    assert_eq!(entries[0].title, "Send report");
    assert_eq!(entries[0].kind, CalendarKind::Deadline);
    assert!(!entries[0].busy);
    assert!(matches!(entries[0].occurrence,
        Occurrence::Deadline(mcbtodo_domain::time::Deadline::Date { value }) if value == day("2026-11-25")));
    assert!(
        calendar::day(&persisted, day("2026-11-24"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failed_vault_write_leaves_form_open_for_retry() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = Vault::load(root.path(), ZONE).unwrap();
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
    assert!(vault.calendar().is_empty());

    let id = save(&mut state, root.path(), &mut vault, "\n");
    assert_eq!(vault.calendar()[0].id, id);
}

#[test]
fn a_project_deadline_added_outside_the_tui_refreshes_an_open_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let mut observer_vault = Vault::load(root.path(), ZONE).unwrap();
    let mut writer_vault = Vault::load(root.path(), ZONE).unwrap();
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
