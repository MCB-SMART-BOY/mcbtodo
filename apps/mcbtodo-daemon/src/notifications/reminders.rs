use std::{
    collections::VecDeque,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use mcbtodo_application::ReminderSummary;
use mcbtodo_domain::recurrence::OccurrenceWindow;
use mcbtodo_ports::notifications::{DueReminder, NotificationStore, ReminderNotification};

const SCAN_INTERVAL: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
// Only occurrences strictly newer than this lookback boundary are materialized.
const OCCURRENCE_LOOKBACK: TimeDelta = TimeDelta::hours(24);
const PUBLISH_BATCH: usize = 64;
const TITLE_CHARS: usize = 512;

pub struct ReminderPoller {
    requests: Sender<ReminderScan>,
    results: Receiver<ReminderScanResult>,
    pending: VecDeque<DueReminder>,
    scanning: bool,
    snapshot_generation: u64,
    wake: Option<Instant>,
    last_scan_error: Option<String>,
}

struct ReminderScan {
    generation: u64,
    now: DateTime<Utc>,
    reminders: Vec<ReminderSummary>,
}

struct ReminderScanResult {
    generation: u64,
    result: Result<VecDeque<DueReminder>>,
}

impl ReminderPoller {
    pub fn start() -> Result<Self> {
        let (requests, requested) = mpsc::channel::<ReminderScan>();
        let (result, results) = mpsc::channel::<ReminderScanResult>();
        thread::Builder::new()
            .name("mcbtodo-reminders".into())
            .spawn(move || {
                while let Ok(scan) = requested.recv() {
                    let generation = scan.generation;
                    if result
                        .send(ReminderScanResult {
                            generation,
                            result: find_due(&scan.reminders, scan.now),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .context("starting reminder reader")?;
        Ok(Self {
            requests,
            results,
            pending: VecDeque::new(),
            scanning: false,
            snapshot_generation: 0,
            wake: Some(Instant::now()),
            last_scan_error: None,
        })
    }

    pub fn wait_timeout(&self) -> Option<Duration> {
        self.wake
            .map(|wake| wake.saturating_duration_since(Instant::now()))
    }

    pub fn tick(
        &mut self,
        inbox: &dyn NotificationStore,
        generation: u64,
        reminders: &[ReminderSummary],
    ) -> Result<Vec<ReminderNotification>> {
        let now = Instant::now();
        if generation != self.snapshot_generation {
            self.snapshot_generation = generation;
            self.pending.clear();
            self.wake = Some(now);
        }
        if !self.wake.is_some_and(|wake| wake <= now) {
            return Ok(Vec::new());
        }
        self.wake = Some(now + SCAN_INTERVAL);
        let mut completed_scan = false;
        let mut stale_scan = false;
        if self.scanning {
            match self.results.try_recv() {
                Ok(scan) => {
                    self.scanning = false;
                    completed_scan = true;
                    if scan.generation == self.snapshot_generation {
                        match scan.result {
                            Ok(due) => {
                                self.pending = due;
                                self.last_scan_error = None;
                            }
                            Err(error) => {
                                let message = format!("{error:#}");
                                if self.last_scan_error.as_ref() != Some(&message) {
                                    eprintln!("mcbtodo-daemon: reading reminders: {message}");
                                    self.last_scan_error = Some(message);
                                }
                            }
                        }
                    } else {
                        stale_scan = true;
                    }
                }
                Err(TryRecvError::Empty) => {
                    self.wake = Some(now + POLL_INTERVAL);
                    return Ok(Vec::new());
                }
                Err(TryRecvError::Disconnected) => {
                    self.wake = None;
                    anyhow::bail!("reminder reader exited");
                }
            }
        }
        if !self.scanning && self.pending.is_empty() && (!completed_scan || stale_scan) {
            if self
                .requests
                .send(ReminderScan {
                    generation: self.snapshot_generation,
                    now: Utc::now(),
                    reminders: reminders.to_vec(),
                })
                .is_err()
            {
                self.wake = None;
                anyhow::bail!("reminder reader exited");
            }
            self.scanning = true;
            self.wake = Some(now + POLL_INTERVAL);
        }
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let count = self.pending.len().min(PUBLISH_BATCH);
        let published =
            inbox.publish_reminders(&self.pending.make_contiguous()[..count], Utc::now())?;
        self.pending.drain(..count);
        if !self.pending.is_empty() {
            self.wake = Some(now + POLL_INTERVAL);
        }
        Ok(published)
    }
}

fn find_due(reminders: &[ReminderSummary], now: DateTime<Utc>) -> Result<VecDeque<DueReminder>> {
    let window = OccurrenceWindow::new(now - OCCURRENCE_LOOKBACK, now)?;
    let mut due = Vec::new();
    for reminder in reminders {
        due.extend(
            reminder
                .schedule
                .occurrences(window)
                .into_iter()
                .map(|scheduled_at| DueReminder {
                    reminder_id: reminder.id,
                    target_id: reminder.target_id,
                    title: reminder.title.chars().take(TITLE_CHARS).collect(),
                    scheduled_at,
                }),
        );
    }
    due.sort_unstable_by_key(|item| (item.scheduled_at, item.reminder_id));
    Ok(due.into())
}
