#[cfg(target_os = "linux")]
mod linux;

fn main() -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("mcbtodo-worker requires Linux");
    #[cfg(target_os = "linux")]
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().as_deref() == Some("--internal-codex-session".as_ref()) {
            // Stderr is initially a private binary stream in this mode.
            if let (Some(timeout), None) = (args.next(), args.next())
                && let Some(timeout) = timeout.to_str()
                && mcbtodo_execution::codex_host::run_isolated_session(timeout).is_ok()
            {
                return Ok(());
            }
            std::process::exit(1);
        }
        linux::run()
    }
}
