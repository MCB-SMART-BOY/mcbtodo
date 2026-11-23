use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use super::{watch::Watcher, *};

const PROJECT: &str = include_str!("../../../../tests/fixtures/vault/projects/rust-os/project.md");
const EVENT: &str = include_str!("../../../../tests/fixtures/vault/calendar/review.md");

fn session(root: &Path) -> (Watcher, State, Vault) {
    let vault = Vault::load(root, "Asia/Shanghai".parse().unwrap()).unwrap();
    let mut state = State::new("2026-11-20".parse().unwrap());
    state.switch(View::Outline, &vault);
    (Watcher::new(root).unwrap(), state, vault)
}

fn project(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("project.md");
    fs::write(&path, PROJECT).unwrap();
    path
}

fn await_change(watch: &mut Watcher) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if watch.changed().unwrap() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("no Vault change notification");
}

fn await_refresh(watch: &mut Watcher, root: &Path, state: &mut State, vault: &mut Vault) {
    await_change(watch);
    assert!(state.refresh_external(root, vault));
}

fn rendered_text(
    terminal: &mut Terminal<ratatui::backend::TestBackend>,
    state: &State,
    vault: &Vault,
) -> String {
    let model = screen_model::ScreenModel::project(state, vault);
    terminal
        .draw(|frame| render::draw(frame, &model, theme::Theme::default()))
        .unwrap()
        .buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn reloads_atomic_editor_save_and_retains_focus() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = project(&root.join("projects/first"));
    let (mut watch, mut state, mut vault) = session(root);
    let selected = state.selected_document(&vault);

    let stage = path.with_extension("tmp");
    fs::write(&stage, PROJECT.replace("Rust OS", "External title")).unwrap();
    fs::rename(stage, &path).unwrap();
    await_refresh(&mut watch, root, &mut state, &mut vault);

    assert_eq!(vault.outline()[0].title, "External title");
    assert_eq!(state.selected_document(&vault), selected);
}

#[test]
fn invalid_external_save_keeps_projection_and_repair_clears_warning() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = project(&root.join("projects/first"));
    let (mut watch, mut state, mut vault) = session(root);

    fs::write(&path, "broken Markdown").unwrap();
    await_refresh(&mut watch, root, &mut state, &mut vault);
    assert_eq!(vault.outline()[0].title, "Rust OS");
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(35, 14)).unwrap();
    let text = rendered_text(&mut terminal, &state, &vault);
    assert!(text.contains("Reload failed"));
    assert!(text.contains("Rust OS"));

    // Even restoring the exact old bytes must clear the failure.
    fs::write(&path, PROJECT).unwrap();
    await_refresh(&mut watch, root, &mut state, &mut vault);
    let text = rendered_text(&mut terminal, &state, &vault);
    assert!(!text.contains("Reload failed"));
}

#[test]
fn watches_populated_projects_directory_moved_into_vault() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    fs::create_dir(&root).unwrap();
    let staged = temp.path().join("staged");
    project(&staged.join("first"));
    let (mut watch, mut state, mut vault) = session(&root);

    fs::rename(staged, root.join("projects")).unwrap();
    await_refresh(&mut watch, &root, &mut state, &mut vault);
    assert_eq!(vault.outline()[0].title, "Rust OS");

    fs::write(
        root.join("projects/first/project.md"),
        PROJECT.replace("Rust OS", "New title"),
    )
    .unwrap();
    await_refresh(&mut watch, &root, &mut state, &mut vault);
    assert_eq!(vault.outline()[0].title, "New title");
}

#[test]
fn removes_deleted_calendar_markdown_from_projection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("calendar")).unwrap();
    let path = root.join("calendar/event.md");
    fs::write(&path, EVENT).unwrap();
    let (mut watch, mut state, mut vault) = session(root);
    assert_eq!(vault.calendar()[0].title, "远程讨论");

    fs::remove_file(path).unwrap();
    await_refresh(&mut watch, root, &mut state, &mut vault);
    assert!(vault.calendar().is_empty());
}

#[test]
fn external_recurring_event_edits_refresh_the_calendar_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("calendar")).unwrap();
    let path = root.join("calendar/review.md");
    let event = "\
---
schema: mcbtodo.calendar-event/v1
id: 00000000-0000-4000-8000-000000000001
when: {kind: date_span, start: '2026-11-20', through: '2026-11-20'}
recurrence:
  start: '2026-11-20'
  frequency: weekly
  dst: {gap: skip, fold: earlier}
---

# Weekly review
";
    fs::write(&path, event).unwrap();
    let (mut watch, mut state, mut vault) = session(root);
    state.switch(View::Day, &vault);

    fs::write(&path, event.replace("Weekly review", "Revised review")).unwrap();
    await_refresh(&mut watch, root, &mut state, &mut vault);

    assert_eq!(
        state.day_entries(&vault).unwrap()[0].title,
        "Revised review"
    );
    assert_eq!(state.source(&vault), Some(path.as_path()));
}

#[test]
fn unchanged_local_write_notification_keeps_action_feedback() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = project(&root.join("projects/first"));
    let (mut watch, mut state, mut vault) = session(root);

    fs::write(path, PROJECT.replace("Rust OS", "Saved title")).unwrap();
    state.reload(root, &mut vault);
    state.message = "Time block saved".into();
    await_change(&mut watch);
    assert!(!state.refresh_external(root, &mut vault));
    assert_eq!(state.message, "Time block saved");
    assert_eq!(vault.outline()[0].title, "Saved title");
}
