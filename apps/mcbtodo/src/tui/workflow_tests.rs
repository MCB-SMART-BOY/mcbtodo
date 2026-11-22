use mcbtodo_protocol::workflow::{RunState, RunSummary, WorkflowResponse};
use serde_json::json;
use uuid::Uuid;

use super::{calendar_tests::screen, tests::fixture, *};

fn key(state: &mut State, vault: &Vault, code: KeyCode) -> Action {
    state.handle(KeyEvent::new(code, KeyModifiers::NONE), vault)
}

fn open(state: &mut State, vault: &Vault) {
    for ch in [' ', 'u'] {
        key(state, vault, KeyCode::Char(ch));
    }
    assert_eq!(
        key(state, vault, KeyCode::Char('f')),
        Action::Workflow(WorkflowRequest::List { before: None })
    );
}

fn run(id: u128, state: RunState, steps: u32) -> RunSummary {
    RunSummary {
        id: Uuid::from_u128(id),
        state,
        step_count: steps,
        current_node: (state == RunState::Waiting).then(|| "review".into()),
        revision: "revision-hash".into(),
        due_at: None,
        schedule_id: None,
        source_run_id: None,
        source_event_id: None,
        detail: None,
        resolution: None,
    }
}

fn open_confirmation(state: &mut State, vault: &Vault, choice: char, run: RunSummary) {
    let inspect = WorkflowRequest::Inspect { run_id: run.id };
    assert_eq!(
        key(state, vault, KeyCode::Char(choice)),
        Action::Workflow(inspect.clone())
    );
    state
        .workflow
        .apply(inspect, WorkflowResponse::Run(run))
        .unwrap();
}

#[test]
fn workflow_paginates_and_shows_recorded_steps_for_the_selected_run() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open(&mut state, &vault);
    let newer = run(2, RunState::Waiting, 0);
    let older = run(1, RunState::Succeeded, 2);
    state
        .workflow
        .apply(
            WorkflowRequest::List { before: None },
            WorkflowResponse::Runs {
                runs: vec![newer.clone()],
                next_before: Some(newer.id),
            },
        )
        .unwrap();
    assert!(screen(&state, &vault, 65, 22).contains("Waiting"));
    let Action::Workflow(page) = key(&mut state, &vault, KeyCode::Char('n')) else {
        panic!("expected an older page request");
    };
    assert_eq!(
        page,
        WorkflowRequest::List {
            before: Some(newer.id)
        }
    );
    state
        .workflow
        .apply(
            page,
            WorkflowResponse::Runs {
                runs: vec![older.clone()],
                next_before: None,
            },
        )
        .unwrap();
    key(&mut state, &vault, KeyCode::Down);
    let Action::Workflow(step) = key(&mut state, &vault, KeyCode::Enter) else {
        panic!("expected a step request");
    };
    assert_eq!(
        step,
        WorkflowRequest::Step {
            run_id: older.id,
            index: 0
        }
    );
    state
        .workflow
        .apply(
            step,
            WorkflowResponse::Step {
                step: json!({"trace": {"node": "read-input", "status": "success"}}),
            },
        )
        .unwrap();
    let trace = screen(&state, &vault, 65, 22);
    assert!(
        trace.contains("Step 1/2") && trace.contains("read-input"),
        "{trace}"
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Right),
        Action::Workflow(WorkflowRequest::Step {
            run_id: older.id,
            index: 1
        })
    );
    key(&mut state, &vault, KeyCode::Esc);
    assert!(screen(&state, &vault, 65, 22).contains("Succeeded"));
}

