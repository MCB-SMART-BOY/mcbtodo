use std::fs;

use super::{calendar_tests::screen, *};

const PROJECT: &str = include_str!("../../../../examples/vault/projects/rust-os/project.md");
const TITLE: &str = "整理笔记 Review ideas";

fn vault_with_inbox() -> (tempfile::TempDir, Vault, Id) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("projects/rust-os")).unwrap();
    fs::write(root.join("projects/rust-os/project.md"), PROJECT).unwrap();
    fs::create_dir_all(root.join("inbox")).unwrap();
    let id = Id::from_u128(0x90000000000040008000000000000001);
    fs::write(
        root.join(format!("inbox/{id}.md")),
        format!(
            "---\nschema: mcbtodo.inbox-entry/v1\nid: {id}\ncaptured_at: \"2026-11-20T08:00:00Z\"\ntags:\n  - idea\n---\n# {TITLE}\n\nKeep this note.\n"
        ),
    )
    .unwrap();
    let vault = Vault::load(root, "Asia/Shanghai".parse().unwrap()).unwrap();
    (temp, vault, id)
}

#[test]
fn inbox_view_lists_entries_and_triage_uses_the_observed_revision() {
    let (temp, mut vault, inbox_id) = vault_with_inbox();
    let mut state = State::new("2026-11-20".parse().unwrap());
    state.switch(View::Inbox, &vault);

    let text = screen(&state, &vault, 80, 18);
    assert!(text.contains(TITLE), "{text}");
    assert!(text.contains("#idea"), "{text}");
    assert!(text.contains("2026-11-20 16:00"), "{text}");

    state.handle(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        &vault,
    );
    assert!(matches!(state.overlay, Overlay::InboxProject { .. }));
    let action = state.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &vault);
    let project_id = vault.outline()[0].id;
    let Action::TriageInbox {
        inbox_id: selected,
        project_id: project,
        revision,
    } = action
    else {
        panic!("expected Inbox triage action");
    };
    assert_eq!(selected, inbox_id);
    assert_eq!(project, project_id);
    assert_eq!(revision, *vault.revision(inbox_id).unwrap());

    state.triage_inbox(temp.path(), &mut vault, inbox_id, project_id, revision);
    assert_eq!(vault.inbox_entries().count(), 1);
    assert_eq!(
        vault.inbox_entries().next().unwrap().data.project_id,
        Some(project_id)
    );
    let inbox_path = vault.source(inbox_id).unwrap();
    assert!(
        fs::read_to_string(inbox_path)
            .unwrap()
            .contains("Keep this note.")
    );
    assert_eq!(
        vault
            .outline()
            .into_iter()
            .find(|row| row.id == project_id)
            .unwrap()
            .title,
        "Rust OS"
    );
    assert_eq!(state.message, "Inbox entry converted to Task");
    let task_id = vault.inbox_entries().next().unwrap().data.task_id();
    assert!(vault.outline().iter().any(|row| row.id == task_id));
}

#[test]
fn inbox_keeps_title_and_assignment_controls_in_a_short_terminal() {
    let (_temp, vault, inbox_id) = vault_with_inbox();
    let mut state = State::new("2026-11-20".parse().unwrap());
    state.switch(View::Inbox, &vault);

    let text = screen(&state, &vault, 24, 5);
    let rows: Vec<_> = text.lines().collect();
    assert!(rows[2].starts_with("│› 整理笔记 Review"), "{text}");
    assert!(rows[2].ends_with('│'), "{text}");
    assert_eq!(state.selected_document(&vault), Some(inbox_id));

    state.handle(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        &vault,
    );
    let text = screen(&state, &vault, 24, 5);
    let rows: Vec<_> = text.lines().collect();
    assert!(rows[0].ends_with("PICKER"), "{text}");
    assert!(rows[2].contains("› Rust OS"), "{text}");
    assert!(
        rows[4].contains("Enter assign") && rows[4].contains("Esc"),
        "{text}"
    );
}

#[test]
fn inbox_picker_without_projects_offers_only_cancel() {
    let (temp, vault, _) = vault_with_inbox();
    fs::remove_dir_all(temp.path().join("projects/rust-os")).unwrap();
    let vault = Vault::load(temp.path(), vault.timezone()).unwrap();
    let mut state = State::new("2026-11-20".parse().unwrap());
    state.switch(View::Inbox, &vault);
    state.handle(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        &vault,
    );
    let text = screen(&state, &vault, 24, 5);
    let rows: Vec<_> = text.lines().collect();
    assert!(rows[2].contains("No Projects available"), "{text}");
    assert_eq!(rows[4].trim(), "Esc cancel");
}

#[test]
fn inbox_project_picker_cancels_without_writing() {
    let (_temp, vault, _) = vault_with_inbox();
    let mut state = State::new("2026-11-20".parse().unwrap());
    state.switch(View::Inbox, &vault);
    state.handle(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        &vault,
    );
    assert_eq!(
        state.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &vault),
        Action::Continue
    );
    assert!(matches!(state.overlay, Overlay::None));
    assert_eq!(vault.inbox_entries().count(), 1);
}

#[test]
fn inbox_triage_refuses_to_overwrite_an_external_edit() {
    let (temp, mut vault, inbox_id) = vault_with_inbox();
    let project_id = vault.outline()[0].id;
    let revision = vault.revision(inbox_id).unwrap().clone();
    let path = vault.source(inbox_id).unwrap().to_owned();
    let source = fs::read_to_string(&path).unwrap();
    fs::write(&path, source.replace(TITLE, "Edited elsewhere")).unwrap();

    let mut state = State::new("2026-11-20".parse().unwrap());
    state.triage_inbox(temp.path(), &mut vault, inbox_id, project_id, revision);

    assert!(state.message.contains("changed"));
    let current = Vault::load(temp.path(), vault.timezone()).unwrap();
    assert_eq!(
        current.inbox_entries().next().unwrap().title,
        "Edited elsewhere"
    );
    assert_eq!(
        current.inbox_entries().next().unwrap().data.project_id,
        None
    );
}
