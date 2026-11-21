#[cfg(target_os = "linux")]
pub use linux::VaultWatcher;

#[cfg(not(target_os = "linux"))]
pub struct VaultWatcher;

#[cfg(not(target_os = "linux"))]
impl VaultWatcher {
    pub fn new(_: &std::path::Path) -> anyhow::Result<Self> {
        anyhow::bail!("filesystem watches are unavailable on this platform")
    }

    pub fn changed(&mut self) -> anyhow::Result<bool> {
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::BTreeMap,
        ffi::OsStr,
        fs,
        io::ErrorKind,
        mem::MaybeUninit,
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result};
    use rustix::{
        fd::OwnedFd,
        fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags},
        io::Errno,
    };

    const QUIET_PERIOD: Duration = Duration::from_millis(120);

    pub struct VaultWatcher {
        root: PathBuf,
        watches: Watches,
        pending: Option<Instant>,
        rebuild: bool,
    }

    impl VaultWatcher {
        pub fn new(root: &Path) -> Result<Self> {
            let root = root.canonicalize().context("resolving Vault path")?;
            let watches = Watches::new(&root)?;
            Ok(Self {
                root,
                watches,
                pending: None,
                rebuild: false,
            })
        }

        /// Drain notifications without touching the filesystem when idle.
        pub fn changed(&mut self) -> Result<bool> {
            let (dirty, rebuild) = self.watches.drain()?;
            self.rebuild |= rebuild;
            if dirty {
                self.pending = Some(Instant::now());
            }
            if !self
                .pending
                .is_some_and(|last| last.elapsed() >= QUIET_PERIOD)
            {
                return Ok(false);
            }
            if self.rebuild {
                // Watch parent directories before scanning their children. A
                // load after registration catches writes during the handover.
                self.watches = Watches::new(&self.root)?;
                self.rebuild = false;
            }
            self.pending = None;
            Ok(true)
        }
    }

    enum Directory {
        Parent(PathBuf),
        Root,
        Projects,
        Project,
        Markdown,
    }

    impl Directory {
        fn classify(&self, name: &Path, flags: ReadFlags) -> (bool, bool) {
            let named = |candidate: &str| name == Path::new(candidate);
            let topology = match self {
                Self::Parent(vault) => name == vault,
                Self::Root => {
                    named("projects") || named("calendar") || named("reminders") || named("inbox")
                }
                // Vault also validates the type of direct entries here.
                Self::Projects => true,
                Self::Project => named("tasks") || named("units"),
                Self::Markdown => false,
            };
            let markdown = match self {
                Self::Project => named("project.md"),
                Self::Markdown => name.extension() == Some(OsStr::new("md")),
                _ => false,
            };
            let rebuild = topology
                && flags.intersects(
                    ReadFlags::CREATE
                        | ReadFlags::DELETE
                        | ReadFlags::MOVED_FROM
                        | ReadFlags::MOVED_TO
                        | ReadFlags::ATTRIB,
                );
            (markdown || rebuild, rebuild)
        }
    }

    struct Watches {
        fd: OwnedFd,
        dirs: BTreeMap<i32, Directory>,
    }

    impl Watches {
        fn new(root: &Path) -> Result<Self> {
            let mut watches = Self {
                fd: inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)
                    .context("starting inotify")?,
                dirs: BTreeMap::new(),
            };
            if let Some((parent, name)) = root.parent().zip(root.file_name()) {
                watches.add(parent, Directory::Parent(name.into()))?;
            }
            if watches.add(root, Directory::Root)? {
                for name in ["calendar", "reminders", "inbox"] {
                    watches.add(&root.join(name), Directory::Markdown)?;
                }
                let projects = root.join("projects");
                if watches.add(&projects, Directory::Projects)? {
                    watches.add_projects(&projects)?;
                }
            }
            Ok(watches)
        }

        fn add_projects(&mut self, projects: &Path) -> Result<()> {
            let entries = match fs::read_dir(projects) {
                Ok(entries) => entries,
                Err(error)
                    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
                {
                    return Ok(());
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("listing {}", projects.display()));
                }
            };
            for entry in entries {
                let project = entry?.path();
                if self.add(&project, Directory::Project)? {
                    self.add(&project.join("tasks"), Directory::Markdown)?;
                    self.add(&project.join("units"), Directory::Markdown)?;
                }
            }
            Ok(())
        }

        fn add(&mut self, path: &Path, dir: Directory) -> Result<bool> {
            let flags = WatchFlags::ONLYDIR
                | WatchFlags::DONT_FOLLOW
                | WatchFlags::CLOSE_WRITE
                | WatchFlags::ATTRIB
                | WatchFlags::CREATE
                | WatchFlags::DELETE
                | WatchFlags::DELETE_SELF
                | WatchFlags::MOVED_FROM
                | WatchFlags::MOVED_TO
                | WatchFlags::MOVE_SELF;
            match inotify::add_watch(&self.fd, path, flags) {
                Ok(wd) => {
                    self.dirs.insert(wd, dir);
                    Ok(true)
                }
                // A parent watch covers paths that are absent or temporarily
                // have the wrong type. The Vault loader diagnoses invalid paths.
                Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => Ok(false),
                Err(error) => Err(error).with_context(|| format!("watching {}", path.display())),
            }
        }

        fn drain(&self) -> Result<(bool, bool)> {
            let mut buffer = [MaybeUninit::uninit(); 8192];
            let mut reader = inotify::Reader::new(&self.fd, &mut buffer);
            let mut dirty = false;
            let mut rebuild = false;
            for _ in 0..4096 {
                let event = match reader.next() {
                    Ok(event) => event,
                    Err(Errno::AGAIN) => break,
                    Err(Errno::INTR) => continue,
                    Err(error) => return Err(error).context("reading filesystem notifications"),
                };
                let flags = event.events();
                if flags.intersects(
                    ReadFlags::QUEUE_OVERFLOW
                        | ReadFlags::IGNORED
                        | ReadFlags::DELETE_SELF
                        | ReadFlags::MOVE_SELF
                        | ReadFlags::UNMOUNT,
                ) {
                    dirty = true;
                    rebuild = true;
                } else if let Some((dir, name)) = self.dirs.get(&event.wd()).zip(event.file_name())
                {
                    let name = Path::new(OsStr::from_bytes(name.to_bytes()));
                    let (changed, topology) = dir.classify(name, flags);
                    dirty |= changed;
                    rebuild |= topology;
                }
            }
            // If the read budget was exhausted mid-buffer, conservatively
            // rescan in case a topology event is among the discarded records.
            if !reader.is_buffer_empty() {
                return Ok((true, true));
            }
            Ok((dirty, rebuild))
        }
    }
}
