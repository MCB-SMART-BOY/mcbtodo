//! Read bounded Vault journal pages off the daemon loop, then deliver on it.

use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use mcbtodo_domain::WorkUnitCompleted;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_vault::VaultStore;

use super::{RunStore, WORKER_POLL};

const IDLE_POLL: Duration = Duration::from_secs(1);
type Page = Vec<(i64, WorkUnitCompleted)>;

pub(super) struct CompletionPoller {
    paths: WorkspacePaths,
    next_poll: Instant,
    reader: Option<Receiver<Result<Page>>>,
    last_error: Option<String>,
}

impl CompletionPoller {
    pub fn new(paths: WorkspacePaths) -> Self {
        Self {
            paths,
            next_poll: Instant::now(),
            reader: None,
            last_error: None,
        }
    }

    pub fn watermark(&self) -> Result<i64> {
        vault(&self.paths)?
            .work_unit_completion_watermark()
            .context("reading default Vault completion cutoff")
    }

    pub fn wake(&mut self) {
        self.next_poll = Instant::now();
    }

    pub fn wait_timeout(&self, store: &RunStore) -> Result<Option<Duration>> {
        if self.reader.is_some() {
            return Ok(Some(WORKER_POLL));
        }
        Ok(store
            .next_completion_cursor()?
            .map(|_| self.next_poll.saturating_duration_since(Instant::now())))
    }

    pub fn tick(&mut self, store: &RunStore) -> Result<()> {
        if let Some(reader) = &self.reader {
            let result = match reader.try_recv() {
                Ok(page) => page,
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
            let paths = self.paths.clone();
            let (sender, receiver) = mpsc::channel();
            match thread::Builder::new()
                .name("work-unit-completions".into())
                .spawn(move || {
                    let result = vault(&paths).and_then(|vault| {
                        vault
                            .work_unit_completion_page(after)
                            .context("reading default Vault completion journal")
                    });
                    let _ = sender.send(result);
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

fn vault(paths: &WorkspacePaths) -> Result<VaultStore> {
    Ok(VaultStore::new(
        paths.vault_root(),
        paths.load_config()?.timezone,
    ))
}
