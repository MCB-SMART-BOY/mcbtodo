//! Local script runner. The caller authorizes an exact revision and may supply its pinned
//! executable. The only writable regular file is the bounded result file.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail, ensure};
use mcbtodo_ports::{
    Cancellation,
    sandbox::{Sandbox, SandboxError, SandboxOutput, Script},
};
use rustix::{
    fs::{Access, OFlags, access, fcntl_setfl},
    process::{geteuid, getuid},
};
use serde_json::Value;

const BASH: &str = "/usr/bin/bash";
const NU: &str = "/usr/bin/nu";
const BWRAP: &str = "/usr/bin/bwrap";
const MAX_SOURCE: usize = 1024 * 1024;
const MAX_EXECUTABLE: usize = 2 * 1024 * 1024;
const MAX_JSON: usize = 1024 * 1024;
const MAX_LOG: usize = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const READY: &[u8] = b"mcbtodo-ready\n";

/// Fixed system executables used by the Linux sandbox adapter.
#[derive(Clone, Copy, Debug)]
pub enum SandboxExecutable {
    Bubblewrap,
    Bash,
    Nushell,
}

impl SandboxExecutable {
    pub fn path(self) -> &'static Path {
        Path::new(match self {
            Self::Bubblewrap => BWRAP,
            Self::Bash => BASH,
            Self::Nushell => NU,
        })
    }

    /// Check installation metadata and execute permission without starting a process.
    /// This checks neither namespace support nor shared libraries or isolation.
    pub fn check_installation(self) -> anyhow::Result<()> {
        trusted_runtime_executable(self.path()).map(|_| ())
    }
}

// Bubblewrap forwards caller descriptors to its command. Close everything above stdio
// before opening the one fd explicitly consumed by --seccomp. No script text enters here.
const HOST_LAUNCHER: &str = r#"
    [[ -d /proc/self/fd && -r /proc/self/fd && -x /proc/self/fd ]] || exit 125
    for entry in /proc/self/fd/*; do
        fd=${entry##*/}
        [[ $fd =~ ^[0-9]+$ ]] || exit 125
        if (( fd > 2 )); then exec {fd}>&- || exit 125; fi
    done
    exec 3<"$1" || exit 125
    shift
    exec "$@"
"#;

// Descriptor 2 carries the model connection only until the host shim moves
// it out of the way of Bubblewrap diagnostics. The sandbox shim gives it to
// the trusted relay helper as descriptor 2, then closes the intermediate fd.
const CODEX_HOST_LAUNCHER: &str = r#"
    exec 4>&2 2>/dev/null || exit 125
    [[ -d /proc/self/fd && -r /proc/self/fd && -x /proc/self/fd ]] || exit 125
    for entry in /proc/self/fd/*; do
        fd=${entry##*/}
        [[ $fd =~ ^[0-9]+$ ]] || exit 125
        if (( fd == 3 || fd > 4 )); then exec {fd}>&- || exit 125; fi
    done
    exec 3<"$1" || exit 125
    shift
    exec "$@"
"#;
const CODEX_SANDBOX_LAUNCHER: &str = r#"
    umask 077
    ulimit -c 0 && ulimit -n 128 && ulimit -f 16384 || exit 125
    exec /bin/mcbtodo-worker --internal-codex-session "$1" 2>&4 4>&-
"#;

pub(crate) fn codex_command(
    filter: &Path,
    codex: &Path,
    timeout: Duration,
) -> anyhow::Result<Command> {
    ensure!(
        geteuid() == getuid() && !geteuid().is_root(),
        "Codex sandbox requires an unprivileged Worker"
    );
    let bash = trusted_runtime_executable(SandboxExecutable::Bash.path())?;
    let bwrap = trusted_runtime_executable(SandboxExecutable::Bubblewrap.path())?;
    fs::write(filter, seccomp_filter(true)?)?;
    // Resolve the installed Worker before entering the user namespace.
    // The model broker is represented solely by the connected stream.
    let worker = protected_executable(&std::env::current_exe()?)?;
    let mut command = Command::new(&bash);
    command
        .env_clear()
        .current_dir("/")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            CODEX_HOST_LAUNCHER,
            "mcbtodo-launcher",
        ])
        .arg(filter)
        .arg(bwrap)
        .args([
            "--unshare-user",
            "--disable-userns",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup",
            "--cap-drop",
            "ALL",
            "--as-pid-1",
            "--die-with-parent",
            "--new-session",
            "--seccomp",
            "3",
            "--clearenv",
            "--setenv",
            "LD_LIBRARY_PATH",
            "/runtime",
            "--setenv",
            "HOME",
            "/scratch",
            "--setenv",
            "CODEX_HOME",
            "/scratch/.codex",
            "--setenv",
            "PATH",
            "/bin",
            "--size",
            "33554432",
            "--tmpfs",
            "/scratch",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--ro-bind",
        ])
        .arg(codex)
        .args(["/bin/codex"]);
    mount_runtime_files(
        &mut command,
        [
            (&*bash, BASH.as_ref()),
            (&*worker, "/bin/mcbtodo-worker".as_ref()),
        ],
    )?;
    command
        .args([
            "--remount-ro",
            "/",
            "--chdir",
            "/scratch",
            "--",
            BASH,
            "--noprofile",
            "--norc",
            "-c",
            CODEX_SANDBOX_LAUNCHER,
            "mcbtodo-codex",
        ])
        .arg(timeout.as_millis().max(1).to_string());
    Ok(command)
}

