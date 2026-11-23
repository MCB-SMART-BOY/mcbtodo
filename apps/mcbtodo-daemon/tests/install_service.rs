#![cfg(target_os = "linux")]

use std::{fs, path::Path, process::Command};

use anyhow::Result;

#[test]
fn installed_daemon_writes_user_unit_without_a_checkout() -> Result<()> {
    let installation = tempfile::tempdir()?;
    let bin = installation.path().join("cargo bin %n");
    fs::create_dir(&bin)?;
    let executable = bin.join("mcbtodo-daemon");
    fs::copy(env!("CARGO_BIN_EXE_mcbtodo-daemon"), &executable)?;

    let home = installation.path().join("user-home");
    let config = installation.path().join("user-config");
    fs::create_dir(&home)?;
    let install = |config_home: Option<&Path>| {
        let mut command = Command::new(&executable);
        command
            .arg("install-service")
            .current_dir(&home)
            .env("HOME", &home)
            .env_remove("XDG_CONFIG_HOME")
            .env("PATH", "");
        if let Some(path) = config_home {
            command.env("XDG_CONFIG_HOME", path);
        }
        command.output()
    };

    let output = install(Some(&config))?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let unit = config.join("systemd/user/mcbtodo-daemon.service");
    let contents = fs::read_to_string(&unit)?;
    assert!(contents.contains(&format!(
        "ExecStart=\"{}/cargo bin %%n/mcbtodo-daemon\"\n",
        installation.path().display()
    )));
    assert!(contents.contains("WantedBy=default.target"));
    assert!(!home.join(".mcbtodo").exists());

    fs::write(&unit, "[Service]\nExecStart=/custom/mcbtodo-daemon\n")?;
    let output = install(Some(&config))?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
    assert_eq!(
        fs::read_to_string(&unit)?,
        "[Service]\nExecStart=/custom/mcbtodo-daemon\n"
    );

    let output = install(None)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/systemd/user/mcbtodo-daemon.service"))?,
        contents
    );
    Ok(())
}
