use std::{env, fs, io, path::Path, sync::Barrier, thread};

use chrono_tz::{Asia::Shanghai, UTC};
use mcbtodo_platform::{AppConfig, HomeViewPolicy, WorkspacePaths, read_regular_file};
use tempfile::TempDir;

#[test]
fn paths_use_the_fixed_home_layout() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    assert_eq!(
        paths.config_path(),
        home.path().join(".config/mcbtodo/config.toml")
    );
    assert_eq!(paths.data_dir(), home.path().join(".mcbtodo"));
    assert_eq!(
        paths.keymap_path(),
        home.path().join(".config/mcbtodo/keymap.toml")
    );
    assert_eq!(
        paths.agent_policy_path(),
        home.path().join(".config/mcbtodo/agents.toml")
    );
    assert_eq!(paths.state_dir(), home.path().join(".mcbtodo/state"));
    assert_eq!(paths.vault_root(), home.path().join(".mcbtodo/vault"));
    assert_eq!(
        paths.socket_path(),
        home.path().join(".mcbtodo/state/mcbtodo-daemon.sock")
    );
    assert_eq!(paths.cache_dir(), home.path().join(".mcbtodo/cache"));
    assert_eq!(
        paths.last_view_path(),
        home.path().join(".mcbtodo/state/last-view")
    );
    assert_eq!(fs::read_dir(home.path())?.count(), 0);
    Ok(())
}

#[test]
fn discovery_uses_the_home_directory() -> anyhow::Result<()> {
    let home = env::home_dir().expect("test process has a home");
    assert_eq!(
        WorkspacePaths::discover()?.state_dir(),
        home.join(".mcbtodo/state")
    );
    Ok(())
}

#[test]
fn initialization_creates_the_default_config_and_empty_vault() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;

    assert_eq!(AppConfig::default().timezone, UTC);
    assert_eq!(AppConfig::default().home_view, HomeViewPolicy::Month);
    assert_eq!(paths.load_config()?.timezone, UTC);
    assert!(paths.state_dir().is_dir());
    assert_eq!(fs::read_dir(paths.vault_root())?.count(), 0);
    assert!(!paths.cache_dir().exists());
    assert!(!paths.socket_path().exists());
    Ok(())
}

#[test]
fn config_parses_the_home_view_policy_and_defaults_it_to_month() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;

    fs::write(paths.config_path(), "timezone = 'UTC'\n")?;
    assert_eq!(paths.load_config()?.home_view, HomeViewPolicy::Month);
    fs::write(
        paths.config_path(),
        "timezone = 'UTC'\nhome_view = 'restore_last_view'\n",
    )?;
    assert_eq!(
        paths.load_config()?.home_view,
        HomeViewPolicy::RestoreLastView
    );
    fs::write(
        paths.config_path(),
        "timezone = 'UTC'\nhome_view = 'inbox'\n",
    )?;
    assert!(paths.load_config().is_err());
    Ok(())
}

#[test]
fn state_file_is_private_bounded_atomic_and_confined_to_state_dir() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let state_path = paths.last_view_path();

    paths.write_state_file(&state_path, b"agenda")?;
    assert_eq!(paths.read_state_file(&state_path)?, "agenda");
    paths.write_state_file(&state_path, b"graph")?;
    assert_eq!(paths.read_state_file(&state_path)?, "graph");
    assert!(
        paths
            .write_state_file(&home.path().join(".mcbtodo/outside"), b"invalid")
            .is_err()
    );
    assert!(paths.read_state_file(&home.path().join("outside")).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn state_file_reader_rejects_symlinks_and_unsafe_permissions() -> anyhow::Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let home = TempDir::new()?;
    let outside = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let state_path = paths.last_view_path();
    paths.write_state_file(&state_path, b"month")?;

    fs::set_permissions(&state_path, fs::Permissions::from_mode(0o644))?;
    assert!(paths.read_state_file(&state_path).is_err());
    fs::remove_file(&state_path)?;
    symlink(outside.path().join("external"), &state_path)?;
    fs::write(outside.path().join("external"), "agenda")?;
    assert!(paths.read_state_file(&state_path).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn config_and_state_readers_reject_hard_linked_files() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;

    let config_link = home.path().join("config-copy.toml");
    fs::hard_link(paths.config_path(), &config_link)?;
    assert!(paths.read_config_file(&paths.config_path()).is_err());

    let state_path = paths.last_view_path();
    paths.write_state_file(&state_path, b"agenda")?;
    let state_link = home.path().join("state-copy");
    fs::hard_link(&state_path, &state_link)?;
    assert!(paths.read_state_file(&state_path).is_err());
    Ok(())
}

#[test]
fn initialization_preserves_the_existing_config() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    let edited = "# My timezone\n  timezone = 'Asia/Shanghai'\n";
    fs::write(paths.config_path(), edited)?;

    paths.initialize()?;

    assert_eq!(fs::read_to_string(paths.config_path())?, edited);
    assert_eq!(paths.load_config()?.timezone, Shanghai);
    Ok(())
}