#[test]
fn short_workflow_trace_keeps_step_navigation_while_reading_wrapped_output() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open(&mut state, &vault);
    let run = run(1, RunState::Succeeded, 2);
    state
        .workflow
        .apply(
            WorkflowRequest::List { before: None },
            WorkflowResponse::Runs {
                runs: vec![run.clone()],
                next_before: None,
            },
        )
        .unwrap();
    let Action::Workflow(step) = key(&mut state, &vault, KeyCode::Enter) else {
        panic!("expected a step request");
    };
    state
        .workflow
        .apply(
            step,
            WorkflowResponse::Step {
                step: json!({"output": "中文 0123456789 流程完成"}),
            },
        )
        .unwrap();

    let top = screen(&state, &vault, 24, 5);
    assert!(top.contains("Step 1/2"));
    assert!(top.lines().last().unwrap().contains("Esc"));
    for _ in 0..2 {
        key(&mut state, &vault, KeyCode::PageDown);
    }
    let continuation = screen(&state, &vault, 24, 5);
    let row = continuation.lines().nth(2).unwrap();
    assert!(row.starts_with('│') && row.ends_with('│'), "{row}");
    assert!(row.contains("流程完成"), "{row}");
    key(&mut state, &vault, KeyCode::End);
    assert!(
        screen(&state, &vault, 24, 5)
            .lines()
            .nth(2)
            .unwrap()
            .contains('}')
    );
    key(&mut state, &vault, KeyCode::Home);
    assert_eq!(screen(&state, &vault, 24, 5), top);
    key(&mut state, &vault, KeyCode::Char('L'));
    assert!(
        screen(&state, &vault, 24, 5)
            .lines()
            .nth(2)
            .unwrap()
            .trim_matches(['│', ' '])
            .is_empty()
    );
    key(&mut state, &vault, KeyCode::Char('H'));
    assert!(
        screen(&state, &vault, 24, 5)
            .lines()
            .nth(2)
            .unwrap()
            .contains('{')
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Right),
        Action::Workflow(WorkflowRequest::Step {
            run_id: run.id,
            index: 1
        })
    );
    key(&mut state, &vault, KeyCode::Esc);
    assert!(screen(&state, &vault, 24, 5).contains("Succeeded"));
}

#[test]
fn workflow_confirmation_uses_the_displayed_run_and_node() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open(&mut state, &vault);
    let waiting = run(10, RunState::Waiting, 0);
    state
        .workflow
        .apply(
            WorkflowRequest::List { before: None },
            WorkflowResponse::Runs {
                runs: vec![waiting.clone(), run(9, RunState::Succeeded, 1)],
                next_before: None,
            },
        )
        .unwrap();
    open_confirmation(&mut state, &vault, 'a', waiting.clone());
    let prompt = screen(&state, &vault, 65, 22);
    assert!(prompt.contains("Approve workflow run?"), "{prompt}");
    assert!(prompt.contains(&waiting.id.to_string()) && prompt.contains("Node review"));
    assert_eq!(key(&mut state, &vault, KeyCode::Enter), Action::Continue);
    assert_eq!(key(&mut state, &vault, KeyCode::Down), Action::Continue);
    assert_eq!(key(&mut state, &vault, KeyCode::Esc), Action::Continue);
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('y')),
        Action::Continue
    );
    open_confirmation(&mut state, &vault, 'd', waiting.clone());
    let Action::Workflow(decision) = key(&mut state, &vault, KeyCode::Char('y')) else {
        panic!("expected a confirmed decision");
    };
    assert_eq!(
        decision,
        WorkflowRequest::Decide {
            run_id: waiting.id,
            node: "review".into(),
            approved: false,
            review: None,
        }
    );
    state
        .workflow
        .apply(
            decision,
            WorkflowResponse::Run(run(10, RunState::Pending, 1)),
        )
        .unwrap();
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('a')),
        Action::Continue
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('y')),
        Action::Continue
    );
    assert!(screen(&state, &vault, 65, 22).contains("Pending"));
}

