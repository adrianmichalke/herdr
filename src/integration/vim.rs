use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::config_file::write_config;
#[cfg(not(windows))]
use super::env::config_dir_from_env_or_home;
use super::env::home_dir;
use super::registry::parse_integration_version;
use super::{ExperimentalIntegrationStatus, IntegrationStatusKind};

const ASSET: &str = include_str!("assets/vim/herdr.vim");
const MARKER: &str = "\" HERDR_INTEGRATION_ID=vim";
const VERSION: u32 = 2;
const PACKAGE_PATH: &str = "pack/herdr/start/herdr/plugin/herdr.vim";

pub(crate) fn vim_process_status(pid: u32) -> &'static str {
    if pid == 0 || pid > i32::MAX as u32 {
        "unknown"
    } else if crate::platform::process_exists(pid) {
        "running"
    } else if cfg!(unix) {
        "exited"
    } else {
        "unknown"
    }
}

#[cfg(unix)]
pub(crate) fn vim_commit_swap(
    original: &Path,
    archive: &Path,
    pid: u32,
) -> io::Result<&'static str> {
    use std::io::Read;
    if pid > i32::MAX as u32 {
        return Ok("unknown");
    }
    let mut source = match fs::File::open(original) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok("changed"),
        Err(error) => return Err(error),
    };
    match source.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Ok("busy"),
        Err(fs::TryLockError::Error(error)) => return Err(error),
    }
    if !crate::platform::user_file_matches_path(&source, original)? {
        return Ok("changed");
    }
    let mut backup = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(archive)?;
    if !crate::platform::user_file_matches_path(&backup, archive)?
        || crate::platform::user_file_matches_path(&source, archive)?
    {
        return Ok("unknown");
    }
    let mut observed = Vec::new();
    let mut preserved = Vec::new();
    source.read_to_end(&mut observed)?;
    backup.read_to_end(&mut preserved)?;
    if !observed.starts_with(b"b0VIM ") || observed != preserved {
        return Ok("changed");
    }
    if pid != 0 && vim_process_status(pid) != "exited" {
        return Ok("changed");
    }
    backup.sync_all()?;
    crate::platform::sync_user_file_parents(archive)?;
    if !crate::platform::user_file_matches_path(&source, original)?
        || !crate::platform::user_file_matches_path(&backup, archive)?
    {
        return Ok("changed");
    }
    fs::remove_file(original)?;
    Ok("archived")
}

#[cfg(not(unix))]
pub(crate) fn vim_commit_swap(
    _original: &Path,
    _archive: &Path,
    _pid: u32,
) -> io::Result<&'static str> {
    Ok("unknown")
}

fn plugin_paths() -> io::Result<Vec<(&'static str, PathBuf)>> {
    let home = home_dir()?;
    #[cfg(windows)]
    let vim = home.join("vimfiles");
    #[cfg(not(windows))]
    let vim = vim_config_dir(
        &home,
        config_dir_from_env_or_home("XDG_CONFIG_HOME", &[".config"])?,
    );
    let app = std::env::var_os("NVIM_APPNAME")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "nvim".into());
    let data = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty());
    #[cfg(windows)]
    let nvim = {
        let mut app = app;
        app.push("-data");
        PathBuf::from(
            data.or_else(|| std::env::var_os("LOCALAPPDATA"))
                .ok_or_else(|| {
                    io::Error::other("LOCALAPPDATA is not set; cannot locate Neovim data directory")
                })?,
        )
        .join(app)
    };
    #[cfg(not(windows))]
    let nvim = data
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join(app);

    Ok(vec![
        ("vim", vim.join(PACKAGE_PATH)),
        ("nvim", nvim.join("site").join(PACKAGE_PATH)),
    ])
}

#[cfg(not(windows))]
fn vim_config_dir(home: &Path, config: PathBuf) -> PathBuf {
    let legacy = home.join(".vim");
    let xdg = config.join("vim");
    if xdg.join("vimrc").is_file()
        && !home.join(".vimrc").is_file()
        && !legacy.join("vimrc").is_file()
    {
        xdg
    } else {
        legacy
    }
}

