use std::path::Path;

use anyhow::{Result, bail};
use mcbtodo_platform::WorkspacePaths;

pub(crate) fn backup(_paths: &WorkspacePaths, _destination: &Path) -> Result<()> {
    bail!("workspace backup is unavailable on this platform")
}

pub(crate) fn restore(_source: &Path, _destination_home: &Path) -> Result<()> {
    bail!("workspace restore is unavailable on this platform")
}
