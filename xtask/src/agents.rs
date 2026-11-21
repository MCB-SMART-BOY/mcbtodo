use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use mcbtodo_platform::WorkspacePaths;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_BYTES: u64 = 128 * 1024;
// Leave room below Linux's single-argument limit on hosts with 4 KiB pages.
const MAX_PROMPT_BYTES: usize = 120 * 1024;

#[derive(Subcommand)]
pub enum Task {
    /// Validate the complete registry without downloading remote sources.
    Validate,
    /// Select and fetch the needed Markdown files, then print the prepared context path.
    Prepare(Selection),
    /// Prepare the context and launch an installed agent with the supplied task.
    Run {
        #[command(flatten)]
        selection: Selection,
        /// Task to send to the selected agent.
        task: String,
    },
}

#[derive(Args)]
pub struct Selection {
    /// Select a harness explicitly or auto-detect from the session and PATH.
    #[arg(long, default_value = "auto")]
    harness: String,
    /// Workflow name from .agents/registry.toml.
    #[arg(long)]
    workflow: Option<String>,
    /// Use bundled files and verified cached downloads only.
    #[arg(long)]
    offline: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    instructions: Source,
    harnesses: BTreeMap<String, Harness>,
    workflows: BTreeMap<String, Source>,
    auto_order: Vec<String>,
    default_workflow: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Harness {
    document: Source,
    executable: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    session_vars: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Source {
    Local(Local),
    Remote(Remote),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Local {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remote {
    url: String,
    sha256: String,
}

pub fn run(root: &Path, command: Task) -> Result<()> {
    if matches!(command, Task::Validate) {
        validate(root)?;
        println!("agent registry valid");
        return Ok(());
    }
    let (agents, registry) = load_registry(root)?;
    let (selection, task) = match command {
        Task::Validate => unreachable!("validate command returned above"),
        Task::Prepare(selection) => (selection, None),
        Task::Run { selection, task } => (selection, Some(task)),
    };
    let (name, harness) = select_harness(
        &registry,
        &selection.harness,
        env::var("MCBTODO_HARNESS").ok().as_deref(),
        task.is_some(),
        |key| env::var_os(key).is_some_and(|value| !value.is_empty()),
        |name| executable(name).is_some(),
    )?;
    let env_workflow = env::var("MCBTODO_WORKFLOW")
        .ok()
        .filter(|name| !name.is_empty());
    let workflow_name = selection
        .workflow
        .as_deref()
        .or(env_workflow.as_deref())
        .unwrap_or(&registry.default_workflow);
    let workflow = registry
        .workflows
        .get(workflow_name)
        .with_context(|| format!("unknown agent workflow {workflow_name:?}"))?;
    let launch = task
        .map(|task| -> Result<_> {
            let command = harness.executable.as_deref().with_context(|| {
                format!("agent harness {name:?} has no executable. Use agent prepare or select an installed harness")
            })?;
            let program = executable(command)
                .with_context(|| format!("agent executable {command:?} was not found on PATH"))?;
            Ok((task, command, program))
        })
        .transpose()?;
    let paths = WorkspacePaths::discover()?;
    let cache = paths.prepare_cache_directory(Path::new("agents"))?;
    let entry = root.join("AGENTS.md");
    let root_instructions = String::from_utf8(read_small(&entry)?)
        .with_context(|| format!("{} must be UTF-8 Markdown", entry.display()))?;
    let context = [
        root_instructions,
        load(&agents, &paths, &registry.instructions, selection.offline)?,
        load(&agents, &paths, &harness.document, selection.offline)?,
        load(&agents, &paths, workflow, selection.offline)?,
    ]
    .join("\n\n");

    eprintln!("Agent harness {name} · workflow {workflow_name}");
    let prepare_context = || {
        let filename = format!("{}.md", digest(context.as_bytes()));
        let path = cache.join("context").join(&filename);
        paths.write_cache_file(
            Path::new("agents").join("context").join(filename).as_path(),
            context.as_bytes(),
        )?;
        Ok::<_, anyhow::Error>(path)
    };
    if let Some((task, command, executable)) = launch {
        let prompt = format!(
            "Follow the repository instructions, harness guide and workflow below.\n\n{context}\n\nTask:\n{task}"
        );
        let prompt = if prompt.len() > MAX_PROMPT_BYTES {
            let path = prepare_context()?;
            format!(
                "Read and follow the repository instructions, harness guide and workflow in {}.\n\nTask:\n{task}",
                path.display()
            )
        } else {
            prompt
        };
        let status = Command::new(executable)
            .current_dir(root)
            .env("MCBTODO_HARNESS", name)
            .env("MCBTODO_WORKFLOW", workflow_name)
            .args(&harness.args)
            .arg(prompt)
            .status()
            .with_context(|| format!("starting {command}"))?;
        ensure!(status.success(), "{command} exited with {status}");
    } else {
        println!("{}", prepare_context()?.display());
    }
    Ok(())
}

pub fn validate(root: &Path) -> Result<()> {
    let entry = root.join("AGENTS.md");
    let bytes = read_small(&entry)
        .with_context(|| format!("validating repository entrypoint {}", entry.display()))?;
    std::str::from_utf8(&bytes)
        .with_context(|| format!("{} must be UTF-8 Markdown", entry.display()))?;
    let (agents, registry) = load_registry(root)?;
    ensure!(
        registry.workflows.contains_key(&registry.default_workflow),
        "default workflow {:?} is not registered",
        registry.default_workflow
    );
    ensure!(
        registry.harnesses.contains_key("generic"),
        "agent registry must define the generic fallback harness"
    );

    let mut ordered = BTreeSet::new();
    for name in &registry.auto_order {
        ensure!(
            ordered.insert(name),
            "duplicate harness {name:?} in auto_order"
        );
        ensure!(
            registry.harnesses.contains_key(name),
            "auto_order references unregistered harness {name:?}"
        );
    }

    validate_source(&agents, &registry.instructions).context("validating common instructions")?;
    for (name, harness) in &registry.harnesses {
        validate_source(&agents, &harness.document)
            .with_context(|| format!("validating harness {name:?} document"))?;
    }
    for (name, source) in &registry.workflows {
        validate_source(&agents, source)
            .with_context(|| format!("validating workflow {name:?} source"))?;
    }
    validate_workflow_indexes(root, &agents, &registry)?;
    Ok(())
}

fn validate_workflow_indexes(root: &Path, agents: &Path, registry: &Registry) -> Result<()> {
    let agent_index = fs::read_to_string(agents.join("README.md"))
        .context("reading .agents/README.md workflow index")?;
    let docs_index = fs::read_to_string(root.join("docs/README.md"))
        .context("reading docs/README.md workflow index")?;

    for (name, source) in &registry.workflows {
        let label = format!("[`{name}`](");
        ensure!(
            agent_index.contains(&label),
            "registered workflow {name:?} is not linked from .agents/README.md"
        );
        ensure!(
            docs_index.contains(&label),
            "registered workflow {name:?} is not linked from docs/README.md"
        );
        if let Source::Local(local) = source {
            let path = local.path.to_string_lossy();
            ensure!(
                agent_index.contains(&format!("]({path})")),
                "workflow {name:?} has a stale or missing link in .agents/README.md"
            );
            ensure!(
                docs_index.contains(&format!("](../.agents/{path})")),
                "workflow {name:?} has a stale or missing link in docs/README.md"
            );
        }
    }
    Ok(())
}

fn load_registry(root: &Path) -> Result<(PathBuf, Registry)> {
    let agents = root.join(".agents").canonicalize()?;
    let registry: Registry = toml::from_str(&fs::read_to_string(agents.join("registry.toml"))?)
        .context("parsing .agents/registry.toml")?;
    Ok((agents, registry))
}

fn validate_source(agents: &Path, source: &Source) -> Result<()> {
    match source {
        Source::Local(local) => {
            ensure!(
                local
                    .path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                    && local.path.extension() == Some(OsStr::new("md")),
                "expected a relative Markdown path in .agents"
            );
            let path = agents
                .join(&local.path)
                .canonicalize()
                .with_context(|| format!("resolving local source {}", local.path.display()))?;
            ensure!(
                path.starts_with(agents),
                "agent source escapes .agents: {}",
                local.path.display()
            );
            let bytes = read_small(&path)?;
            std::str::from_utf8(&bytes).with_context(|| {
                format!("agent source {} must be UTF-8 Markdown", path.display())
            })?;
        }
        Source::Remote(remote) => validate_remote(remote)?,
    }
    Ok(())
}

fn validate_remote(remote: &Remote) -> Result<()> {
    let rest = remote
        .url
        .strip_prefix("https://")
        .context("agent source must use HTTPS")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    ensure!(
        !authority.is_empty()
            && !authority.contains('@')
            && !authority.chars().any(char::is_whitespace),
        "agent source must have a valid HTTPS host without credentials"
    );
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    ensure!(
        path.ends_with(".md"),
        "remote agent source must have a .md path"
    );
    ensure!(
        is_sha256(&remote.sha256),
        "expected a lowercase SHA-256 digest"
    );
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn select_harness<'a>(
    registry: &'a Registry,
    explicit: &str,
    env_override: Option<&str>,
    require_executable: bool,
    in_session: impl Fn(&str) -> bool,
    on_path: impl Fn(&str) -> bool,
) -> Result<(&'a str, &'a Harness)> {
    let lookup = |name: &str| {
        registry
            .harnesses
            .get_key_value(name)
            .map(|(name, harness)| (name.as_str(), harness))
            .with_context(|| format!("unknown agent harness {name:?}"))
    };
    if explicit != "auto" {
        return lookup(explicit);
    }
    if let Some(name) = env_override.filter(|name| !name.is_empty() && *name != "auto") {
        return lookup(name);
    }
    let ordered = registry
        .auto_order
        .iter()
        .map(|name| lookup(name))
        .collect::<Result<Vec<_>>>()?;
    let can_launch = |harness: &Harness| harness.executable.as_deref().is_some_and(&on_path);
    if let Some(&selected) = ordered.iter().find(|(_, harness)| {
        harness.session_vars.iter().any(|key| in_session(key))
            && (!require_executable || can_launch(harness))
    }) {
        return Ok(selected);
    }
    if let Some(&selected) = ordered.iter().find(|(_, harness)| can_launch(harness)) {
        return Ok(selected);
    }
    lookup("generic")
}

fn executable(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|file| {
            fs::metadata(file)
                .is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111) != 0)
        })
        .and_then(|file| {
            if file.is_absolute() {
                Some(file)
            } else {
                Some(env::current_dir().ok()?.join(file))
            }
        })
}

fn load(agents: &Path, paths: &WorkspacePaths, source: &Source, offline: bool) -> Result<String> {
    let content = match source {
        Source::Local(local) => {
            validate_source(agents, source)?;
            let path = agents.join(&local.path).canonicalize()?;
            read_small(&path)?
        }
        Source::Remote(remote) => {
            validate_remote(remote)?;
            load_remote(paths, remote, offline, download)?
        }
    };
    String::from_utf8(content).context("agent instructions must be UTF-8 Markdown")
}

fn load_remote(
    paths: &WorkspacePaths,
    remote: &Remote,
    offline: bool,
    download: impl FnOnce(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    validate_remote(remote)?;
    let expected = &remote.sha256;
    let relative = Path::new("agents")
        .join("downloads")
        .join(format!("{expected}.md"));
    let path = paths.cache_dir().join(&relative);
    let cached = paths.read_cache_file(&relative).and_then(|bytes| {
        ensure!(
            digest(&bytes) == *expected,
            "digest mismatch for cached agent source {}",
            path.display()
        );
        Ok(bytes)
    });
    match cached {
        Ok(bytes) => return Ok(bytes),
        Err(error) if offline => {
            return Err(error).context("agent source is not cached or its cache entry is invalid");
        }
        Err(_) => {}
    }
    ensure!(
        !offline,
        "agent source is not cached and offline mode is enabled: {}",
        remote.url
    );
    let bytes = download(&remote.url)?;
    ensure!(
        digest(&bytes) == *expected,
        "digest mismatch for agent source {}",
        remote.url
    );
    std::str::from_utf8(&bytes).context("downloaded agent instructions must be UTF-8 Markdown")?;
    paths.write_cache_file(&relative, &bytes)?;
    Ok(bytes)
}

fn download(url: &str) -> Result<Vec<u8>> {
    let file = tempfile::NamedTempFile::new()?;
    let status = Command::new("curl")
        .args([
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--globoff",
            "--location",
            "--max-redirs",
            "4",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "30",
            "--max-filesize",
        ])
        .arg(MAX_BYTES.to_string())
        .arg("--output")
        .arg(file.path())
        .arg("--url")
        .arg(url)
        .status()
        .context("running curl to fetch agent instructions")?;
    ensure!(
        status.success(),
        "curl exited with {status} while fetching {url}"
    );
    read_small(file.path())
}

fn read_small(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("not a file: {}", path.display());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "agent source is too large: {}",
        path.display()
    );
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_explicit_override_then_session_then_installed_or_generic() {
        let registry: Registry =
            toml::from_str(include_str!("../../.agents/registry.toml")).unwrap();
        let selected = |explicit, selection, active, installed| {
            select_harness(
                &registry,
                explicit,
                selection,
                false,
                |key| key == active,
                |program| program == installed,
            )
            .unwrap()
            .0
        };
        assert_eq!(
            selected("pi", Some("claude"), "CODEX_THREAD_ID", "codex"),
            "pi"
        );
        assert_eq!(
            selected("auto", Some("claude"), "CODEX_THREAD_ID", "codex"),
            "claude"
        );
        assert_eq!(selected("auto", None, "CLAUDECODE", "codex"), "claude");
        assert_eq!(selected("auto", None, "", "opencode"), "opencode");
        assert_eq!(selected("auto", None, "", ""), "generic");
    }

    #[test]
    fn auto_run_skips_active_harnesses_missing_from_path() {
        let registry: Registry =
            toml::from_str(include_str!("../../.agents/registry.toml")).unwrap();
        let selected = |explicit| {
            select_harness(
                &registry,
                explicit,
                None,
                true,
                |key| key == "CODEX_THREAD_ID" || key == "PI_CODING_AGENT",
                |program| program == "claude" || program == "pi",
            )
            .unwrap()
            .0
        };
        assert_eq!(selected("auto"), "pi");
        assert_eq!(selected("codex"), "codex");
    }

    #[test]
    fn pinned_remote_requires_hash_and_cached_bytes_are_rechecked_offline() {
        let temp = tempfile::tempdir().unwrap();
        let payload = b"# Workflow\n";
        let remote = Remote {
            url: "https://example.invalid/workflow.md".into(),
            sha256: digest(payload),
        };
        let paths = WorkspacePaths::from_home(temp.path().to_path_buf());
        assert!(load_remote(&paths, &remote, true, |_| panic!("offline")).is_err());
        assert!(load_remote(&paths, &remote, false, |_| Ok(b"unexpected".to_vec())).is_err());
        let bytes = load_remote(&paths, &remote, false, |url| {
            assert_eq!(url, remote.url);
            Ok(payload.to_vec())
        })
        .unwrap();
        assert_eq!(bytes, payload);
        assert_eq!(
            load_remote(&paths, &remote, true, |_| panic!("cached")).unwrap(),
            payload
        );
        for corrupt in [
            b"digest mismatch".to_vec(),
            vec![b'a'; MAX_BYTES as usize + 1],
        ] {
            paths
                .write_cache_file(
                    Path::new("agents")
                        .join("downloads")
                        .join(format!("{}.md", remote.sha256))
                        .as_path(),
                    &corrupt,
                )
                .unwrap();
            assert!(load_remote(&paths, &remote, true, |_| panic!("offline")).is_err());
            assert_eq!(
                load_remote(&paths, &remote, false, |_| Ok(payload.to_vec())).unwrap(),
                payload
            );
        }
        assert_eq!(
            load_remote(&paths, &remote, true, |_| panic!("cached")).unwrap(),
            payload
        );
    }
}
