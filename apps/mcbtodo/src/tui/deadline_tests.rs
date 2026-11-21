use super::{calendar_tests::screen, creation_tests::press, *};

#[test]
fn selected_outline_document_can_clear_its_deadline() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " ul");
    let id = state.selected_document(&vault).unwrap();
    assert!(vault.deadline(id).is_some());

    press(&mut state, &vault, " d");
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
    press(&mut state, &vault, " ul d");
    press(&mut state, &vault, "\u{7f}2026-02-30\n");
    assert!(matches!(state.overlay, Overlay::Deadline(_)));
    assert!(state.message.contains("expected a date"));
}

#[test]
fn selected_outline_document_can_set_a_new_date_deadline() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " ul d");
    let id = state.selected_document(&vault).unwrap();

    let Action::SetDeadline(submission) = press(&mut state, &vault, "\u{7f}2026-12-10\n") else {
        panic!("expected deadline update action");
    };
    assert_eq!(submission.id, id);
    assert_eq!(
        submission.deadline,
        Some(mcbtodo_domain::time::Deadline::Date {
            value: "2026-12-10".parse().unwrap()
        })
    );
}

#[test]
fn selected_outline_document_without_a_deadline_can_add_one() {
    let vault = tests::fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.switch(View::Outline, &vault);
    let (row, id) = project_outline(&vault)
        .into_iter()
        .enumerate()
        .find_map(|(row, item)| (vault.deadline(item.id) == Some(None)).then_some((row, item.id)))
        .expect("fixture has a deadline-capable document without a deadline");
    state.row = row;
    assert_eq!(state.selected_document(&vault), Some(id));
    assert_eq!(vault.deadline(id), Some(None));

    press(&mut state, &vault, " d");
    assert!(matches!(state.overlay, Overlay::Deadline(_)));

    let Action::SetDeadline(submission) = press(&mut state, &vault, "2026-12-10\n") else {
        panic!("expected deadline update action");
    };
    assert_eq!(submission.id, id);
    assert_eq!(
        submission.deadline,
        Some(mcbtodo_domain::time::Deadline::Date {
            value: "2026-12-10".parse().unwrap()
        })
    );
}
