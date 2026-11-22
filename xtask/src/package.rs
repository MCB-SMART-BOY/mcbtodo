#[cfg(target_os = "linux")]
use std::{env::consts::ARCH, os::unix::fs::PermissionsExt, path::PathBuf};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub fn verify_crate_packages(root: &Path) -> Result<()> {
    let metadata = cargo_output(
        root,
        &["metadata", "--format-version", "1", "--no-deps", "--locked"],
    )?;
    let metadata: Value =
        serde_json::from_slice(&metadata.stdout).context("parsing cargo metadata")?;
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata has no packages array")?;
    let workspace_license = fs::read(root.join("LICENSE")).context("reading workspace LICENSE")?;
    let mut verified = 0usize;

    for package in packages {
        let name = package["name"]
            .as_str()
            .context("workspace package has no name")?;
        if package["publish"].as_array().is_some_and(Vec::is_empty) {
            continue;
        }

        ensure!(
            name.starts_with("mcbtodo-") || name == "mcbtodo",
            "publishable package {name} does not follow the mcbtodo naming convention"
        );
        ensure!(
            package["license"].as_str() == Some("Apache-2.0"),
            "publishable package {name} must declare Apache-2.0"
        );
        let manifest_path = package["manifest_path"]
            .as_str()
            .context("workspace package has no manifest path")?;
        let package_license = Path::new(manifest_path)
            .parent()
            .context("package manifest has no parent directory")?
            .join("LICENSE");
        ensure!(
            fs::read(&package_license).with_context(|| format!("reading license for {name}"))?
                == workspace_license,
            "publishable package {name} LICENSE differs from the workspace license"
        );
        let output = cargo_output(
            root,
            &[
                "package",
                "--list",
                "--package",
                name,
                "--locked",
                "--no-verify",
                "--allow-dirty",
            ],
        )
        .with_context(|| format!("listing packaged files for {name}"))?;
        let files = String::from_utf8(output.stdout)
            .with_context(|| format!("Cargo emitted non-UTF-8 package listing for {name}"))?;
        ensure!(
            files.lines().any(|line| line == "LICENSE"),
            "publishable package {name} does not include LICENSE in its archive"
        );
        println!("verified {name}: Apache-2.0 metadata and LICENSE archive entry");
        verified += 1;
    }

    ensure!(verified > 0, "workspace contains no publishable packages");
    Ok(())
}

fn cargo_output(root: &Path, args: &[&str]) -> Result<Output> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root)
        .args(args)
        .output()
        .with_context(|| format!("starting cargo {}", args[0]))?;
    ensure!(
        output.status.success(),
        "cargo {} failed: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

#[cfg(target_os = "linux")]
pub fn build(root: &Path, bin_dir: &Path, out_dir: &Path) -> Result<PathBuf> {
    ensure!(
        matches!(ARCH, "aarch64" | "x86_64"),
        "packaging requires a native Linux aarch64 or x86_64 build"
    );
    let staging = tempfile::tempdir().context("creating package staging directory")?;
    for dir in [
        "usr",
        "usr/bin",
        "usr/lib",
        "usr/lib/systemd",
        "usr/lib/systemd/user",
        "usr/share",
        "usr/share/licenses",
        "usr/share/licenses/mcbtodo",
    ] {
        let path = staging.path().join(dir);
        fs::create_dir(&path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    for (source, destination, mode) in [
        (bin_dir.join("mcbtodo"), "usr/bin/mcbtodo", 0o755),
        (
            bin_dir.join("mcbtodo-daemon"),
            "usr/bin/mcbtodo-daemon",
            0o755,
        ),
        (
            bin_dir.join("mcbtodo-worker"),
            "usr/bin/mcbtodo-worker",
            0o755,
        ),
        (
            root.join("deploy/systemd/mcbtodo-daemon.service"),
            "usr/lib/systemd/user/mcbtodo-daemon.service",
            0o644,
        ),
        (
            root.join("LICENSE"),
            "usr/share/licenses/mcbtodo/LICENSE",
            0o644,
        ),
    ] {
        let destination = staging.path().join(destination);
        fs::copy(&source, &destination).with_context(|| format!("staging {}", source.display()))?;
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    }

    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let archive = out_dir.join(format!("mcbtodo-linux-{ARCH}.tar.gz"));
    let pending = tempfile::NamedTempFile::new_in(out_dir)?;
    let status = Command::new("tar")
        .arg("-czf")
        .arg(pending.path())
        .arg("-C")
        .arg(staging.path())
        .arg("usr")
        .status()
        .context("starting tar")?;
    ensure!(status.success(), "tar exited with {status}");
    pending
        .persist(&archive)
        .with_context(|| format!("publishing {}", archive.display()))?;
    Ok(archive)
}
