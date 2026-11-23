use std::{
    cell::RefCell,
    collections::BTreeSet,
    fmt,
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

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
    outline_loader: Option<RefCell<OutlineLoader>>,
    inbox: QueryCache<(), InboxSummary>,
    graph: QueryCache<Tz, super::graph::Node>,
}

type CachedRows<T> = Result<Rc<[T]>, String>;

#[derive(Debug)]
pub(super) struct OutlineLoading;

impl fmt::Display for OutlineLoading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Loading Outline…")
    }
}

impl std::error::Error for OutlineLoading {}

#[derive(Default)]
struct OutlineLoader {
    pending: Option<OutlineRequest>,
}

struct OutlineRequest {
    timezone: Tz,
    current: bool,
    reply: Receiver<Result<Vec<OutlineItem>, String>>,
}

pub(super) enum OutlineUpdate {
    RowsChanged,
    Redraw,
}

struct QueryCache<K, T>(RefCell<Option<(K, CachedRows<T>)>>);

impl<K: PartialEq, T> QueryCache<K, T> {
    fn cached(&self, key: &K) -> Option<Result<Rc<[T]>>> {
        self.0
            .borrow()
            .as_ref()
            .filter(|(previous, _)| previous == key)
            .map(|(_, rows)| rows.clone().map_err(Error::msg))
    }

    fn install(&self, key: K, rows: Result<Vec<T>, String>) -> Result<Rc<[T]>> {
        let rows = rows.map(Rc::from);
        let result = rows.clone().map_err(Error::msg);
        *self.0.borrow_mut() = Some((key, rows));
        result
    }

    fn rows(&self, key: K, query: impl FnOnce() -> Result<Vec<T>>) -> Result<Rc<[T]>> {
        self.cached(&key)
            .unwrap_or_else(|| self.install(key, query().map_err(|error| format!("{error:#}"))))
    }
}

impl DaemonViews {
    pub(super) fn new(paths: WorkspacePaths) -> Self {
        Self {
            paths,
            calendar: QueryCache(RefCell::new(None)),
            outline: QueryCache(RefCell::new(None)),
            outline_loader: None,
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
        let Some(loader) = &self.outline_loader else {
            return self.outline_for_action(timezone);
        };
        if let Some(rows) = self.outline.cached(&timezone) {
            return rows;
        }
        let mut loader = loader.borrow_mut();
        if loader.pending.is_none() {
            let (send, reply) = mpsc::channel();
            let paths = self.paths.clone();
            let worker = thread::Builder::new()
                .name("mcbtodo-outline".into())
                .spawn(move || {
                    let rows = mcbtodo_client::daemon_client::outline(&paths, timezone)
                        .map_err(|error| format!("{error:#}"));
                    let _ = send.send(rows);
                });
            if let Err(error) = worker {
                return self.outline.install(timezone, Err(error.to_string()));
            }
            loader.pending = Some(OutlineRequest {
                timezone,
                current: true,
                reply,
            });
        }
        Err(OutlineLoading.into())
    }

    pub(super) fn enable_outline_loader(&mut self) {
        self.outline_loader = Some(RefCell::default());
    }

    /// Drains the single outstanding read. Invalidated replies trigger a redraw
    /// so an active consumer can submit the replacement query.
    pub(super) fn poll_outline(&mut self, timezone: Tz) -> Option<OutlineUpdate> {
        let loader = self.outline_loader.as_mut()?.get_mut();
        let pending = loader.pending.as_ref()?;
        let rows = match pending.reply.try_recv() {
            Ok(rows) => rows,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Outline reader disconnected".into()),
        };
        let update = if pending.current
            && pending.timezone == timezone
            && self.outline.install(timezone, rows).is_ok()
        {
            OutlineUpdate::RowsChanged
        } else {
            OutlineUpdate::Redraw
        };
        loader.pending = None;
        Some(update)
    }

    pub(super) fn outline_for_action(&self, timezone: Tz) -> Result<Rc<[OutlineItem]>> {
        self.outline.rows(timezone, || {
            // An explicit operation can need Outline outside the browsing views.
            // Its fresh synchronous read supersedes the outstanding display read.
            if let Some(loader) = &self.outline_loader
                && let Some(pending) = &mut loader.borrow_mut().pending
            {
                pending.current = false;
            }
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
        if let Some(loader) = &mut self.outline_loader
            && let Some(pending) = &mut loader.get_mut().pending
        {
            pending.current = false;
        }
        self.inbox.0.get_mut().take();
        self.graph.0.get_mut().take();
    }
}
