//! Per-user directory locations.
//!
//! Why not read `HOME`:
//! Windows does not set `HOME`, so every subcommand that resolved its log path
//! there exited with "HOME 未设置". `etcetera`'s base strategy follows XDG on
//! Linux and macOS (`$XDG_CONFIG_HOME` or `~/.config`, `$XDG_STATE_HOME` or
//! `~/.local/state`) and the known folders on Windows (`%APPDATA%`,
//! `%LOCALAPPDATA%`). It takes the home directory from the OS, which on Windows
//! means the user profile.

use std::{env, path::PathBuf};

use anyhow::{Context as _, Result};
use etcetera::BaseStrategy;

/// Directory name used under the config and state roots.

const APP_DIR: &str = "iclass-buaa";

fn strategy() -> Result<impl BaseStrategy> {

    etcetera::choose_base_strategy().context("无法定位用户主目录")
}

/// The current user's home directory.
#[cfg_attr(windows, allow(dead_code))]

pub fn home_dir() -> Result<PathBuf> {

    Ok(strategy()?.home_dir().to_path_buf())
}

/// The user configuration root: `$XDG_CONFIG_HOME` or `~/.config` on Unix,
/// `%APPDATA%` on Windows.

pub fn config_home() -> Result<PathBuf> {

    Ok(strategy()?.config_dir())
}

/// This application's configuration directory.

pub fn config_dir() -> Result<PathBuf> {

    Ok(config_home()?.join(APP_DIR))
}

/// This application's state directory (logs, locks).

pub fn state_dir() -> Result<PathBuf> {

    Ok(state_home(&strategy()?).join(APP_DIR))
}

/// `$XDG_STATE_HOME` or `~/.local/state` on Unix.
///
/// Windows has no state directory. `%LOCALAPPDATA%` stands in for it: logs and
/// locks belong to one machine and should not roam with `%APPDATA%`.

fn state_home(strategy: &impl BaseStrategy) -> PathBuf {

    strategy.state_dir().unwrap_or_else(|| strategy.cache_dir())
}

/// Locations to search for a config file, most preferred first.
///
/// The platform directory comes first. The XDG and `$HOME/.config` locations
/// follow it; on Unix they collapse into the platform directory, and on Windows
/// they still find a config placed there while the tool required `HOME`.
/// System-wide locations come last, on Unix only.

pub fn config_file_candidates(file: &str) -> Vec<PathBuf> {

    let strategy = strategy().ok();

    let native = strategy.as_ref().map(BaseStrategy::config_dir);

    let legacy = [
        env_path("XDG_CONFIG_HOME"),
        env_path("HOME").map(|home| home.join(".config")),
        strategy.as_ref().map(|s| s.home_dir().join(".config")),
    ];

    search_order(native, legacy, system_config_dirs(), file)
}

fn search_order(
    native: Option<PathBuf>,
    legacy: [Option<PathBuf>; 3],
    system: Vec<PathBuf>,
    file: &str,
) -> Vec<PathBuf> {

    let mut paths = Vec::new();

    let roots = std::iter::once(native)
        .chain(legacy)
        .flatten()
        .chain(system);

    for root in roots {

        let path = root.join(APP_DIR).join(file);

        if !paths.contains(&path) {

            paths.push(path);
        }
    }

    paths
}

/// An environment variable as a path, treating an empty value as unset.

fn env_path(name: &str) -> Option<PathBuf> {

    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_CONFIG_DIRS` (default `/etc/xdg`), then `/etc`.
#[cfg(unix)]

fn system_config_dirs() -> Vec<PathBuf> {

    let raw = env::var_os("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into());

    raw.to_string_lossy()
        .split(':')
        .filter(|segment| !segment.trim().is_empty())
        .map(PathBuf::from)
        .chain([PathBuf::from("/etc")])
        .collect()
}

/// Windows has no system-wide config location for this tool.
#[cfg(not(unix))]

fn system_config_dirs() -> Vec<PathBuf> {

    Vec::new()
}

#[cfg(test)]

mod tests {

    use std::path::Path;

    use super::*;

    #[test]

    fn windows_searches_appdata_before_the_xdg_locations() {

        let paths = search_order(
            Some(PathBuf::from(r"C:\Users\me\AppData\Roaming")),
            [
                None,
                Some(PathBuf::from(r"D:\home\.config")),
                Some(PathBuf::from(r"C:\Users\me\.config")),
            ],
            Vec::new(),
            "config.toml",
        );

        let expected = [
            r"C:\Users\me\AppData\Roaming",
            r"D:\home\.config",
            r"C:\Users\me\.config",
        ]
        .map(|root| Path::new(root).join("iclass-buaa").join("config.toml"));

        assert_eq!(paths, expected);
    }

    #[test]

    fn unix_lists_the_home_config_once_then_system_locations() {

        let home = PathBuf::from("/home/me/.config");

        let paths = search_order(
            Some(home.clone()),
            [Some(home.clone()), Some(home.clone()), Some(home)],
            vec![PathBuf::from("/etc/xdg"), PathBuf::from("/etc")],
            "config.toml",
        );

        assert_eq!(
            paths,
            [
                "/home/me/.config/iclass-buaa/config.toml",
                "/etc/xdg/iclass-buaa/config.toml",
                "/etc/iclass-buaa/config.toml",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]

    fn without_a_home_the_system_locations_remain() {

        let paths = search_order(
            None,
            [None, None, None],
            vec![PathBuf::from("/etc")],
            "config.toml",
        );

        assert_eq!(paths, [PathBuf::from("/etc/iclass-buaa/config.toml")]);
    }

    /// A platform without a state directory, as Windows is.

    struct NoStateDir;

    impl BaseStrategy for NoStateDir {
        fn home_dir(&self) -> &Path {

            Path::new("/home/me")
        }

        fn config_dir(&self) -> PathBuf {

            PathBuf::from("/home/me/roaming")
        }

        fn data_dir(&self) -> PathBuf {

            PathBuf::from("/home/me/roaming")
        }

        fn cache_dir(&self) -> PathBuf {

            PathBuf::from("/home/me/local")
        }

        fn state_dir(&self) -> Option<PathBuf> {

            None
        }

        fn runtime_dir(&self) -> Option<PathBuf> {

            None
        }
    }

    #[test]

    fn state_lives_in_the_local_directory_when_there_is_no_state_dir() {

        assert_eq!(state_home(&NoStateDir), PathBuf::from("/home/me/local"));
    }

    #[test]

    fn the_current_platform_resolves_every_directory() {

        // Holds on Windows too, where the original code failed for lack of
        // `HOME`: the strategy reads the user profile instead.
        assert!(home_dir().is_ok());

        assert!(config_dir().unwrap().ends_with(APP_DIR));

        assert!(state_dir().unwrap().ends_with(APP_DIR));
    }
}