// PID 1 is already in the new namespace with its parent-death signal armed. The
// supervisor releases it only while the Bubblewrap monitor is alive. EOF during startup
// (including if the daemon dies before Bubblewrap arms its own signal) never runs a script.
// Hard limits apply to PID 1 before it execs the shell and are inherited by descendants.
const SANDBOX_LAUNCHER: &str = r#"
    umask 077
    ulimit -c 0 && ulimit -n 64 && ulimit -u 32 && ulimit -v 262144 &&
        ulimit -f "$1" && ulimit -t "$2" || exit 125
    shift 2
    printf 'mcbtodo-ready\n' || exit 125
    IFS= read -r token && [[ $token == start ]] || exit 125
    exec "$@" </dev/null
"#;

pub struct Bubblewrap<'a> {
    state_dir: &'a Path,
}

impl<'a> Bubblewrap<'a> {
    pub fn new(state_dir: &'a Path) -> Self {
        Self { state_dir }
    }
}

impl Sandbox for Bubblewrap<'_> {
    fn execute(
        &self,
        script: Script<'_>,
        timeout_seconds: u32,
        inputs: &BTreeMap<String, Value>,
        cancelled: &dyn Cancellation,
    ) -> Result<SandboxOutput, SandboxError> {
        execute_with_bwrap(
            self.state_dir,
            script,
            timeout_seconds,
            inputs,
            cancelled,
            BWRAP,
        )
    }
}

fn execute_with_bwrap(
    state_dir: &Path,
    script: Script<'_>,
    timeout_seconds: u32,
    inputs: &BTreeMap<String, Value>,
    cancelled: &dyn Cancellation,
    bwrap: &str,
) -> Result<SandboxOutput, SandboxError> {
    let timeout = Duration::from_secs(u64::from(timeout_seconds));
    let start = Instant::now();
    check_deadline(cancelled, start, timeout)?;
    let runner_args: &[&str] = match script.runner {
        "bash" => &[BASH, "--noprofile", "--norc", "/work/script"],
        "nushell" => &[NU, "--no-config-file", "--no-history", "/work/script.nu"],
        _ => {
            return Err(SandboxError::Process(anyhow!(
                "unsupported script runner {:?}",
                script.runner
            )));
        }
    };
    if script.source.len() > MAX_SOURCE {
        return Err(SandboxError::Process(anyhow!(
            "script source exceeds {MAX_SOURCE} bytes"
        )));
    }
    if let Some(executable) = script.executable
        && (!executable.starts_with(b"\x7fELF") || executable.len() > MAX_EXECUTABLE)
    {
        return Err(SandboxError::Process(anyhow!(
            "pinned executable must be an ELF file of at most {MAX_EXECUTABLE} bytes"
        )));
    }
    let (_scratch, output_file, mut command) =
        prepare(state_dir, bwrap, runner_args[0], script, inputs)
            .map_err(SandboxError::NotStarted)?;
    command
        .args([
            "--",
            BASH,
            "--noprofile",
            "--norc",
            "-c",
            SANDBOX_LAUNCHER,
            "mcbtodo-sandbox",
        ])
        // Bash expresses RLIMIT_FSIZE in units of 1024 bytes. Leave a block to detect oversize.
        .arg((MAX_JSON / 1024 + 1).to_string())
        .arg(timeout_seconds.to_string())
        .args(runner_args);
    check_deadline(cancelled, start, timeout)?;
    let (stdout, stderr) = supervise(&mut command, cancelled, start, timeout)?;
    let mut result = Vec::new();
    output_file
        .take(MAX_JSON as u64 + 1)
        .read_to_end(&mut result)
        .context("read sandbox result file")
        .map_err(SandboxError::Infrastructure)?;
    check_deadline(cancelled, start, timeout)?;
    if result.len() > MAX_JSON {
        return Err(SandboxError::Process(anyhow!(
            "sandbox JSON result exceeds {MAX_JSON} bytes"
        )));
    }
    let outputs = serde_json::from_slice::<BTreeMap<String, Value>>(&result)
        .context("sandbox result must be a JSON object")
        .map_err(SandboxError::Process)?;
    check_deadline(cancelled, start, timeout)?;
    Ok(SandboxOutput {
        outputs,
        stdout,
        stderr,
    })
}

