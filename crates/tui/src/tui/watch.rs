use std::{
    io,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use mcbtodo_client::daemon_client;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::VaultWatchStatus;

pub(super) use mcbtodo_vault::VaultWatcher as Watcher;

const PROBE_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct DaemonWatcher {
    updates: Receiver<Option<VaultWatchStatus>>,
    current: Option<VaultWatchStatus>,
    _stop: Sender<()>,
}

impl DaemonWatcher {
    pub(super) fn new(paths: WorkspacePaths) -> io::Result<Self> {
        let (updates_tx, updates) = mpsc::sync_channel(1);
        let (stop, stop_rx) = mpsc::channel();
        // Keep health probes off the terminal thread so a stalled service cannot delay keys.
        thread::Builder::new()
            .name("mcbtodo-vault-watch".into())
            .spawn(move || {
                loop {
                    let status = daemon_client::vault_watch_status(&paths).ok();
                    let _ = updates_tx.try_send(status);
                    if stop_rx.recv_timeout(PROBE_INTERVAL) != Err(RecvTimeoutError::Timeout) {
                        break;
                    }
                }
            })?;
        Ok(Self {
            updates,
            current: None,
            _stop: stop,
        })
    }

    pub(super) fn changed(&mut self) -> bool {
        let Ok(status) = self.updates.try_recv() else {
            return false;
        };
        if self.current == status {
            return false;
        }
        self.current = status;
        true
    }

    pub(super) fn auto_refresh_unavailable(&self) -> bool {
        self.current.is_some_and(|status| !status.watching)
    }
}
