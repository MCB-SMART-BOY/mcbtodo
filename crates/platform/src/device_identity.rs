use super::{WorkspacePaths, ensure_private_dir};
use anyhow::{Context, Result, ensure};
use std::{fs, io, io::Write, path::PathBuf};
use tempfile::NamedTempFile;
use uuid::{Uuid, Variant, Version};

impl WorkspacePaths {
    pub fn device_identity_path(&self) -> PathBuf {
        self.state_dir().join("device-id")
    }

    /// Returns the private installation identity shared by this home's Vault writers.
    /// An absent identity is published atomically and is excluded from workspace backups.
    pub fn load_or_create_device_identity(&self) -> Result<Uuid> {
        let data = self.data_dir();
        let state = self.state_dir();
        ensure_private_dir(&data)?;
        ensure_private_dir(&state)?;
        let path = self.device_identity_path();
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut temporary = NamedTempFile::new_in(&state)?;
                writeln!(temporary, "{}", Uuid::new_v4())?;
                temporary.as_file().sync_all()?;
                #[cfg(unix)]
                let result = rustix::fs::renameat_with(
                    rustix::fs::CWD,
                    temporary.path(),
                    rustix::fs::CWD,
                    &path,
                    rustix::fs::RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from);
                #[cfg(not(unix))]
                let result = temporary
                    .persist_noclobber(&path)
                    .map(|_| ())
                    .map_err(|e| e.error);
                if let Err(error) = result
                    && error.kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(error).with_context(|| format!("publishing {}", path.display()));
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", path.display()));
            }
        }
        let text = self.read_state_file(&path)?;
        let identity = Uuid::parse_str(text.trim())
            .with_context(|| format!("parsing the installation identity at {}", path.display()))?;
        ensure!(
            identity.get_variant() == Variant::RFC4122
                && identity.get_version() == Some(Version::Random),
            "{} must contain a random UUID",
            path.display()
        );
        // A caller that finds another process's new identity also makes its
        // directory entries durable before it can publish an attributed event.
        #[cfg(unix)]
        for directory in [state.as_path(), data.as_path(), self.home_dir()] {
            fs::File::open(directory)
                .and_then(|file| file.sync_all())
                .with_context(|| format!("syncing {}", directory.display()))?;
        }
        Ok(identity)
    }
}
