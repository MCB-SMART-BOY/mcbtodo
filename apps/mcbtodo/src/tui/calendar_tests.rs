use super::{tests::fixture, *};
use ratatui::backend::TestBackend;

fn press(state: &mut State, vault: &Vault, keys: &str) {
    for key in keys.chars() {
        let code = match key {
            '\n' => KeyCode::Enter,
            '\x1b' => KeyCode::Esc,
            ch => KeyCode::Char(ch),
        };
        state.handle(KeyEvent::new(code, KeyModifiers::NONE), vault);
    }
}

pub(super) fn screen(state: &State, vault: &Vault, width: u16, height: u16) -> String {
    projected_screen(
        &screen_model::ScreenModel::project(state, vault),
        width,
        height,
    )
}

pub(super) fn projected_screen(
    model: &screen_model::ScreenModel<'_>,
    width: u16,
    height: u16,
) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let frame = terminal
        .draw(|frame| render::draw(frame, model, theme::Theme::default()))
        .unwrap();
    (0..height)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0;
            while x < width {
                let symbol = frame.buffer[(x, y)].symbol();
                line.push_str(symbol);
                x += ratatui::text::Span::raw(symbol).width().max(1) as u16;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(target_os = "linux")]
#[test]
fn daemon_calendar_uses_the_same_rows_for_render_selection_and_refresh() -> Result<()> {
    use std::{os::unix::net::UnixListener, thread};

    use mcbtodo_domain::time::DateSpan;
    use mcbtodo_protocol::{Response, wire};

    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    let root = paths.vault_root();
    let mut vault = Vault::load(&root, chrono_tz::UTC)?;
    let start = "2026-11-24".parse()?;
    let through = "2026-11-25".parse()?;
    let occurrence = |date, title: &str| -> Result<CalendarItem> {
        Ok(CalendarItem {
            id: "40000000-0000-4000-8000-000000000001".parse()?,
            title: title.into(),
            kind: CalendarKind::Event,
            busy: true,
            occurrence: Occurrence::Span(Span::DateSpan(DateSpan::new(date, date)?)),
        })
    };
    let first = occurrence(start, "Earlier occurrence")?;
    let second = occurrence(through, "Second occurrence")?;
    let second_focus = calendar_focus(&second);
    let listener = UnixListener::bind(paths.socket_path())?;
    let peer = thread::spawn(move || -> Result<()> {
        for items in [vec![first.clone(), second.clone()], vec![second, first]] {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(1)))?;
            assert!(matches!(
                wire::read(&mut stream)?,
                Request::Calendar { start: from, through: to, timezone }
                    if from == start && to == through && timezone == "UTC"
            ));
            wire::write(&mut stream, &Response::Calendar { items })?;
        }
        Ok(())
    });
    let mut state = State::new(through);
    state.daemon_views = Some(daemon_views::DaemonViews::new(paths));
    state.anchor = Some(start);
    state.switch(View::Agenda, &vault);
    assert!(screen(&state, &vault, 80, 16).contains("Earlier occurrence"));
    press(&mut state, &vault, "j");
    assert_eq!(state.selected_focus(&vault), Some(second_focus));
    assert!(screen(&state, &vault, 40, 6).contains("Second occurrence"));

    state.reload(&root, &mut vault);
    assert_eq!(state.row, 0);
    assert_eq!(state.selected_focus(&vault), Some(second_focus));
    peer.join().expect("test calendar peer panicked")?;

    state.reload(&root, &mut vault);
    assert!(screen(&state, &vault, 80, 16).contains("Start mcbtodo-daemon"));
    assert_eq!(state.selected_focus(&vault), None);
    press(&mut state, &vault, "hl");
    assert_eq!(state.day, through);
    Ok(())
}

#[test]
fn extended_calendar_windows_are_clipped_to_the_display_years() -> Result<()> {
    let last = "9999-12-31".parse()?;
    for view in [View::Month, View::Week, View::Timeline] {
        let window = calendar::window(view, last, (last, last));
        assert_eq!(window.through(), last, "{view:?}");
        assert!(window.start() < last, "{view:?}");
    }
    let first = "0001-01-01".parse()?;
    assert_eq!(
        calendar::window(View::Week, first, (first, first)).start(),
        first
    );
    Ok(())
}

#[test]
fn month_shows_the_same_cross_month_recurring_event_as_day() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("calendar/review.md"),
        "\
---
schema: mcbtodo.calendar-event/v1
id: 00000000-0000-4000-8000-000000000001
when: {kind: date_span, start: '2026-11-29', through: '2026-12-01'}
availability: busy
recurrence:
  start: '2026-11-29'
  through: '2026-11-29'
  frequency: weekly
  dst: {gap: skip, fold: earlier}
---

# 月度回顾
",
    )
    .unwrap();
    let vault = Vault::load(root.path(), chrono_tz::Asia::Shanghai).unwrap();
    let mut state = State::new("2026-12-01".parse().unwrap());
    let compact = screen(&state, &vault, 24, 5);
    assert!(compact.contains("Tue 01*"), "{compact}");

    press(&mut state, &vault, ":view day\n");
    assert_eq!(state.view, View::Day);
    assert!(screen(&state, &vault, 42, 12).contains("月度回顾"));
    assert!(state.source(&vault).unwrap().ends_with("review.md"));
}

