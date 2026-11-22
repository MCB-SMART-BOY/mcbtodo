//! Read bounded Vault journal pages off the daemon loop, then deliver on it.

use std::{
    io,
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_ports::workflow::CompletionDeliveryStore;
use mcbtodo_ports::{CompletionEventStore, CompletionPage, CompletionReadError};
use mcbtodo_vault::VaultStore;

use super::WORKER_POLL;

const IDLE_POLL: Duration = Duration::from_secs(1);
pub(super) struct CompletionPoller {
    events: Arc<dyn CompletionEventStore>,
    next_poll: Instant,
    reader: Option<Receiver<Result<CompletionPage, CompletionReadError>>>,
    last_error: Option<String>,
}

impl CompletionPoller {
    pub fn new(paths: WorkspacePaths) -> Self {
        Self::with_store(DefaultVaultCompletionStore(paths))
    }

    pub fn with_store(events: impl CompletionEventStore) -> Self {
        Self {
            events: Arc::new(events),
            next_poll: Instant::now(),
            reader: None,
            last_error: None,
        }
    }

    pub fn watermark(&self) -> Result<i64, CompletionReadError> {
        self.events.watermark()
    }

    pub fn wake(&mut self) {
        self.next_poll = Instant::now();
    }

    pub fn wait_timeout(&self, store: &dyn CompletionDeliveryStore) -> Result<Option<Duration>> {
        if self.reader.is_some() {
            return Ok(Some(WORKER_POLL));
        }
        Ok(store
            .next_completion_cursor()?
            .map(|_| self.next_poll.saturating_duration_since(Instant::now())))
    }

    pub fn tick(&mut self, store: &dyn CompletionDeliveryStore) -> Result<()> {
        if let Some(reader) = &self.reader {
            let result = match reader.try_recv() {
                Ok(Err(CompletionReadError::Busy)) => {
                    self.reader = None;
                    self.next_poll = Instant::now() + IDLE_POLL;
                    return Ok(());
                }
                Ok(page) => page.map_err(anyhow::Error::from),
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("Vault reader exited")),
            };
            self.reader = None;
            match result.and_then(|page| {
                store
                    .deliver_completion_page(&page)
                    .context("delivering WorkUnit completion events")?;
                Ok(page.is_empty())
            }) {
                Ok(empty) => {
                    self.last_error = None;
                    self.next_poll =
                        Instant::now() + if empty { IDLE_POLL } else { Duration::ZERO };
                }
                Err(error) => self.retry_later(error),
            }
        }
        if self.next_poll <= Instant::now()
            && let Some(after) = store.next_completion_cursor()?
        {
            let events = Arc::clone(&self.events);
            let (sender, receiver) = mpsc::channel();
            match thread::Builder::new()
                .name("work-unit-completions".into())
                .spawn(move || {
                    let _ = sender.send(events.page(after));
                }) {
                Ok(_) => self.reader = Some(receiver),
                Err(error) => self.retry_later(error.into()),
            }
        }
        Ok(())
    }

    fn retry_later(&mut self, error: anyhow::Error) {
        let message = format!("{error:#}");
        if self.last_error.as_ref() != Some(&message) {
            eprintln!("mcbtodo-daemon: WorkUnit completion trigger will retry: {message}");
            self.last_error = Some(message);
        }
        self.next_poll = Instant::now() + IDLE_POLL;
    }
}

struct DefaultVaultCompletionStore(WorkspacePaths);

impl DefaultVaultCompletionStore {
    fn vault(&self) -> Result<VaultStore> {
        Ok(VaultStore::new(
            self.0.vault_root(),
            self.0.load_config()?.timezone,
        ))
    }
}

impl CompletionEventStore for DefaultVaultCompletionStore {
    fn watermark(&self) -> Result<i64, CompletionReadError> {
        read_completion("reading default Vault completion cutoff", || {
            Ok(self.vault()?.work_unit_completion_watermark()?)
        })
    }

    fn page(&self, after: i64) -> Result<CompletionPage, CompletionReadError> {
        read_completion("reading default Vault completion journal", || {
            Ok(self.vault()?.work_unit_completion_page(after)?)
        })
    }
}

fn read_completion<T>(
    operation: &'static str,
    read: impl FnOnce() -> Result<T>,
) -> Result<T, CompletionReadError> {
    read().context(operation).map_err(|error| {
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock)
        }) {
            CompletionReadError::Busy
        } else {
            CompletionReadError::Storage(format!("{error:#}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use chrono::Utc;
    use mcbtodo_domain::{WorkUnitCompleted, WorkUnitStatus};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn delivery_failure_retries_from_the_durable_cursor() -> Result<()> {
        struct Journal(WorkUnitCompleted);

        impl CompletionEventStore for Journal {
            fn watermark(&self) -> Result<i64, CompletionReadError> {
                Ok(41)
            }

            fn page(&self, after: i64) -> Result<CompletionPage, CompletionReadError> {
                match after {
                    41 => Ok(vec![(42, self.0.clone())]),
                    42 => Ok(Vec::new()),
                    _ => Err(CompletionReadError::Storage(format!(
                        "unexpected cursor {after}"
                    ))),
                }
            }
        }

        struct Deliveries {
            cursor: Cell<i64>,
            blocked: Cell<bool>,
            events: RefCell<Vec<Uuid>>,
        }

        impl CompletionDeliveryStore for Deliveries {
            fn next_completion_cursor(&self) -> Result<Option<i64>> {
                Ok(Some(self.cursor.get()))
            }

            fn deliver_completion_page(&self, page: &[(i64, WorkUnitCompleted)]) -> Result<()> {
                anyhow::ensure!(!self.blocked.get(), "delivery commit failed");
                for (cursor, event) in page {
                    self.events.borrow_mut().push(event.event_id);
                    self.cursor.set(*cursor);
                }
                Ok(())
            }
        }

        let event = WorkUnitCompleted {
            event_id: Uuid::new_v4(),
            work_unit_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            parent_task_id: Uuid::new_v4(),
            occurred_at: Utc::now(),
            previous_status: WorkUnitStatus::Todo,
            evidence: Vec::new(),
        };
        let expected = event.event_id;
        let mut poller = CompletionPoller::with_store(Journal(event));
        let store = Deliveries {
            cursor: Cell::new(41),
            blocked: Cell::new(true),
            events: RefCell::new(Vec::new()),
        };
        let read_and_deliver = |poller: &mut CompletionPoller| -> Result<()> {
            poller.tick(&store)?;
            let reader = poller.reader.take().context("journal read did not start")?;
            let page = reader.recv_timeout(Duration::from_secs(5))??;
            let (sender, receiver) = mpsc::channel();
            sender.send(Ok(page))?;
            poller.reader = Some(receiver);
            poller.tick(&store)
        };

        read_and_deliver(&mut poller)?;
        assert_eq!(store.cursor.get(), 41);
        assert!(store.events.borrow().is_empty());
        assert!(
            poller
                .wait_timeout(&store)?
                .is_some_and(|delay| delay > Duration::ZERO)
        );

        store.blocked.set(false);
        poller.wake();
        read_and_deliver(&mut poller)?;
        assert_eq!(store.cursor.get(), 42);
        assert_eq!(*store.events.borrow(), [expected]);
        Ok(())
    }
}
