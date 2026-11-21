use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use mcbtodo_domain::{Id, TaskGraph, WorkUnit};
use mcbtodo_vault::Vault;
use serde::{Deserialize, Serialize};

use crate::validate_markdown;

const MAX_UNITS: usize = 32;
const MAX_PREREQUISITES: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub task_id: Id,
    pub units: Vec<UnitDraft>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnitDraft {
    pub id: Id,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub depends_on: Vec<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_minutes: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphDiff {
    pub project_id: Id,
    pub task_id: Id,
    pub added_nodes: Vec<UnitDraft>,
    pub added_edges: Vec<Edge>,
    pub ready: Vec<Id>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub prerequisite: Id,
    pub dependent: Id,
}

impl GraphDiff {
    pub fn existing_dependencies(&self) -> BTreeSet<Id> {
        let added: BTreeSet<_> = self.added_nodes.iter().map(|unit| unit.id).collect();
        self.added_edges
            .iter()
            .map(|edge| edge.prerequisite)
            .filter(|id| !added.contains(id))
            .collect()
    }

    pub fn validate_against(&self, draft: &Draft) -> Result<()> {
        ensure!(
            self.task_id == draft.task_id && self.project_id != self.task_id,
            "graph diff has inconsistent parent IDs"
        );
        let draft_units: BTreeMap<_, _> = draft.units.iter().map(|unit| (unit.id, unit)).collect();
        let positions: BTreeMap<_, _> = self
            .added_nodes
            .iter()
            .enumerate()
            .map(|(index, unit)| (unit.id, index))
            .collect();
        ensure!(
            draft_units.len() == positions.len() && positions.len() == self.added_nodes.len(),
            "graph diff has inconsistent work unit IDs"
        );
        for (index, unit) in self.added_nodes.iter().enumerate() {
            ensure!(
                draft_units.get(&unit.id) == Some(&unit) && unit.id != self.project_id,
                "graph diff has an inconsistent work unit {}",
                unit.id
            );
            ensure!(
                unit.depends_on.iter().all(|prerequisite| {
                    *prerequisite != self.project_id
                        && *prerequisite != self.task_id
                        && positions
                            .get(prerequisite)
                            .is_none_or(|&before| before < index)
                }),
                "graph diff has an invalid dependency or order"
            );
        }
        ensure!(
            self.added_edges == edges(&draft_units),
            "graph diff has inconsistent edges"
        );
        let ready: BTreeSet<_> = self.ready.iter().copied().collect();
        ensure!(
            ready.len() == self.ready.len()
                && self.ready.iter().all(|id| {
                    draft_units.get(id).is_some_and(|unit| {
                        unit.depends_on.iter().all(|id| !positions.contains_key(id))
                    })
                })
                && draft_units
                    .values()
                    .all(|unit| !unit.depends_on.is_empty() || ready.contains(&unit.id)),
            "graph diff has inconsistent ready units"
        );
        Ok(())
    }
}

impl Draft {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_UNITS).contains(&self.units.len()),
            "provide between 1 and {MAX_UNITS} work units"
        );
        let mut ids = BTreeSet::new();
        for unit in &self.units {
            validate_markdown(&unit.title, &unit.body)?;
            ensure!(
                unit.depends_on.len() <= MAX_PREREQUISITES,
                "work unit {} exceeds {MAX_PREREQUISITES} prerequisites",
                unit.id
            );
            ensure!(
                unit.depends_on.iter().collect::<BTreeSet<_>>().len() == unit.depends_on.len(),
                "work unit {} lists a prerequisite more than once",
                unit.id
            );
            ensure!(
                ids.insert(unit.id),
                "document UUID {} already exists in the draft",
                unit.id
            );
        }
        ensure!(
            !ids.contains(&self.task_id),
            "a new work unit reuses the parent task UUID"
        );
        Ok(())
    }

    /// Builds a validated candidate DAG against one consistent Vault snapshot.
    ///
    /// Proposal capture passes its already-loaded snapshot here so the preview
    /// and the expected revisions are derived from the same state.
    pub fn preview(&self, vault: &Vault) -> Result<GraphDiff> {
        self.validate()?;
        let mut project = None;
        let mut parent_project = None;
        for row in vault.outline() {
            match row.depth {
                0 => project = Some(row.id),
                1 if row.id == self.task_id => {
                    parent_project = project;
                    break;
                }
                _ => (),
            }
        }
        let project_id = parent_project.context("parent task does not exist")?;
        let mut added = BTreeMap::new();
        let mut units = Vec::new();
        for draft in &self.units {
            ensure!(
                vault.source(draft.id).is_none(),
                "document UUID {} already exists in the Vault",
                draft.id
            );
            added.insert(draft.id, draft);
            units.push(draft.work_unit(project_id, self.task_id));
        }
        let existing = vault
            .task_graph()
            .order()
            .iter()
            .map(|&id| &vault.work_unit(id).expect("validated graph work unit").data);
        let candidate = TaskGraph::build(existing.chain(&units))?;
        let added_nodes: Vec<_> = candidate
            .order()
            .iter()
            .filter_map(|id| added.get(id).map(|&draft| draft.clone()))
            .collect();
        let added_edges = edges(&added);
        let ready = added_nodes
            .iter()
            .map(|unit| unit.id)
            .filter(|&id| !candidate.blocked(id))
            .collect();
        Ok(GraphDiff {
            project_id,
            task_id: self.task_id,
            added_nodes,
            added_edges,
            ready,
        })
    }
}

fn edges(units: &BTreeMap<Id, &UnitDraft>) -> Vec<Edge> {
    units
        .values()
        .flat_map(|unit| {
            let mut prerequisites = unit.depends_on.clone();
            prerequisites.sort_unstable();
            prerequisites.into_iter().map(|prerequisite| Edge {
                prerequisite,
                dependent: unit.id,
            })
        })
        .collect()
}

impl UnitDraft {
    pub fn work_unit(&self, project_id: Id, task_id: Id) -> WorkUnit {
        WorkUnit {
            id: self.id,
            project_id,
            parent_task_id: task_id,
            status: Default::default(),
            priority: Default::default(),
            depends_on: self.depends_on.clone(),
            estimated_minutes: self.estimated_minutes,
            deadline: None,
            time_blocks: Vec::new(),
            workflow_revision: None,
            evidence: Vec::new(),
            tags: Vec::new(),
        }
    }
}