#[test]
fn day_and_agenda_preserve_the_timed_occurrence_during_the_repeated_hour() {
    let root = tempfile::tempdir().unwrap();
    let calendar = root.path().join("calendar");
    std::fs::create_dir(&calendar).unwrap();
    let path = calendar.join("review.md");
    std::fs::write(
        &path,
        r#"---
schema: mcbtodo.calendar-event/v1
id: 00000000-0000-4000-8000-000000000001
when:
  kind: date_time_span
  start: '2026-10-25T01:50:00-04:00'
  end: '2026-10-25T02:10:00-04:00'
  timezone: America/New_York
recurrence:
  start: '2026-10-25'
  frequency: weekly
  dst: {gap: skip, fold: earlier}
---
# 回顾 Review
"#,
    )
    .unwrap();
    let zone = chrono_tz::America::New_York;
    let mut vault = Vault::load(root.path(), zone).unwrap();
    let day = "2026-11-01".parse().unwrap();
    let mut state = State::new(day);
    press(&mut state, &vault, ":view day\n");
    let selected = state.selected_focus(&vault).unwrap();
    let screen = screen(&state, &vault, 48, 8);
    assert!(
        screen.contains("01:50 -04:00–01:10 -05:00  回顾 Review"),
        "{screen}"
    );

    press(&mut state, &vault, " ua");
    assert_eq!(state.view, View::Agenda);
    assert_eq!(state.selected_focus(&vault), Some(selected));
    let next = Vault::load(root.path(), zone).unwrap();
    state.install_vault(&mut vault, next);
    press(&mut state, &vault, " ud");
    assert_eq!(state.view, View::Day);
    assert_eq!(state.selected_focus(&vault), Some(selected));
    assert_eq!(state.source(&vault), Some(path.as_path()));
}

#[test]
fn dense_recurring_event_reports_projection_limit_and_navigation_can_recover() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("calendar/review.md"),
        "\
---
schema: mcbtodo.calendar-event/v1
id: 00000000-0000-4000-8000-000000000001
when: {kind: date_span, start: '2020-01-01', through: '2024-01-01'}
recurrence:
  start: '2020-01-01'
  through: '2024-01-01'
  frequency: daily
  dst: {gap: skip, fold: earlier}
---

# Dense recurrence
",
    )
    .unwrap();
    let vault = Vault::load(root.path(), chrono_tz::Asia::Shanghai).unwrap();
    let mut state = State::new("2024-01-01".parse().unwrap());
    let compact = screen(&state, &vault, 24, 5);
    assert!(compact.contains("Calendar error"), "{compact}");
    assert!(compact.contains("too many occurrences"), "{compact}");
    for view in [
        View::Month,
        View::Week,
        View::Day,
        View::Agenda,
        View::Timeline,
    ] {
        state.switch(view, &vault);
        let text = screen(&state, &vault, 80, 12);
        assert!(text.contains("Calendar error"), "{text}");
        assert!(text.contains("1024"), "{text}");
        assert!(state.selected_document(&vault).is_none());
    }
    press(&mut state, &vault, ":date 2019-01-01\n");
    assert!(!screen(&state, &vault, 80, 12).contains("Calendar error"));
}

#[test]
fn command_mode_navigates_views_and_searches_the_outline() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());

    press(&mut state, &vault, ":view graph\n");
    assert!(state.view == View::Graph);

    press(&mut state, &vault, ":search 地址转换\n");
    assert!(matches!(state.overlay, Overlay::Picker { .. }));
    let text = screen(&state, &vault, 72, 18);
    assert!(text.contains("实现地址转换"), "{text}");
    press(&mut state, &vault, "\n");
    assert!(state.view == View::Outline);
    assert!(state.source(&vault).unwrap().ends_with("translation.md"));

    press(&mut state, &vault, ":workflow\n");
    assert!(state.view == View::Workflow);
}

#[test]
fn short_document_picker_keeps_match_and_actions_visible() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, ":search 地址转换\n");
    let text = screen(&state, &vault, 28, 5);
    let rows: Vec<_> = text.lines().collect();
    assert!(rows[1].contains("/地址转换▏"), "{text}");
    assert!(
        rows[2].contains("›") && rows[2].contains("实现地址转换"),
        "{text}"
    );
    assert!(
        rows[4].contains("Enter open") && rows[4].contains("Esc"),
        "{text}"
    );
    press(&mut state, &vault, "\n");
    assert!(state.source(&vault).unwrap().ends_with("translation.md"));
}

#[test]
fn narrow_outline_keeps_the_blocked_indicator_visible_before_a_long_title() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, ":search 地址转换\n\n");
    assert_eq!(state.view, View::Outline);
    let model = screen_model::ScreenModel::project(&state, &vault);
    let mut terminal = Terminal::new(TestBackend::new(23, 5)).unwrap();
    let monochrome = theme::Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
    let frame = terminal
        .draw(|frame| render::draw(frame, &model, monochrome))
        .unwrap();
    assert_eq!(frame.buffer[(1, 2)].symbol(), "›");
    assert_eq!(frame.buffer[(7, 2)].symbol(), "!");
    assert_eq!(frame.buffer[(9, 2)].symbol(), "实");
    assert_eq!(frame.buffer[(11, 2)].symbol(), "现");
    assert_eq!(frame.buffer[(22, 2)].symbol(), "│");
    let legend: String = (0..23).map(|x| frame.buffer[(x, 3)].symbol()).collect();
    assert!(legend.contains("! Blocked"), "{legend}");
    assert!(
        frame.buffer[(7, 2)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert!(state.source(&vault).unwrap().ends_with("translation.md"));
}

#[test]
fn views_menu_covers_calendar_and_planning_views_without_losing_date_cursor() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    let date = state.day;

    for (key, expected) in [
        ('m', "Month"),
        ('w', "Week"),
        ('d', "Day"),
        ('a', "Agenda"),
        ('l', "Outline"),
        ('g', "Graph"),
        ('t', "Timeline"),
        ('i', "Inbox"),
        ('f', "Workflow"),
    ] {
        press(&mut state, &vault, &format!(" u{key}"));
        assert_eq!(state.view.label(), expected);
        assert_eq!(state.day, date, "switching to {expected} changed the date");
    }
}