#[test]
fn loading_a_missing_config_returns_not_found() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let error = paths.load_config().unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::NotFound
    );
    assert!(
        error
            .to_string()
            .contains(&*paths.config_path().to_string_lossy())
    );
    assert_eq!(fs::read_dir(home.path())?.count(), 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn config_loading_and_initialization_reject_symlinks() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let outside = TempDir::new()?;
    let outside_config = outside.path().join("config.toml");
    fs::write(&outside_config, "timezone = 'Asia/Shanghai'\n")?;

    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    fs::remove_file(paths.config_path())?;
    symlink(&outside_config, paths.config_path())?;

    assert!(paths.load_config().is_err());
    assert!(paths.initialize().is_err());
    assert_eq!(
        fs::read_to_string(outside_config)?,
        "timezone = 'Asia/Shanghai'\n"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn config_loading_revalidates_directory_chain_after_initialization() -> anyhow::Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let outside = TempDir::new()?;
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    fs::remove_dir_all(paths.config_path().parent().unwrap())?;
    symlink(outside.path(), paths.config_path().parent().unwrap())?;

    assert!(paths.load_config().is_err());
    assert_eq!(fs::read_dir(outside.path())?.count(), 0);

    fs::remove_file(paths.config_path().parent().unwrap())?;
    fs::create_dir(paths.config_path().parent().unwrap())?;
    fs::set_permissions(
        home.path().join(".config"),
        fs::Permissions::from_mode(0o777),
    )?;
    assert!(paths.load_config().is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn config_reader_rejects_group_writable_files_and_unrelated_paths() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    fs::set_permissions(paths.config_path(), fs::Permissions::from_mode(0o620))?;

    assert!(paths.read_config_file(&paths.config_path()).is_err());
    assert!(
        paths
            .read_config_file(&home.path().join("unrelated.toml"))
            .is_err()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn initialization_rejects_a_symlinked_config_parent_without_writing_through_it()
-> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let outside = TempDir::new()?;
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    symlink(outside.path(), home.path().join(".config"))?;

    assert!(paths.initialize().is_err());
    assert_eq!(fs::read_dir(outside.path())?.count(), 0);
    Ok(())
}

#[test]
fn config_file_reader_rejects_oversized_input() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let path = dir.path().join("config.toml");
    fs::write(&path, vec![b' '; 1024 * 1024 + 1])?;

    assert!(read_regular_file(&path).is_err());
    Ok(())
}

#[test]
fn config_rejects_missing_invalid_and_unknown_fields() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;

    for invalid in [
        "",
        "timezone = '+08:00'",
        "timezone = 'Mars/Olympus'",
        "timezone = 8",
        "timezone = 'UTC'\ntimezone = 'Asia/Shanghai'",
        "timezone = 'UTC'\nvault_path = '/tmp/example'",
    ] {
        fs::write(paths.config_path(), invalid)?;
        let error = paths.load_config().unwrap_err();
        assert!(
            error.downcast_ref::<toml::de::Error>().is_some(),
            "{invalid:?}: {error:#}"
        );
    }
    Ok(())
}

#[test]
fn concurrent_initialization_publishes_a_complete_config() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let start = Barrier::new(4);
    thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| -> anyhow::Result<()> {
                    start.wait();
                    paths.initialize()?;
                    assert_eq!(paths.load_config()?.timezone, UTC);
                    Ok(())
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("initializer thread panicked")?;
        }
        Ok::<(), anyhow::Error>(())
    })?;
    assert_eq!(
        fs::read_dir(paths.config_path().parent().unwrap())?.count(),
        1
    );
    Ok(())
}

#[test]
fn prepare_cache_only_creates_the_cache_hierarchy_and_is_repeatable() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let cache = paths.prepare_cache()?;
    assert_eq!(cache, paths.cache_dir());
    fs::write(cache.join("entry"), "existing cache data")?;

    assert_eq!(paths.prepare_cache()?, cache);
    assert_eq!(
        fs::read_to_string(cache.join("entry"))?,
        "existing cache data"
    );
    assert!(!home.path().join(".config").exists());
    assert!(!paths.vault_root().exists());
    assert!(!paths.state_dir().exists());
    assert!(!paths.socket_path().exists());
    Ok(())
}

