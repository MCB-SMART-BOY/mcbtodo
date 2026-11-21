use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{Id, WorkUnit, WorkUnitStatus};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphError {
    #[error("duplicate work unit {0}")]
    DuplicateUnit(Id),
    #[error("work unit {unit} depends on missing work unit {prerequisite}")]
    Missing { unit: Id, prerequisite: Id },
    #[error("work unit {unit} lists prerequisite {prerequisite} more than once")]
    DuplicateEdge { unit: Id, prerequisite: Id },
    #[error("cannot override dependencies for missing work unit {0}")]
    MissingOverride(Id),
    #[error("work-unit dependencies contain a cycle (unresolved work unit {0})")]
    Cycle(Id),
}

#[derive(Debug)]
pub struct TaskGraph {
    incoming: BTreeMap<Id, BTreeSet<Id>>,
    statuses: BTreeMap<Id, WorkUnitStatus>,
    order: Vec<Id>,
}

impl TaskGraph {
    pub fn build<'a>(units: impl IntoIterator<Item = &'a WorkUnit>) -> Result<Self, GraphError> {
        Self::build_inner(
            units
                .into_iter()
                .map(|unit| (unit.id, unit.status, unit.depends_on.clone())),
        )
    }

    pub fn build_with_dependencies<'a>(
        units: impl IntoIterator<Item = &'a WorkUnit>,
        target: Id,
        dependencies: &[Id],
    ) -> Result<Self, GraphError> {
        let units: Vec<_> = units.into_iter().collect();
        if !units.iter().any(|unit| unit.id == target) {
            return Err(GraphError::MissingOverride(target));
        }
        Self::build_inner(units.into_iter().map(|unit| {
            (
                unit.id,
                unit.status,
                if unit.id == target {
                    dependencies.to_vec()
                } else {
                    unit.depends_on.clone()
                },
            )
        }))
    }

    fn build_inner(
        units: impl IntoIterator<Item = (Id, WorkUnitStatus, Vec<Id>)>,
    ) -> Result<Self, GraphError> {
        let mut incoming = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        for (unit, status, depends_on) in units {
            if statuses.insert(unit, status).is_some() {
                return Err(GraphError::DuplicateUnit(unit));
            }
            let mut prerequisites = BTreeSet::new();
            for prerequisite in depends_on {
                if !prerequisites.insert(prerequisite) {
                    return Err(GraphError::DuplicateEdge { unit, prerequisite });
                }
            }
            incoming.insert(unit, prerequisites);
        }

        let mut outgoing: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
        let mut counts = BTreeMap::new();
        let mut ready = BTreeSet::new();
        for (&unit, prerequisites) in &incoming {
            if prerequisites.is_empty() {
                ready.insert(unit);
            }
            counts.insert(unit, prerequisites.len());
            for &prerequisite in prerequisites {
                if !statuses.contains_key(&prerequisite) {
                    return Err(GraphError::Missing { unit, prerequisite });
                }
                outgoing.entry(prerequisite).or_default().push(unit);
            }
        }
        let mut order = Vec::with_capacity(incoming.len());
        while let Some(unit) = ready.pop_first() {
            order.push(unit);
            for dependent in outgoing.get(&unit).into_iter().flatten() {
                let count = counts.get_mut(dependent).expect("known dependent");
                *count -= 1;
                if *count == 0 {
                    ready.insert(*dependent);
                }
            }
        }
        if let Some((&unit, _)) = counts.iter().find(|(_, count)| **count > 0) {
            return Err(GraphError::Cycle(unit));
        }
        Ok(Self {
            incoming,
            statuses,
            order,
        })
    }

    pub fn order(&self) -> &[Id] {
        &self.order
    }

    pub fn edges(&self) -> impl Iterator<Item = (Id, Id)> + '_ {
        self.incoming.iter().flat_map(|(&unit, prerequisites)| {
            prerequisites
                .iter()
                .map(move |&prerequisite| (prerequisite, unit))
        })
    }

    pub fn blocked(&self, unit: Id) -> bool {
        self.incoming.get(&unit).is_some_and(|prerequisites| {
            prerequisites
                .iter()
                .any(|id| self.statuses[id] != WorkUnitStatus::Completed)
        })
    }
}
