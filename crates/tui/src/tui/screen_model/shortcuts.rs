use ratatui::text::Line;

use super::super::{
    View,
    keymap::{Action::*, Keymap},
};

pub(in crate::tui) struct MainHints {
    help: String,
    menu: String,
    compact: String,
    wide: String,
    full_week: Option<String>,
}

impl MainHints {
    pub(in crate::tui) fn project(keys: &Keymap, view: View) -> Self {
        let help = keys.label(Help);
        Self {
            menu: format!("{} menu  {help} help", keys.label(Space)),
            help,
            compact: main(keys, view, true, false),
            wide: main(keys, view, false, false),
            full_week: (view == View::Week).then(|| main(keys, view, false, true)),
        }
    }

    pub(in crate::tui) fn visible(&self, width: u16, height: u16, hide_view_actions: bool) -> &str {
        let show_view_actions = !hide_view_actions && width >= 24;
        [
            self.full_week
                .as_deref()
                .filter(|_| show_view_actions && width >= 72 && height >= 8),
            (show_view_actions && width >= 52).then_some(self.wide.as_str()),
            show_view_actions.then_some(self.compact.as_str()),
            Some(self.menu.as_str()),
            Some(self.help.as_str()),
        ]
        .into_iter()
        .flatten()
        // The renderer leaves one column of padding before the hint.
        .find(|hint| !hint.is_empty() && Line::raw(*hint).width() < usize::from(width))
        .unwrap_or_default()
    }
}

fn main(keys: &Keymap, view: View, compact: bool, full_week: bool) -> String {
    let menu = keys.label(Space);
    let help = keys.label(Help);
    let horizontal = keys.horizontal_label();
    let vertical = keys.vertical_label();
    let left = keys.label(MoveLeft);
    let right = keys.label(MoveRight);
    let all = format!("{left}{vertical}{right}");
    let select = keys.label(Select);
    if compact {
        return match view {
            View::Month => format!("{all} move  {select} select  {help}"),
            View::Week => format!("{horizontal} day  Enter day  {help}"),
            View::Day | View::Agenda => format!("{vertical} entry  g Graph  {help}"),
            View::Outline => format!("{vertical} move  g Graph  {help}"),
            View::Graph => format!("{vertical} nodes  {horizontal} edges  {help}"),
            View::Timeline => format!("{horizontal} date  {vertical} plan  {help}"),
            View::Inbox => format!("{vertical} select  t assign  {help}"),
            View::Workflow => String::new(),
        };
    }

    let edit = keys.label(Edit);
    let search = keys.label(Picker);
    match view {
        View::Month => {
            format!("{all} move  {select} select  Enter agenda  {search} search  {menu} menu")
        }
        View::Week if !full_week => {
            format!("{horizontal} day  PgUp/Dn week  Enter day  {select} select  {menu} menu")
        }
        View::Week => {
            format!("{horizontal} day  {vertical} hours  PgUp/Dn week  Enter day  {menu} menu")
        }
        View::Day => {
            format!("{horizontal} date  {vertical} entry  g Graph  Enter agenda  {menu} menu")
        }
        View::Agenda => {
            format!("{horizontal} date  {vertical} entry  g Graph  {edit} edit  {menu} menu")
        }
        View::Outline => {
            format!("{vertical} move  g Graph  {edit} edit  {search} search  {menu} menu")
        }
        View::Graph => {
            format!(
                "{vertical} nodes  {horizontal} edges  {select}/x select  {edit} edit  {menu} menu"
            )
        }
        View::Timeline => format!("{horizontal} date  {vertical} plan  {edit} edit  {menu} menu"),
        View::Workflow => String::new(),
        View::Inbox => format!("{vertical} select  t assign to Project  {edit} edit  {menu} menu"),
    }
}

pub(in crate::tui) fn space(keys: &Keymap, can_create: bool) -> String {
    let create = if can_create { "c create\n" } else { "" };
    let reload = keys.label(Reload);
    let help = keys.label(Help);
    format!(
        "{create}Enter settings\nd deadline\nu switch view\nt time operations\na AI proposals\n{reload} reload\n{help} help"
    )
}

