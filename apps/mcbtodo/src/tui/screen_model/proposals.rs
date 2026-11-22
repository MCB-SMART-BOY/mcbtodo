use super::super::{
    proposal_review::{DetailKind, Review, label},
    review::PannableReview,
};

pub(in crate::tui) struct ProposalScreenModel<'a> {
    pub(in crate::tui) panel: ProposalPanel<'a>,
    full_hint: &'static str,
    compact_hint: &'static str,
}

pub(in crate::tui) enum ProposalPanel<'a> {
    Detail {
        title: &'static str,
        text: &'a str,
        viewport: &'a PannableReview,
    },
    List {
        labels: Vec<String>,
        selected: usize,
    },
    Empty(&'static str),
}

impl<'a> ProposalScreenModel<'a> {
    pub(in crate::tui) fn project(review: &'a Review, has_feedback: bool) -> Self {
        let (panel, full_hint, compact_hint) = if let Some(detail) = &review.detail {
            let title = match detail.kind {
                DetailKind::Proposal => " AI proposal ",
                DetailKind::GraphDiff => " Graph Diff ",
                DetailKind::WorkflowSnapshot => " Workflow snapshot ",
            };
            let (full_hint, compact_hint) = if detail.can_approve() {
                (
                    "a approve  x reject  Esc list  jk scroll  w wrap",
                    "a approve  x reject Esc",
                )
            } else {
                ("Esc list  jk scroll  w wrap", "Esc list  jk  w wrap")
            };
            (
                ProposalPanel::Detail {
                    title,
                    text: detail.text.trim_end_matches('\n'),
                    viewport: &review.viewport,
                },
                full_hint,
                compact_hint,
            )
        } else if review.rows.is_empty() {
            let message = if has_feedback {
                "Proposal list unavailable"
            } else {
                "No proposals for this Vault and timezone"
            };
            (
                ProposalPanel::Empty(message),
                "r refresh  Esc back",
                "r refresh  Esc",
            )
        } else {
            (
                ProposalPanel::List {
                    labels: review.rows.iter().map(label).collect(),
                    selected: review.row,
                },
                "jk select  Enter review  r refresh  Esc back",
                "jk  Enter review  r  Esc",
            )
        };
        Self {
            panel,
            full_hint,
            compact_hint,
        }
    }

    pub(in crate::tui) fn hint(&self, width: u16) -> &'static str {
        if usize::from(width) > self.full_hint.len() {
            self.full_hint
        } else {
            self.compact_hint
        }
    }
}
