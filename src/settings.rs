//! User preferences, stored as JSON in the config dir. API keys go in the
//! system keyring (see [`crate::secrets`]); only without one are they kept
//! in the file, which is created readable by the user only.

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
    /// Rules that hide conflicts by path; see `merge::ignore_rules`.
    #[serde(default)]
    pub ignore_rules: String,
    /// How many objects Merge All merges at once, if not the provider's
    /// default.
    #[serde(default)]
    pub parallel_merges: Option<u32>,
    /// Shows conflicts between a mod and the mods it depends on, which are
    /// hidden by default since patches and submods are meant to override.
    #[serde(default)]
    pub show_dependency_conflicts: bool,
    #[serde(default)]
    pub game: GameOptions,
    #[serde(default)]
    pub updates: UpdateOptions,
    /// Smart Sort sections the user chose for mods, by descriptor id; see
    /// `mods::smart_sort::Category::key`.
    #[serde(default)]
    pub sort_categories: std::collections::BTreeMap<String, String>,
    /// Names the user gave mods, by descriptor id. They're only shown here:
    /// descriptors and exports keep the real name.
    #[serde(default)]
    pub aliases: std::collections::BTreeMap<String, String>,
    /// Notes the user wrote on mods, by descriptor id, such as why a mod
    /// sits where it does.
    #[serde(default)]
    pub notes: std::collections::BTreeMap<String, String>,
    /// How the Disabled list is ordered.
    #[serde(default)]
    pub disabled_order: DisabledOrder,
    /// The provider's API key lives in the keyring, so the file leaves it
    /// out.
    #[serde(skip)]
    pub key_in_keyring: bool,
    /// A Steam Web API key typed in Preferences, to store in the keyring;
    /// `None` when it wasn't changed. Never written to the file.
    #[serde(skip)]
    pub new_steam_key: Option<String>,
    /// The Steam Web API key, kept here only while there's no keyring to
    /// hold it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub steam_key: String,
    /// Workshop items the user said aren't really required, by id.
    #[serde(default)]
    pub ignored_requirements: std::collections::BTreeSet<u64>,
    /// Workshop items the user dismissed from the suggested compatibility
    /// patches, by id.
    #[serde(default)]
    pub dismissed_patches: std::collections::BTreeSet<u64>,
    /// Suggested patches not updated in this many weeks are hidden.
    #[serde(default = "default_patch_weeks")]
    pub patch_weeks: u32,
    /// The game version seen last time, to notice game updates.
    #[serde(default)]
    pub last_game_version: Option<String>,
    /// The external merge tool's command, with `{left}`, `{right}`,
    /// `{base}` and `{output}`; empty for none.
    #[serde(default)]
    pub merge_tool: String,
    /// Hides the Smart Sort category headers in the load order.
    #[serde(default)]
    pub hide_category_headers: bool,
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
    /// The localisation languages the conflict scan reads, as the game's
    /// files name them; `None` for the game's own language.
    pub scan_languages: Option<Vec<String>>,
    /// Leaves localisation out of the conflict scan altogether.
    pub skip_localisation: bool,
    /// The name Merge into One Mod gives the merged mod, with `{Name}` for
    /// the collection's; `None` for `collection::DEFAULT_TEMPLATE`.
    pub merged_name: Option<String>,
    /// Merge into One Mod writes one zip instead of a folder.
    pub merge_to_zip: bool,
    /// Another folder to load mods from.
    pub extra_mod_dir: Option<PathBuf>,
    /// Merge into One Mod writes into `extra_mod_dir` instead of the game's
    /// mod folder.
    pub merge_to_extra: bool,
}

impl GameOptions {
    /// Where Merge into One Mod writes the merged mod.
    pub fn merge_dir(&self, user_dir: &Path) -> PathBuf {
        match &self.extra_mod_dir {
            Some(dir) if self.merge_to_extra => dir.clone(),
            _ => user_dir.join("mod"),
        }
    }
}

