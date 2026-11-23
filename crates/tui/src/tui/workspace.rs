use std::{path::Path, rc::Rc};

use anyhow::Result;
use chrono_tz::Tz;
use mcbtodo_application::{OutlineItem, project_outline};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_vault::Vault;

use super::daemon_views::DaemonViews;

pub(super) enum Workspace {
    Local(Box<Vault>),
    Daemon { timezone: Tz, views: DaemonViews },
}

impl Workspace {
    pub(super) fn open(root: &Path, timezone: Tz, paths: &WorkspacePaths) -> Result<Self> {
        if mcbtodo_client::daemon_client::owns_vault(paths, root)? {
            Ok(Self::daemon(timezone, paths.clone()))
        } else {
            Self::load(root, timezone)
        }
    }

    pub(super) fn load(root: &Path, timezone: Tz) -> Result<Self> {
        Vault::load(root, timezone).map(|vault| Self::Local(Box::new(vault)))
    }

    pub(super) fn daemon(timezone: Tz, paths: WorkspacePaths) -> Self {
        Self::Daemon {
            timezone,
            views: DaemonViews::new(paths),
        }
    }

    pub(super) fn timezone(&self) -> Tz {
        match self {
            Self::Local(vault) => vault.timezone(),
            Self::Daemon { timezone, .. } => *timezone,
        }
    }

    pub(super) fn outline(&self) -> Result<Rc<[OutlineItem]>> {
        match self {
            Self::Local(vault) => Ok(project_outline(vault).into()),
            Self::Daemon { timezone, views } => views.outline(*timezone),
        }
    }

    pub(super) fn invalidate_daemon_views(&mut self) {
        if let Self::Daemon { views, .. } = self {
            views.invalidate();
        }
    }

    #[cfg(test)]
    pub(super) fn local(&self) -> &Vault {
        match self {
            Self::Local(vault) => vault,
            Self::Daemon { .. } => panic!("expected a local Vault"),
        }
    }
}
