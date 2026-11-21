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
    let mut vault = Vault::load(root.path(), ZONE).unwrap();
    let paths = mcbtodo_platform::WorkspacePaths::from_home(root.path().to_owned());
    std::fs::create_dir_all(paths.data_dir()).unwrap();
    std::os::unix::fs::symlink(root.path(), paths.vault_root()).unwrap();
    let mut state = State::new(day("2026-11-25"));
    let Action::CreateReminder(request) = press(
        &mut state,
        &vault,
        " crReview notes\t2026-11-25T09:30:00+08:00\n",
    ) else {
        panic!("expected reminder creation, got {}", state.message);
    };

    state.create_reminder(root.path(), &mut vault, &paths, request);

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
    state.create_task(root.path(), &mut vault, request);

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
    state.create_task(root.path(), &mut observed, request);
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
    let before = document_ids(vault);
    let Action::CreateCalendar(submission) = press(state, vault, input) else {
        panic!("expected a calendar submission, got {}", state.message);
    };
    state.create_calendar(root, vault, submission);
    assert!(matches!(state.overlay, Overlay::None), "{}", state.message);
    let id = state.selected_document(vault).expect("saved item focused");
    assert!(!before.contains(&id));
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
    assert!(state.agenda(&persisted).iter().any(|entry| entry.id == id));
    press(&mut state, &persisted, " um");
    assert!(screen(&state, &persisted, 80, 24).contains(" 1."));
    state.set_day(day("2027-03-02"));
    assert!(state.agenda(&persisted).is_empty());
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
    let entries = calendar::day(&persisted, day("2026-11-25"));
    let entry = entries.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(entry.title, "Campus event");
    assert_eq!(entry.kind, CalendarKind::Event);
    assert!(entry.busy);
    assert!(
        matches!(entry.occurrence, Occurrence::Span(Span::DateSpan(span))
        if span.start() == day("2026-11-24") && span.through() == day("2026-11-25"))
    );
    assert!(calendar::day(&persisted, day("2026-11-26")).is_empty());
    let rendered = screen(&state, &persisted, 80, 18);
    assert!(
        rendered.contains("All-day") && rendered.contains("Campus event"),
        "{rendered}"
    );
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
    let entries = state.agenda(&persisted);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
    assert_eq!(entries[0].title, "Send report");
    assert_eq!(entries[0].kind, CalendarKind::Deadline);
    assert!(!entries[0].busy);
    assert!(matches!(entries[0].occurrence,
        Occurrence::Deadline(mcbtodo_domain::time::Deadline::Date { value }) if value == day("2026-11-25")));
    assert!(calendar::day(&persisted, day("2026-11-24")).is_empty());
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
    state.create_calendar(&root.path().join("missing"), &mut vault, request);
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
    assert_eq!(observer.agenda(&observer_vault)[0].id, id);
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