fn check_owned(path: &Path) -> io::Result<()> {
    match fs::read_to_string(path) {
        Ok(content) if content.lines().any(|line| line == MARKER) => Ok(()),
        Ok(_) => Err(io::Error::other(format!(
            "refusing to change non-Herdr Vim plugin at {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn install_at(paths: &[(&str, PathBuf)]) -> io::Result<Vec<String>> {
    for (_, path) in paths {
        check_owned(path)?;
    }
    paths
        .iter()
        .map(|(editor, path)| {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_config(path, ASSET)?;
            Ok(format!(
                "installed {editor} integration to {}",
                path.display()
            ))
        })
        .collect()
}

pub(crate) fn install_vim() -> io::Result<Vec<String>> {
    install_at(&plugin_paths()?)
}

fn uninstall_at(paths: &[(&str, PathBuf)]) -> io::Result<Vec<String>> {
    for (_, path) in paths {
        check_owned(path)?;
    }
    paths
        .iter()
        .map(|(editor, path)| {
            let action = if super::file_ops::remove_file_if_exists(path)? {
                "removed"
            } else {
                "no installed"
            };
            Ok(format!(
                "{action} {editor} integration at {}",
                path.display()
            ))
        })
        .collect()
}

pub(crate) fn uninstall_vim() -> io::Result<Vec<String>> {
    uninstall_at(&plugin_paths()?)
}

pub(crate) fn vim_integration_statuses() -> Vec<ExperimentalIntegrationStatus> {
    plugin_paths()
        .unwrap_or_default()
        .into_iter()
        .map(|(label, path)| {
            let installed_version = fs::read_to_string(&path)
                .ok()
                .and_then(|content| parse_integration_version(&content));
            let state = if !path.exists() {
                IntegrationStatusKind::NotInstalled
            } else if installed_version.is_some_and(|version| version >= VERSION) {
                IntegrationStatusKind::Current
            } else {
                IntegrationStatusKind::Outdated
            };
            ExperimentalIntegrationStatus {
                label,
                path,
                state,
                installed_version,
                expected_version: VERSION,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-vim-install-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn vim_process_status_preserves_live_and_unknown_processes() {
        assert_eq!(vim_process_status(std::process::id()), "running");
        assert_eq!(vim_process_status(0), "unknown");
        assert_eq!(vim_process_status(u32::MAX), "unknown");
    }

    #[cfg(unix)]
    #[test]
    fn vim_process_status_identifies_a_completed_process() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        assert_eq!(vim_process_status(pid), "exited");
    }

    #[cfg(unix)]
    #[test]
    fn vim_swap_commit_requires_a_durable_distinct_matching_backup() {
        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let original = dir.join("source.swp");
        let backup = dir.join("recovery.swp");
        let contents = b"b0VIM recovery bytes";
        fs::write(&original, contents).unwrap();
        fs::write(&backup, b"different bytes").unwrap();
        assert_eq!(vim_commit_swap(&original, &backup, 0).unwrap(), "changed");
        assert_eq!(fs::read(&original).unwrap(), contents);
        assert_eq!(vim_commit_swap(&original, &original, 0).unwrap(), "unknown");
        fs::remove_file(&backup).unwrap();
        fs::hard_link(&original, &backup).unwrap();
        assert_eq!(vim_commit_swap(&original, &backup, 0).unwrap(), "unknown");
        fs::remove_file(&backup).unwrap();
        fs::write(&backup, contents).unwrap();
        assert_eq!(
            vim_commit_swap(&original, &backup, std::process::id()).unwrap(),
            "changed"
        );
        assert_eq!(vim_commit_swap(&original, &backup, 0).unwrap(), "archived");
        assert!(!original.exists());
        assert_eq!(fs::read(&backup).unwrap(), contents);
        assert_eq!(vim_commit_swap(&original, &backup, 0).unwrap(), "changed");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn vim_swap_commit_never_removes_a_locked_or_replaced_file() {
        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let original = dir.join("source.swp");
        let backup = dir.join("recovery.swp");
        let contents = b"b0VIM original recovery";
        fs::write(&original, contents).unwrap();
        fs::write(&backup, contents).unwrap();
        let held = fs::File::open(&original).unwrap();
        held.try_lock().unwrap();
        assert_eq!(vim_commit_swap(&original, &backup, 0).unwrap(), "busy");
        assert_eq!(fs::read(&original).unwrap(), contents);
        fs::rename(&original, dir.join("previous.swp")).unwrap();
        fs::write(&original, b"b0VIM live replacement").unwrap();
        assert!(!crate::platform::user_file_matches_path(&held, &original).unwrap());
        assert_eq!(vim_commit_swap(&original, &backup, 0).unwrap(), "changed");
        assert_eq!(fs::read(&original).unwrap(), b"b0VIM live replacement");
        fs::remove_file(&original).unwrap();
        std::os::unix::fs::symlink(&backup, &original).unwrap();
        assert_eq!(vim_commit_swap(&original, &backup, 0).unwrap(), "changed");
        assert!(original
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn vim_package_path_follows_standard_xdg_configuration_selection() {
        let dir = test_dir();
        let config = dir.join("config");
        fs::create_dir_all(config.join("vim")).unwrap();
        assert_eq!(vim_config_dir(&dir, config.clone()), dir.join(".vim"));
        fs::write(config.join("vim/vimrc"), "set number\n").unwrap();
        assert_eq!(vim_config_dir(&dir, config.clone()), config.join("vim"));
        fs::write(dir.join(".vimrc"), "set number\n").unwrap();
        assert_eq!(vim_config_dir(&dir, config), dir.join(".vim"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn vim_install_and_uninstall_preserve_unrelated_files() {
        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let paths = vec![
            ("vim", dir.join("vim").join(PACKAGE_PATH)),
            ("nvim", dir.join("nvim").join(PACKAGE_PATH)),
        ];
        let unrelated = dir.join("vimrc");
        fs::write(&unrelated, "set number\n").unwrap();
        for _ in 0..2 {
            assert_eq!(install_at(&paths).unwrap().len(), 2);
            for (_, path) in &paths {
                let content = fs::read_to_string(path).unwrap();
                assert_eq!(content, ASSET);
                assert_eq!(parse_integration_version(&content), Some(VERSION));
            }
        }
        for _ in 0..2 {
            assert_eq!(uninstall_at(&paths).unwrap().len(), 2);
            assert!(paths.iter().all(|(_, path)| !path.exists()));
        }
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "set number\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn vim_install_and_uninstall_refuse_unowned_plugins() {
        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("herdr.vim");
        fs::write(&path, "let g:user_plugin = 1\n").unwrap();
        let paths = vec![("vim", path.clone())];
        assert!(install_at(&paths).is_err());
        assert!(uninstall_at(&paths).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "let g:user_plugin = 1\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