impl GameOptions {
    /// The languages the conflict scan reads: empty when localisation is
    /// skipped, else the chosen ones or the game's language.
    pub fn languages(&self, user_dir: &Path) -> std::collections::BTreeSet<String> {
        if self.skip_localisation {
            return Default::default();
        }
        match &self.scan_languages {
            Some(chosen) => chosen.iter().cloned().collect(),
            None => [game_language(user_dir)].into(),
        }
    }
}

/// The language the game is set to, from its `settings.txt`
/// (`language="l_english"`), or English.
pub fn game_language(user_dir: &Path) -> String {
    std::fs::read_to_string(user_dir.join("settings.txt"))
        .ok()
        .and_then(|src| {
            src.lines().find_map(|line| {
                let value = line.trim().strip_prefix("language")?.trim_start().strip_prefix('=')?;
                let value = value.trim().trim_matches('"').strip_prefix("l_")?;
                crate::merge::conflicts::LANGUAGES.iter().find(|(id, _)| *id == value).map(|(id, _)| id.to_string())
            })
        })
        .unwrap_or_else(|| "english".to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateOptions {
    /// On by default only in the Flatpak, where Flatpak itself is asked;
    /// native packages get updates from the package manager.
    pub check_at_startup: bool,
    /// A release the user chose not to be told about again.
    pub skipped_version: Option<String>,
    /// Also offers test versions (betas and release candidates).
    #[serde(default)]
    pub prereleases: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self { check_at_startup: crate::sandbox::is_flatpak(), skipped_version: None, prereleases: false }
    }
}

/// The most merges Merge All runs at once.
pub const MAX_PARALLEL: u32 = 8;

/// Merges at once unless chosen: one for Claude, whose rate limits are
/// tight for long requests, a few for other providers.
pub fn default_parallel(provider: &Provider) -> u32 {
    match provider {
        Provider::Anthropic { .. } => 1,
        Provider::OpenAiCompatible { .. } => 4,
    }
}

fn default_patch_weeks() -> u32 {
    52
}

fn default_provider() -> Provider {
    Provider::claude("")
}

impl Default for Settings {
    fn default() -> Self {
        Self { provider: default_provider(), ignored: Default::default(), ignore_rules: String::new(), parallel_merges: None, show_dependency_conflicts: false, game: Default::default(), updates: Default::default(), sort_categories: Default::default(), aliases: Default::default(), notes: Default::default(), disabled_order: Default::default(), key_in_keyring: false, new_steam_key: None, steam_key: String::new(), ignored_requirements: Default::default(), dismissed_patches: Default::default(), patch_weeks: default_patch_weeks(), last_game_version: None, merge_tool: String::new(), hide_category_headers: false }
    }
}

/// The order of the Disabled list, where order doesn't affect the game.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisabledOrder {
    #[default]
    Name,
    RecentlyUpdated,
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
        let mut on_disk = self.clone();
        if self.key_in_keyring {
            *api_key_mut(&mut on_disk.provider) = String::new();
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&on_disk).map_err(io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// Brings in the keyring: a key still in the file (from before the
    /// keyring was used, or saved while there was none) moves there, and an
    /// empty one is filled in from it. Blocks; call off the main thread.
    pub fn with_keyring(mut self, path: &Path) -> Self {
        let kind = crate::secrets::kind(&self.provider);
        let key = api_key_mut(&mut self.provider);
        if !key.is_empty() {
            if crate::secrets::set(kind, key).is_ok() {
                self.key_in_keyring = true;
                let _ = self.save(path);
            }
        } else if let Ok(found) = crate::secrets::get(kind) {
            // Reachable: keys saved from now on go there.
            self.key_in_keyring = true;
            if let Some(found) = found {
                *key = found;
            }
        }
        // A Steam key saved while there was no keyring moves there too.
        if !self.steam_key.is_empty() && crate::secrets::set(crate::secrets::STEAM, &self.steam_key).is_ok() {
            self.steam_key.clear();
            let _ = self.save(path);
        }
        self
    }

    /// The provider to use, with an empty Anthropic key filled in from
    /// `ANTHROPIC_API_KEY`. `None` if it isn't usable yet.
    /// How many objects Merge All merges at once.
    pub fn merges_at_once(&self) -> usize {
        self.parallel_merges.unwrap_or_else(|| default_parallel(&self.provider)).clamp(1, MAX_PARALLEL) as usize
    }

    pub fn ready_provider(&self) -> Option<Provider> {
        match &self.provider {
            Provider::Anthropic { api_key, model } if api_key.is_empty() => {
                std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty()).map(|api_key| Provider::Anthropic { api_key, model: model.clone() })
            }
            Provider::OpenAiCompatible { base_url, model, .. } if base_url.trim().is_empty() || model.trim().is_empty() => None,
            p => Some(p.clone()),
        }
    }
}

