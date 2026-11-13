//! Compatibility patches the load order probably needs: Workshop items
//! whose required items are all enabled, found by asking Steam what lists
//! each enabled mod as required. Patches declare the mods they patch that
//! way, so a patch for A and B shows up once both are enabled.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::Mod;
use super::requirements::NOT_MODS;
use crate::paradox::workshop::Dependent;

/// How long a mod's list of dependents is trusted before it's fetched
/// again.
const FRESH: Duration = Duration::from_secs(3 * 24 * 3600);

/// What lists each Workshop mod as required, as Steam said when asked.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    /// By Workshop id: when it was fetched (Unix seconds) and its dependents.
    items: HashMap<u64, (u64, Vec<Dependent>)>,
}

impl Cache {
    pub fn path() -> PathBuf {
        crate::platform::cache_dir().join("workshop-dependents.json")
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    /// Of `ids`, those whose dependents are unknown or old; all of them
    /// when `all`.
    pub fn stale(&self, ids: &[u64], all: bool) -> Vec<u64> {
        let now = now();
        let mut ids: Vec<u64> = ids.iter().copied().filter(|id| all || self.items.get(id).is_none_or(|(at, _)| now.saturating_sub(*at) > FRESH.as_secs())).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Records what Steam said about `id`, fetched just now.
    pub fn update(&mut self, id: u64, dependents: Vec<Dependent>) {
        self.items.insert(id, (now(), dependents));
    }
}

/// A Workshop item worth suggesting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub item: Dependent,
    /// The enabled mods it requires, by index into the mods.
    pub needs: Vec<usize>,
    /// Its name or tags say it's a patch; others are shown only on request.
    pub patch: bool,
}

impl Suggestion {
    /// Its author updated it in the last `weeks` weeks.
    pub fn updated_within(&self, weeks: u32) -> bool {
        now().saturating_sub(self.item.updated) <= u64::from(weeks) * 7 * 24 * 3600
    }
}

/// The enabled Workshop mods' ids, for asking Steam about.
pub fn enabled_ids(mods: &[Mod], enabled: &[usize]) -> Vec<u64> {
    enabled.iter().filter_map(|&i| mods.get(i)?.descriptor.remote_file_id).collect()
}

/// The items the cache knows to depend on enabled mods whose required
/// items are all enabled, that aren't installed, dismissed or a
/// translation: patches first, then by subscribers.
pub fn suggest(cache: &Cache, mods: &[Mod], enabled: &[usize], dismissed: &BTreeSet<u64>) -> Vec<Suggestion> {
    let on: HashMap<u64, usize> = enabled.iter().filter_map(|&i| Some((mods.get(i)?.descriptor.remote_file_id?, i))).collect();
    let installed: HashSet<u64> = mods.iter().filter_map(|m| m.descriptor.remote_file_id).collect();
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for id in enabled_ids(mods, enabled) {
        let Some((_, dependents)) = cache.items.get(&id) else { continue };
        for d in dependents {
            if !seen.insert(d.id) || installed.contains(&d.id) || dismissed.contains(&d.id) || is_translation(d) {
                continue;
            }
            let required: Vec<u64> = d.requires.iter().copied().filter(|r| *r != d.id && !NOT_MODS.contains(r)).collect();
            let Some(needs) = required.iter().map(|r| on.get(r).copied()).collect::<Option<Vec<usize>>>() else { continue };
            if needs.is_empty() {
                continue;
            }
            found.push(Suggestion { item: d.clone(), needs, patch: is_patch(d) });
        }
    }
    found.sort_by_key(|s| (!s.patch, std::cmp::Reverse(s.item.subscriptions)));
    found
}

/// A patch by its name, as Smart Sort reads names, or by its tags.
fn is_patch(d: &Dependent) -> bool {
    let lower = d.title.to_lowercase();
    ["patch", "compat", "bridge"].iter().any(|w| lower.contains(w))
        || words(&lower).any(|w| w.starts_with("fix"))
        || super::smart_sort::plus_joined(&lower)
        || d.tags.iter().any(|t| t.eq_ignore_ascii_case("compatibility"))
}

/// Language names and words for translating; translations require the
/// mod they translate but are never patches.
const LANGUAGE_WORDS: [&str; 34] = [
    "translation", "translations", "translated", "localisation", "localization", "localisations", "localizations", "russian", "japanese", "chinese",
    "korean", "french", "german", "deutsch", "italian", "italiano", "spanish", "español", "espanol", "portuguese", "polish", "polski", "turkish",
    "ukrainian", "czech", "traduction", "traducción", "traducao", "tradução", "traduzione", "übersetzung", "russification", "rus", "ptbr",
];
/// Language codes, counted only in brackets: "[RU]", "(FR)".
const LANGUAGE_CODES: [&str; 18] = ["ru", "rus", "jp", "ja", "cn", "zh", "kr", "ko", "fr", "de", "it", "es", "pt", "br", "pl", "tr", "ua", "cz"];
/// Words in other scripts, matched anywhere.
const LANGUAGE_TEXT: [&str; 9] = ["перевод", "русификатор", "локализация", "汉化", "中文", "翻译", "簡體", "日本語", "한국어"];

fn is_translation(d: &Dependent) -> bool {
    let lower = d.title.to_lowercase();
    d.tags.iter().any(|t| t.eq_ignore_ascii_case("translation"))
        || words(&lower).any(|w| LANGUAGE_WORDS.contains(&w))
        || LANGUAGE_TEXT.iter().any(|w| lower.contains(w))
        || bracketed(&lower).any(|inside| words(inside).any(|w| LANGUAGE_CODES.contains(&w)))
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty())
}