#[test]
fn workflow_confirmation_reviews_the_entire_snapshot_in_a_narrow_terminal() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open(&mut state, &vault);
    let long_value = "0123456789界".repeat(25);
    let escaped_title = r#"title = "quote\" slash\\ newline\n tab\t""#;
    let waiting = run(10, RunState::Waiting, 0);
    let mut reviewed_run = waiting.clone();
    reviewed_run.detail = Some(format!(
        "Review task.create\n{escaped_title}\nbody = \"{long_value}END-OF-VALUE\"\n\
         raw = A\tB\u{1b}C\u{202e}D\nLAST-FIELD"
    ));
    let mut other = run(9, RunState::Waiting, 0);
    other.detail = Some("OTHER-RUN".into());
    state
        .workflow
        .apply(
            WorkflowRequest::List { before: None },
            WorkflowResponse::Runs {
                runs: vec![waiting.clone(), other],
                next_before: None,
            },
        )
        .unwrap();
    let review = reviewed_run.approval_review();
    assert!(review.is_some());
    open_confirmation(&mut state, &vault, 'a', reviewed_run);
    state.workflow.row = 1;
    let mut prompt = screen(&state, &vault, 26, 9);
    assert!(prompt.contains("Approve workflow run?"), "{prompt}");

    let mut reviewed = String::new();
    for _ in 0..100 {
        assert!(
            prompt.contains("y approve") && prompt.contains("Esc"),
            "{prompt}"
        );
        let mut rows = prompt.lines().skip(1).take(6).map(|line| {
            line.strip_prefix('│')
                .unwrap()
                .strip_suffix('│')
                .unwrap()
                .trim_end()
        });
        if prompt.contains("LAST-FIELD") {
            reviewed.extend(&mut rows);
            break;
        }
        reviewed.push_str(rows.next().unwrap());
        assert_eq!(key(&mut state, &vault, KeyCode::Down), Action::Continue);
        prompt = screen(&state, &vault, 26, 9);
    }
    for expected in [
        "Review task.create",
        escaped_title,
        &format!("{long_value}END-OF-VALUE"),
        r"raw = A\tB\u{1b}C\u{202e}D",
        "LAST-FIELD",
    ] {
        assert!(
            reviewed.contains(expected),
            "missing {expected} in {reviewed}"
        );
    }
    assert!(!reviewed.contains("OTHER-RUN"));
    assert_eq!(key(&mut state, &vault, KeyCode::End), Action::Continue);
    let bottom = screen(&state, &vault, 26, 9);
    assert!(bottom.contains("LAST-FIELD"), "{bottom}");
    assert_eq!(key(&mut state, &vault, KeyCode::PageDown), Action::Continue);
    assert_eq!(screen(&state, &vault, 26, 9), bottom);
    assert_eq!(key(&mut state, &vault, KeyCode::PageUp), Action::Continue);
    assert!(!screen(&state, &vault, 26, 9).contains("LAST-FIELD"));
    assert_eq!(key(&mut state, &vault, KeyCode::Home), Action::Continue);
    assert!(screen(&state, &vault, 26, 9).contains("Approve workflow run?"));
    assert_eq!(
        state.handle(
            KeyEvent::new_with_kind(KeyCode::Char('y'), KeyModifiers::NONE, KeyEventKind::Repeat),
            &vault,
        ),
        Action::Continue
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('y')),
        Action::Workflow(WorkflowRequest::Decide {
            run_id: waiting.id,
            node: "review".into(),
            approved: true,
            review,
        })
    );
}

#[test]
fn workflow_confirmation_advertises_remapped_scrolling_in_short_monochrome() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.keymap = keymap::Keymap::from_test_config("[keys]\nmove_down = 'J'\nmove_up = 'K'\n");
    open(&mut state, &vault);
    let waiting = run(10, RunState::Waiting, 0);
    state
        .workflow
        .apply(
            WorkflowRequest::List { before: None },
            WorkflowResponse::Runs {
                runs: vec![waiting.clone()],
                next_before: None,
            },
        )
        .unwrap();
    open_confirmation(&mut state, &vault, 'a', waiting);

    let draw = |state: &State| {
        let model = screen_model::ScreenModel::project(state, &vault);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(24, 5)).unwrap();
        let monochrome = theme::Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
        let buffer = terminal
            .draw(|frame| render::draw(frame, &model, monochrome))
            .unwrap()
            .buffer;
        assert_eq!(buffer[(23, 1)].symbol(), "│");
        let row = |y| -> String { (0..24).map(|x| buffer[(x, y)].symbol()).collect() };
        (row(1), row(4))
    };
    let (top, footer) = draw(&state);
    assert!(top.contains("Approve workflow run?"), "{top}");
    assert!(
        footer.contains("y approve") && footer.contains("Esc") && footer.contains("J/K"),
        "{footer}"
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('J')),
        Action::Continue
    );
    assert!(draw(&state).0.contains("Run"));
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('K')),
        Action::Continue
    );
    assert_eq!(draw(&state).0, top);
    assert_eq!(key(&mut state, &vault, KeyCode::Esc), Action::Continue);
    assert!(matches!(state.workflow.panel, workflow::Panel::Runs));
}

#[test]
fn workflow_confirmation_keeps_movement_safe_when_y_is_remapped() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.keymap = keymap::Keymap::from_test_config("[keys]\nmove_down = 'y'\nmove_up = 'K'\n");
    open(&mut state, &vault);
    let waiting = run(10, RunState::Waiting, 0);
    state
        .workflow
        .apply(
            WorkflowRequest::List { before: None },
            WorkflowResponse::Runs {
                runs: vec![waiting.clone()],
                next_before: None,
            },
        )
        .unwrap();
    open_confirmation(&mut state, &vault, 'd', waiting.clone());

    let prompt = screen(&state, &vault, 65, 8);
    let footer = prompt.lines().last().unwrap();
    assert!(
        footer.contains("Enter reject") && footer.contains("y/K"),
        "{footer}"
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('y')),
        Action::Continue
    );
    assert!(matches!(state.workflow.panel, workflow::Panel::Confirm(_)));
    assert_eq!(
        state.handle(
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat),
            &vault,
        ),
        Action::Continue
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Enter),
        Action::Workflow(WorkflowRequest::Decide {
            run_id: waiting.id,
            node: "review".into(),
            approved: false,
            review: None,
        })
    );
}
