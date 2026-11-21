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
    let matches = picker::matches(&vault, "projects/rust-os/project.md");

    assert_eq!(matches.len(), 1);
    assert!(matches[0].title.contains("Rust OS"));
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
