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
const VERSION: u32 = 1;
const PACKAGE_PATH: &str = "pack/herdr/start/herdr/plugin/herdr.vim";

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
