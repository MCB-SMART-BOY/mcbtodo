use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};

pub(crate) fn build(root: &Path, destination: &Path, inject_failure: bool) -> Result<PathBuf> {
    let source = root.join("examples/workflow").canonicalize()?;
    fs::create_dir_all(destination)?;
    let destination = destination.canonicalize()?;
    ensure!(
        !destination.starts_with(&source),
        "build the workflow outside its source directory"
    );
    fs::create_dir_all(destination.join("scripts"))?;
    let mut rustc = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    rustc.args([
        "--edition=2024",
        "-C",
        "opt-level=z",
        "-C",
        "panic=abort",
        "-C",
        "lto=fat",
        "-C",
        "target-feature=+crt-static",
        "-C",
        "strip=symbols",
    ]);
    if inject_failure {
        rustc.args(["--cfg", "inject_page_offset_failure"]);
    }
    let status = rustc
        .arg(source.join("page_table.rs"))
        .arg("-o")
        .arg(destination.join("page-table-check"))
        .status()
        .context("building the Rust fixture executable")?;
    ensure!(status.success(), "Rust fixture build exited with {status}");
    fs::copy(
        source.join("scripts/check.nu"),
        destination.join("scripts/check.nu"),
    )?;
    let graph = destination.join("graph.yaml");
    fs::copy(source.join("graph.yaml"), &graph)?;
    Ok(graph)
}