pub fn api_key_mut(provider: &mut Provider) -> &mut String {
    match provider {
        Provider::Anthropic { api_key, .. } | Provider::OpenAiCompatible { api_key, .. } => api_key,
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
        let s = Settings { provider: Provider::OpenAiCompatible { base_url: "http://localhost:11434/v1".into(), model: "qwen".into(), api_key: String::new() }, ignored: ["file:x".to_owned()].into(), ignore_rules: "gfx/**".into(), parallel_merges: Some(3), show_dependency_conflicts: true, game: GameOptions { launch_args: "-debug".into(), close_after_launch: true, ..Default::default() }, updates: UpdateOptions { check_at_startup: true, skipped_version: Some("0.2.0".into()), prereleases: true }, sort_categories: [("mod/ugc_1.mod".to_owned(), "ui".to_owned())].into(), aliases: [("mod/ugc_2.mod".to_owned(), "UIOD".to_owned())].into(), notes: [("mod/ugc_2.mod".to_owned(), "Keep below Giga".to_owned())].into(), disabled_order: DisabledOrder::RecentlyUpdated, key_in_keyring: false, new_steam_key: None, steam_key: String::new(), ignored_requirements: Default::default(), dismissed_patches: Default::default(), patch_weeks: default_patch_weeks(), last_game_version: None, merge_tool: "meld {left} {right}".into(), hide_category_headers: true };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert!(s.ready_provider().is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(Settings { provider: openrouter(), ..Default::default() }.ready_provider().is_none(), "needs a model");

        // With the key in the keyring, the file leaves it out.
        let mut with_key = Settings { provider: Provider::claude("sk-secret"), ..Default::default() };
        with_key.save(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("sk-secret"), "no keyring: kept in the private file");
        with_key.key_in_keyring = true;
        with_key.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("sk-secret"));
        assert_eq!(with_key.provider, Provider::claude("sk-secret"), "still in memory");

        // The Steam key is in the file only while no keyring holds it.
        assert!(!std::fs::read_to_string(&path).unwrap().contains("steam_key"));
        let with_steam = Settings { steam_key: "STEAMKEY".into(), ..Default::default() };
        with_steam.save(&path).unwrap();
        assert_eq!(Settings::load(&path).steam_key, "STEAMKEY");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn reads_settings_from_before_game_options() {
        let old = r#"{"provider":{"kind":"anthropic","api_key":"k"},"ignored":[]}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.game, GameOptions::default());
        assert_eq!(s.updates, UpdateOptions::default());
    }

    #[test]
    fn reads_the_game_language() {
        let dir = std::env::temp_dir().join(format!("dm-game-language-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(game_language(&dir), "english", "no settings.txt");
        std::fs::write(dir.join("settings.txt"), "gui_scale=1.0\nlanguage=\"l_german\"\n").unwrap();
        assert_eq!(game_language(&dir), "german");
        let options = GameOptions { scan_languages: Some(vec!["french".into(), "polish".into()]), ..Default::default() };
        assert_eq!(options.languages(&dir), ["french".to_owned(), "polish".to_owned()].into());
        assert_eq!(GameOptions::default().languages(&dir), ["german".to_owned()].into());
        assert!(GameOptions { skip_localisation: true, ..options }.languages(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