fn check_deadline(
    cancelled: &dyn Cancellation,
    start: Instant,
    timeout: Duration,
) -> Result<(), SandboxError> {
    if cancelled.is_cancelled() {
        return Err(SandboxError::Cancelled);
    }
    if start.elapsed() >= timeout {
        return Err(SandboxError::Process(anyhow!(
            "sandbox execution timed out"
        )));
    }
    Ok(())
}

fn prepare(
    state_dir: &Path,
    bwrap: &str,
    runner: &str,
    script: Script<'_>,
    inputs: &BTreeMap<String, Value>,
) -> anyhow::Result<(tempfile::TempDir, File, Command)> {
    ensure!(
        geteuid() == getuid() && !geteuid().is_root(),
        "sandbox requires an unprivileged daemon"
    );
    for executable in [bwrap, BASH, runner] {
        trusted_runtime_executable(executable.as_ref())?;
    }
    let state_dir = state_dir
        .canonicalize()
        .context("resolve daemon state directory")?;
    let scratch = tempfile::Builder::new()
        .prefix("script-sandbox-")
        .tempdir_in(state_dir)
        .context("create private sandbox scratch directory")?;
    let path = scratch.path();
    fs::write(path.join("script"), script.source).context("stage script revision")?;
    let mut input = BoundedFile(File::create(path.join("input.json"))?, MAX_JSON);
    serde_json::to_writer(&mut input, inputs).context("stage sandbox JSON input")?;
    File::create(path.join("output.json")).context("create sandbox result file")?;
    let output = File::open(path.join("output.json")).context("open sandbox result file")?;
    fs::write(path.join("seccomp.bpf"), seccomp_filter(false)?)
        .context("stage sandbox syscall filter")?;

    let mut command = Command::new(BASH);
    command
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
            "--noprofile",
            "--norc",
            "-c",
            HOST_LAUNCHER,
            "mcbtodo-launcher",
        ])
        .arg(path.join("seccomp.bpf"))
        .args([
            bwrap,
            "--unshare-user",
            "--disable-userns",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup",
            "--cap-drop",
            "ALL",
            "--as-pid-1",
            "--die-with-parent",
            "--new-session",
            "--seccomp",
            "3",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/bin",
            "--setenv",
            "LC_ALL",
            "C",
            "--setenv",
            "LD_LIBRARY_PATH",
            "/runtime",
            "--setenv",
            "RAYON_NUM_THREADS",
            "2",
            "--setenv",
            "XDG_CONFIG_HOME",
            "/config",
            "--dir",
            "/config",
            "--setenv",
            "PROD_INPUT_JSON",
            "/work/input.json",
            "--setenv",
            "PROD_OUTPUT_JSON",
            "/work/output.json",
            "--dev-bind",
            "/dev/null",
            "/dev/null",
        ]);
    if runner == NU {
        // Nushell resolves its own executable at startup. A literal symlink
        // supplies that path without exposing kernel or host state via procfs.
        command.args(["--symlink", NU, "/proc/self/exe"]);
    }
    mount_runtime(&mut command, runner)?;
    if let Some(executable) = script.executable {
        let native = path.join("executable");
        fs::write(&native, executable).context("stage pinned executable")?;
        fs::set_permissions(&native, fs::Permissions::from_mode(0o500))?;
        command.arg("--ro-bind").arg(native).args([
            "/work/executable",
            "--setenv",
            "PROD_EXECUTABLE",
            "/work/executable",
        ]);
    }
    for (host, guest) in [
        (
            "script",
            if runner == NU {
                "/work/script.nu"
            } else {
                "/work/script"
            },
        ),
        ("input.json", "/work/input.json"),
    ] {
        command.arg("--ro-bind").arg(path.join(host)).arg(guest);
    }
    command
        .arg("--bind")
        .arg(path.join("output.json"))
        .arg("/work/output.json")
        .args(["--remount-ro", "/", "--chdir", "/work"]);
    Ok((scratch, output, command))
}

