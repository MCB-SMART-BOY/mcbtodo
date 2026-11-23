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
use mcbtodo_platform::WorkerSlots;

#[cfg(all(test, target_os = "linux"))]
const DEFAULT_WORKER_SLOTS: u8 = WorkerSlots::MIN;

#[derive(Parser)]
#[command(about = "Run the local mcbtodo workspace daemon in the foreground")]
struct Args {
    /// Override the Worker limit in config.toml (default: 1).
    #[arg(
        long,
        value_parser = clap::value_parser!(u8)
            .range(i64::from(WorkerSlots::MIN)..=i64::from(WorkerSlots::MAX))
    )]
    worker_slots: Option<u8>,
}

#[cfg(target_os = "linux")]
impl Args {
    fn worker_limit(&self, paths: &mcbtodo_platform::WorkspacePaths) -> u8 {
        if let Some(limit) = self.worker_slots {
            return limit;
        }
        match paths.load_optional_config() {
            Ok(config) => config.unwrap_or_default().worker_slots.get(),
            Err(error) => {
                let limit = WorkerSlots::DEFAULT.get();
                eprintln!("mcbtodo-daemon: using {limit} Worker slot: {error:#}");
                limit
            }
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    #[cfg(target_os = "linux")]
    {
        let paths = mcbtodo_platform::WorkspacePaths::discover()?;
        let worker_limit = args.worker_limit(&paths);
        server::Server::open(paths, worker_limit)?.run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        anyhow::bail!("mcbtodo-daemon requires Linux");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use mcbtodo_platform::WorkspacePaths;

    #[test]
    fn daemon_prefers_cli_worker_slots_to_the_saved_limit() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let default_args = Args::try_parse_from(["mcbtodo-daemon"])?;
        assert_eq!(default_args.worker_limit(&paths), 1);

        paths.initialize_config()?;
        std::fs::write(paths.config_path(), "worker_slots = 3\n")?;
        assert_eq!(default_args.worker_limit(&paths), 3);
        let override_args = Args::try_parse_from(["mcbtodo-daemon", "--worker-slots", "2"])?;
        assert_eq!(override_args.worker_limit(&paths), 2);

        std::fs::write(paths.config_path(), "worker_slots = 5\n")?;
        assert!(paths.load_config().is_err());
        assert_eq!(default_args.worker_limit(&paths), 1);
        assert_eq!(override_args.worker_limit(&paths), 2);
        assert!(Args::try_parse_from(["mcbtodo-daemon", "--worker-slots", "5"]).is_err());
        Ok(())
    }
}
