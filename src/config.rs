//! embarch-ui's own config: just enough to build an `embarch_core_client::
//! CoreClient` — no project/build config, unlike embarch-api's own
//! `config.rs` (embarch-ui never builds/flashes anything itself).

use anyhow::{Context, Result};
use embarch_core_client::CoreConfig;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_core")]
    pub core: CoreConfig,
    /// Absent means no project is open *by default* — not that the tab is
    /// unavailable. The Study Designer tab is always present; "Open project"
    /// can pick a firmware repo at runtime, and this field is only the
    /// zero-click default for a single-repo bench (decision 14). It used to
    /// be the only way in — `None` left the whole tab dead, every route
    /// answering `404` including the one that could have opened a project —
    /// resolved via `AskUserQuestion` against guessing which firmware repo
    /// was meant, matching how `embarch-api`'s own
    /// `[dev_bench].source_path` already works.
    #[serde(default)]
    pub study_designer: Option<StudyDesignerConfig>,
    /// Where `embarch-api`'s project config is, for the Study Designer's
    /// build toggle (decision 11, reversed).
    ///
    /// Absent is the ordinary case: `EMBARCH_API_CONFIG` is checked next,
    /// and a bench that has configured `embarch-api` at all has already
    /// answered this. The field exists for the bench where this UI runs
    /// without that variable set — not so a second list of projects can be
    /// written here, which is why it is a *path to that file* rather than a
    /// projects table of its own.
    #[serde(default)]
    pub build: Option<BuildConfig>,
    /// Tab folders this UI adds to its page (`tabs.rs`). Empty is the ordinary
    /// case, and the page is then the built-in one.
    #[serde(default)]
    pub tabs: Vec<TabConfig>,
}

/// One `[[tabs]]` entry: the tab's id, and the folder holding its `tab.toml`
/// and files. The folder is absolute or starts with `~/`.
#[derive(Debug, Clone, Deserialize)]
pub struct TabConfig {
    pub id: String,
    pub folder: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildConfig {
    pub config_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StudyDesignerConfig {
    /// The checked-out firmware repo whose `embarch/study-actions.toml`
    /// (registry) this tab reads/writes, and whose source tree the static
    /// GATT extractor reads (`embarch-study-designer` decision 33).
    ///
    /// The extractor itself is **not** configured: the build ships exactly
    /// one and it runs against whatever repo is open. A `static_extractor`
    /// key left over in an older config file is ignored rather than refused.
    pub firmware_repo_path: PathBuf,
}

/// Zero-config default: `base_url = "auto"`, the same zero-config ethos
/// `embarch-api`'s own `base_url = "auto"` follows (`embarch-api` decision
/// 11) — no config file needed to find a Core already running on this
/// machine. Every other `CoreConfig` field has its own `#[serde(default)]`
/// (timeouts, token discovery falling through to the machine-wide token
/// file), so this one-line TOML snippet is enough to fill the whole struct.
fn default_core() -> CoreConfig {
    toml::from_str("base_url = \"auto\"\n").expect("embedded default CoreConfig TOML is valid")
}

impl Default for Config {
    fn default() -> Self {
        Config { core: default_core(), study_designer: None, build: None, tabs: Vec::new() }
    }
}

/// Where the config is looked for when `EMBARCH_UI_CONFIG` is unset: the user's
/// config directory, `embarch-ui/config.toml` under it (decision 102).
/// `XDG_CONFIG_HOME`, else `~/.config`; on Windows, `%APPDATA%`.
///
/// Pure over its inputs so a test can pass them. `None` when the platform
/// gives no home at all, which is then the zero-config case.
fn default_path_from(xdg: Option<PathBuf>, home: Option<PathBuf>, appdata: Option<PathBuf>) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        appdata.or_else(|| home.map(|h| h.join("AppData").join("Roaming")))?
    } else {
        xdg.or_else(|| home.map(|h| h.join(".config")))?
    };
    Some(base.join("embarch-ui").join("config.toml"))
}

fn non_empty(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn default_path() -> Option<PathBuf> {
    let home = non_empty(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    default_path_from(non_empty("XDG_CONFIG_HOME"), home, non_empty("APPDATA"))
}

/// The file `load()` reads, or `None` for the zero-config default: the path
/// `EMBARCH_UI_CONFIG` names (whether or not it exists, so a typo is an error),
/// else [`default_path`] when a file is there.
pub fn resolve() -> Option<PathBuf> {
    resolve_with(non_empty("EMBARCH_UI_CONFIG"), default_path())
}

fn resolve_with(explicit: Option<PathBuf>, default: Option<PathBuf>) -> Option<PathBuf> {
    explicit.or_else(|| default.filter(|p| p.is_file()))
}

/// The config is a TOML file with a `[core]` table — identical schema to
/// `embarch-api`'s own `[core]`, since both are the same
/// `embarch_core_client::CoreConfig` (decision 5). Its path is
/// `EMBARCH_UI_CONFIG` if set, which the service unit carries from the shell
/// that installed it (decision 101), else the default path; with neither the
/// variable nor the file, the zero-config default above is used.
pub fn load() -> Result<Config> {
    match resolve() {
        Some(path) => {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read config file at {}", path.display()))?;
            toml::from_str(&raw)
                .with_context(|| format!("failed to parse config file at {}", path.display()))
        }
        None => Ok(Config::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(windows))]
    fn the_default_path_is_under_the_config_directory() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(default_path_from(p("/x"), p("/h"), None), p("/x/embarch-ui/config.toml"));
        assert_eq!(default_path_from(None, p("/h"), None), p("/h/.config/embarch-ui/config.toml"));
        assert_eq!(default_path_from(None, None, None), None);
    }

    #[test]
    fn an_unset_variable_finds_the_default_file_and_a_missing_one_is_zero_config() {
        let dir = std::env::temp_dir().join(format!("embarch-ui-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        let _ = std::fs::remove_file(&file);
        assert_eq!(resolve_with(None, Some(file.clone())), None, "no file: zero-config");
        assert_eq!(resolve_with(None, None), None);
        std::fs::write(&file, "").unwrap();
        assert_eq!(resolve_with(None, Some(file.clone())), Some(file.clone()));
        let named = PathBuf::from("/nowhere/c.toml");
        assert_eq!(resolve_with(Some(named.clone()), Some(file)), Some(named), "the variable wins, even unread");
        std::fs::remove_dir_all(&dir).ok();
    }
}
