use std::collections::BTreeMap;

use anyhow::Result;
use mcbtodo_application::{
    DependencyOperation, WorkUnitDependencyPreview, preview_work_unit_dependency_change,
};
use mcbtodo_domain::{Id, WorkUnitGraphNode};
use mcbtodo_vault::{Revision, Vault};

pub(super) struct Node {
    pub(super) id: Id,
    pub(super) title: String,
    pub(super) blocked: bool,
    pub(super) prerequisites: Vec<usize>,
    pub(super) dependents: Vec<usize>,
}

pub(super) type Edit = WorkUnitDependencyPreview;

pub(super) fn nodes(items: Vec<WorkUnitGraphNode>) -> Vec<Node> {
    let indices: BTreeMap<_, _> = items
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    items
        .into_iter()
        .map(|node| Node {
            id: node.id,
            title: node.title,
            blocked: node.blocked,
            prerequisites: node
                .prerequisites
                .iter()
                .filter_map(|id| indices.get(id).copied())
                .collect(),
            dependents: node
                .dependents
                .iter()
                .filter_map(|id| indices.get(id).copied())
                .collect(),
        })
        .collect()
}

pub(super) fn edit(
    vault: &Vault,
    target: Id,
    sources: &std::collections::BTreeSet<Id>,
    adding: bool,
) -> Result<Edit> {
    preview_work_unit_dependency_change(
        vault,
        target,
        sources,
        if adding {
            DependencyOperation::Add
        } else {
            DependencyOperation::Remove
        },
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub(super) target: Id,
    pub(super) prerequisites: Vec<Id>,
    pub(super) revision: Revision,
}
