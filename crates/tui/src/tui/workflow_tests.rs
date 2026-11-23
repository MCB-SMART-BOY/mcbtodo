use crossterm::event::KeyEventKind;
use mcbtodo_client::daemon_client::WorkflowRunPage;
use mcbtodo_workflow::run::{RunState, RunSummary};
use serde_json::json;
use uuid::Uuid;

use super::{
    calendar_tests::screen,
    tests::fixture,
    workflow::{Operation, ReadRequest},
    *,
};

fn key(state: &mut State, vault: &Workspace, code: KeyCode) -> Action {
    state.handle(KeyEvent::new(code, KeyModifiers::NONE), vault)
}

fn prompt(state: &mut State, vault: &Workspace, command: &str) -> Action {
    for ch in command.chars() {
        assert_eq!(key(state, vault, KeyCode::Char(ch)), Action::Continue);
    }
    key(state, vault, KeyCode::Enter)
}

fn open(state: &mut State, vault: &Workspace) {
    for ch in [' ', 'u'] {
        key(state, vault, KeyCode::Char(ch));
    }
    assert_eq!(
        key(state, vault, KeyCode::Char('f')),
        ReadRequest::List { before: None }.action()
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

fn open_confirmation(state: &mut State, vault: &Workspace, choice: char, run: RunSummary) {
    let inspect = ReadRequest::Review { run_id: run.id };
    assert_eq!(key(state, vault, KeyCode::Char(choice)), inspect.action());
    state.workflow.review_decision(run).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn workflow_refresh_supersedes_an_older_page_without_blocking_input() -> anyhow::Result<()> {
    use std::{
        os::unix::net::UnixListener,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use mcbtodo_protocol::{
        Request, Response, wire,
        workflow::{WorkflowRequest, WorkflowResponse},
    };

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let listener = UnixListener::bind(paths.socket_path())?;
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse()?);
    open(&mut state, &vault);
    let first = run(3, RunState::Pending, 0);
    let selected = run(2, RunState::Pending, 0);
    let before = Some(selected.id);
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![first.clone(), selected.clone()],
            next_before: before,
        },
    );
    let ids = vec![selected.id, first.id];
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let peer = thread::spawn(move || -> anyhow::Result<()> {
        for (cursor, runs) in [
            (before, vec![run(1, RunState::Succeeded, 0)]),
            (None, vec![selected, first]),
        ] {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            assert!(matches!(
                wire::read(&mut stream)?,
                Request::Workflow(WorkflowRequest::List { before }) if before == cursor
            ));
            if cursor.is_some() {
                entered.send(())?;
                wait.recv_timeout(Duration::from_secs(2))?;
            }
            wire::write(
                &mut stream,
                &Response::Workflow(WorkflowResponse::Runs {
                    runs,
                    next_before: None,
                }),
            )?;
        }
        Ok(())
    });

    let Action::Workflow(older) = key(&mut state, &vault, KeyCode::Char('n')) else {
        panic!("expected an older-page action");
    };
    assert_eq!(state.workflow.execute(&paths, older), "");
    ready.recv_timeout(Duration::from_secs(2))?;
    assert!(screen(&state, &vault, 65, 22).contains("Loading runs"));
    assert_eq!(key(&mut state, &vault, KeyCode::Down), Action::Continue);
    assert_eq!(state.workflow.selected().unwrap().id, ids[0]);
    assert_eq!(key(&mut state, &vault, KeyCode::Char('q')), Action::Quit);
    let Action::Workflow(refresh) = key(&mut state, &vault, KeyCode::Char('r')) else {
        panic!("expected a refresh action");
    };
    assert_eq!(state.workflow.execute(&paths, refresh), "");
    release.send(())?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while state.workflow.is_loading_page() && Instant::now() < deadline {
        state.workflow.poll_read(&paths, true, &mut state.message);
        assert!(state.workflow.runs.iter().all(|run| ids.contains(&run.id)));
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!state.workflow.is_loading_page(), "page refresh stalled");
    peer.join().unwrap()?;
    let shown: Vec<_> = state.workflow.runs.iter().map(|run| run.id).collect();
    assert_eq!(shown, ids);
    assert_eq!(state.workflow.selected().unwrap().id, ids[0]);
    assert!(state.message.is_empty(), "{}", state.message);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn workflow_detail_reads_only_open_the_panel_that_requested_them() -> anyhow::Result<()> {
    use std::{
        os::unix::net::UnixListener,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use mcbtodo_protocol::{
        Request, Response, wire,
        workflow::{WorkflowRequest, WorkflowResponse},
    };

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let listener = UnixListener::bind(paths.socket_path())?;
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse()?);
    open(&mut state, &vault);
    let mut waiting = run(10, RunState::Waiting, 2);
    waiting.detail = Some("Review the loaded snapshot".into());
    let run_id = waiting.id;
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![run(10, RunState::Waiting, 2), run(9, RunState::Pending, 0)],
            next_before: None,
        },
    );

    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let reviewed = waiting.clone();
    let peer = thread::spawn(move || -> anyhow::Result<()> {
        for request in [
            WorkflowRequest::Step { run_id, index: 0 },
            WorkflowRequest::Step { run_id, index: 0 },
            WorkflowRequest::Inspect { run_id },
            WorkflowRequest::Inspect { run_id },
            WorkflowRequest::List { before: None },
            WorkflowRequest::Inspect { run_id },
        ] {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let Request::Workflow(received) = wire::read(&mut stream)? else {
                panic!("expected Workflow request");
            };
            assert_eq!(received, request);
            entered.send(())?;
            wait.recv_timeout(Duration::from_secs(2))?;
            let reply = match request {
                WorkflowRequest::Step { .. } => WorkflowResponse::Step {
                    step: json!({"output": "loaded trace"}),
                },
                WorkflowRequest::List { .. } => WorkflowResponse::Runs {
                    runs: vec![reviewed.clone()],
                    next_before: None,
                },
                _ => WorkflowResponse::Run(reviewed.clone()),
            };
            wire::write(&mut stream, &Response::Workflow(reply))?;
        }
        Ok(())
    });
    let request = |state: &mut State, code| -> anyhow::Result<()> {
        let Action::Workflow(request) = key(state, &vault, code) else {
            panic!("expected a Workflow request");
        };
        assert_eq!(state.workflow.execute(&paths, request), "");
        ready.recv_timeout(Duration::from_secs(2))?;
        Ok(())
    };
    let finish = |state: &mut State| -> anyhow::Result<()> {
        release.send(())?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if state
                .workflow
                .poll_read(&paths, state.view == View::Workflow, &mut state.message)
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
        anyhow::bail!("Workflow reply was never handled")
    };

    request(&mut state, KeyCode::Enter)?;
    assert!(screen(&state, &vault, 65, 22).contains("Loading step"));
    assert_eq!(key(&mut state, &vault, KeyCode::Down), Action::Continue);
    assert_eq!(key(&mut state, &vault, KeyCode::Up), Action::Continue);
    finish(&mut state)?;
    assert!(matches!(state.workflow.panel, workflow::Panel::Runs));

    request(&mut state, KeyCode::Enter)?;
    finish(&mut state)?;
    assert!(screen(&state, &vault, 65, 22).contains("loaded trace"));
    assert_eq!(key(&mut state, &vault, KeyCode::Esc), Action::Continue);

    request(&mut state, KeyCode::Char('a'))?;
    assert!(screen(&state, &vault, 65, 22).contains("Loading review"));
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('y')),
        Action::Continue
    );
    assert_eq!(key(&mut state, &vault, KeyCode::Esc), Action::Continue);
    finish(&mut state)?;
    assert!(matches!(state.workflow.panel, workflow::Panel::Runs));

    request(&mut state, KeyCode::Char('a'))?;
    assert_eq!(
        state.execute(Command::View(View::Month), &vault),
        Action::Continue
    );
    let Action::Workflow(refresh) = state.execute(Command::View(View::Workflow), &vault) else {
        panic!("expected a Workflow refresh");
    };
    assert_eq!(state.workflow.execute(&paths, refresh), "");
    finish(&mut state)?;
    assert!(matches!(state.workflow.panel, workflow::Panel::Runs));
    assert!(state.workflow.is_loading_page());
    ready.recv_timeout(Duration::from_secs(2))?;
    finish(&mut state)?;

    request(&mut state, KeyCode::Char('a'))?;
    finish(&mut state)?;
    assert!(screen(&state, &vault, 65, 22).contains("Review the loaded snapshot"));
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('y')),
        Action::Workflow(Operation::Decide {
            run_id,
            node: "review".into(),
            approved: true,
            review: waiting.approval_review(),
        }),
    );
    peer.join().unwrap()?;
    Ok(())
}

