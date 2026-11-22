mod agents;
#[cfg(target_os = "linux")]
mod codex_probe;
#[cfg(target_os = "linux")]
mod example;
mod package;
#[cfg(target_os = "linux")]
mod smoke;
#[cfg(target_os = "linux")]
mod systemd_smoke;

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
    /// Probe a trusted system Codex executable in an offline Linux namespace.
    #[cfg(target_os = "linux")]
    CodexProbe {
        /// Absolute path to the Codex executable, without a version manager shim.
        #[arg(long)]
        codex: PathBuf,
        /// Also verify the Workflow path using binaries in this directory.
        #[arg(long)]
        bin_dir: Option<PathBuf>,
    },
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    CodexFixture,
    /// Build the native Rust page-table workflow example.
    WorkflowExample {
        /// Destination directory relative to the workspace root, or an absolute path.
        #[arg(long, default_value = "target/examples/workflow")]
        out_dir: PathBuf,
        /// Build a version with a deliberately failing page-offset check.
        #[arg(long)]
        inject_failure: bool,
    },
    #[command(hide = true)]
    WorkerFixture,
    /// Prepare or launch a project agent harness with a selected workflow.
    Agent {
        #[command(subcommand)]
        command: agents::Task,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    #[cfg(target_os = "linux")]
    if let Task::CodexFixture = cli.command {
        return codex_probe::run_fixture();
    }
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
        #[cfg(target_os = "linux")]
        Task::Smoke { bin_dir, sandbox } => smoke::run(root, &root.join(bin_dir), sandbox),
        #[cfg(not(target_os = "linux"))]
        Task::Smoke { .. } => anyhow::bail!("workspace smoke requires Linux"),
        #[cfg(target_os = "linux")]
        Task::SmokeSystemd { bin_dir } => systemd_smoke::run(root, &root.join(bin_dir)),
        #[cfg(not(target_os = "linux"))]
        Task::SmokeSystemd { .. } => anyhow::bail!("smoke-systemd requires a Linux user manager"),
        #[cfg(target_os = "linux")]
        Task::Package { bin_dir, out_dir } => {
            let archive = package::build(root, &root.join(bin_dir), &root.join(out_dir))?;
            println!("{}", archive.display());
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        Task::Package { .. } => anyhow::bail!("packaging requires Linux"),
        Task::VerifyCratePackages => package::verify_crate_packages(root),
        #[cfg(target_os = "linux")]
        Task::CodexProbe { codex, bin_dir } => {
            let bin_dir = bin_dir.map(|path| root.join(path));
            codex_probe::run(&codex, bin_dir.as_deref())
        }
        #[cfg(target_os = "linux")]
        Task::CodexFixture => unreachable!("fixture dispatched before workspace discovery"),
        #[cfg(target_os = "linux")]
        Task::WorkflowExample {
            out_dir,
            inject_failure,
        } => {
            let graph = example::build(root, &root.join(out_dir), inject_failure)?;
            println!("{}", graph.display());
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        Task::WorkflowExample { .. } => {
            anyhow::bail!("the sandbox workflow example requires Linux")
        }
        #[cfg(target_os = "linux")]
        Task::WorkerFixture => smoke::worker_fixture_malformed_result(),
        #[cfg(not(target_os = "linux"))]
        Task::WorkerFixture => anyhow::bail!("the Worker fixture requires Linux"),
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