pub(in crate::tui) fn views(keys: &Keymap, current: View) -> String {
    [
        (View::Month, keys.label(ViewMonth)),
        (View::Week, keys.label(ViewWeek)),
        (View::Day, keys.label(ViewDay)),
        (View::Agenda, keys.label(ViewAgenda)),
        (View::Timeline, keys.label(ViewTimeline)),
        (View::Outline, keys.label(ViewOutline)),
        (View::Graph, keys.label(ViewGraph)),
        (View::Inbox, "i".into()),
        (View::Workflow, keys.label(ViewWorkflow)),
    ]
    .map(|(view, key)| {
        let suffix = if view == current { " · current" } else { "" };
        format!("{key} {}{suffix}", view.label())
    })
    .join("\n")
}

pub(in crate::tui) fn help(keys: &Keymap) -> Vec<String> {
    let [
        search,
        menu,
        select,
        edit,
        reload,
        command,
        quit,
        month,
        week,
        day,
        agenda,
        outline,
        graph,
        timeline,
        workflow,
    ] = [
        Picker,
        Space,
        Select,
        Edit,
        Reload,
        Command,
        Quit,
        ViewMonth,
        ViewWeek,
        ViewDay,
        ViewAgenda,
        ViewOutline,
        ViewGraph,
        ViewTimeline,
        ViewWorkflow,
    ]
    .map(|action| keys.label(action));
    let vertical = keys.vertical_label();
    let horizontal = keys.horizontal_label();
    vec![
        format!("{search}   Search Projects, Tasks and WorkUnits; Enter focuses in Outline"),
        format!(
            "{menu} u {month}/{week}/{day}/{agenda}/{outline}/{graph}/{workflow}/i   Switch views"
        ),
        format!("{menu} u {timeline}   Timeline"),
        format!("{menu} c p/e/d/b   Create project / all-day event / deadline / time block"),
        format!("Outline: select a Project, then {menu} c t to capture a Task"),
        format!("{menu} d   Edit deadline on the selected Project, Task or WorkUnit"),
        format!("{menu} t   Time operations"),
        format!("{menu} t b   Add a time block on the calendar date"),
        format!("{menu} a   Review AI proposals"),
        format!("{menu} Enter   Settings"),
        "Proposal detail: ↑↓/jk scroll, PgUp/PgDn page, Home/End jump, w wrap, ←→/hl pan".into(),
        format!("Inbox: {vertical} select, t assign entry to a Project"),
        format!("{select}   Select an inclusive date range"),
        "Week: Enter opens Day, PgUp/PgDn moves by week".into(),
        "Month / Day: Enter opens selection in Agenda".into(),
        "Day / Agenda / Outline: g opens Graph on the focused WorkUnit".into(),
        format!(
            "Graph: {vertical} nodes, {horizontal} edges, {select}/x select sources, a add edges, d remove edges"
        ),
        "Graph dependency review: ↑↓/jk scroll, PgUp/PgDn page, Home/End jump, y apply, Esc cancel"
            .into(),
        format!("Workflow runs: {vertical} select, n older, {reload} refresh, Enter trace"),
        "Workflow decisions: a approve, d reject, y confirm".into(),
        format!(
            "Workflow confirmation: {vertical} scroll, PgUp/PgDn page, Home/End jump, Esc cancel"
        ),
        format!(
            "Workflow trace: {horizontal} step, {vertical} scroll, PgUp/PgDn page, \
             Home/End jump, w wrap, HL pan, Esc back"
        ),
        format!("{edit} / {command}edit   Open source in $VISUAL / $EDITOR / hx"),
        format!("{reload}   Reload vault"),
        format!("{command}   view, search, timer, proposals, date, today, reload, quit"),
        format!("{command}time-block / {command}reminder   Create calendar entries"),
        format!("{command}help   Show this help"),
        format!("{quit}   Quit"),
    ]
}
