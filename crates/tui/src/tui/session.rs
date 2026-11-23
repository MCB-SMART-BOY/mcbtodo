//! Terminal lifecycle and dispatch for an interactive workspace session.

use std::{
    env,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use chrono::NaiveDate;
use chrono_tz::Tz;
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use mcbtodo_domain::Id;
use mcbtodo_platform::{AppConfig, AppConfigSnapshot, HomeViewPolicy, TuiTheme, WorkspacePaths};
use ratatui::{DefaultTerminal, Terminal, backend::CrosstermBackend};

use super::{
    Action, State, View, Workspace, initial_view, keymap, proposal_review, render, screen_data,
    screen_model, settings, theme, watch,
};

enum TerminalHandoff {
    Quit,
    Edit(PathBuf),
    ApplyProposal(Id),
}

pub fn repair_config(
    paths: &WorkspacePaths,
    snapshot: AppConfigSnapshot,
) -> Result<Option<AppConfig>> {
    require_terminal().with_context(|| format!("Invalid {}", paths.config_path().display()))?;
    let mut preferences = settings::Preferences::from_snapshot(snapshot);
    let theme = theme::Theme::for_session(TuiTheme::Default, no_color_requested());
    let mut feedback = "Invalid config.toml".to_owned();
    let (_screen, mut terminal) = Screen::enter()?;
    loop {
        let model = screen_model::ScreenModel::repair_settings(&preferences, &feedback);
        terminal.draw(|frame| render::draw(frame, &model, theme))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                return Ok(None);
            }
            continue;
        }
        match settings::Input::editor_key(key.code) {
            Some(settings::Input::Close) => return Ok(None),
            Some(settings::Input::Command(settings::Command::Reload)) => {
                feedback = settings::reload_feedback(preferences.reload(paths));
            }
            Some(settings::Input::Command(settings::Command::Save)) => {
                match preferences.save(paths) {
                    Ok((config, _)) => return Ok(Some(config)),
                    Err(error) => feedback = settings::save_error(&error),
                }
            }
            _ => preferences.edit(key.code),
        }
    }
}

fn require_terminal() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "the TUI requires an interactive terminal"
    );
    Ok(())
}

fn no_color_requested() -> bool {
    env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

pub(super) fn present<'a>(state: &'a State, vault: &Workspace) -> screen_model::ScreenModel<'a> {
    let data = screen_data::ScreenData::load(state, vault);
    screen_model::ScreenModel::project(state, vault.timezone(), data)
}