#[test]
fn timeline_view_can_be_opened_and_renders_calendar_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("projects/planning");
    let calendar_dir = dir.path().join("calendar");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&calendar_dir).unwrap();
    std::fs::write(
        project_dir.join("project.md"),
        r#"---
schema: mcbtodo.project/v1
id: 10000000-0000-4000-8000-000000000001
deadline:
  kind: date
  value: 2026-11-27
---
# Plan
"#,
    )
    .unwrap();
    std::fs::write(
        calendar_dir.join("university.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_span
  start: 2026-11-25
  through: 2026-11-25
availability: busy
---
# 校园活动
"#,
    )
    .unwrap();
    std::fs::write(
        calendar_dir.join("review.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000002
when:
  kind: date_time_span
  start: "2026-11-25T14:00:00+09:00"
  end: "2026-11-25T16:00:00+09:00"
  timezone: Asia/Tokyo
availability: free
---
# 远程讨论
"#,
    )
    .unwrap();
    let vault = Vault::load(dir.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let mut state = State::new("2026-11-25".parse().unwrap());

    press(&mut state, &vault, ":view timeline\n");

    assert_eq!(state.view.label(), "Timeline");
    press(&mut state, &vault, " ut");
    assert_eq!(state.view.label(), "Timeline");
    let text = screen(&state, &vault, 100, 24);
    assert!(text.contains("远程讨论"), "{text}");
    assert!(text.contains("校园活动"), "{text}");
    assert!(text.contains("!! deadline"), "{text}");

    let remote = state
        .calendar_entries(&vault, View::Timeline)
        .unwrap()
        .iter()
        .position(|entry| entry.title == "远程讨论")
        .unwrap();
    press(&mut state, &vault, &"j".repeat(remote));
    let compact = screen(&state, &vault, 30, 6);
    assert!(compact.contains("Plan · 2026-11-25"), "{compact}");
    assert!(compact.contains("› >>  E 远程讨论"), "{compact}");
    assert!(
        compact
            .lines()
            .all(|line| ratatui::text::Span::raw(line).width() == 30),
        "{compact}"
    );
    assert!(
        compact
            .lines()
            .last()
            .unwrap()
            .contains("hl date  jk plan  ?")
    );
    let source = state.source(&vault);

    press(&mut state, &vault, "l");
    assert_eq!(state.day.to_string(), "2026-11-26");
    let compact = screen(&state, &vault, 30, 6);
    assert!(compact.contains("Plan · 2026-11-26"), "{compact}");
    assert!(compact.contains("› --  E 远程讨论"), "{compact}");
    assert_eq!(state.source(&vault), source);
    press(&mut state, &vault, "j");
    assert!(state.source(&vault).is_some());
}

#[test]
fn agenda_focus_preserves_the_selected_recurring_occurrence_across_views_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let calendar = dir.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join("daily.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_span
  start: 2026-11-24
  through: 2026-11-24
availability: busy
recurrence:
  start: "2026-11-24"
  through: "2026-11-26"
  frequency: daily
  interval: 1
  dst: {gap: skip, fold: earlier}
---
# Daily review
"#,
    )
    .unwrap();
    let mut vault = Vault::load(dir.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let mut state = State::new("2026-11-24".parse().unwrap());
    state.anchor = Some(state.day);
    state.day = "2026-11-26".parse().unwrap();
    state.switch(View::Agenda, &vault);

    let target_day = "2026-11-26".parse().unwrap();
    state.row = state
        .agenda(&vault)
        .unwrap()
        .iter()
        .position(|entry| {
            entry.id.to_string() == "40000000-0000-4000-8000-000000000001"
                && entry.occurrence.on_day(target_day, vault.timezone())
        })
        .unwrap();
    let selected = state.selected_focus(&vault).unwrap();

    state.switch(View::Day, &vault);
    assert_eq!(state.day, target_day);
    assert_eq!(state.selected_focus(&vault), Some(selected));
    assert_eq!(
        state.selected_document(&vault),
        Some("40000000-0000-4000-8000-000000000001".parse().unwrap())
    );

    let next = Vault::load(dir.path(), vault.timezone()).unwrap();
    state.install_vault(&mut vault, next);
    assert_eq!(state.selected_focus(&vault), Some(selected));

    state.switch(View::Agenda, &vault);
    assert_eq!(state.selected_focus(&vault), Some(selected));
}

#[test]
fn recurring_occurrence_focus_survives_round_trips_through_calendar_and_outline_views() {
    let dir = tempfile::tempdir().unwrap();
    let calendar = dir.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join("daily.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_span
  start: 2026-11-24
  through: 2026-11-24
availability: busy
recurrence:
  start: "2026-11-24"
  through: "2026-11-26"
  frequency: daily
  interval: 1
  dst: {gap: skip, fold: earlier}
---
# Daily review
"#,
    )
    .unwrap();
    let vault = Vault::load(dir.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let target_day = "2026-11-26".parse().unwrap();
    let mut state = State::new(target_day);
    state.anchor = Some(target_day);
    state.switch(View::Agenda, &vault);
    state.row = state
        .agenda(&vault)
        .unwrap()
        .iter()
        .position(|entry| {
            entry.id.to_string() == "40000000-0000-4000-8000-000000000001"
                && entry.occurrence.on_day(target_day, vault.timezone())
        })
        .unwrap();
    let selected = state.selected_focus(&vault).unwrap();

    for (key, expected_view) in [
        ('m', View::Month),
        ('w', View::Week),
        ('d', View::Day),
        ('l', View::Outline),
    ] {
        press(&mut state, &vault, &format!(" u{key}"));
        assert_eq!(state.view, expected_view);
        if expected_view == View::Day {
            assert_eq!(state.selected_focus(&vault), Some(selected));
        } else if expected_view == View::Outline {
            assert_eq!(state.document_id, Some(selected.document_id()));
        }

        press(&mut state, &vault, " ua");
        assert_eq!(state.view, View::Agenda);
        assert_eq!(state.selected_focus(&vault), Some(selected));
    }
}

#[test]
fn calendar_views_preserve_recurring_focus_across_every_direct_transition() {
    let dir = tempfile::tempdir().unwrap();
    let calendar = dir.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join("daily.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_span
  start: 2026-11-24
  through: 2026-11-24
availability: busy
recurrence:
  start: "2026-11-24"
  through: "2026-11-26"
  frequency: daily
  interval: 1
  dst: {gap: skip, fold: earlier}
---
# Daily review
"#,
    )
    .unwrap();
    let vault = Vault::load(dir.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let target_day = "2026-11-26".parse().unwrap();
    let range_start = "2026-11-24".parse().unwrap();
    let transitions = [
        (View::Month, 'm'),
        (View::Week, 'w'),
        (View::Day, 'd'),
        (View::Agenda, 'a'),
    ];

    for (source, source_key) in transitions {
        for (target, target_key) in transitions {
            let mut state = State::new(target_day);
            state.anchor = Some(range_start);
            press(&mut state, &vault, " ua");
            state.row = state
                .agenda(&vault)
                .unwrap()
                .iter()
                .position(|entry| {
                    entry.id.to_string() == "40000000-0000-4000-8000-000000000001"
                        && entry.occurrence.on_day(target_day, vault.timezone())
                })
                .unwrap();
            let selected = state.selected_focus(&vault).unwrap();

            if source != View::Agenda {
                press(&mut state, &vault, &format!(" u{source_key}"));
            }
            assert_eq!(state.view, source);

            if target != source {
                press(&mut state, &vault, &format!(" u{target_key}"));
            }
            assert_eq!(state.view, target, "{source:?} -> {target:?}");
            assert_eq!(state.day, target_day, "{source:?} -> {target:?}");
            assert_eq!(
                state.selected_range(),
                (range_start, target_day),
                "{source:?} -> {target:?}"
            );
            if matches!(target, View::Day | View::Agenda) {
                assert_eq!(
                    state.selected_focus(&vault),
                    Some(selected),
                    "{source:?} -> {target:?}"
                );
            }

            if target != View::Agenda {
                press(&mut state, &vault, " ua");
            }
            assert_eq!(
                state.view,
                View::Agenda,
                "{source:?} -> {target:?} -> Agenda"
            );
            assert_eq!(
                state.selected_focus(&vault),
                Some(selected),
                "{source:?} -> {target:?} -> Agenda"
            );
        }
    }
}

#[test]
fn recurring_occurrence_focus_survives_month_and_week_intermediates_for_multiday_agenda() {
    let dir = tempfile::tempdir().unwrap();
    let calendar = dir.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join("daily.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_span
  start: 2026-11-24
  through: 2026-11-24
availability: busy
recurrence:
  start: "2026-11-24"
  through: "2026-11-26"
  frequency: daily
  interval: 1
  dst: {gap: skip, fold: earlier}
---
# Daily review
"#,
    )
    .unwrap();
    let vault = Vault::load(dir.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let mut state = State::new("2026-11-24".parse().unwrap());
    state.anchor = Some(state.day);
    state.day = "2026-11-26".parse().unwrap();
    state.switch(View::Agenda, &vault);
    state.row = state
        .agenda(&vault)
        .unwrap()
        .iter()
        .position(|entry| {
            entry.id.to_string() == "40000000-0000-4000-8000-000000000001"
                && entry.occurrence.on_day(state.day, vault.timezone())
        })
        .unwrap();
    let selected = state.selected_focus(&vault).unwrap();

    for intermediate in [View::Month, View::Week] {
        state.switch(intermediate, &vault);
        state.switch(View::Agenda, &vault);
        assert_eq!(state.selected_focus(&vault), Some(selected));
    }
}

#[test]
fn agenda_focus_preserves_a_multiday_all_day_occurrence_from_a_continuation_day() {
    let dir = tempfile::tempdir().unwrap();
    let calendar = dir.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join("conference.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_span
  start: 2026-11-24
  through: 2026-11-26
availability: busy
---
# Multi-day conference
"#,
    )
    .unwrap();
    let mut vault = Vault::load(dir.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let target_day = "2026-11-25".parse().unwrap();
    let mut state = State::new(target_day);
    state.anchor = Some(target_day);
    state.switch(View::Agenda, &vault);

    state.row = state
        .agenda(&vault)
        .unwrap()
        .iter()
        .position(|entry| {
            entry.id.to_string() == "40000000-0000-4000-8000-000000000001"
                && entry.occurrence.on_day(target_day, vault.timezone())
        })
        .unwrap();
    let selected = state.selected_focus(&vault).unwrap();

    state.switch(View::Day, &vault);
    assert_eq!(state.day, target_day);
    assert_eq!(state.selected_focus(&vault), Some(selected));

    let next = Vault::load(dir.path(), vault.timezone()).unwrap();
    state.install_vault(&mut vault, next);
    assert_eq!(state.selected_focus(&vault), Some(selected));
}

#[test]
fn command_mode_dispatches_timer_and_proposal_actions() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());

    state.handle(
        KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE),
        &vault,
    );
    for character in "timer start".chars() {
        state.handle(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &vault,
        );
    }
    assert_eq!(
        state.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &vault),
        Action::Timer(Request::Start { work_unit_id: None })
    );

    assert_eq!(
        state.handle(
            KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE),
            &vault
        ),
        Action::Continue
    );
    for character in "proposals".chars() {
        state.handle(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &vault,
        );
    }
    assert_eq!(
        state.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &vault),
        Action::Proposals(proposal_review::Request::List)
    );
    assert!(matches!(state.overlay, Overlay::Proposals));
}