struct BoundedFile(File, usize);

impl Write for BoundedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.1 {
            return Err(io::Error::other("sandbox JSON input is too large"));
        }
        let n = self.0.write(buf)?;
        self.1 -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn trusted_runtime_file(path: &Path) -> anyhow::Result<PathBuf> {
    let real = path
        .canonicalize()
        .with_context(|| format!("sandbox runtime file {} is unavailable", path.display()))?;
    let meta = fs::metadata(&real)?;
    ensure!(
        (real.starts_with("/usr/bin")
            || real.starts_with("/usr/lib")
            || real.starts_with("/usr/lib64")
            || real.starts_with("/lib")
            || real.starts_with("/lib64"))
            && meta.is_file()
            && meta.uid() == 0
            && meta.mode() & 0o6022 == 0,
        "sandbox runtime file {} is outside the trusted system installation",
        real.display()
    );
    Ok(real)
}

fn trusted_runtime_executable(path: &Path) -> anyhow::Result<PathBuf> {
    let real = trusted_runtime_file(path)?;
    access(&real, Access::EXEC_OK)
        .with_context(|| format!("sandbox runtime file {} is not executable", real.display()))?;
    Ok(real)
}

/// Resolve the launch target and reject canonical paths that another user
/// can replace between inspection and the bind mount. The installing user
/// and root are trusted to keep the installation stable during an invocation.
pub(crate) fn protected_executable(path: &Path) -> anyhow::Result<PathBuf> {
    let real = path.canonicalize()?;
    let uid = geteuid().as_raw();
    let protected =
        |meta: &fs::Metadata| (meta.uid() == uid || meta.uid() == 0) && meta.mode() & 0o022 == 0;
    for directory in real
        .ancestors()
        .skip(1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let meta = fs::symlink_metadata(directory)?;
        ensure!(
            meta.is_dir() && protected(&meta),
            "unprotected executable directory: {}",
            directory.display()
        );
    }
    let meta = fs::symlink_metadata(&real)?;
    ensure!(
        meta.is_file() && protected(&meta) && meta.mode() & 0o6000 == 0 && meta.mode() & 0o111 != 0,
        "unprotected or non-executable file: {}",
        real.display()
    );
    Ok(real)
}

// Only mount the trusted shells, their ELF loader, and their resolved shared-library closure.
// The loader resolves dependencies of the installed executables before any user script is read.
fn mount_runtime(command: &mut Command, runner: &str) -> anyhow::Result<()> {
    let files = [BASH, runner]
        .into_iter()
        .take(if runner == BASH { 1 } else { 2 })
        .map(|dest| Ok((trusted_runtime_file(dest.as_ref())?, Path::new(dest))))
        .collect::<anyhow::Result<Vec<_>>>()?;
    mount_runtime_files(
        command,
        files.iter().map(|(src, dest)| (src.as_path(), *dest)),
    )
}

fn mount_runtime_files<'a>(
    command: &mut Command,
    executables: impl IntoIterator<Item = (&'a Path, &'a Path)>,
) -> anyhow::Result<()> {
    let loader = match std::env::consts::ARCH {
        "aarch64" => "/lib/ld-linux-aarch64.so.1",
        "x86_64" => "/lib64/ld-linux-x86-64.so.2",
        arch => bail!("sandbox runtime is unavailable on {arch}"),
    };
    command
        .arg("--ro-bind")
        .arg(trusted_runtime_file(loader.as_ref())?)
        .arg(loader);
    let mut libraries = BTreeMap::new();
    for (source, dest) in executables {
        let executable = dest.display();
        let listing = Command::new(loader)
            .arg("--list")
            .arg(source)
            .env_clear()
            .env("LC_ALL", "C")
            .current_dir("/")
            .output()
            .with_context(|| format!("inspect dependencies of {executable}"))?;
        ensure!(
            listing.status.success(),
            "cannot resolve runtime dependencies of {executable}"
        );
        for line in std::str::from_utf8(&listing.stdout)?.lines().map(str::trim) {
            let Some((name, resolution)) = line.split_once(" => ") else {
                ensure!(
                    line.starts_with("linux-vdso.so.1 ") || line.starts_with(loader),
                    "unexpected loader entry for {executable}: {line}"
                );
                continue;
            };
            let (resolved, _) = resolution
                .rsplit_once(" (0x")
                .context("invalid runtime dependency path")?;
            ensure!(
                Path::new(name).file_name().is_some_and(|base| base == name),
                "invalid runtime library name"
            );
            let real = trusted_runtime_file(resolved.as_ref())?;
            if let Some(previous) = libraries.insert(name.to_owned(), real.clone()) {
                ensure!(
                    previous == real,
                    "conflicting sandbox runtime library {name}"
                );
            }
        }
        command.arg("--ro-bind").arg(source).arg(dest);
    }
    for (name, real) in libraries {
        command
            .arg("--ro-bind")
            .arg(real)
            .arg(Path::new("/runtime").join(name));
    }
    Ok(())
}

