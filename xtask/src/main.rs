mod agents;
mod example;
mod package;
mod smoke;

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Development tasks for the mcbtodo workspace")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Check formatting, lint all targets, then run the workspace tests.
    Check,
    /// Exercise built binaries against an isolated workspace on this Linux host.
    Smoke {
        /// Directory relative to the workspace root, or an absolute path.
        #[arg(long, default_value = "target/release")]
        bin_dir: PathBuf,
        /// Require native Bubblewrap and Nushell and exercise the script worker.
        #[arg(long)]
        sandbox: bool,
    },
    /// Verify timer recovery through a transient systemd user service.
    SmokeSystemd {
        /// Directory relative to the workspace root, or an absolute path.
        #[arg(long, default_value = "target/release")]
        bin_dir: PathBuf,
    },
    /// Package the native Linux release binaries and systemd user unit.
    Package {
        /// Directory containing the native Linux release binaries.
        #[arg(long, default_value = "target/release")]
        bin_dir: PathBuf,
        /// Destination directory relative to the workspace root, or an absolute path.
        #[arg(long, default_value = "target/dist")]
        out_dir: PathBuf,
    },
    /// Verify Apache-2.0 metadata and license inclusion for publishable crates.
    VerifyCratePackages,
    /// Build the native Rust page-table workflow example.
    WorkflowExample {
        /// Destination directory relative to the workspace root, or an absolute path.
        #[arg(long, default_value = "target/examples/workflow")]
        out_dir: PathBuf,
        /// Build a version with a deliberately failing page-offset check.
        #[arg(long)]
        inject_failure: bool,
    },
    /// Prepare or launch a project agent harness with a selected workflow.
    Agent {
        #[command(subcommand)]
        command: agents::Task,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = env::current_dir()?;
    let root = cwd
        .ancestors()
        .find(|path| {
            path.join("Cargo.toml").is_file()
                && path.join("xtask/Cargo.toml").is_file()
                && path.join(".agents/registry.toml").is_file()
        })
        .context("run cargo xtask from the mcbtodo workspace or one of its subdirectories")?;
    match cli.command {
        Task::Check => check(root),
        Task::Smoke { bin_dir, sandbox } => smoke::run(root, &root.join(bin_dir), sandbox),
        Task::SmokeSystemd { bin_dir } => {
            let status = Command::new("bash")
                .arg(root.join("deploy/systemd/smoke.sh"))
                .arg(root.join(bin_dir))
                .status()
                .context("starting systemd user service smoke")?;
            ensure!(
                status.success(),
                "systemd user service smoke exited with {status}"
            );
            Ok(())
        }
        Task::Package { bin_dir, out_dir } => {
            let archive = package::build(root, &root.join(bin_dir), &root.join(out_dir))?;
            println!("{}", archive.display());
            Ok(())
        }
        Task::VerifyCratePackages => package::verify_crate_packages(root),
        Task::WorkflowExample {
            out_dir,
            inject_failure,
        } => {
            let graph = example::build(root, &root.join(out_dir), inject_failure)?;
            println!("{}", graph.display());
            Ok(())
        }
        Task::Agent { command } => agents::run(root, command),
    }
}

fn check(root: &Path) -> Result<()> {
    agents::validate(root).context("validating the agent registry")?;
    for args in [
        &["fmt", "--all", "--", "--check"][..],
        &[
            "clippy",
            "--locked",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--locked", "--workspace"],
    ] {
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .current_dir(root)
            .args(args)
            .status()
            .with_context(|| format!("starting cargo {}", args[0]))?;
        ensure!(status.success(), "cargo {} exited with {status}", args[0]);
    }
    Ok(())
}