#[test]
fn week_and_day_keep_the_date_cursor_and_inclusive_anchor() {
    let vault = fixture();
    let mut state = State::new("2026-11-22".parse().unwrap());
    press(&mut state, &vault, "v ul uwl");
    assert_eq!(state.view.label(), "Week");
    assert_eq!(state.day.to_string(), "2026-11-23");
    let text = screen(&state, &vault, 84, 24);
    assert!(text.contains("Mon 23") && text.contains("Sun 29"), "{text}");
    state.handle(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), &vault);
    assert_eq!(state.day.to_string(), "2026-11-16");
    state.handle(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &vault);
    press(&mut state, &vault, "\n");
    assert_eq!(state.view.label(), "Day");
    press(&mut state, &vault, "ll\n");
    assert_eq!(state.view.label(), "Agenda");
    let (start, end) = state.selected_range();
    assert_eq!(start.to_string(), "2026-11-22");
    assert_eq!(end.to_string(), "2026-11-25");
}

#[test]
fn day_displays_separate_sections_with_display_zone_times() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " ud");
    let text = screen(&state, &vault, 80, 24);
    for label in [
        "All-day",
        "校园活动",
        "Timed",
        "13:00–15:00",
        "free",
        "Deadlines",
        "Ongoing",
    ] {
        assert!(text.contains(label), "missing {label}\n{text}");
    }
    assert!(!text.contains("00:00"), "{text}");
}

