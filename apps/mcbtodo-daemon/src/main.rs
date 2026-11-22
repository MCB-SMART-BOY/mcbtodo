#[cfg(target_os = "linux")]
mod endpoint;
#[cfg(target_os = "linux")]
mod notifications;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod timer;
#[cfg(target_os = "linux")]
mod worker_process;

use anyhow::Result;
use clap::Parser;

const DEFAULT_WORKER_SLOTS: u8 = 1;
const MAX_WORKER_SLOTS: u8 = 4;

#[derive(Parser)]
#[command(about = "Run the local mcbtodo workspace daemon in the foreground")]
struct Args {
    /// Maximum number of supervised Script and Agent Workers.
    #[arg(
        long,
        default_value_t = DEFAULT_WORKER_SLOTS,
        value_parser = clap::value_parser!(u8).range(1..=i64::from(MAX_WORKER_SLOTS))
    )]
    worker_slots: u8,
}

fn main() -> Result<()> {
    let args = Args::parse();
    #[cfg(target_os = "linux")]
    {
        let paths = mcbtodo_platform::WorkspacePaths::discover()?;
        server::Server::open(paths, args.worker_slots)?.run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        anyhow::bail!("mcbtodo-daemon requires Linux");
    }
}
