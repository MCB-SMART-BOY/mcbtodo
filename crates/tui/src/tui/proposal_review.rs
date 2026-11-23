use std::{collections::BTreeMap, fmt::Write, path::Path};

use anyhow::Result;
use chrono_tz::Tz;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use mcbtodo_domain::Id;
use mcbtodo_platform::WorkspacePaths;
use sha2::{Digest, Sha256};

use super::{Action, move_row, review::PannableReview};
use mcbtodo_application::graph_diff::GraphDiff;

use mcbtodo_client::proposal::{Operation, Proposal, Queue, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Request {
    List,
    Show(Id),
    Reject(Id),
}

pub(super) fn label(proposal: &Proposal) -> String {
    let status = match proposal.status {
        Status::Pending => "pending",
        Status::Applying => "applying",
        Status::Applied => "applied",
        Status::Rejected => "rejected",
    };
    let operation = match &proposal.operation {
        Operation::CaptureTask { title, .. } => format!("Capture task · {title}"),
        Operation::TriageInbox { project_id, .. } => {
            format!("Assign Inbox entry to Project · {project_id}")
        }
        Operation::CompleteWorkUnit { .. } => "Complete WorkUnit".into(),
        Operation::CreateTimeBlock { .. } => "Schedule time block".into(),
        Operation::SetDeadline { date, .. } => format!("Set deadline · {date}"),
        Operation::ClearDeadline { .. } => "Clear deadline".into(),
        Operation::CreateReminder { title, .. } => format!("Create reminder · {title}"),
        Operation::WorkUnitGraphDiff(_) => "WorkUnit graph diff".into(),
        Operation::PublishWorkflow { .. } => "Publish workflow snapshot".into(),
    };
    terminal_text(&format!(
        "{status} · {operation} · {}",
        proposal.target_title
    ))
}

fn graph_preview(diff: &GraphDiff, target: &str) -> String {
    let mut preview = format!("Task: {}\n\nWorkUnits to add\n", terminal_text(target));
    let titles: BTreeMap<_, _> = diff
        .added_nodes
        .iter()
        .map(|node| (node.id, terminal_text(&node.title)))
        .collect();
    for node in &diff.added_nodes {
        let _ = write!(preview, "+ {}", titles[&node.id]);
        if let Some(minutes) = node.estimated_minutes {
            let _ = write!(preview, " · {minutes} min");
        }
        if diff.ready.contains(&node.id) {
            preview.push_str(" · ready");
        }
        preview.push('\n');
    }
    preview.push_str("\nDependencies (prerequisite → dependent)\n");
    if diff.added_edges.is_empty() {
        preview.push_str("None\n");
    }
    let title = |id| titles.get(&id).cloned().unwrap_or_else(|| format!("{id}"));
    for edge in &diff.added_edges {
        let _ = writeln!(
            preview,
            "{} → {}",
            title(edge.prerequisite),
            title(edge.dependent)
        );
    }
    preview.push_str("\nExact proposal\n");
    preview
}

fn workflow_preview(draft: &mcbtodo_workflow::WorkflowDraft) -> String {
    let mut preview = String::from("Workflow snapshot\n\nGraph\n");
    for line in draft.graph().lines() {
        let _ = writeln!(preview, "{}", terminal_text(line));
    }
    preview.push_str("\nSources\n");
    for (path, bytes) in draft.script_sources() {
        let digest = Sha256::digest(bytes);
        let digest = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let _ = writeln!(preview, "{path} · {} bytes · {}", bytes.len(), digest);
        if let Ok(source) = std::str::from_utf8(bytes) {
            for line in source.lines() {
                let _ = writeln!(preview, "  {}", terminal_text(line));
            }
            if source.ends_with('\n') {
                preview.push_str("  \n");
            }
        } else {
            preview.push_str("  [binary source; exact bytes pinned by digest]\n");
        }
    }
    preview.push('\n');
    preview
}

pub(super) struct Detail {
    pub text: String,
    pub kind: DetailKind,
    pending: Option<Id>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DetailKind {
    Proposal,
    GraphDiff,
    WorkflowSnapshot,
}

impl Detail {
    pub fn can_approve(&self) -> bool {
        self.pending.is_some()
    }
}

#[derive(Default)]
pub(super) struct Review {
    pub rows: Vec<Proposal>,
    pub row: usize,
    pub detail: Option<Detail>,
    pub viewport: PannableReview,
}

impl Review {
    pub fn handle(&mut self, key: KeyEvent, message: &mut String) -> Option<Action> {
        if let Some(detail) = &self.detail {
            match key.code {
                KeyCode::Esc => {
                    self.detail = None;
                    message.clear();
                }
                KeyCode::Char('a') if key.kind == KeyEventKind::Press => {
                    if let Some(id) = detail.pending {
                        return Some(Action::ApplyProposal(id));
                    }
                }
                KeyCode::Char('x') if key.kind == KeyEventKind::Press => {
                    if let Some(id) = detail.pending {
                        return Some(Action::Proposals(Request::Reject(id)));
                    }
                }
                KeyCode::Char('w') if key.kind == KeyEventKind::Press => {
                    self.viewport.toggle_wrap();
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    self.viewport.pan_right(8, &detail.text);
                }
                KeyCode::Char('h') | KeyCode::Left => self.viewport.pan_left(8),
                code => self.viewport.scroll(code, &detail.text),
            }
        } else {
            match key.code {
                KeyCode::Esc => return None,
                KeyCode::Char('r') => return Some(Action::Proposals(Request::List)),
                KeyCode::Enter => {
                    if let Some(summary) = self.rows.get(self.row) {
                        return Some(Action::Proposals(Request::Show(summary.id)));
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => self.row = self.row.saturating_sub(1),
                KeyCode::Char('j') | KeyCode::Down => {
                    self.row = move_row(self.row, true, self.rows.len());
                }
                _ => {}
            }
        }
        Some(Action::Continue)
    }

    pub fn execute(
        &mut self,
        root: &Path,
        zone: Tz,
        paths: &WorkspacePaths,
        request: Request,
    ) -> Result<()> {
        let queue = Queue::open(root, zone, paths)?;
        if let Request::Reject(id) = request {
            queue.reject(id)?;
            self.detail = None;
            if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                *row = queue.review(id)?;
            }
            return Ok(());
        }
        match request {
            Request::List => {
                let previous = self.rows.get(self.row).map(|row| row.id);
                self.rows = queue.list()?;
                self.row = self
                    .rows
                    .iter()
                    .position(|row| Some(row.id) == previous)
                    .unwrap_or(0);
            }
            Request::Show(id) => {
                let record = queue.review(id)?;
                let mut text = match (&record.operation, record.graph_diff()) {
                    (_, Some(graph)) => graph_preview(graph, &record.target_title),
                    (Operation::PublishWorkflow { draft }, None) => workflow_preview(draft),
                    (
                        Operation::CreateTimeBlock {
                            start,
                            end,
                            availability,
                            ..
                        },
                        None,
                    ) => {
                        format!(
                            "WorkUnit · {}\nStart · {start}\nEnd · {end}\nAvailability · {availability:?}\n\n",
                            terminal_text(&record.target_title)
                        )
                    }
                    (Operation::SetDeadline { date, .. }, None) => {
                        format!(
                            "Set deadline\nTarget · {}\nDate · {date}\n\n",
                            terminal_text(&record.target_title)
                        )
                    }
                    (Operation::ClearDeadline { .. }, None) => {
                        format!(
                            "Clear deadline\nTarget · {}\n\n",
                            terminal_text(&record.target_title)
                        )
                    }
                    _ => String::new(),
                };
                if !matches!(&record.operation, Operation::PublishWorkflow { .. }) {
                    let exact = serde_json::to_string_pretty(&record)?;
                    for line in exact.lines() {
                        let _ = writeln!(text, "{}", terminal_text(line));
                    }
                }
                self.detail = Some(Detail {
                    text,
                    kind: if record.graph_diff().is_some() {
                        DetailKind::GraphDiff
                    } else if matches!(&record.operation, Operation::PublishWorkflow { .. }) {
                        DetailKind::WorkflowSnapshot
                    } else {
                        DetailKind::Proposal
                    },
                    pending: (record.status == Status::Pending
                        || (record.status == Status::Applying
                            && record.workflow_revision().is_some()))
                    .then_some(id),
                });
                self.viewport = PannableReview::default();
            }
            Request::Reject(_) => unreachable!("handled above"),
        }
        Ok(())
    }
}

fn terminal_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control()
            || matches!(
                ch,
                '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            write!(text, "\\u{:04x}", u32::from(ch)).expect("writing to String");
        } else {
            text.push(ch);
        }
    }
    text
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tui::{Overlay, State, calendar_tests::screen, tests::fixture};
    use crossterm::event::KeyModifiers;
    use mcbtodo_client::proposal::submit;
    use mcbtodo_vault::Vault;
    use serde_json::json;

    const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
    const ZONE: Tz = chrono_tz::Asia::Shanghai;

    fn review(state: &mut State, vault: &Vault, paths: &WorkspacePaths, code: KeyCode) -> Action {
        let action = state.handle(KeyEvent::new(code, KeyModifiers::NONE), vault);
        if let Action::Proposals(request) = action {
            state
                .proposals
                .execute(Path::new(ROOT), ZONE, paths, request)
                .unwrap();
        }
        action
    }

    #[test]
    fn compact_proposal_review_keeps_actions_contextual() {
        use crate::tui::{
            calendar_tests::projected_screen,
            screen_model::{Content, ProposalScreenModel, ScreenChrome, ScreenModel},
        };

        let mut state = State::new("2026-11-20".parse().unwrap());
        state.overlay = Overlay::Proposals;
        let screen = |state: &State| {
            let model = ScreenModel {
                content: Content::Proposals(ProposalScreenModel::project(
                    &state.proposals,
                    !state.message.is_empty(),
                )),
                chrome: ScreenChrome::project(state, ZONE),
            };
            projected_screen(&model, 28, 6)
        };

        let empty = screen(&state);
        assert_eq!(empty.lines().last().unwrap().trim(), "r refresh  Esc back");
        assert!(!empty.contains("Enter review"));

        state.proposals.detail = Some(Detail {
            text: "中文 Rust review".into(),
            kind: DetailKind::GraphDiff,
            pending: Some("20000000-0000-4000-8000-000000000001".parse().unwrap()),
        });
        let pending = screen(&state);
        assert!(pending.contains("中文 Rust review"));
        assert_eq!(
            pending.lines().last().unwrap().trim(),
            "a approve  x reject Esc"
        );

        state.proposals.detail.as_mut().unwrap().pending = None;
        let read_only = screen(&state);
        assert_eq!(
            read_only.lines().last().unwrap().trim(),
            "Esc list  jk scroll  w wrap"
        );
    }

    #[test]
    fn narrow_proposal_review_wraps_long_lines_and_pages_to_the_end() {
        let vault = fixture();
        let mut state = State::new("2026-11-20".parse().unwrap());
        state.overlay = Overlay::Proposals;
        let id = "20000000-0000-4000-8000-000000000001".parse().unwrap();
        state.proposals.detail = Some(Detail {
            text: "中文 Rust 1234567890  END\nlast · 完成\n".into(),
            kind: DetailKind::Proposal,
            pending: Some(id),
        });
        let press =
            |state: &mut State, code| state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault);
        let first = screen(&state, &vault, 24, 5);
        assert_eq!(first.lines().nth(2).unwrap(), "│中文 Rust 1234567890  │");
        assert_eq!(
            first.lines().last().unwrap().trim(),
            "a approve  x reject Esc"
        );
        press(&mut state, KeyCode::PageDown);
        assert_eq!(
            screen(&state, &vault, 24, 5).lines().nth(2).unwrap(),
            "│END                   │"
        );
        press(&mut state, KeyCode::End);
        assert!(
            screen(&state, &vault, 24, 5)
                .lines()
                .nth(2)
                .unwrap()
                .contains("last · 完成")
        );
        press(&mut state, KeyCode::Char('w'));
        assert!(
            screen(&state, &vault, 24, 5)
                .lines()
                .nth(2)
                .unwrap()
                .contains("last · 完成"),
            "switching to unwrapped review should keep the current source line"
        );
        press(&mut state, KeyCode::Char('w'));
        press(&mut state, KeyCode::Home);
        assert_eq!(screen(&state, &vault, 24, 5), first);
        press(&mut state, KeyCode::Right);
        assert!(
            screen(&state, &vault, 24, 5)
                .lines()
                .nth(2)
                .unwrap()
                .contains("1234567890  END")
        );
        press(&mut state, KeyCode::Left);
        assert!(
            !screen(&state, &vault, 24, 5)
                .lines()
                .nth(2)
                .unwrap()
                .contains("END")
        );
        press(&mut state, KeyCode::Char('w'));
        assert_eq!(screen(&state, &vault, 24, 5), first);
        assert_eq!(
            press(&mut state, KeyCode::Char('a')),
            Action::ApplyProposal(id)
        );
    }

    #[test]
    fn review_keeps_its_source_line_when_reflowing_or_returning_to_wrapped_mode() {
        let vault = fixture();
        let mut state = State::new("2026-11-20".parse().unwrap());
        state.overlay = Overlay::Proposals;
        state.proposals.detail = Some(Detail {
            text: format!(
                "{}\n中段 target\n下一行 original\nAnother line\nLast line",
                "abcdefghij".repeat(6)
            ),
            kind: DetailKind::Proposal,
            pending: None,
        });
        let press =
            |state: &mut State, code| state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault);
        let row = |state: &State, width| {
            screen(state, &vault, width, 5)
                .lines()
                .nth(2)
                .unwrap()
                .to_string()
        };

        row(&state, 24);
        for _ in 0..3 {
            press(&mut state, KeyCode::Down);
            row(&state, 24);
        }
        assert!(row(&state, 24).contains("中段 target"));
        assert!(row(&state, 70).contains("中段 target"));
        press(&mut state, KeyCode::Char('w'));
        press(&mut state, KeyCode::Down);
        assert!(row(&state, 70).contains("下一行 original"));
        press(&mut state, KeyCode::Char('w'));
        assert!(row(&state, 24).contains("下一行 original"));
    }

    #[test]
    fn review_keeps_its_place_within_a_long_line_across_resizes() {
        let vault = fixture();
        let mut state = State::new("2026-11-20".parse().unwrap());
        state.overlay = Overlay::Proposals;
        state.proposals.detail = Some(Detail {
            text: format!("{}目标{}\nLast line", "x".repeat(44), "y".repeat(32)),
            kind: DetailKind::Proposal,
            pending: None,
        });
        let draw = |state: &State, width, height| screen(state, &vault, width, height);
        draw(&state, 24, 5);
        for _ in 0..2 {
            state.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &vault);
            draw(&state, 24, 5);
        }
        for (width, height) in [(24, 5), (28, 5), (88, 5), (24, 5), (24, 12), (24, 5)] {
            let visible = draw(&state, width, height);
            assert!(visible.contains("目标"), "{width}×{height}:\n{visible}");
        }
        state.handle(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &vault);
        assert!(draw(&state, 24, 5).contains("Last line"));
        assert!(draw(&state, 28, 5).contains("Last line"));
    }

    #[test]
    fn graph_proposal_can_be_reviewed_before_handing_off_to_apply() {
        let home = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let vault = fixture();
        let mut state = State::new("2026-11-20".parse().unwrap());
        let operation = serde_json::from_value(json!({
            "kind": "work_unit_graph_diff",
            "task_id": "20000000-0000-4000-8000-000000000001",
            "units": [
                {"id": "5dd2a1f8-270f-44b3-9013-22a39bd33550", "title": "Read spec", "estimated_minutes": 30},
                {"id": "5dd2a1f8-270f-44b3-9013-22a39bd33551", "title": "Implement mapping", "depends_on": ["5dd2a1f8-270f-44b3-9013-22a39bd33550"]}
            ]
        })).unwrap();
        let id = submit(Path::new(ROOT), ZONE, &paths, operation, None)
            .unwrap()
            .proposal_id;
        let press = |state: &mut State, code| review(state, &vault, &paths, code);
        press(&mut state, KeyCode::Char(' '));
        assert!(screen(&state, &vault, 80, 24).contains("a AI proposals"));
        assert_eq!(
            press(&mut state, KeyCode::Char('a')),
            Action::Proposals(Request::List)
        );
        assert!(screen(&state, &vault, 80, 24).contains("pending · WorkUnit graph diff"));
        assert_eq!(press(&mut state, KeyCode::Char('a')), Action::Continue);
        assert_eq!(
            press(&mut state, KeyCode::Enter),
            Action::Proposals(Request::Show(id))
        );
        let text = screen(&state, &vault, 80, 24);
        assert!(text.contains("Graph Diff"));
        assert!(text.contains("Read spec → Implement mapping"));
        assert!(!text.contains("Workflow snapshot "));
        assert!(text.contains("a approve"));
        let narrow = screen(&state, &vault, 40, 14);
        assert!(narrow.contains("Graph Diff"));
        press(&mut state, KeyCode::PageDown);
        let next_page = screen(&state, &vault, 40, 14);
        assert!(
            narrow.contains("Read spec → Implement mapping")
                || next_page.contains("Read spec → Implement mapping")
        );
        assert_eq!(
            press(&mut state, KeyCode::Char('a')),
            Action::ApplyProposal(id)
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(press(&mut state, KeyCode::Char('a')), Action::Continue);
    }

    #[test]
    fn workflow_proposal_review_shows_graph_source_digest_and_text() {
        let home = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let vault = fixture();
        let graph = r#"schema: mcbtodo.workflow/v1
id: tui-reviewed-workflow
name: TUI reviewed workflow
entry: check
nodes:
  check:
    kind: script
    runner: nushell
    source: scripts/check.nu
    timeout_seconds: 30
    capabilities: []
    outputs: {summary: string}
  report: {kind: action, action: report.create}
edges:
  - {from: check, to: report}
bindings:
  - {from: {node: check, port: summary}, to: {node: report, port: summary}}
"#;
        let source = b"print 'review this exact source'\n";
        let mut sources = mcbtodo_workflow::ScriptSources::new();
        sources.insert("scripts/check.nu".to_owned(), source.to_vec());
        let draft = mcbtodo_workflow::WorkflowDraft::new(graph.to_owned(), sources).unwrap();
        let id = submit(
            Path::new(ROOT),
            ZONE,
            &paths,
            Operation::PublishWorkflow { draft },
            None,
        )
        .unwrap()
        .proposal_id;
        let mut state = State::new("2026-11-20".parse().unwrap());
        let press = |state: &mut State, code| review(state, &vault, &paths, code);
        press(&mut state, KeyCode::Char(' '));
        press(&mut state, KeyCode::Char('a'));
        assert!(screen(&state, &vault, 80, 24).contains("pending · Publish workflow snapshot"));
        press(&mut state, KeyCode::Enter);
        let detail = state.proposals.detail.as_ref().unwrap();
        assert_eq!(detail.kind, super::DetailKind::WorkflowSnapshot);
        assert!(detail.text.contains("Workflow snapshot"));
        assert!(detail.text.contains("id: tui-reviewed-workflow"));
        assert!(detail.text.contains("scripts/check.nu · 33 bytes ·"));
        assert!(detail.text.contains("print 'review this exact source'"));
        let wide = screen(&state, &vault, 80, 24);
        assert!(wide.contains(" Workflow snapshot "));
        assert!(wide.contains("a approve"));
        let narrow = screen(&state, &vault, 40, 14);
        assert!(narrow.contains(" Workflow snapshot "));
        assert!(narrow.contains("id: tui-reviewed-workflow"));
        assert_eq!(
            press(&mut state, KeyCode::Char('a')),
            Action::ApplyProposal(id)
        );
    }

    #[test]
    fn time_block_review_shows_exact_offsets_and_can_reject_without_a_vault_write() {
        let home = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let vault = fixture();
        let unit = "30000000-0000-4000-8000-000000000002".parse().unwrap();
        let revision = vault.revision(unit).unwrap().digest();
        let operation = serde_json::from_value(json!({
            "kind": "create_time_block",
            "work_unit_id": unit,
            "expected_revision": revision,
            "start": "2026-11-21T09:00:00+08:00",
            "end": "2026-11-21T11:00:00+08:00",
            "availability": "free",
        }))
        .unwrap();
        let id = submit(Path::new(ROOT), ZONE, &paths, operation, None)
            .unwrap()
            .proposal_id;
        let mut state = State::new("2026-11-20".parse().unwrap());
        let press = |state: &mut State, code| review(state, &vault, &paths, code);
        press(&mut state, KeyCode::Char(' '));
        press(&mut state, KeyCode::Char('a'));
        assert!(screen(&state, &vault, 80, 24).contains("pending · Schedule time block"));
        press(&mut state, KeyCode::Enter);
        let report = screen(&state, &vault, 80, 24);
        assert!(report.contains("09:00:00 +08:00"));
        assert!(report.contains("11:00:00 +08:00"));
        assert!(report.contains("Availability · Free"));
        assert_eq!(
            press(&mut state, KeyCode::Char('x')),
            Action::Proposals(Request::Reject(id))
        );
        assert!(screen(&state, &vault, 80, 24).contains("rejected · Schedule time block"));
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Char('a')), Action::Continue);
        press(&mut state, KeyCode::Esc);
        press(&mut state, KeyCode::Esc);
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!(state.day.to_string(), "2026-11-20");
        assert_eq!(
            Vault::load(Path::new(ROOT), ZONE)
                .unwrap()
                .revision(unit)
                .unwrap()
                .digest(),
            revision
        );
    }

    #[test]
    fn deadline_proposals_show_action_and_date_in_list_and_detail() {
        let home = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let vault = fixture();
        let project: Id = "10000000-0000-4000-8000-000000000001".parse().unwrap();
        let revision = vault.revision(project).unwrap().digest();
        let operation = serde_json::from_value(json!({
            "kind": "set_deadline",
            "target_id": project,
            "expected_revision": revision,
            "date": "2026-12-10"
        }))
        .unwrap();
        let id = submit(Path::new(ROOT), ZONE, &paths, operation, None)
            .unwrap()
            .proposal_id;
        let mut state = State::new("2026-11-20".parse().unwrap());
        let press = |state: &mut State, code| review(state, &vault, &paths, code);
        press(&mut state, KeyCode::Char(' '));
        press(&mut state, KeyCode::Char('a'));
        assert!(screen(&state, &vault, 80, 24).contains("pending · Set deadline · 2026-12-10"));
        press(&mut state, KeyCode::Enter);
        let detail = screen(&state, &vault, 80, 24);
        assert!(detail.contains("Set deadline"));
        assert!(detail.contains("Date · 2026-12-10"));
        assert!(detail.contains("Rust OS"));
        assert_eq!(
            press(&mut state, KeyCode::Char('a')),
            Action::ApplyProposal(id)
        );
    }
}
