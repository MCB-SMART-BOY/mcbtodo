use std::path::Path;

use anyhow::Context;
use mcbtodo_application::{VaultCommands, VaultQueries, deadline_edit_context};
use mcbtodo_domain::Id;
use mcbtodo_vault::WriteError;

use super::{Overlay, State, Workspace, deadline};

impl State {
    pub(super) fn open_deadline(&mut self, vault: &Workspace, id: Id) {
        let context = match vault {
            Workspace::Daemon { views, .. } => views
                .deadline_context(id)
                .map(|(revision, value)| (deadline::EditRevision::Daemon(revision), value)),
            Workspace::Local(vault) => deadline_edit_context(vault, id)
                .context("Selection does not support deadlines")
                .map(|context| {
                    (
                        deadline::EditRevision::Local(context.revision),
                        context.deadline,
                    )
                }),
        };
        match context {
            Ok((revision, value)) => {
                self.overlay =
                    Overlay::Deadline(deadline::Form::new(id, revision, value, vault.timezone()));
                self.message.clear();
            }
            Err(error) => self.message = format!("{error:#}"),
        }
    }

    pub(super) fn refresh_deadline(&mut self, root: &Path, vault: &Workspace) {
        let Overlay::Deadline(form) = &mut self.overlay else {
            return;
        };
        let timezone = vault.timezone();
        let context = match vault {
            Workspace::Daemon { views, .. } => views
                .deadline_context(form.id)
                .map(|(revision, value)| (deadline::EditRevision::Daemon(revision), value)),
            Workspace::Local(_) => VaultQueries::new(root, timezone)
                .deadline_edit_context(form.id)
                .and_then(|context| context.context("Selected document is no longer available"))
                .map(|context| {
                    (
                        deadline::EditRevision::Local(context.revision),
                        context.deadline,
                    )
                }),
        };
        match context {
            Ok((revision, value)) => {
                form.refresh(revision, value, timezone);
                self.message = "Revision refreshed; review the date before saving".into();
            }
            Err(error) => self.message = format!("Deadline refresh failed · {error:#}"),
        }
    }

    pub(super) fn set_deadline(
        &mut self,
        root: &Path,
        vault: &mut Workspace,
        request: deadline::Submission,
    ) {
        let deadline::Submission {
            id,
            deadline,
            revision,
        } = request;
        let revision = match (&*vault, revision) {
            (Workspace::Local(_), deadline::EditRevision::Local(revision)) => revision,
            (Workspace::Daemon { views, .. }, deadline::EditRevision::Daemon(revision)) => {
                use mcbtodo_client::daemon_client::WriteFailure;

                self.message = match views.set_deadline(id, revision, deadline) {
                    Ok(synced) => {
                        self.overlay = Overlay::None;
                        vault.invalidate_daemon_views();
                        if synced {
                            "Deadline saved".into()
                        } else {
                            "Deadline published · directory sync could not be confirmed".into()
                        }
                    }
                    Err(WriteFailure::StaleRevision) => {
                        if let Overlay::Deadline(form) = &mut self.overlay {
                            form.needs_refresh = true;
                        }
                        deadline::STALE.into()
                    }
                    Err(WriteFailure::Rejected(error)) => {
                        format!("Deadline update failed · {error}")
                    }
                    Err(WriteFailure::Unknown) => {
                        self.overlay = Overlay::None;
                        vault.invalidate_daemon_views();
                        format!(
                            "Deadline update outcome unknown · Check document {id} before retrying"
                        )
                    }
                };
                return;
            }
            _ => {
                self.message = "Deadline form belongs to a different Vault source".into();
                return;
            }
        };
        match VaultCommands::new(root, vault.timezone()).set_deadline(id, deadline, revision) {
            Ok(_) => {
                self.overlay = Overlay::None;
                self.reload(root, vault);
                if self.message == "Vault reloaded" {
                    self.message = "Deadline saved".into();
                }
            }
            Err(WriteError::StaleRevision { .. }) => {
                if let Overlay::Deadline(form) = &mut self.overlay {
                    form.needs_refresh = true;
                }
                self.message = deadline::STALE.into();
            }
            Err(WriteError::PublishedButNotSynced { source, .. }) => {
                self.overlay = Overlay::None;
                self.reload(root, vault);
                self.message = format!("Deadline published · directory sync failed: {source}");
            }
            Err(error) => self.message = format!("Deadline update failed · {error:#}"),
        }
    }
}