#[test]
fn day_rows_allow_editing_the_focused_entry_and_keep_document_focus_across_views() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " udjj");
    assert!(state.source(&vault).unwrap().ends_with("mapping.md"));
    press(&mut state, &vault, " ul um ud");
    assert!(state.source(&vault).unwrap().ends_with("mapping.md"));
}

#[test]
fn narrow_week_keeps_weekdays_and_semantic_counts_visible() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " uw");
    let text = screen(&state, &vault, 42, 18);
    assert!(text.contains("Mon 23") && text.contains("Sun 29"), "{text}");
    let wednesday = text.lines().find(|line| line.contains("Wed 25")).unwrap();
    assert!(
        wednesday.contains("1 all") && wednesday.contains("1 timed") && wednesday.contains("1 due"),
        "{text}"
    );
    assert!(text.contains("Wed 25  远程讨论"), "{text}");
}

#[test]
fn week_positions_timed_events_under_display_day_and_hour() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " uw");
    let text = screen(&state, &vault, 84, 24);
    let header = text.lines().find(|line| line.contains("Wed 25")).unwrap();
    let y = text.lines().find(|line| line.contains("·13:00")).unwrap();
    assert_eq!(
        header.chars().position(|ch| ch == '>'),
        y.chars().position(|ch| ch == '·'),
        "{text}"
    );
    assert!(y.starts_with("│> 13:00"), "{text}");
}

#[test]
fn week_hour_cursor_previews_and_opens_the_focused_entry() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " uwj");
    let text = screen(&state, &vault, 80, 24);
    assert!(text.contains("13:00–15:00  远程讨论"), "{text}");
    press(&mut state, &vault, "\n");
    assert!(state.source(&vault).unwrap().ends_with("review.md"));
}

#[test]
fn compact_week_keeps_the_focused_hour_and_occurrence_visible() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " uwj");

    let model = screen_model::ScreenModel::project(&state, &vault);
    let mut terminal = Terminal::new(TestBackend::new(24, 5)).unwrap();
    let monochrome = theme::Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
    let frame = terminal
        .draw(|frame| render::draw(frame, &model, monochrome))
        .unwrap();
    let heading: String = (0..24).map(|x| frame.buffer[(x, 1)].symbol()).collect();
    assert!(heading.contains("13:00"), "{heading}");
    assert_eq!(frame.buffer[(1, 2)].symbol(), "›");
    let date: String = (3..9).map(|x| frame.buffer[(x, 2)].symbol()).collect();
    assert_eq!(date, "Wed 25");
    for (x, character) in [(11, "远"), (13, "程"), (15, "讨"), (17, "论")] {
        assert_eq!(frame.buffer[(x, 2)].symbol(), character);
    }
    assert_eq!(frame.buffer[(23, 2)].symbol(), "│");
    let footer: String = (0..24).map(|x| frame.buffer[(x, 4)].symbol()).collect();
    assert!(footer.contains("Enter day"), "{footer}");

    press(&mut state, &vault, "\n");
    assert_eq!(state.view, View::Day);
    assert!(state.source(&vault).unwrap().ends_with("review.md"));
}

