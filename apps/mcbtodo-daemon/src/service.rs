use std::{
    env, fs,
    io::{ErrorKind, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

const UNIT_NAME: &str = "mcbtodo-daemon.service";
const UNIT_TEMPLATE: &str = include_str!("../assets/mcbtodo-daemon.service");

pub(super) fn install(home: &Path) -> Result<()> {
    let executable = env::current_exe().context("locating the installed mcbtodo-daemon")?;
    let executable = executable
        .to_str()
        .context("use a UTF-8 Cargo installation path for the systemd service")?;
    ensure!(
        !executable.chars().any(char::is_control),
        "the daemon executable path contains a control character"
    );
    // ExecStart is parsed by systemd. Quote the executable as one argument and
    // escape specifiers so a literal percent in a Cargo root stays a percent.
    let executable = executable
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    let contents = UNIT_TEMPLATE.replace(
        "ExecStart=/usr/bin/mcbtodo-daemon",
        &format!("ExecStart=\"{executable}\""),
    );

    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let directory = config_home.join("systemd/user");
    fs::create_dir_all(&directory).with_context(|| format!("creating {}", directory.display()))?;
    let unit = directory.join(UNIT_NAME);
    let mut staged = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o644))
        .tempfile_in(directory)?;
    staged.write_all(contents.as_bytes())?;
    if let Err(error) = staged.persist_noclobber(&unit) {
        if error.error.kind() == ErrorKind::AlreadyExists {
            bail!(
                "{} already exists. Use systemctl --user edit --full {UNIT_NAME} to update it.",
                unit.display()
            );
        }
        return Err(error).with_context(|| format!("installing {}", unit.display()));
    }
    println!("Installed {}", unit.display());
    Ok(())
}