#[test]
fn workflow_prompt_and_keys_share_run_commands() {
    let vault = fixture();
    let mut waiting = run(10, RunState::Waiting, 1);
    waiting.detail = Some("Review task creation".into());
    let review = waiting.approval_review().expect("waiting run has a review");
    for (command, shortcut, request) in [
        (
            "refresh",
            KeyCode::Char('r'),
            ReadRequest::List { before: None },
        ),
        (
            "older",
            KeyCode::Char('n'),
            ReadRequest::List {
                before: Some(waiting.id),
            },
        ),
        (
            "trace",
            KeyCode::Enter,
            ReadRequest::Step {
                run_id: waiting.id,
                index: 0,
            },
        ),
        (
            "approve",
            KeyCode::Char('a'),
            ReadRequest::Review { run_id: waiting.id },
        ),
        (
            "reject",
            KeyCode::Char('d'),
            ReadRequest::Review { run_id: waiting.id },
        ),
    ] {
        let setup = || {
            let mut state = State::new("2026-11-25".parse().unwrap());
            open(&mut state, &vault);
            state.workflow.install_page(
                None,
                WorkflowRunPage {
                    runs: vec![waiting.clone()],
                    next_before: Some(waiting.id),
                },
            );
            state
        };
        let mut typed = setup();
        let action = prompt(&mut typed, &vault, &format!(":workflow {command}"));
        assert_eq!(action, request.action());
        let mut keyed = setup();
        assert_eq!(key(&mut keyed, &vault, shortcut), action);

        if matches!(request, ReadRequest::Review { .. }) {
            typed.workflow.review_decision(waiting.clone()).unwrap();
            assert_eq!(key(&mut typed, &vault, KeyCode::Enter), Action::Continue);
            let approved = command == "approve";
            assert_eq!(
                key(&mut typed, &vault, KeyCode::Char('y')),
                Action::Workflow(Operation::Decide {
                    run_id: waiting.id,
                    node: "review".into(),
                    approved,
                    review: if approved { Some(review.clone()) } else { None },
                }),
            );
        }
    }
}