// Classic BPF instructions as read by bwrap --seccomp. Deny other syscall ABIs, socket
// creation (including non-network-namespaced address families), io_uring, global keyrings,
// and anonymous file creation. PID 1 may not clear the kernel's parent-death signal.
fn seccomp_filter(codex_loopback: bool) -> anyhow::Result<Vec<u8>> {
    let (arch, prctl, sockets, denied): (u32, u32, [u32; 2], &[u32]) = match std::env::consts::ARCH
    {
        // unshare, ptrace, msgget, semget, shmget, socket, socketpair, add_key,
        // request_key, keyctl, perf_event_open, setns, memfd_create, bpf, userfaultfd
        "aarch64" => (
            0xc000_00b7,
            167,
            [198, 199],
            &[
                97, 117, 186, 190, 194, 217, 218, 219, 241, 268, 279, 280, 282,
            ],
        ),
        "x86_64" => (
            0xc000_003e,
            157,
            [41, 53],
            &[272, 101, 68, 64, 29, 248, 249, 250, 298, 308, 319, 321, 323],
        ),
        arch => bail!("sandbox syscall filter is unavailable on {arch}"),
    };
    const LD_WORD: u16 = 0x20;
    const JEQ: u16 = 0x15;
    const JGE: u16 = 0x35;
    const RET: u16 = 0x06;
    const KILL: u32 = 0x8000_0000;
    const EPERM: u32 = 0x0005_0001;
    const ALLOW: u32 = 0x7fff_0000;
    let mut bytes = Vec::new();
    let mut insn = |code: u16, yes: u8, no: u8, value: u32| {
        bytes.extend_from_slice(&code.to_ne_bytes());
        bytes.extend_from_slice(&[yes, no]);
        bytes.extend_from_slice(&value.to_ne_bytes());
    };
    insn(LD_WORD, 0, 0, 4); // seccomp_data.arch
    insn(JEQ, 1, 0, arch);
    insn(RET, 0, 0, KILL);
    insn(LD_WORD, 0, 0, 0); // seccomp_data.nr
    insn(JGE, 0, 1, 0x4000_0000); // x32 syscall ABI
    insn(RET, 0, 0, KILL);
    // io_uring_setup/enter/register, memfd_secret, and newer syscalls need review.
    insn(JGE, 0, 1, 447); // memfd_secret and newer syscalls
    insn(RET, 0, 0, EPERM);
    for nr in denied.iter().copied().chain([425, 426, 427]) {
        insn(JEQ, 0, 1, nr);
        insn(RET, 0, 0, EPERM);
    }
    let arg0 = if cfg!(target_endian = "little") {
        16
    } else {
        20
    };
    for nr in sockets {
        if codex_loopback {
            insn(JEQ, 0, 6, nr);
            insn(LD_WORD, 0, 0, arg0);
            insn(JEQ, 3, 0, 1); // AF_UNIX
            insn(JEQ, 2, 0, 2); // AF_INET
            insn(JEQ, 1, 0, 10); // AF_INET6
            insn(RET, 0, 0, EPERM);
            insn(RET, 0, 0, ALLOW);
        } else {
            insn(JEQ, 0, 1, nr);
            insn(RET, 0, 0, EPERM);
        }
    }
    insn(JEQ, 0, 3, prctl);
    insn(LD_WORD, 0, 0, arg0);
    insn(JEQ, 0, 1, 1); // PR_SET_PDEATHSIG
    insn(RET, 0, 0, EPERM);
    insn(RET, 0, 0, ALLOW);
    Ok(bytes)
}

