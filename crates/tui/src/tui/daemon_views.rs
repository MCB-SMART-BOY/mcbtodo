use std::{cell::RefCell, collections::BTreeSet, path::PathBuf, rc::Rc};

use anyhow::{Error, Result, bail};
use chrono_tz::Tz;
use mcbtodo_domain::{
    CalendarItem, DependencyOperation, Id, InboxSummary, OutlineItem, WorkUnitDependencyPreview,
    time::{DateSpan, Deadline},
};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{Request, Response};

/// Rendering and input use the same responses until the query or Vault changes.
pub(super) struct DaemonViews {
    paths: WorkspacePaths,
    calendar: QueryCache<(DateSpan, Tz), CalendarItem>,
    outline: QueryCache<Tz, OutlineItem>,
    inbox: QueryCache<(), InboxSummary>,
    graph: QueryCache<Tz, super::graph::Node>,
}

type CachedRows<T> = Result<Rc<[T]>, String>;

struct QueryCache<K, T>(RefCell<Option<(K, CachedRows<T>)>>);

impl<K: PartialEq, T> QueryCache<K, T> {
    fn rows(&self, key: K, query: impl FnOnce() -> Result<Vec<T>>) -> Result<Rc<[T]>> {
        let mut cached = self.0.borrow_mut();
        if let Some((previous, rows)) = cached.as_ref()
            && *previous == key
        {
            return rows.clone().map_err(Error::msg);
        }
        let rows = query().map(Rc::from).map_err(|error| format!("{error:#}"));
        let result = rows.clone().map_err(Error::msg);
        *cached = Some((key, rows));
        result
    }
}

impl DaemonViews {
    pub(super) fn new(paths: WorkspacePaths) -> Self {
        Self {
            paths,
            calendar: QueryCache(RefCell::new(None)),
            outline: QueryCache(RefCell::new(None)),
            inbox: QueryCache(RefCell::new(None)),
            graph: QueryCache(RefCell::new(None)),
        }
    }

    pub(super) fn calendar(&self, dates: DateSpan, timezone: Tz) -> Result<Rc<[CalendarItem]>> {
        self.calendar.rows((dates, timezone), || {
            mcbtodo_client::daemon_client::calendar(&self.paths, dates, timezone)
        })
    }

    pub(super) fn outline(&self, timezone: Tz) -> Result<Rc<[OutlineItem]>> {
        self.outline.rows(timezone, || {
            mcbtodo_client::daemon_client::outline(&self.paths, timezone)
        })
    }

    pub(super) fn deadline_context(&self, id: Id) -> Result<(String, Option<Deadline>)> {
        match mcbtodo_client::daemon_client::exchange(&self.paths, Request::DeadlineContext { id })?
        {
            Response::DeadlineContext { revision, deadline } => Ok((revision, deadline)),
            _ => bail!("unexpected response to deadline context request"),
        }
    }

    pub(super) fn document_source(&self, id: Id) -> Result<PathBuf> {
        mcbtodo_client::daemon_client::document_source(&self.paths, id)
    }

    pub(super) fn set_deadline(
        &self,
        id: Id,
        revision: String,
        deadline: Option<Deadline>,
    ) -> Result<bool, mcbtodo_client::daemon_client::WriteFailure> {
        mcbtodo_client::daemon_client::submit_write(
            &self.paths,
            Request::SetDeadline {
                id,
                revision,
                deadline,
            },
            |response| match response {
                Response::DeadlineSet { synced } => Some(synced),
                _ => None,
            },
        )
    }

    pub(super) fn inbox(&self) -> Result<Rc<[InboxSummary]>> {
        self.inbox.rows((), || {
            match mcbtodo_client::daemon_client::exchange(&self.paths, Request::Inbox)? {
                Response::Inbox { items, more: false } => Ok(items),
                _ => bail!("unexpected response to Inbox request"),
            }
        })
    }

    pub(super) fn graph(&self, timezone: Tz) -> Result<Rc<[super::graph::Node]>> {
        self.graph
            .rows(timezone, || match mcbtodo_client::daemon_client::exchange(
                &self.paths,
                Request::WorkUnitGraph {
                    timezone: timezone.name().into(),
                },
            )? {
                Response::WorkUnitGraph { items, more: false } => Ok(super::graph::nodes(items)),
                _ => bail!("unexpected response to WorkUnit graph request"),
            })
    }

    pub(super) fn preview_dependency_change(
        &self,
        target: Id,
        sources: &BTreeSet<Id>,
        operation: DependencyOperation,
        timezone: Tz,
    ) -> Result<WorkUnitDependencyPreview> {
        match mcbtodo_client::daemon_client::exchange(
            &self.paths,
            Request::PreviewWorkUnitDependencyChange {
                target,
                sources: sources.clone(),
                operation,
                timezone: timezone.name().into(),
            },
        )? {
            Response::WorkUnitDependencyPreview(edit) => Ok(edit),
            _ => bail!("unexpected response to dependency preview"),
        }
    }

    pub(super) fn set_dependencies(
        &self,
        edit: super::graph::Submission,
    ) -> Result<bool, mcbtodo_client::daemon_client::WriteFailure> {
        mcbtodo_client::daemon_client::submit_write(
            &self.paths,
            Request::SetWorkUnitDependencies {
                target: edit.target,
                prerequisites: edit.prerequisites,
                revision: edit.revision,
            },
            |response| match response {
                Response::WorkUnitDependenciesSet { synced } => Some(synced),
                _ => None,
            },
        )
    }

    pub(super) fn invalidate(&mut self) {
        self.calendar.0.get_mut().take();
        self.outline.0.get_mut().take();
        self.inbox.0.get_mut().take();
        self.graph.0.get_mut().take();
    }
}