#[test]
fn week_continuation_hour_opens_the_same_multiday_occurrence_in_day_view() {
    let dir = tempfile::tempdir().unwrap();
    let calendar_dir = dir.path().join("calendar");
    std::fs::create_dir(&calendar_dir).unwrap();
    std::fs::write(
        calendar_dir.join("overnight.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_time_span
  start: "2026-11-24T22:00:00+08:00"
  end: "2026-11-26T02:00:00+08:00"
  timezone: Asia/Shanghai
availability: busy
---
# Overnight maintenance
"#,
    )
    .unwrap();
    let vault = Vault::load(dir.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let mut state = State::new("2026-11-25".parse().unwrap());

    press(&mut state, &vault, " uw");
    let selected = state
        .week(&vault)
        .unwrap()
        .focused_timed(state.day, state.week_hour, vault.timezone())
        .map(calendar_focus);
    let text = screen(&state, &vault, 84, 24);
    assert!(text.contains("← cont."), "{text}");
    assert!(text.contains("Overnight maintenance"), "{text}");
    assert!(matches!(
        selected,
        Some(FocusIdentity::Calendar {
            id,
            kind: CalendarKind::Event,
            occurrence: Occurrence::Span(Span::DateTimeSpan(_)),
        }) if id.to_string() == "40000000-0000-4000-8000-000000000001"
    ));

    press(&mut state, &vault, "\n");

    assert_eq!(state.view, View::Day);
    let focused = state.selected_focus(&vault).unwrap();
    assert!(matches!(
        focused,
        FocusIdentity::Calendar {
            id,
            kind: CalendarKind::Event,
            occurrence: Occurrence::Span(Span::DateTimeSpan(span)),
        } if id.to_string() == "40000000-0000-4000-8000-000000000001"
            && span.start().to_rfc3339() == "2026-11-24T22:00:00+08:00"
            && span.end().to_rfc3339() == "2026-11-26T02:00:00+08:00"
    ));
}

#[test]
fn day_preserves_dst_offsets_and_excludes_the_midnight_end_date() {
    let dir = tempfile::tempdir().unwrap();
    let calendar_dir = dir.path().join("calendar");
    std::fs::create_dir(&calendar_dir).unwrap();
    std::fs::write(
        calendar_dir.join("night.md"),
        r#"---
schema: mcbtodo.calendar-event/v1
id: 40000000-0000-4000-8000-000000000001
when:
  kind: date_time_span
  start: "2026-11-01T01:15:00-04:00"
  end: "2026-11-02T00:00:00-05:00"
  timezone: America/New_York
availability: busy
---
# Overnight
"#,
    )
    .unwrap();
    let vault = Vault::load(dir.path(), "America/New_York".parse().unwrap()).unwrap();
    let mut state = State::new("2026-11-01".parse().unwrap());
    press(&mut state, &vault, " ud");
    let text = screen(&state, &vault, 84, 20);
    assert!(
        text.contains("01:15 -04:00–2026-11-02 00:00 -05:00"),
        "{text}"
    );
    press(&mut state, &vault, "l");
    let text = screen(&state, &vault, 84, 20);
    assert!(!text.contains("Overnight"), "{text}");
}

#[test]
fn graph_locates_outline_focus_and_follows_outgoing_and_incoming_edges() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    let mapping_id = Id::parse_str("30000000-0000-4000-8000-000000000002").unwrap();
    state.view = View::Outline;
    state.row = mcbtodo_application::project_outline(&vault)
        .iter()
        .position(|item| item.id == mapping_id)
        .unwrap();
    press(&mut state, &vault, "g");
    assert_eq!(state.view.label(), "Graph");
    assert!(state.source(&vault).unwrap().ends_with("mapping.md"));
    let text = screen(&state, &vault, 52, 22);
    for label in [
        "阅读 Sv39 规范",
        "实现地址转换",
        "[blocked]",
        "Prerequisites",
        "Dependents",
    ] {
        assert!(text.contains(label), "missing {label}\n{text}");
    }
    press(&mut state, &vault, "vlx");
    assert!(state.source(&vault).unwrap().ends_with("translation.md"));
    let text = screen(&state, &vault, 52, 22);
    assert!(text.contains("2 nodes"), "{text}");
    press(&mut state, &vault, "h ul");
    assert!(state.source(&vault).unwrap().ends_with("mapping.md"));
}

#[test]
fn graph_projection_cache_refreshes_with_the_validated_vault_snapshot() {
    let (home, mut vault) = writable_project();
    let translation: Id = "30000000-0000-4000-8000-000000000003".parse().unwrap();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.switch(View::Graph, &vault);

    assert_eq!(
        state
            .graph_nodes(&vault)
            .iter()
            .find(|node| node.id == translation)
            .unwrap()
            .prerequisites
            .len(),
        1
    );

    let path = vault.source(translation).unwrap().to_owned();
    let contents = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        contents.replace(
            "depends_on: [30000000-0000-4000-8000-000000000002]",
            "depends_on: []",
        ),
    )
    .unwrap();
    let next = Vault::load(home.path(), vault.timezone()).unwrap();
    state.install_vault(&mut vault, next);

    assert!(
        state
            .graph_nodes(&vault)
            .iter()
            .find(|node| node.id == translation)
            .unwrap()
            .prerequisites
            .is_empty()
    );
}

#[test]
fn narrow_graph_keeps_the_selected_node_in_the_visible_window() {
    let (home, _) = writable_project();
    let units = home.path().join("projects/rust-os/units");
    for index in 100..132 {
        let title = if index == 131 {
            "Tail unit"
        } else if index == 100 {
            "Head unit"
        } else {
            "Middle unit"
        };
        std::fs::write(
            units.join(format!("unit-{index}.md")),
            format!(
                "---\nschema: mcbtodo.work-unit/v1\nid: 30000000-0000-4000-8000-{index:012}\nproject_id: 10000000-0000-4000-8000-000000000001\nparent_task_id: 20000000-0000-4000-8000-000000000001\nstatus: todo\n---\n\n# {title}\n"
            ),
        )
        .unwrap();
    }
    let vault = Vault::load(home.path(), "Asia/Shanghai".parse().unwrap()).unwrap();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.switch(View::Graph, &vault);
    let selected_row = state
        .graph_nodes(&vault)
        .iter()
        .position(|node| node.title == "Tail unit")
        .unwrap();
    state.row = selected_row;

    let text = screen(&state, &vault, 42, 14);

    assert!(text.contains("Tail unit"), "{text}");
    assert!(!text.contains("Head unit"), "{text}");
}

#[test]
fn short_graph_shows_edge_actions_and_keeps_the_target_visible() {
    let vault = fixture();
    let source: Id = "30000000-0000-4000-8000-000000000001".parse().unwrap();
    let target: Id = "30000000-0000-4000-8000-000000000003".parse().unwrap();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.switch(View::Graph, &vault);
    let ids = state.row_ids(&vault);
    state.row = ids.iter().position(|id| *id == source).unwrap();
    press(&mut state, &vault, "x");
    state.row = ids.iter().position(|id| *id == target).unwrap();

    let text = screen(&state, &vault, 30, 6);
    let lines: Vec<_> = text.lines().collect();
    assert!(lines[0].contains("SELECT · 1 nodes"), "{text}");
    assert!(
        lines[1..5].iter().any(|line| line.contains("实现地址转换")),
        "{text}"
    );
    assert_eq!(lines[5].trim(), "a add  d remove  Esc clear", "{text}");

    press(&mut state, &vault, "\x1b");
    let text = screen(&state, &vault, 30, 6);
    assert!(text.lines().last().unwrap().contains("nodes"), "{text}");
    assert!(!text.contains("Esc clear"), "{text}");
}