pub fn run(
    root: &Path,
    timezone: Tz,
    day: NaiveDate,
    paths: &WorkspacePaths,
    config: AppConfig,
    has_timezone_override: bool,
) -> Result<()> {
    let mut vault = Workspace::open(root, timezone, paths)?;
    require_terminal()?;
    let mut state = State::new(day);
    state.has_timezone_override = has_timezone_override;
    state.no_color = no_color_requested();
    let mut theme = theme::Theme::for_session(config.tui_theme, state.no_color);
    state.home_view = config.home_view;
    let restored_view = initial_view(paths, config.home_view);
    if let Ok(view) = &restored_view {
        state.view = *view;
    }
    if state.view == View::Workflow
        && let Action::Workflow(request) = state.workflow.open()
    {
        state.message = state.workflow.execute(paths, request);
    }
    let mut watcher = match watch::Watcher::new(root) {
        Ok(watcher) => {
            // Close the gap between the local snapshot load and watch registration.
            if matches!(vault, Workspace::Local(_)) {
                state.refresh_external(root, &mut vault);
            }
            Some(watcher)
        }
        Err(error) => {
            state.message = format!("Auto-reload unavailable · {error:#} · r reload");
            None
        }
    };
    match keymap::Keymap::load_config(paths) {
        Ok(keymap) => state.keymap = keymap,
        Err(_) => state.message = "Cannot load keymap · Default keys active · Settings F2".into(),
    }
    if restored_view.is_err() {
        if !state.message.is_empty() {
            state.message.push_str(" · ");
        }
        state
            .message
            .push_str("Saved view unavailable · Showing Month");
    }
    loop {
        let (screen, mut terminal) = Screen::enter()?;
        let mut redraw = true;
        let handoff = loop {
            if let Some(active) = watcher.as_mut() {
                match active.changed() {
                    Ok(true) => redraw |= state.refresh_external(root, &mut vault),
                    Ok(false) => {}
                    Err(error) => {
                        state.message = format!("Auto-reload unavailable · {error:#} · r reload");
                        watcher = None;
                        redraw = true;
                    }
                }
            }
            if redraw {
                let model = present(&state, &vault);
                terminal.draw(|frame| render::draw(frame, &model, theme))?;
                redraw = false;
            }
            // crossterm owns terminal input and resize notifications. Give the
            // nonblocking inotify queue time even when no keys are pressed.
            if watcher.is_some() && !event::poll(Duration::from_millis(50))? {
                continue;
            }
            redraw = true;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match state.handle(key, &vault) {
                Action::Continue => {}
                Action::Quit => break TerminalHandoff::Quit,
                Action::Reload => state.reload(root, &mut vault),
                Action::OpenSettings => state.open_settings(paths),
                Action::Settings(command) => {
                    if let Some(palette) = state.apply_settings(command, paths, vault.timezone()) {
                        theme = theme::Theme::for_session(palette, state.no_color);
                    }
                }
                Action::Timer(operation) => {
                    state.message = match mcbtodo_client::daemon_client::timer(paths, operation) {
                        Ok(status) => mcbtodo_client::timer_label(&status),
                        Err(error) => format!("Timer failed · {error:#}"),
                    };
                }
                Action::CreateCalendar(request) => {
                    state.create_calendar(root, &mut vault, paths, request);
                }
                Action::CreateReminder(request) => {
                    state.create_reminder(root, &mut vault, paths, request);
                }
                Action::CreateTask(request) => {
                    state.create_task(root, &mut vault, paths, request);
                }
                Action::CreateTimeBlock(block) => {
                    state.create_time_block(root, &mut vault, paths, block);
                }
                Action::OpenDeadline(id) => state.open_deadline(&vault, id),
                Action::RefreshDeadline => state.refresh_deadline(root, &vault),
                Action::SetDeadline(request) => {
                    state.set_deadline(root, &mut vault, request);
                }
                Action::SetGraphDependencies(request) => {
                    state.set_graph_dependencies(root, &mut vault, request);
                }
                Action::Proposals(request) => {
                    state.message =
                        match state
                            .proposals
                            .execute(root, vault.timezone(), paths, request)
                        {
                            Ok(()) if let proposal_review::Request::Reject(id) = request => {
                                format!("Rejected proposal {id}")
                            }
                            Ok(()) => String::new(),
                            Err(error) => format!("Proposal review failed · {error:#}"),
                        };
                }
                Action::ApplyProposal(id) => break TerminalHandoff::ApplyProposal(id),
                Action::Workflow(command) => {
                    state.message = state.workflow.execute(paths, command);
                }
                Action::TriageInbox {
                    inbox_id,
                    project_id,
                    revision,
                } => state.triage_inbox(root, &mut vault, paths, inbox_id, project_id, revision),
                Action::Edit => match state.source(&vault) {
                    Ok(Some(path)) => break TerminalHandoff::Edit(path),
                    Ok(None) => state.message = "Select a document to edit".into(),
                    Err(error) => state.message = format!("Cannot locate document · {error:#}"),
                },
            }
        };
        drop(terminal);
        drop(screen);
        match handoff {
            TerminalHandoff::Quit => {
                if state.home_view == HomeViewPolicy::RestoreLastView
                    && let Err(error) = paths.write_state_file(
                        &paths.last_view_path(),
                        state.view.saved_name().as_bytes(),
                    )
                {
                    eprintln!("mcbtodo: could not save the last view: {error:#}");
                }
                return Ok(());
            }
            TerminalHandoff::ApplyProposal(id) => {
                state.apply_proposal(root, &mut vault, paths, id);
            }
            TerminalHandoff::Edit(path) => {
                let edit_result = edit(&path);
                state.reload(root, &mut vault);
                if let Err(error) = edit_result {
                    state.message = format!("Editor failed · {error:#} · {}", state.message);
                }
            }
        }
    }
}

struct Screen;

impl Screen {
    fn enter() -> io::Result<(Self, DefaultTerminal)> {
        enable_raw_mode()?;
        let screen = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok((screen, terminal))
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

fn edit(path: &Path) -> Result<()> {
    let editor = env::var_os("VISUAL")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("EDITOR").filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "hx".into());
    let status = Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("starting {}", editor.to_string_lossy()))?;
    ensure!(status.success(), "editor exited with {status}");
    Ok(())
}
