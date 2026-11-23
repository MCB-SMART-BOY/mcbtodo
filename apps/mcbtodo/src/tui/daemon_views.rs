use std::{cell::RefCell, rc::Rc};

use anyhow::{Error, Result, bail};
use chrono_tz::Tz;
use mcbtodo_domain::{CalendarItem, OutlineItem, time::DateSpan};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{Request, Response};

/// Rendering and input use the same responses until the query or Vault changes.
pub(super) struct DaemonViews {
    paths: WorkspacePaths,
    calendar: QueryCache<(DateSpan, Tz), CalendarItem>,
    outline: QueryCache<Tz, OutlineItem>,
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
        }
    }

    pub(super) fn calendar(&self, dates: DateSpan, timezone: Tz) -> Result<Rc<[CalendarItem]>> {
        self.calendar.rows((dates, timezone), || {
            crate::daemon_calendar(&self.paths, dates, timezone)
        })
    }

    pub(super) fn outline(&self, timezone: Tz) -> Result<Rc<[OutlineItem]>> {
        self.outline.rows(timezone, || {
            match crate::daemon_request(
                &self.paths,
                Request::Outline {
                    timezone: timezone.name().into(),
                },
            )? {
                Response::Outline { items, more: false } => Ok(items),
                _ => bail!("unexpected response to outline request"),
            }
        })
    }

    pub(super) fn invalidate(&mut self) {
        self.calendar.0.get_mut().take();
        self.outline.0.get_mut().take();
    }
}