#[test]
fn graph_shortcut_uses_the_highlighted_work_unit_and_keeps_the_date_range() {
    let vault = fixture();
    let mapping = "30000000-0000-4000-8000-000000000002".parse().unwrap();
    let previously_focused = "30000000-0000-4000-8000-000000000001".parse().unwrap();
    for view_key in ['d', 'a', 'l'] {
        let mut state = State::new("2026-11-24".parse().unwrap());
        press(&mut state, &vault, &format!("vl u{view_key}"));
        let row = state
            .row_ids(&vault)
            .iter()
            .position(|&id| id == mapping)
            .unwrap();
        press(&mut state, &vault, &"j".repeat(row));
        // A prior view can leave a remembered ID different from the visible row.
        state.document_id = Some(previously_focused);
        let dates = state.selected_range();
        let day = state.day;
        assert!(screen(&state, &vault, 38, 18).contains("g Graph"));

        press(&mut state, &vault, "g");
        assert!(state.view == View::Graph);
        assert_eq!(state.selected_document(&vault), Some(mapping));
        assert_eq!(state.day, day);
        assert_eq!(state.selected_range(), dates);
    }
}

#[test]
fn graph_shortcut_explains_why_a_calendar_event_cannot_open_graph() {
    let vault = fixture();
    let mut state = State::new("2026-11-25".parse().unwrap());
    press(&mut state, &vault, " ud");
    assert!(state.source(&vault).unwrap().ends_with("university.md"));
    let selected = state.selected_document(&vault);
    press(&mut state, &vault, "g");
    assert!(state.view == View::Day);
    assert_eq!(state.selected_document(&vault), selected);
    assert!(screen(&state, &vault, 38, 18).contains("Select a WorkUnit"));
}

#[test]
fn graph_dependency_edit_previews_then_confirms_or_cancels() {
    let vault = fixture();
    let read_spec: Id = "30000000-0000-4000-8000-000000000001".parse().unwrap();
    let mapping: Id = "30000000-0000-4000-8000-000000000002".parse().unwrap();
    let translation: Id = "30000000-0000-4000-8000-000000000003".parse().unwrap();
    let mut state = State::new("2026-11-25".parse().unwrap());
    state.switch(View::Graph, &vault);
    state.row = state
        .row_ids(&vault)
        .iter()
        .position(|&id| id == read_spec)
        .unwrap();
    state.handle(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &vault,
    );
    state.row = state
        .row_ids(&vault)
        .iter()
        .position(|&id| id == translation)
        .unwrap();
    assert_eq!(
        state.handle(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &vault
        ),
        Action::Continue
    );
    let preview = screen(&state, &vault, 72, 18);
    assert!(preview.contains("Review dependency changes"), "{preview}");
    assert!(
        preview.contains("阅读 Sv39 规范 → 实现地址转换"),
        "{preview}"
    );
    let narrow = screen(&state, &vault, 28, 6);
    assert!(narrow.contains("Target:"), "{narrow}");
    state.handle(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &vault);
    let end = screen(&state, &vault, 28, 6);
    let impact: String = end
        .lines()
        .skip(2)
        .take(2)
        .map(|line| {
            line.strip_prefix('│')
                .unwrap()
                .strip_suffix('│')
                .unwrap()
                .trim_end()
        })
        .collect();
    assert_eq!(impact, "No WorkUnit blocking status changes", "{end}");
    assert!(end.contains("y apply  Esc cancel"), "{end}");
    state.handle(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), &vault);
    assert_eq!(screen(&state, &vault, 28, 6), narrow);
    assert_eq!(
        state.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &vault),
        Action::Continue
    );
    assert!(matches!(state.overlay, Overlay::None));
    assert!(
        !vault
            .work_unit(translation)
            .unwrap()
            .data
            .depends_on
            .contains(&read_spec)
    );

    state.row = state
        .row_ids(&vault)
        .iter()
        .position(|&id| id == mapping)
        .unwrap();
    state.handle(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &vault,
    );
    state.row = state
        .row_ids(&vault)
        .iter()
        .position(|&id| id == translation)
        .unwrap();
    state.handle(
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        &vault,
    );
    let preview = screen(&state, &vault, 72, 18);
    assert!(preview.contains("Unblocked: 实现地址转换"), "{preview}");
    let Action::SetGraphDependencies(edit) = state.handle(
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        &vault,
    ) else {
        panic!("expected confirmed dependency edit");
    };
    assert_eq!(edit.target, translation);
    assert!(edit.prerequisites.is_empty());
    assert!(edit.revision == *vault.revision(translation).unwrap());
}

fn writable_project() -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().unwrap();
    let vault = copy_project(dir.path());
    (dir, vault)
}

fn copy_project(root: &Path) -> Vault {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault/projects/rust-os");
    let target = root.join("projects/rust-os");
    for relative in [
        "project.md",
        "tasks/memory.md",
        "units/mapping.md",
        "units/read-spec.md",
        "units/translation.md",
    ] {
        let path = target.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(source.join(relative), path).unwrap();
    }
    Vault::load(root, chrono_tz::Asia::Shanghai).unwrap()
}

fn submit_time_block(state: &mut State, vault: &Vault) -> time_block::Submission {
    match state.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), vault) {
        Action::CreateTimeBlock(block) => block,
        action => panic!("expected a time block submission, got {action:?}"),
    }
}

#[test]
fn short_time_block_form_keeps_active_field_and_escape_visible() {
    let vault = fixture();
    let mut state = State::new("2026-11-20".parse().unwrap());
    press(&mut state, &vault, " tb映射");
    let picker = screen(&state, &vault, 28, 5);
    let rows: Vec<_> = picker.lines().collect();
    assert!(
        rows[1..4].iter().any(|row| row.contains("Sv39 页表映射")),
        "{picker}"
    );
    assert!(
        rows[4].contains("Enter") && rows[4].contains("Esc"),
        "{picker}"
    );

    press(&mut state, &vault, "\n13:30\n15:00");
    let editor = screen(&state, &vault, 28, 5);
    let rows: Vec<_> = editor.lines().collect();
    assert!(
        rows[1..4]
            .iter()
            .any(|row| row.contains("End") && row.contains("15:00▏")),
        "{editor}"
    );
    assert!(
        rows[4].contains("Enter save") && rows[4].contains("Esc"),
        "{editor}"
    );

    press(&mut state, &vault, "\x1b\x1b");
    assert!(matches!(state.overlay, Overlay::None));
}

