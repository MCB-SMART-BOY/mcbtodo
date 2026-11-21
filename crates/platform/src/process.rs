use std::io;

#[cfg(target_os = "linux")]
pub fn arm_parent_death_signal(expected_parent_pid: u32) -> io::Result<()> {
    use rustix::process::{Pid, Signal, getppid, set_parent_process_death_signal};

    set_parent_process_death_signal(Some(Signal::KILL)).map_err(io::Error::from)?;
    let expected_parent = i32::try_from(expected_parent_pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid parent PID"))?;
    if getppid() != Some(expected_parent) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "parent process exited before death signal was armed",
        ));
    }
    Ok(())
}
