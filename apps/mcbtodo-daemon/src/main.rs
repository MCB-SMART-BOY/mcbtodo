#[cfg(target_os = "linux")]
mod endpoint;
#[cfg(target_os = "linux")]
mod notifications;
#[cfg(target_os = "linux")]
mod run_store;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod timer;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(about = "Run the local mcbtodo workspace daemon in the foreground")]
struct Args {}

fn main() -> Result<()> {
    Args::parse();
    #[cfg(target_os = "linux")]
    {
        let paths = mcbtodo_platform::WorkspacePaths::discover()?;
        server::Server::open(paths)?.run()
    }
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("mcbtodo-daemon requires Linux");
}