#[test]
fn month_menu_creates_work_unit_time_block_on_the_selected_date() {
    let (dir, mut vault) = writable_project();
    let mut state = State::new("2026-11-19".parse().unwrap());
    press(&mut state, &vault, "l t");
    assert!(screen(&state, &vault, 80, 24).contains("b add time block"));
    press(&mut state, &vault, "b映射");
    let text = screen(&state, &vault, 42, 14);
    assert!(text.contains("Time block · 2026-11-20"), "{text}");
    assert!(text.contains("实现 Sv39 页表映射"), "{text}");
    assert!(!text.contains("阅读 Sv39"), "{text}");

    press(&mut state, &vault, "\n13:30\n12:30\n");
    let text = screen(&state, &vault, 80, 24);
    assert!(text.contains("Enter save"), "{text}");
    assert_eq!(
        state.message,
        mcbtodo_domain::time::TimeError::TimeOrder.to_string()
    );
    assert!(text.contains("13:30") && text.contains("12:30"), "{text}");
    for _ in 0..5 {
        state.handle(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &vault,
        );
    }
    press(&mut state, &vault, "15:00");
    let block = submit_time_block(&mut state, &vault);
    let (id, when) = (block.unit, block.when);
    assert!(!vault.calendar().iter().any(|entry| {
        entry.id == id && matches!(entry.occurrence, Occurrence::Span(Span::DateTimeSpan(span)) if span == when)
    }));
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    state.create_time_block(dir.path(), &mut vault, &paths, block);
    let text = screen(&state, &vault, 80, 24);
    assert!(text.contains("Day  2026-11-20"), "{text}");
    assert!(text.contains("Time block saved"), "{text}");
    assert!(text.contains("13:30–15:00  实现 Sv39 页表映射"), "{text}");
    assert_eq!(state.day_entries(&vault).unwrap()[state.row].id, id);

    let persisted = Vault::load(dir.path(), vault.timezone()).unwrap();
    assert!(persisted.calendar().iter().any(|entry| {
        entry.id == id
            && entry.busy
            && matches!(entry.occurrence, Occurrence::Span(Span::DateTimeSpan(span)) if span == when)
    }));
}

#[test]
#[cfg(target_os = "linux")]
fn default_time_block_leaves_the_work_unit_unchanged_when_the_daemon_is_absent() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let mut vault = copy_project(&root);
    let mut state = State::new("2026-11-20".parse().unwrap());
    press(&mut state, &vault, " tb映射\n13:30\n15:00");
    let block = submit_time_block(&mut state, &vault);
    let path = vault.source(block.unit).unwrap().to_owned();
    let original = std::fs::read(&path).unwrap();

    state.create_time_block(&root, &mut vault, &paths, block);

    assert!(state.message.contains("Start mcbtodo-daemon"));
    assert!(matches!(state.overlay, Overlay::TimeBlock(_)));
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
#[cfg(target_os = "linux")]
fn default_time_block_response_loss_reports_the_interval_without_replaying() {
    use super::creation_tests::disconnect_after_request;
    use mcbtodo_protocol::Request;

    let home = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize().unwrap();
    let root = paths.vault_root();
    let mut vault = copy_project(&root);
    let mut state = State::new("2026-11-20".parse().unwrap());
    press(&mut state, &vault, " tb映射\n13:30\n15:00");
    let block = submit_time_block(&mut state, &vault);
    let (unit, when, observed) = (block.unit, block.when, block.revision.digest());
    let path = vault.source(unit).unwrap().to_owned();
    let original = std::fs::read(&path).unwrap();

    let sent = disconnect_after_request(&paths, || {
        state.create_time_block(&root, &mut vault, &paths, block);
    });

    assert_eq!(
        sent,
        Request::CreateTimeBlock {
            work_unit_id: unit,
            work_unit_revision: observed,
            start: when.start(),
            end: when.end(),
        }
    );
    assert!(matches!(state.overlay, Overlay::None));
    assert!(state.message.contains("outcome unknown"));
    assert!(state.message.contains(&unit.to_string()));
    assert!(state.message.contains("13:30–15:00"));
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn a_stale_time_block_write_keeps_the_form_and_preserves_concurrent_edits() {
    let (dir, mut vault) = writable_project();
    let mut state = State::new("2026-11-20".parse().unwrap());
    press(&mut state, &vault, " tb映射\n13:30\n15:00");
    let block = submit_time_block(&mut state, &vault);
    let path = vault.source(block.unit).unwrap().to_owned();
    let edited = format!(
        "{}\nConcurrent edit.\n",
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, &edited).unwrap();
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    state.create_time_block(dir.path(), &mut vault, &paths, block);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    let text = screen(&state, &vault, 80, 24);
    assert!(text.contains("WorkUnit changed"), "{text}");
    assert!(text.contains("13:30") && text.contains("15:00"), "{text}");
    press(&mut state, &vault, "\x1b\x1b");
    assert!(matches!(state.overlay, Overlay::None));
    assert!(state.view == View::Month);
}

#[test]
fn time_block_confirmation_keeps_the_revision_chosen_before_auto_reload() {
    let (dir, mut vault) = writable_project();
    let mut state = State::new("2026-11-20".parse().unwrap());
    press(&mut state, &vault, " tb映射\n13:30\n15:00");

    let path = dir.path().join("projects/rust-os/units/mapping.md");
    let edited = format!(
        "{}\nConcurrent edit.\n",
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, &edited).unwrap();
    assert!(state.refresh_external(dir.path(), &mut vault));

    let block = submit_time_block(&mut state, &vault);
    let paths = WorkspacePaths::from_home(dir.path().to_owned());
    state.create_time_block(dir.path(), &mut vault, &paths, block);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    let text = screen(&state, &vault, 80, 24);
    assert!(text.contains("WorkUnit changed"), "{text}");
    assert!(text.contains("13:30") && text.contains("15:00"), "{text}");
}
