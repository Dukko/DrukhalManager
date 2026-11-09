//! User preferences, stored as JSON in the config dir. The file holds API
//! keys, so it is created readable by the user only.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::merge::ai::{OPENROUTER_URL, Provider};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_provider")]
    pub provider: Provider,
    /// Conflicts the user chose to ignore, as keys from `conflicts_view`.
    #[serde(default)]
    pub ignored: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub game: GameOptions,
    #[serde(default)]
    pub updates: UpdateOptions,
}

/// How the game is found and launched, as in Irony Mod Manager's Game
/// Options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameOptions {
    /// Replaces the executable found in the Steam libraries.
    pub executable: Option<PathBuf>,
    /// Replaces `~/.local/share/Paradox Interactive/Stellaris`.
    pub data_dir: Option<PathBuf>,
    /// Added to the arguments from `launcher-settings.json`.
    pub launch_args: String,
    /// Fix descriptors whose paths went stale before launching.
    pub refresh_descriptors: bool,
    pub close_after_launch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateOptions {
    /// On by default only in the Flatpak, where Flatpak itself is asked;
    /// native packages get updates from the package manager.
    pub check_at_startup: bool,
    /// A release the user chose not to be told about again.
    pub skipped_version: Option<String>,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self { check_at_startup: crate::sandbox::is_flatpak(), skipped_version: None }
    }
}

fn default_provider() -> Provider {
    Provider::Anthropic { api_key: String::new() }
}

impl Default for Settings {
    fn default() -> Self {
        Self { provider: default_provider(), ignored: Default::default(), game: Default::default(), updates: Default::default() }
    }
}

pub fn path() -> PathBuf {
    crate::sandbox::config_dir().join("DrukhalManager/settings.json")
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|src| serde_json::from_str(&src).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(self).map_err(io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// The provider to use, with an empty Anthropic key filled in from
    /// `ANTHROPIC_API_KEY`. `None` if it isn't usable yet.
    pub fn ready_provider(&self) -> Option<Provider> {
        match &self.provider {
            Provider::Anthropic { api_key } if api_key.is_empty() => {
                std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty()).map(|api_key| Provider::Anthropic { api_key })
            }
            Provider::OpenAiCompatible { base_url, model, .. } if base_url.trim().is_empty() || model.trim().is_empty() => None,
            p => Some(p.clone()),
        }
    }
}

/// Defaults for a freshly chosen OpenAI-compatible provider.
pub fn openrouter() -> Provider {
    Provider::OpenAiCompatible { base_url: OPENROUTER_URL.into(), model: String::new(), api_key: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_privately_and_loads() {
        let path = std::env::temp_dir().join(format!("drukhal-settings-{}/settings.json", std::process::id()));
        assert_eq!(Settings::load(&path), Settings::default());
        let s = Settings { provider: Provider::OpenAiCompatible { base_url: "http://localhost:11434/v1".into(), model: "qwen".into(), api_key: String::new() }, ignored: ["file:x".to_owned()].into(), game: GameOptions { launch_args: "-debug".into(), close_after_launch: true, ..Default::default() }, updates: UpdateOptions { check_at_startup: true, skipped_version: Some("0.2.0".into()) } };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert!(s.ready_provider().is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(Settings { provider: openrouter(), ..Default::default() }.ready_provider().is_none(), "needs a model");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn reads_settings_from_before_game_options() {
        let old = r#"{"provider":{"kind":"anthropic","api_key":"k"},"ignored":[]}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.game, GameOptions::default());
        assert_eq!(s.updates, UpdateOptions::default());
    }
}
