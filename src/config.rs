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
    /// Absent means the Study Designer tab is unavailable — a clear "not
    /// configured" state, not a guess at which firmware repo was meant
    /// (`embarch-ui/milestone-1.md` §4.6, resolved via `AskUserQuestion`
    /// rather than picked silently: a config field, matching how
    /// `embarch-api`'s own `[dev_bench].source_path` already works).
    #[serde(default)]
    pub study_designer: Option<StudyDesignerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StudyDesignerConfig {
    /// The checked-out firmware repo whose `embarch/study-actions.toml`
    /// (registry) this tab reads/writes, and whose source tree a
    /// configured `static_extractor` runs against.
    pub firmware_repo_path: PathBuf,
    /// Name of a registered `GattConfigExtractor` to run for static GATT
    /// discovery (`embarch-study-designer/design.md` §3 decision 33) — e.g.
    /// `"zephyr-ble-def"`. Absent, static extraction is simply skipped (the
    /// merged action list still works from live discovery + the registry
    /// alone), matching `study-designer-ui`'s own opt-in `--static-extractor`
    /// precedent rather than guessing at an unrelated firmware's conventions.
    #[serde(default)]
    pub static_extractor: Option<String>,
}

/// Zero-config default: `base_url = "auto"`, the same zero-config ethos
/// `embarch-api`'s own `base_url = "auto"` follows (`embarch-api/design.md`
/// §3.11) — no config file needed to find a Core already running on this
/// machine. Every other `CoreConfig` field has its own `#[serde(default)]`
/// (timeouts, token discovery falling through to the machine-wide token
/// file), so this one-line TOML snippet is enough to fill the whole struct.
fn default_core() -> CoreConfig {
    toml::from_str("base_url = \"auto\"\n").expect("embedded default CoreConfig TOML is valid")
}

impl Default for Config {
    fn default() -> Self {
        Config { core: default_core(), study_designer: None }
    }
}

/// `EMBARCH_UI_CONFIG` names an optional TOML file with a `[core]` table —
/// identical schema to `embarch-api`'s own `[core]`, since both are the
/// same `embarch_core_client::CoreConfig` (embarch-ui/design.md §3 decision
/// 5). Absent, the zero-config default above is used — no file needed for
/// the common single-machine case.
pub fn load() -> Result<Config> {
    match std::env::var_os("EMBARCH_UI_CONFIG") {
        Some(path) => {
            let path = PathBuf::from(path);
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read config file at {}", path.display()))?;
            toml::from_str(&raw)
                .with_context(|| format!("failed to parse config file at {}", path.display()))
        }
        None => Ok(Config::default()),
    }
}