#[test]
fn workflow_trace_commands_follow_the_visible_step() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open(&mut state, &vault);
    let run = run(1, RunState::Succeeded, 3);
    let run_id = run.id;
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![run],
            next_before: None,
        },
    );
    assert_eq!(
        prompt(&mut state, &vault, ":workflow step next"),
        Action::Continue
    );
    assert_eq!(state.message, "Open a Workflow trace first");
    state
        .workflow
        .show_step(run_id, 1, r#"{"output": "the visible trace"}"#.into())
        .unwrap();
    for (direction, shortcut, index) in [
        ("previous", KeyCode::Left, 0),
        ("next", KeyCode::Right, 2),
        ("refresh", KeyCode::Char('r'), 1),
    ] {
        let action = prompt(&mut state, &vault, &format!(":workflow step {direction}"));
        assert_eq!(action, ReadRequest::Step { run_id, index }.action());
        assert_eq!(key(&mut state, &vault, shortcut), action);
    }
    assert_eq!(
        prompt(&mut state, &vault, ":workflow approve"),
        Action::Continue
    );
    assert_eq!(state.message, "Open the Workflow runs list first");
}

#[test]
fn workflow_paginates_and_shows_recorded_steps_for_the_selected_run() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    open(&mut state, &vault);
    let newer = run(2, RunState::Waiting, 0);
    let older = run(1, RunState::Succeeded, 2);
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![newer.clone()],
            next_before: Some(newer.id),
        },
    );
    assert!(screen(&state, &vault, 65, 22).contains("Waiting"));
    assert_eq!(
        key(&mut state, &vault, KeyCode::Char('n')),
        ReadRequest::List {
            before: Some(newer.id)
        }
        .action()
    );
    state.workflow.install_page(
        Some(newer.id),
        WorkflowRunPage {
            runs: vec![older.clone()],
            next_before: None,
        },
    );
    key(&mut state, &vault, KeyCode::Down);
    assert_eq!(
        key(&mut state, &vault, KeyCode::Enter),
        ReadRequest::Step {
            run_id: older.id,
            index: 0
        }
        .action()
    );
    state
        .workflow
        .show_step(
            older.id,
            0,
            r#"{"trace": {"node": "read-input", "status": "success"}}"#.into(),
        )
        .unwrap();
    let trace = screen(&state, &vault, 65, 22);
    assert!(
        trace.contains("Step 1/2") && trace.contains("read-input"),
        "{trace}"
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Right),
        ReadRequest::Step {
            run_id: older.id,
            index: 1
        }
        .action()
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
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![run.clone()],
            next_before: None,
        },
    );
    assert_eq!(
        key(&mut state, &vault, KeyCode::Enter),
        ReadRequest::Step {
            run_id: run.id,
            index: 0
        }
        .action()
    );
    state
        .workflow
        .show_step(
            run.id,
            0,
            "{\n  \"output\": \"中文 0123456789 流程完成\"\n}".into(),
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
        ReadRequest::Step {
            run_id: run.id,
            index: 1
        }
        .action()
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
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![waiting.clone(), run(9, RunState::Succeeded, 1)],
            next_before: None,
        },
    );
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
        Operation::Decide {
            run_id: waiting.id,
            node: "review".into(),
            approved: false,
            review: None,
        }
    );
    state
        .workflow
        .record_decision(run(10, RunState::Pending, 1));
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
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![waiting.clone(), other],
            next_before: None,
        },
    );
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
        Action::Workflow(Operation::Decide {
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
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![waiting.clone()],
            next_before: None,
        },
    );
    open_confirmation(&mut state, &vault, 'a', waiting);

    let draw = |state: &State| {
        let model = session::present(state, &vault);
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
    state.workflow.install_page(
        None,
        WorkflowRunPage {
            runs: vec![waiting.clone()],
            next_before: None,
        },
    );
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
        Action::Workflow(Operation::Decide {
            run_id: waiting.id,
            node: "review".into(),
            approved: false,
            review: None,
        })
    );
}