#[test]
fn cache_file_helpers_support_files_at_the_cache_root() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.write_cache_file(Path::new("root-entry"), b"cached")?;

    assert_eq!(paths.read_cache_file(Path::new("root-entry"))?, b"cached");
    Ok(())
}

#[test]
fn prepare_cache_rejects_a_file_in_place_of_the_cache() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    fs::write(paths.cache_dir(), "unrelated data")?;

    assert!(paths.prepare_cache().is_err());
    assert_eq!(fs::read_to_string(paths.cache_dir())?, "unrelated data");
    Ok(())
}

#[cfg(unix)]
#[test]
fn prepare_cache_rejects_symlinks_at_either_directory() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let target = TempDir::new()?;

    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    symlink(target.path(), home.path().join(".mcbtodo"))?;
    assert!(paths.prepare_cache().is_err());
    assert_eq!(fs::read_dir(target.path())?.count(), 0);

    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.initialize()?;
    symlink(target.path(), paths.cache_dir())?;
    assert!(paths.prepare_cache().is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn initialize_rejects_a_symlinked_workspace_before_creating_children() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let target = TempDir::new()?;
    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    symlink(target.path(), home.path().join(".mcbtodo"))?;

    assert!(paths.initialize().is_err());
    assert_eq!(fs::read_dir(target.path())?.count(), 0);
    assert!(!home.path().join(".config").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn newly_created_workspace_paths_are_private() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let cache = paths.prepare_cache()?;
    paths.initialize()?;

    let config = paths.config_path();
    for dir in [
        config.parent().unwrap().to_path_buf(),
        home.path().join(".mcbtodo"),
        paths.state_dir(),
        paths.vault_root(),
        cache,
    ] {
        assert_eq!(fs::metadata(dir)?.permissions().mode() & 0o777, 0o700);
    }
    assert_eq!(fs::metadata(config)?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

#[cfg(unix)]
#[test]
fn prepare_cache_rejects_existing_nonprivate_directories() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let cache = paths.prepare_cache()?;
    for dir in [cache, home.path().join(".mcbtodo")] {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
        assert!(paths.prepare_cache().is_err());
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o755);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn cache_file_helpers_reject_nested_symlinks_hard_links_and_unsafe_modes() -> anyhow::Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let home = TempDir::new()?;
    let outside = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    let nested = Path::new("agents/downloads/pinned.md");
    paths.write_cache_file(nested, b"verified")?;
    assert_eq!(paths.read_cache_file(nested)?, b"verified");

    fs::remove_file(paths.cache_dir().join("agents/downloads/pinned.md"))?;
    fs::remove_dir(paths.cache_dir().join("agents/downloads"))?;
    symlink(outside.path(), paths.cache_dir().join("agents/downloads"))?;
    assert!(paths.read_cache_file(nested).is_err());
    assert!(paths.write_cache_file(nested, b"must not escape").is_err());
    assert_eq!(fs::read_dir(outside.path())?.count(), 0);

    fs::remove_file(paths.cache_dir().join("agents/downloads"))?;
    fs::create_dir_all(paths.cache_dir().join("agents/downloads"))?;
    let file = paths.cache_dir().join(nested);
    fs::write(&file, b"shared")?;
    fs::hard_link(&file, outside.path().join("linked.md"))?;
    assert!(paths.read_cache_file(nested).is_err());

    fs::remove_file(outside.path().join("linked.md"))?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644))?;
    assert!(paths.read_cache_file(nested).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn cache_file_helpers_reject_final_symlink_and_non_normal_paths() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let home = TempDir::new()?;
    let outside = TempDir::new()?;
    let paths = WorkspacePaths::from_home(home.path().to_path_buf());
    paths.prepare_cache_directory(Path::new("agents/context"))?;
    let link = paths.cache_dir().join("agents/context/current.md");
    symlink(outside.path().join("source.md"), &link)?;
    assert!(
        paths
            .read_cache_file(Path::new("agents/context/current.md"))
            .is_err()
    );
    assert!(
        paths
            .write_cache_file(Path::new("agents/context/current.md"), b"no")
            .is_err()
    );
    assert!(paths.read_cache_file(Path::new("../outside")).is_err());
    Ok(())
}