// The direct child is the Bubblewrap monitor. Killing it kills namespace PID 1 via
// PDEATHSIG, and the kernel then tears down the whole PID namespace, including any setsid jobs.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn supervise(
    command: &mut Command,
    cancelled: &dyn Cancellation,
    start: Instant,
    timeout: Duration,
) -> Result<(String, String), SandboxError> {
    let mut child = Running(
        command
            .spawn()
            .context("launch Bubblewrap")
            .map_err(SandboxError::NotStarted)?,
    );
    let mut stdin = child.0.stdin.take();
    let mut stdout = child.0.stdout.take().expect("piped sandbox stdout");
    let mut stderr = child.0.stderr.take().expect("piped sandbox stderr");
    fcntl_setfl(&stdout, OFlags::NONBLOCK)
        .and_then(|()| fcntl_setfl(&stderr, OFlags::NONBLOCK))
        .context("set sandbox pipes nonblocking")
        .map_err(SandboxError::NotStarted)?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        check_deadline(cancelled, start, timeout)?;
        let io_error = |error| {
            if stdin.is_some() {
                SandboxError::NotStarted(error)
            } else {
                SandboxError::Infrastructure(error)
            }
        };
        let log_limit = MAX_LOG + if stdin.is_some() { READY.len() } else { 0 };
        let out_eof = read_log(&mut stdout, &mut out, log_limit)
            .context("read sandbox stdout")
            .map_err(io_error)?;
        let err_eof = read_log(&mut stderr, &mut err, MAX_LOG)
            .context("read sandbox stderr")
            .map_err(io_error)?;
        let status = child
            .0
            .try_wait()
            .context("wait for Bubblewrap")
            .map_err(io_error)?;
        if stdin.is_some() && out.starts_with(READY) && status.is_none() {
            check_deadline(cancelled, start, timeout)?;
            stdin
                .take()
                .expect("startup pipe is open")
                .write_all(b"start\n")
                .context("release sandbox startup gate")
                .map_err(SandboxError::Infrastructure)?;
            out.drain(..READY.len());
        }
        if out.len() > MAX_LOG || err.len() > MAX_LOG {
            let error = anyhow!("sandbox log exceeds {MAX_LOG} bytes");
            return Err(if stdin.is_some() {
                SandboxError::NotStarted(error)
            } else {
                SandboxError::Process(error)
            });
        }
        if let Some(status) = status.filter(|_| out_eof && err_eof) {
            check_deadline(cancelled, start, timeout)?;
            let stdout = log_text(&out);
            let stderr = log_text(&err);
            if stdin.is_some() {
                return Err(SandboxError::NotStarted(anyhow!(
                    "Bubblewrap failed to start the isolated runner ({status}): {stderr}"
                )));
            }
            if !status.success() {
                return Err(SandboxError::Process(anyhow!(
                    "script exited with {status}\nstdout: {stdout}\nstderr: {stderr}"
                )));
            }
            return Ok((stdout, stderr));
        }
        thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(start.elapsed())));
    }
}

