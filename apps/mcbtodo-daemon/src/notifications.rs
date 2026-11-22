use std::{
    collections::HashSet,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::Duration,
};

use anyhow::Result;
use chrono::Utc;
use mcbtodo_ports::NotificationSink;
use mcbtodo_ports::notifications::NotificationStore;
use mcbtodo_protocol::workflow::RunState;
use uuid::Uuid;

pub mod reminders;

const POPUP_QUEUE_SIZE: usize = 8;
const POPUP_TIMEOUT: Duration = Duration::from_secs(2);

pub struct DesktopNotifications {
    last_sequence: i64,
    sender: SyncSender<Popup>,
    results: Receiver<ReminderDeliveryResult>,
    in_flight: HashSet<Uuid>,
    pending_ack: Option<ReminderDeliveryResult>,
}

struct Popup {
    title: &'static str,
    body: String,
    reminder_id: Option<Uuid>,
}

struct ReminderDeliveryResult {
    id: Uuid,
    result: std::result::Result<(), String>,
}

impl DesktopNotifications {
    pub fn start(inbox: &dyn NotificationStore, sink: impl NotificationSink) -> Result<Self> {
        // Keep existing and startup-recovered runs in the inbox without replaying popups.
        let last_sequence = inbox.workflow_watermark()?;
        let (sender, receiver) = mpsc::sync_channel::<Popup>(POPUP_QUEUE_SIZE);
        let (result_sender, results) = mpsc::channel();
        thread::Builder::new()
            .name("mcbtodo-notifications".into())
            .spawn(move || {
                for popup in receiver {
                    let result = sink
                        .deliver(popup.title, &popup.body, POPUP_TIMEOUT)
                        .map_err(|error| error.to_string());
                    if let Some(id) = popup.reminder_id
                        && result_sender
                            .send(ReminderDeliveryResult { id, result })
                            .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            last_sequence,
            sender,
            results,
            in_flight: HashSet::new(),
            pending_ack: None,
        })
    }

    pub fn dispatch(&mut self, inbox: &dyn NotificationStore) -> Result<()> {
        for (sequence, run_id, state) in
            inbox.recent_workflow_completions(self.last_sequence, POPUP_QUEUE_SIZE as u32)?
        {
            self.last_sequence = sequence;
            let title = match state {
                RunState::Succeeded => "Workflow succeeded",
                RunState::Failed => "Workflow failed",
                RunState::Cancelled => "Workflow cancelled",
                RunState::OutcomeUnknown => "Workflow outcome unknown",
                _ => continue,
            };
            // A busy or unavailable desktop never delays daemon work.
            let body = format!("Run {run_id}");
            let _ = self.sender.try_send(Popup {
                title,
                body,
                reminder_id: None,
            });
        }
        Ok(())
    }

    pub fn dispatch_reminders(&mut self, inbox: &dyn NotificationStore) -> Result<()> {
        while let Some(completion) = self
            .pending_ack
            .take()
            .or_else(|| self.results.try_recv().ok())
        {
            let result = completion
                .result
                .as_ref()
                .map(|_| ())
                .map_err(String::as_str);
            if let Err(error) = inbox.finish_reminder_delivery(completion.id, result, Utc::now()) {
                self.pending_ack = Some(completion);
                return Err(error);
            }
            self.in_flight.remove(&completion.id);
        }
        let available = POPUP_QUEUE_SIZE.saturating_sub(self.in_flight.len());
        let now = Utc::now();
        let lease_until = now + POPUP_TIMEOUT + Duration::from_secs(5);
        let claimed = inbox.claim_reminder_deliveries(now, available, lease_until)?;
        let mut claimed = claimed.into_iter();
        while let Some(reminder) = claimed.next() {
            let popup = Popup {
                title: "Reminder",
                body: reminder.title,
                reminder_id: Some(reminder.id),
            };
            match self.sender.try_send(popup) {
                Ok(()) => {
                    self.in_flight.insert(reminder.id);
                }
                Err(_) => {
                    inbox.release_reminder_delivery(reminder.id)?;
                    for remaining in claimed {
                        inbox.release_reminder_delivery(remaining.id)?;
                    }
                    break;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
