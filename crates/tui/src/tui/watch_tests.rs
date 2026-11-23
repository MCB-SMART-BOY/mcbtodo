use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use super::{watch::Watcher, *};

const PROJECT: &str = include_str!("../../../../tests/fixtures/vault/projects/rust-os/project.md");
const EVENT: &str = include_str!("../../../../tests/fixtures/vault/calendar/review.md");

fn session(root: &Path) -> (Watcher, State, Workspace) {
    let vault = Workspace::load(root, "Asia/Shanghai".parse().unwrap()).unwrap();
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

fn await_refresh(watch: &mut Watcher, root: &Path, state: &mut State, vault: &mut Workspace) {
    await_change(watch);
    assert!(state.refresh_external(root, vault));
}

fn rendered_text(
    terminal: &mut Terminal<ratatui::backend::TestBackend>,
    state: &State,
    vault: &Workspace,
) -> String {
    let model = session::present(state, vault);
    terminal
        .draw(|frame| render::draw(frame, &model, theme::Theme::default()))
        .unwrap()
        .buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn connected_reload_reconciles_outline_focus_without_reading_the_local_vault() -> Result<()> {
    use std::os::unix::net::UnixListener;

    use mcbtodo_protocol::wire;

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let unavailable_root = home.path().join("missing-vault");
    let rows = mcbtodo_application::project_outline(tests::fixture().local());
    let mut reversed = rows.clone();
    reversed.reverse();
    let selected = rows[0].id;

    let listener = UnixListener::bind(paths.socket_path())?;
    let peer = thread::spawn(move || -> Result<()> {
        for rows in [rows.clone(), reversed, rows] {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            assert!(matches!(wire::read(&mut stream)?, Request::Outline { .. }));
            wire::write_outline(&mut stream, rows)?;
        }
        Ok(())
    });

    let mut state = State::new("2026-11-20".parse()?);
    let mut vault = Workspace::open(&paths.vault_root(), chrono_tz::Asia::Shanghai, &paths)?;
    state.view = View::Outline;
    assert_eq!(state.selected_document(&vault), Some(selected));
    assert!(!unavailable_root.exists());
    assert!(state.refresh_external(&unavailable_root, &mut vault));
    assert!(state.vault_warning.is_none());
    assert!(state.row > 0);
    assert_eq!(state.selected_document(&vault), Some(selected));
    state.reload(&unavailable_root, &mut vault);
    assert_eq!(state.message, "Vault reloaded");
    assert_eq!(state.row, 0);
    assert_eq!(state.selected_document(&vault), Some(selected));
    peer.join().unwrap()?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn outline_keeps_input_responsive_and_discards_a_reply_after_invalidation() -> Result<()> {
    use std::{os::unix::net::UnixListener, sync::mpsc::channel};

    use mcbtodo_protocol::wire;

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let listener = UnixListener::bind(paths.socket_path())?;
    let original = mcbtodo_application::project_outline(tests::fixture().local());
    let selected = original[1].id;
    let mut superseded = original.clone();
    superseded[1].title = "Superseded title".into();
    let mut refreshed = original.clone();
    refreshed[1].title = "Refreshed title".into();
    refreshed.reverse();

    let accept = |listener: &UnixListener| -> Result<_> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        assert_eq!(
            wire::read::<Request>(&mut stream)?,
            Request::Outline {
                timezone: "UTC".into()
            }
        );
        Ok(stream)
    };
    let (started_tx, started_rx) = channel();
    let (release_tx, release_rx) = channel();
    let peer = thread::spawn(move || -> Result<()> {
        wire::write_outline(&mut accept(&listener)?, original)?;
        let mut stream = accept(&listener)?;
        started_tx.send(())?;
        release_rx.recv_timeout(Duration::from_secs(2))?;
        wire::write_outline(&mut stream, superseded)?;
        wire::write_outline(&mut accept(&listener)?, refreshed)?;
        Ok(())
    });

    let mut vault = Workspace::daemon(chrono_tz::UTC, paths);
    let Workspace::Daemon { views, .. } = &mut vault else {
        unreachable!()
    };
    views.enable_outline_loader();
    let mut state = State::new("2026-11-20".parse()?);
    state.view = View::Outline;
    let wait_for_reply = |state: &mut State, vault: &mut Workspace| {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if state.poll_outline(vault) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("Outline reply did not arrive");
    };
    assert!(calendar_tests::screen(&state, &vault, 80, 12).contains("Loading Outline"));
    wait_for_reply(&mut state, &mut vault);
    assert_eq!(
        creation_tests::press(&mut state, &vault, "j"),
        Action::Continue
    );
    assert_eq!(state.selected_document(&vault), Some(selected));

    state.refresh_daemon_views(&mut vault);
    started_rx.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(
        creation_tests::press(&mut state, &vault, ":time-block\n"),
        Action::Continue
    );
    assert!(matches!(state.overlay, Overlay::None));
    assert!(state.message.contains("Loading Outline"));
    assert_eq!(state.document_id, Some(selected));
    assert_eq!(
        creation_tests::press(&mut state, &vault, ":help\n"),
        Action::Continue
    );
    assert!(matches!(state.overlay, Overlay::Help(_)));
    state.refresh_daemon_views(&mut vault);
    release_tx.send(())?;
    wait_for_reply(&mut state, &mut vault);
    assert_eq!(state.document_id, Some(selected));
    assert!(matches!(state.overlay, Overlay::Help(_)));
    creation_tests::press(&mut state, &vault, "\x1b");
    let loading = calendar_tests::screen(&state, &vault, 80, 12);
    assert!(loading.contains("Loading Outline"), "{loading}");
    assert!(!loading.contains("Superseded title"), "{loading}");
    wait_for_reply(&mut state, &mut vault);
    assert_eq!(state.selected_document(&vault), Some(selected));
    assert_ne!(state.row, 1);
    assert!(calendar_tests::screen(&state, &vault, 80, 12).contains("Refreshed title"));
    peer.join().unwrap()?;
    Ok(())
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

    assert_eq!(vault.local().outline()[0].title, "External title");
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
    assert_eq!(vault.local().outline()[0].title, "Rust OS");
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
    assert_eq!(vault.local().outline()[0].title, "Rust OS");

    fs::write(
        root.join("projects/first/project.md"),
        PROJECT.replace("Rust OS", "New title"),
    )
    .unwrap();
    await_refresh(&mut watch, &root, &mut state, &mut vault);
    assert_eq!(vault.local().outline()[0].title, "New title");
}

#[test]
fn removes_deleted_calendar_markdown_from_projection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("calendar")).unwrap();
    let path = root.join("calendar/event.md");
    fs::write(&path, EVENT).unwrap();
    let (mut watch, mut state, mut vault) = session(root);
    assert_eq!(vault.local().calendar()[0].title, "远程讨论");

    fs::remove_file(path).unwrap();
    await_refresh(&mut watch, root, &mut state, &mut vault);
    assert!(vault.local().calendar().is_empty());
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
    assert_eq!(state.source(&vault).unwrap(), Some(path));
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
    assert_eq!(vault.local().outline()[0].title, "Saved title");
}