/// What's inside each pair of brackets or parentheses.
fn bracketed(text: &str) -> impl Iterator<Item = &str> {
    let opens = |c: char| c == '[' || c == '(';
    let closes = |c: char| c == ']' || c == ')';
    text.split(opens).skip(1).filter_map(move |rest| rest.split_once(closes).map(|(inside, _)| inside))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::{Descriptor, Source};

    fn m(id: u64) -> Mod {
        Mod { id: format!("mod/ugc_{id}.mod"), descriptor: Descriptor { remote_file_id: Some(id), ..Default::default() }, content: None, source: Source::Workshop, load_position: None, error: None, steam: None, requires: Vec::new() }
    }

    fn item(id: u64, title: &str, requires: &[u64], subscriptions: u64, tags: &[&str]) -> Dependent {
        Dependent { id, title: title.into(), requires: requires.to_vec(), subscriptions, updated: now(), tags: tags.iter().map(|t| t.to_string()).collect() }
    }

    #[test]
    fn suggests_patches_for_enabled_mods() {
        // 1, 2 and 3 enabled, 4 disabled, 5 installed.
        let mods = vec![m(1), m(2), m(3), m(4), m(5)];
        let enabled = [0, 1, 2];
        let mut cache = Cache::default();
        assert_eq!(cache.stale(&enabled_ids(&mods, &enabled), false), [1, 2, 3]);
        let mut old = item(106, "Old A + B Patch", &[1, 2], 9000, &[]);
        old.updated = now() - 60 * 7 * 24 * 3600;
        cache.update(1, vec![
            item(100, "A + B Compatibility Patch", &[1, 2], 10, &[]),
            item(101, "A – D Patch", &[1, 4], 50, &[]),
            item(102, "A Expanded", &[1], 500, &[]),
            item(103, "A [RUS]", &[1], 900, &[]),
            item(104, "A Japanese Translation", &[1], 900, &[]),
            item(105, "Something", &[1, 3], 20, &["Compatibility"]),
            old,
            item(5, "Installed Patch", &[1], 1, &[]),
            item(107, "Dismissed Patch", &[1], 1, &[]),
            item(108, "Make It Work Patch (with How to Read)", &[1, NOT_MODS[0]], 5, &[]),
        ]);
        // A patch for A and B is listed under both.
        cache.update(2, vec![item(100, "A + B Compatibility Patch", &[1, 2], 10, &[])]);
        assert_eq!(cache.stale(&enabled_ids(&mods, &enabled), false), [3]);
        assert_eq!(cache.stale(&[1], true), [1], "all, to refresh");

        let found = suggest(&cache, &mods, &enabled, &BTreeSet::from([107]));
        let ids: Vec<(u64, bool)> = found.iter().map(|s| (s.item.id, s.patch)).collect();
        assert_eq!(ids, [(106, true), (105, true), (100, true), (108, true), (102, false)], "patches by subscribers, then others");
        let old = &found[0];
        assert!(!old.updated_within(52) && old.updated_within(61), "updated 60 weeks ago");
        assert!(found[1].updated_within(1));
        assert_eq!(found[2].needs, [0, 1]);
        assert_eq!(found[3].needs, [0], "items that aren't mods aren't needed");
    }

    #[test]
    fn tells_patches_and_translations() {
        let named = |title: &str| item(1, title, &[], 0, &[]);
        assert!(is_patch(&named("Giga-ACOT Bridge")));
        assert!(is_patch(&named("UIOD ETFDLC + MP Spacebar Pause")));
        assert!(is_patch(&named("Planetary Diversity Fixes")));
        assert!(!is_patch(&named("Expanded Starbases")));
        assert!(is_translation(&named("Gigastructures [RU]")));
        assert!(is_translation(&named("Gigastructural Engineering 简体中文汉化")));
        assert!(is_translation(&named("ACOT - Italian")));
        assert!(!is_translation(&named("Make It Work (UIOD Patch)")), "\"it\" outside brackets is a word");
        assert!(is_translation(&item(1, "Giga", &[], 0, &["Translation"])));
    }

    #[test]
    fn saves_and_loads() {
        let mut cache = Cache::default();
        cache.update(1, vec![item(100, "Patch", &[1, 2], 10, &[])]);
        let path = std::env::temp_dir().join(format!("drukhal-dependents-{}/cache.json", std::process::id()));
        cache.save(&path).unwrap();
        assert_eq!(Cache::load(&path).items[&1].1, cache.items[&1].1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
