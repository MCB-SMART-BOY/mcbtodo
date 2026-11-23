use crate::tui::{
    screen_model::{PickerScreenModel, display_line},
    time_block::{Field, Form},
};

pub(in crate::tui) struct TimeBlockScreenModel<'a> {
    pub(in crate::tui) title: String,
    pub(in crate::tui) panel: TimeBlockPanel<'a>,
    actions: (&'static str, &'static str),
}

pub(in crate::tui) enum TimeBlockPanel<'a> {
    Picker(PickerScreenModel),
    Editor {
        context: String,
        fields: [(&'static str, &'a str); 2],
        focused: usize,
    },
}

impl<'a> TimeBlockScreenModel<'a> {
    pub(in crate::tui) fn from_form(form: &'a Form) -> Self {
        let (panel, actions) = if let Some(choice) = form.selected() {
            let focused = usize::from(form.field == Field::End);
            let actions = [
                (
                    "Enter next  Tab switch field  Esc WorkUnits",
                    "Enter next  Tab  Esc",
                ),
                (
                    "Enter save  Tab switch field  Esc WorkUnits",
                    "Enter save  Tab  Esc",
                ),
            ][focused];
            (
                TimeBlockPanel::Editor {
                    context: format!("{} · busy · {}", display_line(&choice.title), form.timezone),
                    fields: [("Start", &form.start), ("End", &form.end)],
                    focused,
                },
                actions,
            )
        } else {
            (
                TimeBlockPanel::Picker(PickerScreenModel {
                    query: format!("Find WorkUnit: {}▏", display_line(&form.query)),
                    labels: form
                        .matches()
                        .map(|(_, choice)| {
                            display_line(&format!(
                                "{}  ·  {} / {}",
                                choice.title, choice.project, choice.task
                            ))
                        })
                        .collect(),
                    selected: form.row,
                    empty: if form.query.is_empty() {
                        "No WorkUnits"
                    } else {
                        "No matching WorkUnits"
                    },
                }),
                ("↑↓ WorkUnit  Enter choose  Esc cancel", "↑↓  Enter  Esc"),
            )
        };
        Self {
            title: format!(" Time block · {} ", form.day),
            panel,
            actions,
        }
    }

    pub(in crate::tui) fn hint(&self, width: u16) -> &'static str {
        if width >= 44 {
            self.actions.0
        } else if width >= 21 {
            self.actions.1
        } else {
            "Enter  Esc"
        }
    }
}
