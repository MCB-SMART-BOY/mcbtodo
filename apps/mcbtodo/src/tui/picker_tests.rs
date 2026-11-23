use super::{calendar_tests::screen, tests::fixture, *};

fn press(state: &mut State, vault: &Vault, keys: &str) {
    for key in keys.chars() {
        let code = match key {
            '\n' => KeyCode::Enter,
            '\x1b' => KeyCode::Esc,
            character => KeyCode::Char(character),
        };
        state.handle(KeyEvent::new(code, KeyModifiers::NONE), vault);
    }
}

#[test]
fn global_picker_fuzzy_searches_and_focuses_the_selected_outline_document() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, "/rso");

    let text = screen(&state, &vault, 64, 20);
    assert!(
        text.contains("Picker") && text.contains("Rust OS"),
        "{text}"
    );

    press(&mut state, &vault, "\n");
    assert_eq!(state.view.label(), "Outline");
    assert!(state.source(&vault).unwrap().ends_with("project.md"));
}

#[test]
fn global_picker_matches_vault_source_paths_after_outline_projection() {
    let vault = fixture();
    let matches = picker::matches(&vault, None, "projects/rust-os/project.md").unwrap();

    assert_eq!(matches.len(), 1);
    assert!(matches[0].title.contains("Rust OS"));
}

#[cfg(target_os = "linux")]
#[test]
fn default_picker_opens_a_document_absent_from_the_local_snapshot() -> Result<()> {
    use std::{os::unix::net::UnixListener, thread};

    let vault = fixture();
    let temp = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(temp.path().to_owned());
    paths.initialize()?;
    let listener = UnixListener::bind(paths.socket_path())?;
    let id = Id::new_v4();
    let mut remote = project_outline(&vault).remove(0);
    remote.id = id;
    remote.project_id = Some(id);
    remote.title = "New from another client".into();
    remote.source_path = "projects/another-client/project.md".into();
    let peer = thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        assert!(matches!(
            mcbtodo_protocol::wire::read(&mut stream)?,
            mcbtodo_protocol::Request::Outline { .. }
        ));
        mcbtodo_protocol::wire::write_outline(&mut stream, vec![remote])?;
        Ok(())
    });

    let mut state = State::new("2026-11-25".parse().unwrap());
    state.daemon_views = Some(daemon_views::DaemonViews::new(paths));
    press(&mut state, &vault, ":search new from another client\n");
    let picker = screen(&state, &vault, 64, 20);
    assert!(picker.contains("› New from another client"), "{picker}");
    press(
        &mut state,
        &vault,
        "\x1b:search projects/another-client/project.md\n",
    );
    let picker = screen(&state, &vault, 64, 20);
    assert!(picker.contains("› New from another client"), "{picker}");
    press(&mut state, &vault, "\n");
    assert_eq!(state.view, View::Outline);
    assert_eq!(state.selected_document(&vault), Some(id));
    assert!(vault.source(id).is_none());
    peer.join().unwrap()?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn default_picker_reports_an_offline_daemon_and_can_be_closed() -> Result<()> {
    let vault = fixture();
    let temp = tempfile::tempdir()?;
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.daemon_views = Some(daemon_views::DaemonViews::new(WorkspacePaths::from_home(
        temp.path().to_owned(),
    )));

    press(&mut state, &vault, ":search rust\n");
    let picker = screen(&state, &vault, 80, 20);
    assert!(picker.contains("Start mcbtodo-daemon"), "{picker}");
    assert!(picker.contains("Esc cancel"), "{picker}");
    press(&mut state, &vault, "\n");
    assert!(matches!(state.overlay, Overlay::Picker { .. }));
    press(&mut state, &vault, "\x1b");
    assert!(matches!(state.overlay, Overlay::None));
    Ok(())
}

#[test]
fn global_picker_no_match_and_escape_preserve_the_current_focus() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.switch(View::Outline, &vault);
    let focused = state.selected_document(&vault);

    press(&mut state, &vault, "/not-a-document");
    assert!(screen(&state, &vault, 64, 20).contains("No matching documents"));
    press(&mut state, &vault, "\x1b");

    assert_eq!(state.selected_document(&vault), focused);
    assert_eq!(state.view.label(), "Outline");
}
