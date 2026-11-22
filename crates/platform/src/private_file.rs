use std::{fs::File, os::unix::fs::MetadataExt, path::Path};

use anyhow::{Context, Result, ensure};
use rustix::fs::{Mode, OFlags};
use rustix::process::geteuid;

/// Open or create a single-link, user-owned private regular file without following
/// a final symlink. The caller owns validation of the parent directory.
pub fn open_private_file(path: &Path) -> Result<File> {
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .with_context(|| format!("opening {}", path.display()))?,
    );
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == geteuid().as_raw()
            && meta.mode() & 0o077 == 0
            && meta.nlink() == 1,
        "{} must be a private regular file owned by the current user",
        path.display()
    );
    Ok(file)
}