fn read_log(reader: &mut impl Read, buf: &mut Vec<u8>, limit: usize) -> io::Result<bool> {
    let mut chunk = [0; 8192];
    while buf.len() <= limit {
        let max = (limit + 1 - buf.len()).min(chunk.len());
        match reader.read(&mut chunk[..max]) {
            Ok(0) => return Ok(true),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn log_text(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.len() > MAX_LOG {
        text.truncate(text.floor_char_boundary(MAX_LOG));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::{
        event::{PollFd, PollFlags, Timespec, poll},
        process::{Pid, PidfdFlags, pidfd_open},
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    const WAITING_SCRIPT: &[u8] = br#"
        if (: >&19) 2>/dev/null; then exit 23; fi
        (printf '{}' > "$PROD_OUTPUT_JSON"; kill -STOP "$BASHPID") &
        wait
    "#;

    fn execute(
        state_dir: &Path,
        source: &[u8],
        runner: &str,
        timeout_seconds: u32,
        inputs: &BTreeMap<String, Value>,
        cancelled: &AtomicBool,
    ) -> Result<SandboxOutput, SandboxError> {
        Bubblewrap::new(state_dir).execute(
            Script {
                source,
                runner,
                executable: None,
            },
            timeout_seconds,
            inputs,
            cancelled,
        )
    }

    #[test]
    fn rejects_unsupported_runners_and_cancelled_requests() {
        let state = tempfile::tempdir().unwrap();
        let inputs = BTreeMap::new();
        let cancelled = AtomicBool::new(false);
        let err = execute(state.path(), b"", "python", 2, &inputs, &cancelled).unwrap_err();
        assert!(matches!(err, SandboxError::Process(_)) && err.to_string().contains("unsupported"));
        cancelled.store(true, Ordering::Relaxed);
        let err = execute(state.path(), b"", "bash", 2, &inputs, &cancelled).unwrap_err();
        assert!(matches!(err, SandboxError::Cancelled));
    }

    #[test]
    fn unavailable_bubblewrap_fails_closed() {
        let state = tempfile::tempdir().unwrap();
        let err = execute_with_bwrap(
            state.path(),
            Script {
                source: b"printf 'must not run'",
                runner: "bash",
                executable: None,
            },
            2,
            &BTreeMap::new(),
            &AtomicBool::new(false),
            "/usr/bin/nonexistent-mcbtodo-bwrap",
        )
        .unwrap_err();
        assert!(matches!(err, SandboxError::NotStarted(_)));
        assert_eq!(state.path().read_dir().unwrap().count(), 0);
    }

    #[test]
    fn namespace_startup_failure_never_releases_the_script() {
        let mut command = Command::new("/usr/bin/bash");
        command
            .args(["--noprofile", "--norc", "-c", "exit 125"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let error = supervise(
            &mut command,
            &AtomicBool::new(false),
            Instant::now(),
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(matches!(error, SandboxError::NotStarted(_)), "{error:#}");
    }

    // Availability is checked with a real namespace probe. Set MCBTODO_REQUIRE_SANDBOX=1
    // on a sandbox-enabled machine to make absence a test failure.
    fn available(state: &Path) -> bool {
        let result = execute(
            state,
            b"printf '{}' > \"$PROD_OUTPUT_JSON\"",
            "bash",
            5,
            &BTreeMap::new(),
            &AtomicBool::new(false),
        );
        match result {
            Ok(_) => true,
            Err(err @ SandboxError::NotStarted(_))
                if std::env::var_os("MCBTODO_REQUIRE_SANDBOX").is_none() =>
            {
                eprintln!("sandbox unavailable: {err:#}");
                false
            }
            Err(err) => panic!("sandbox probe failed: {err:#}"),
        }
    }

    #[test]
    fn bash_uses_separate_json_and_logs_and_an_isolated_filesystem() {
        let state = tempfile::tempdir().unwrap();
        if !available(state.path()) {
            return;
        }
        let input = BTreeMap::from([("answer".into(), json!(42))]);
        let script = br#"
            set -eu
            [[ ! -v HOME && ! -e /home && ! -e /run && ! -e /etc && ! -e /tmp ]]
            [[ ! -e /usr/bin/env && ! -e /proc ]]
            [[ ! -w "$PROD_INPUT_JSON" && ! -w /work ]]
            printf 'stdout\n'
            printf 'stderr\n' >&2
            IFS= read -r value < "$PROD_INPUT_JSON" || [[ -n $value ]]
            printf '%s' "$value" > "$PROD_OUTPUT_JSON"
        "#;
        let result = execute(
            state.path(),
            script,
            "bash",
            5,
            &input,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.outputs, input);
        assert_eq!(result.stdout, "stdout\n");
        assert_eq!(result.stderr, "stderr\n");
        assert_eq!(state.path().read_dir().unwrap().count(), 0);
    }

    #[test]
    fn nushell_reads_and_writes_json_without_host_config() {
        let state = tempfile::tempdir().unwrap();
        if !available(state.path()) {
            return;
        }
        if !Path::new(NU).exists() && std::env::var_os("MCBTODO_REQUIRE_SANDBOX").is_none() {
            return;
        }
        let input = BTreeMap::from([("answer".into(), json!(42))]);
        let source = br#"
            print 'nu stdout'
            open --raw $env.PROD_INPUT_JSON | from json | to json | save --force $env.PROD_OUTPUT_JSON
        "#;
        let result = execute(
            state.path(),
            source,
            "nushell",
            5,
            &input,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.outputs, input);
        assert_eq!(result.stdout, "nu stdout\n");
    }

    #[test]
    fn rejects_nonzero_exit_invalid_result_and_excess_logs() {
        let state = tempfile::tempdir().unwrap();
        if !available(state.path()) {
            return;
        }
        for (script, message) in [
            ("exit 23", "exit status: 23"),
            ("printf '[]' > \"$PROD_OUTPUT_JSON\"", "JSON object"),
            ("printf '%070000d' 0", "log exceeds"),
            ("printf '%070000d' 0 >&2", "log exceeds"),
            (
                "printf '%01048577d' 0 > \"$PROD_OUTPUT_JSON\"",
                "JSON result exceeds",
            ),
        ] {
            let err = execute(
                state.path(),
                script.as_bytes(),
                "bash",
                5,
                &BTreeMap::new(),
                &AtomicBool::new(false),
            )
            .unwrap_err();
            assert!(
                matches!(err, SandboxError::Process(_)) && err.to_string().contains(message),
                "{err:#}"
            );
        }
    }

    #[test]
    fn timeout_and_cancellation_stop_the_namespace() {
        let state = tempfile::tempdir().unwrap();
        if !available(state.path()) {
            return;
        }
        let cancelled = AtomicBool::new(false);
        let err = execute(
            state.path(),
            WAITING_SCRIPT,
            "bash",
            1,
            &BTreeMap::new(),
            &cancelled,
        )
        .unwrap_err();
        assert!(
            matches!(err, SandboxError::Process(_)) && err.to_string().contains("timed out"),
            "{err:#}"
        );
        thread::scope(|scope| {
            scope.spawn(|| {
                wait_for_result_marker(state.path());
                cancelled.store(true, Ordering::Release);
            });
            let started = Instant::now();
            let err = execute(
                state.path(),
                WAITING_SCRIPT,
                "bash",
                10,
                &BTreeMap::new(),
                &cancelled,
            )
            .unwrap_err();
            assert!(matches!(err, SandboxError::Cancelled), "{err:#}");
            assert!(started.elapsed() < Duration::from_secs(2));
        });
        assert_eq!(state.path().read_dir().unwrap().count(), 0);
    }

    #[test]
    fn daemon_subprocess() {
        let Some(state) = std::env::var_os("MCBTODO_SANDBOX_TEST_STATE") else {
            return;
        };
        assert!(Path::new("/proc/self/fd/19").exists());
        execute(
            Path::new(&state),
            WAITING_SCRIPT,
            "bash",
            15,
            &BTreeMap::new(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    }

    #[test]
    fn daemon_death_kills_descendants_and_inherited_fds_stay_outside() {
        let state = tempfile::tempdir().unwrap();
        if !available(state.path()) {
            return;
        }
        let self_test = module_path!()
            .split("::")
            .skip(1)
            .chain(std::iter::once("daemon_subprocess"))
            .collect::<Vec<_>>()
            .join("::");
        let mut daemon = Running(
            Command::new(BASH)
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    "exec 19<>/dev/null; exec \"$@\"",
                    "test-launcher",
                ])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", &self_test, "--nocapture"])
                .env("MCBTODO_SANDBOX_TEST_STATE", state.path())
                .spawn()
                .unwrap(),
        );
        wait_for_result_marker(state.path());

        // Once the marker is visible the test daemon has a Bubblewrap monitor,
        // which parents namespace PID 1, which in turn parents the stopped job.
        let monitor = only_child(daemon.0.id());
        let init = only_child(monitor);
        let stopped = only_child(init);
        let fd = pidfd_open(
            Pid::from_raw(stopped.try_into().unwrap()).unwrap(),
            PidfdFlags::empty(),
        )
        .unwrap();
        daemon.0.kill().unwrap();
        daemon.0.wait().unwrap();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        assert_eq!(
            poll(
                &mut fds,
                Some(&Timespec {
                    tv_sec: 2,
                    tv_nsec: 0
                })
            )
            .unwrap(),
            1
        );
        assert!(fds[0].revents().contains(PollFlags::IN));
    }

    fn wait_for_result_marker(state: &Path) {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if state.read_dir().unwrap().any(|entry| {
                fs::read(entry.unwrap().path().join("output.json"))
                    .as_deref()
                    .ok()
                    == Some(b"{}")
            }) {
                return;
            }
            assert!(
                Instant::now() < until,
                "sandbox script did not reach its marker"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn only_child(parent: u32) -> u32 {
        let children: Vec<u32> = fs::read_dir(format!("/proc/{parent}/task"))
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|task| fs::read_to_string(task.path().join("children")).ok())
            .flat_map(|list| {
                list.split_whitespace()
                    .map(|pid| pid.parse().unwrap())
                    .collect::<Vec<_>>()
            })
            .collect();
        match children.as_slice() {
            [pid] => *pid,
            _ => panic!("expected one child for {parent}, found {children:?}"),
        }
    }
}
