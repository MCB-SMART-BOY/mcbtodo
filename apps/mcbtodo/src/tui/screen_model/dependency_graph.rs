use std::{collections::BTreeSet, rc::Rc};

use mcbtodo_domain::Id;

use super::super::{
    graph::{Edit, Node},
    review::{ReviewViewport, append_review_line},
};

pub(in crate::tui) struct GraphScreenModel<'a> {
    nodes: Rc<[Node]>,
    row: usize,
    selection: Option<&'a BTreeSet<Id>>,
}

pub(in crate::tui) struct GraphRow {
    pub(in crate::tui) label: String,
    pub(in crate::tui) focused: bool,
}

pub(in crate::tui) struct GraphDetails {
    pub(in crate::tui) title: String,
    pub(in crate::tui) prerequisites: Neighbors,
    pub(in crate::tui) dependents: Neighbors,
}

pub(in crate::tui) struct Neighbors {
    pub(in crate::tui) heading: String,
    pub(in crate::tui) rows: Vec<String>,
}

impl<'a> GraphScreenModel<'a> {
    pub(super) fn project(
        nodes: Rc<[Node]>,
        row: usize,
        selection: Option<&'a BTreeSet<Id>>,
    ) -> Self {
        Self {
            row: row.min(nodes.len().saturating_sub(1)),
            nodes,
            selection,
        }
    }

    pub(in crate::tui) fn selection_hint(&self, width: u16) -> Option<&'static str> {
        let target = self.nodes.get(self.row)?;
        if self.selection?.iter().any(|source| *source != target.id) {
            Some(if width >= 27 {
                "a add  d remove  Esc clear"
            } else {
                "a +  d -  Esc"
            })
        } else {
            Some("x mark  Esc clear")
        }
    }

    pub(in crate::tui) fn visible_rows(
        &self,
        capacity: usize,
    ) -> impl Iterator<Item = GraphRow> + '_ {
        let first = self
            .row
            .saturating_sub(capacity / 2)
            .min(self.nodes.len().saturating_sub(capacity));
        self.nodes
            .iter()
            .enumerate()
            .skip(first)
            .take(capacity)
            .map(|(index, node)| {
                let marked = self.selection.is_some_and(|ids| ids.contains(&node.id));
                let marker = if marked { '*' } else { ' ' };
                let blocked = if node.blocked { " [blocked]" } else { "" };
                GraphRow {
                    label: format!("{marker} {}  {}{blocked}", index + 1, node.title),
                    focused: index == self.row,
                }
            })
    }

    pub(in crate::tui) fn details(&self, capacity: usize) -> Option<GraphDetails> {
        let focused = self.nodes.get(self.row)?;
        let incoming = focused
            .prerequisites
            .len()
            .min(capacity - focused.dependents.len().min(capacity / 2));
        let outgoing = capacity - incoming;
        Some(GraphDetails {
            title: format!(" {}  {} ", self.row + 1, focused.title),
            prerequisites: self.neighbors("Prerequisites", "←", &focused.prerequisites, incoming),
            dependents: self.neighbors("Dependents", "→", &focused.dependents, outgoing),
        })
    }

    fn neighbors(&self, label: &str, arrow: &str, indices: &[usize], limit: usize) -> Neighbors {
        let count = indices.len();
        let mut heading = format!("{label} ({count})");
        if count > limit {
            heading.push_str(&format!(" · +{} more", count - limit));
        }
        let rows = indices
            .iter()
            .take(limit)
            .map(|&index| format!("{arrow} {}  {}", index + 1, self.nodes[index].title))
            .collect();
        Neighbors { heading, rows }
    }
}

pub(in crate::tui) struct GraphEditScreenModel<'a> {
    pub(in crate::tui) text: String,
    pub(in crate::tui) viewport: &'a ReviewViewport,
}

impl<'a> GraphEditScreenModel<'a> {
    pub(super) fn project<'title>(
        edit: &Edit,
        viewport: &'a ReviewViewport,
        title: impl Fn(Id) -> Option<&'title str>,
    ) -> Self {
        let title = |id| title(id).unwrap_or("<missing WorkUnit>");
        let target = title(edit.target);
        let mut text = String::from("Target: ");
        append_review_line(&mut text, target);
        text.push_str("\nPrerequisite → dependent");
        for (heading, marker, sources) in [
            ("Add edges", "+ ", &edit.added),
            ("Remove edges", "- ", &edit.removed),
        ] {
            text.push('\n');
            text.push_str(heading);
            for &id in sources {
                text.push('\n');
                text.push_str(marker);
                append_review_line(&mut text, title(id));
                text.push_str(" → ");
                append_review_line(&mut text, target);
            }
        }
        text.push_str("\nReadiness impact");
        if edit.newly_blocked.is_empty() && edit.unblocked.is_empty() {
            text.push_str("\nNo WorkUnit blocking status changes");
        }
        for (label, units) in [
            ("\nBlocked: ", &edit.newly_blocked),
            ("\nUnblocked: ", &edit.unblocked),
        ] {
            for &id in units {
                text.push_str(label);
                append_review_line(&mut text, title(id));
            }
        }
        Self { text, viewport }
    }

    pub(in crate::tui) fn hint(width: u16) -> &'static str {
        match width {
            52.. => "y apply  Esc cancel  ↑↓/jk  PgUp/PgDn  Home/End",
            24.. => "y apply  Esc cancel  ↑↓",
            _ => "y Esc ↑↓",
        }
    }
}
