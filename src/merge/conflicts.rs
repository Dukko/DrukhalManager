//! Finding where enabled mods step on each other.
//!
//! Two kinds of conflict are reported:
//!
//! - **File overrides**: several mods ship the same file path. The game loads
//!   only the copy from the mod that comes last in the load order.
//! - **Object conflicts**: different files define the same object, e.g. the
//!   same building in `common/buildings`, the same event id, or the same
//!   localisation key. Which definition the game uses depends on file
//!   names and the folder's rule; see [`ObjectConflict::winner`].
//!
//! Only mod-vs-mod conflicts are reported; overriding vanilla is normal.
//! The approach (per-file indexing, `replace_path` dropping earlier files,
//! grouping objects by folder) follows Irony Mod Manager's conflict solver
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::scan_cache::{ScanCache, ScanError};
use crate::paradox::script::{self, Entries, Entry};

/// One enabled mod, in load order.
#[derive(Debug, Clone)]
pub struct ModInput {
    /// The mod's folder (for a zipped mod, its unpacked copy), or `None` if
    /// its files could not be found.
    pub root: Option<PathBuf>,
    pub replace_path: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub files: Vec<FileConflict>,
    pub objects: Vec<ObjectConflict>,
    pub errors: Vec<FileError>,
    /// Mods that could not be scanned, by load position, with the reason.
    pub skipped: Vec<(usize, String)>,
    /// Every object the scan found, conflicting or not, by (category, key),
    /// with its definitions in load order, for looking objects up.
    pub defined: BTreeMap<(String, String), Vec<Definition>>,
    /// How many files the scan read from each mod, by scanned position;
    /// 0 for skipped mods.
    pub shipped: Vec<usize>,
    /// For each scanned mod, by scanned position, how many of its files each
    /// later mod replaces (replacer position -> file count): a later mod ships
    /// the same path, or a `replace_path` covers it. A mod's files counted here
    /// never reach the game.
    pub replaced_by: Vec<BTreeMap<usize, usize>>,
    /// Whether each scanned mod, by scanned position, declares a `replace_path`.
    /// Such a mod hides base-game files even if all it ships is replaced.
    pub has_replace_path: Vec<bool>,
    /// Each key of `defined`, in its order, lowercased whole and without a
    /// localisation key's language, made on the first look-up. Left to
    /// `look_up`; `pub(crate)` only so tests can build a `Report` with
    /// `..Default::default()`.
    pub(crate) lowered: std::sync::OnceLock<Vec<(String, String)>>,
}

impl Report {
    /// Objects whose key contains `query`, ignoring case, exact matches
    /// first, then by key; at most `limit`.
    pub fn look_up(&self, query: &str, limit: usize) -> Vec<ObjectConflict> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        // Localisation keys are stored as `l_english:key`; people type the key.
        // Lowercased once per scan, not on every keystroke.
        let lowered = self.lowered.get_or_init(|| {
            self.defined
                .keys()
                .map(|(_, key)| (key.to_lowercase(), key.split_once(':').map_or(key.as_str(), |(_, name)| name).to_lowercase()))
                .collect()
        });
        let mut found: Vec<(bool, &(String, String))> = self
            .defined
            .keys()
            .zip(lowered)
            .filter_map(|(id, (whole, part))| (whole.contains(&query) || part.contains(&query)).then_some((*whole == query || *part == query, id)))
            .collect();
        // Stable sort keeps the map's (category, key) order within each group.
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1 .1.cmp(&b.1 .1)));
        found
            .into_iter()
            .take(limit)
            .map(|(_, id)| ObjectConflict { category: id.0.clone(), key: id.1.clone(), definitions: self.defined[id].clone() })
            .collect()
    }
}

/// The same file shipped by several mods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConflict {
    pub path: String,
    /// Load positions of the mods that ship it, ascending. The last one wins.
    pub mods: Vec<usize>,
}

/// The same object defined by several mods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectConflict {
    /// Where the object lives, e.g. `common/buildings`, `events` or
    /// `localisation`.
    pub category: String,
    pub key: String,
    pub definitions: Vec<Definition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub mod_position: usize,
    pub file: String,
}

/// Why a definition is the one the game uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Its file name sorts last, and in this folder later files override
    /// earlier ones (last in, only served).
    LastFile,
    /// Its file name sorts first, and this folder keeps the first
    /// definition it reads (first in, only served).
    FirstFile,
    /// Its file has the same name as another mod's, so the load order
    /// decides.
    LoadOrder,
    /// It's a localisation file in a `replace` folder, which overrides
    /// the others.
    Replace,
}

impl Reason {
    /// Why it wins, to follow "<mod>'s version is used: ".
    pub fn explain(self, fios: bool) -> &'static str {
        match self {
            Reason::LastFile => "its file name sorts last, and in this folder later files override earlier ones",
            Reason::FirstFile => "its file name sorts first, and this folder keeps the first definition it reads",
            Reason::LoadOrder if fios => "its file has the same name as another mod's, and this mod loads first",
            Reason::LoadOrder => "its file has the same name as another mod's, and this mod loads later",
            Reason::Replace => "it's in a localisation replace folder, which overrides the others",
        }
    }
}

/// Folders where the game keeps the first definition of an object it
/// reads rather than the last, from Irony Mod Manager's Stellaris rules.
const FIOS_FOLDERS: &[&str] = &[
    "common/component_sets",
    "common/component_templates",
    "common/event_chains",
    "common/global_ship_designs",
    "common/scripted_variables",
    "common/section_templates",
    "common/ship_behaviors",
    "common/special_projects",
    "common/strategic_resources",
    "events",
    "common/solar_system_initializers",
    "common/traits",
    "common/start_screen_messages",
    "common/governments/authorities",
];

/// Whether objects in `category` follow first-in-only-served rules.
pub fn is_fios(category: &str) -> bool {
    FIOS_FOLDERS.iter().any(|f| category.eq_ignore_ascii_case(f) || is_under(category, f))
}

impl ObjectConflict {
    /// The definition the game uses, as an index into `definitions`, and
    /// why. The game reads a folder's files sorted by name, whichever mod
    /// they come from; same-named files are read in load order.
    pub fn winner(&self) -> Option<(usize, Reason)> {
        let fios = is_fios(&self.category);
        fn stem(d: &Definition) -> &str {
            let name = d.file.rsplit('/').next().unwrap_or(&d.file);
            name.rsplit_once('.').map_or(name, |(stem, _)| stem)
        }
        let mut candidates: Vec<usize> = (0..self.definitions.len()).collect();
        let mut replaced = false;
        if self.category == "localisation" {
            let in_replace = |d: &Definition| d.file.split('/').rev().skip(1).any(|part| part.eq_ignore_ascii_case("replace"));
            let replacing: Vec<usize> = candidates.iter().copied().filter(|&i| in_replace(&self.definitions[i])).collect();
            if !replacing.is_empty() && replacing.len() < candidates.len() {
                candidates = replacing;
                replaced = true;
            }
        }
        // By file name, then load position.
        let key = |i: usize| (stem(&self.definitions[i]), self.definitions[i].mod_position);
        let best = if fios {
            candidates.iter().copied().min_by(|&a, &b| key(a).cmp(&key(b)))?
        } else {
            candidates.iter().copied().max_by(|&a, &b| key(a).cmp(&key(b)))?
        };
        let w = &self.definitions[best];
        let tied = candidates.iter().any(|&i| {
            let d = &self.definitions[i];
            d.mod_position != w.mod_position && stem(d) == stem(w)
        });
        let reason = if tied {
            Reason::LoadOrder
        } else if replaced && candidates.len() == 1 {
            Reason::Replace
        } else if fios {
            Reason::FirstFile
        } else {
            Reason::LastFile
        };
        Some((best, reason))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub mod_position: usize,
    pub file: String,
    pub message: String,
    /// The line the parser failed at, when the file was read but didn't
    /// parse; `None` for read failures.
    pub line: Option<usize>,
}

impl Report {
    /// Number of conflicts each load position takes part in.
    #[cfg(test)]
    pub fn counts_by_mod(&self, mod_count: usize) -> Vec<usize> {
        self.counts_by_mod_where(mod_count, |_| true, |_| true)
    }

    /// Like `counts_by_mod`, counting only the conflicts the filters keep.
    pub fn counts_by_mod_where(&self, mod_count: usize, keep_file: impl Fn(&FileConflict) -> bool, keep_object: impl Fn(&ObjectConflict) -> bool) -> Vec<usize> {
        let mut counts = vec![0; mod_count];
        for c in self.files.iter().filter(|c| keep_file(c)) {
            c.mods.iter().for_each(|&m| counts[m] += 1);
        }
        for c in self.objects.iter().filter(|c| keep_object(c)) {
            let mods: BTreeSet<usize> = c.definitions.iter().map(|d| d.mod_position).collect();
            mods.into_iter().for_each(|m| counts[m] += 1);
        }
        counts
    }
}

/// For each mod, by load position, the mods it depends on, directly or
/// through other dependencies. `dependencies` are each mod's descriptor
/// dependencies, which name other mods; `names` are the mods' names.
pub fn depends_on(names: &[String], dependencies: &[Vec<String>]) -> Vec<BTreeSet<usize>> {
    let by_name: std::collections::HashMap<String, usize> = names.iter().enumerate().map(|(i, n)| (n.trim().to_lowercase(), i)).collect();
    let direct: Vec<Vec<usize>> = dependencies
        .iter()
        .enumerate()
        .map(|(i, deps)| deps.iter().filter_map(|d| by_name.get(&d.trim().to_lowercase()).copied()).filter(|&d| d != i).collect())
        .collect();
    (0..names.len())
        .map(|start| {
            let mut found = BTreeSet::new();
            let mut stack = direct[start].clone();
            while let Some(d) = stack.pop() {
                if d != start && found.insert(d) {
                    stack.extend(&direct[d]);
                }
            }
            found
        })
        .collect()
}

/// Whether every mod among `mods` (load positions) depends on all those
/// loading before it, as a patch or submod does: the later ones are meant
/// to override the earlier.
pub fn builds_on_dependencies(mods: impl IntoIterator<Item = usize>, depends: &[BTreeSet<usize>]) -> bool {
    let mods: Vec<usize> = mods.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
    mods.len() > 1 && mods.iter().enumerate().skip(1).all(|(at, later)| mods[..at].iter().all(|earlier| depends.get(*later).is_some_and(|d| d.contains(earlier))))
}

impl FileConflict {
    pub fn builds_on_dependencies(&self, depends: &[BTreeSet<usize>]) -> bool {
        builds_on_dependencies(self.mods.iter().copied(), depends)
    }
}

impl ObjectConflict {
    pub fn builds_on_dependencies(&self, depends: &[BTreeSet<usize>]) -> bool {
        builds_on_dependencies(self.definitions.iter().map(|d| d.mod_position), depends)
    }
}

/// The languages Stellaris ships localisation for, as its files name them
/// (`*_l_english.yml`), with how to show them.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("english", "English"),
    ("braz_por", "Brazilian Portuguese"),
    ("french", "French"),
    ("german", "German"),
    ("japanese", "Japanese"),
    ("korean", "Korean"),
    ("polish", "Polish"),
    ("russian", "Russian"),
    ("simp_chinese", "Simplified Chinese"),
    ("spanish", "Spanish"),
];

/// The language a localisation file is for, from its `_l_<language>.yml`
/// name, if it's one the game knows.
pub fn file_language(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next()?.to_lowercase();
    let stem = name.strip_suffix(".yml")?;
    LANGUAGES.iter().map(|(id, _)| *id).find(|id| stem.strip_suffix(id).is_some_and(|rest| rest.ends_with("l_")))
}

/// Whether the scan reads `path`, given the languages it reads (`None` for
/// all). Files of languages the game doesn't know are always read.
fn reads(path: &str, languages: Option<&BTreeSet<String>>) -> bool {
    let Some(languages) = languages else { return true };
    let lower = path.to_lowercase();
    if !(lower.starts_with("localisation/") || lower.starts_with("localization/")) {
        return true;
    }
    !lower.ends_with(".yml") || file_language(path).is_none_or(|l| languages.contains(l))
}

/// Folders whose objects the game merges rather than overrides.
const MERGED_FOLDERS: &[&str] = &["common/on_actions", "common/defines"];

/// Blocks the game adds to an existing object instead of replacing it,
/// by folder: an object holding nothing but these blocks is an addition.
pub const ADDITIVE_BLOCKS: &[(&str, &str)] = &[("common/governments/authorities", "advanced_authority_swap")];

/// Whether an object of `category` with this `body` only adds blocks to an
/// object defined elsewhere, so it defines nothing itself.
pub fn is_addition(category: &str, body: &[Entry]) -> bool {
    let blocks: Vec<&str> = ADDITIVE_BLOCKS.iter().filter(|(f, _)| category.eq_ignore_ascii_case(f) || is_under(category, f)).map(|(_, b)| *b).collect();
    !body.is_empty() && !blocks.is_empty() && body.iter().all(|e| matches!(e, Entry::Assign { key, .. } if blocks.iter().any(|b| key.eq_ignore_ascii_case(b))))
}

/// Folders of script fragments pasted into other scripts, not objects.
pub const FRAGMENT_FOLDERS: &[&str] = &["common/inline_scripts"];

/// Finds the files and objects more than one of `mods` defines. Only the
/// localisation of `languages` is read; `None` reads every language, and an
/// empty set none.
#[cfg(test)]
pub fn analyze(mods: &[ModInput], languages: Option<&BTreeSet<String>>) -> Report {
    analyze_cached(mods, languages, &mut ScanCache::default())
}

/// Like `analyze`, parsing only the files `cache` doesn't know as they are.
pub fn analyze_cached(mods: &[ModInput], languages: Option<&BTreeSet<String>>, cache: &mut ScanCache) -> Report {
    let mut report = Report { shipped: vec![0; mods.len()], replaced_by: vec![BTreeMap::new(); mods.len()], has_replace_path: mods.iter().map(|m| !m.replace_path.is_empty()).collect(), ..Report::default() };

    // Lowercased relative path -> (load position, path as spelled in that
    // mod) for every mod shipping it.
    let mut files: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    for (position, m) in mods.iter().enumerate() {
        let Some(root) = &m.root else {
            report.skipped.push((position, t!("Mod files not found")));
            continue;
        };
        if root.is_file() {
            // Zips are unpacked before scanning; this one couldn't be.
            report.skipped.push((position, t!("Could not unpack this zipped mod")));
            continue;
        }
        let mut found = Vec::new();
        if let Err(e) = walk(root, root, &mut found) {
            report.skipped.push((position, tf!("Could not read mod files: {error}", error = e)));
            continue;
        }
        // A later `replace_path` makes the game ignore earlier mods' files in
        // that folder, so they cannot conflict.
        if !m.replace_path.is_empty() {
            files.retain(|path, shippers| {
                let dropped = m.replace_path.iter().any(|r| is_under(path, r));
                if dropped {
                    for (earlier, _) in shippers.iter() {
                        *report.replaced_by[*earlier].entry(position).or_default() += 1;
                    }
                }
                !dropped
            });
        }
        for path in found.into_iter().filter(|p| reads(p, languages)) {
            report.shipped[position] += 1;
            files.entry(path.to_lowercase()).or_default().push((position, path));
        }
    }

    // Objects come only from the copy of each file that actually loads.
    // The files to read are gathered first, so the uncached ones can be
    // parsed in parallel.
    let mut to_read: Vec<(usize, &String, String)> = Vec::new();
    for shipped in files.values() {
        let (winner, path) = shipped.last().unwrap();
        for (m, _) in shipped.iter().filter(|(m, _)| m != winner) {
            *report.replaced_by[*m].entry(*winner).or_default() += 1;
        }
        if shipped.len() > 1 {
            report.files.push(FileConflict { path: path.clone(), mods: shipped.iter().map(|(m, _)| *m).collect() });
        }
        if let Some(category) = object_category(path) {
            to_read.push((*winner, path, category));
        }
    }
    let full: Vec<(PathBuf, String)> = to_read.iter().map(|(winner, path, category)| (mods[*winner].root.as_ref().unwrap().join(path), category.clone())).collect();
    let all_keys = cache.keys_many(&full, object_keys);
    let mut objects: BTreeMap<(String, String), Vec<Definition>> = BTreeMap::new();
    for ((winner, path, category), keys) in to_read.into_iter().zip(all_keys) {
        let keys = match keys {
            Ok(keys) => keys,
            Err(e) => {
                report.errors.push(FileError { mod_position: winner, file: path.clone(), message: e.message, line: e.line });
                continue;
            }
        };
        // Entries under another language's header, in a file named for one
        // that's read, are left out too.
        let keys = keys.into_iter().filter(|k| {
            category != "localisation" || languages.is_none_or(|l| k.split_once(':').is_none_or(|(lang, _)| lang.strip_prefix("l_").is_none_or(|lang| !LANGUAGES.iter().any(|(id, _)| *id == lang) || l.contains(lang))))
        });
        for key in keys {
            objects.entry((category.clone(), key)).or_default().push(Definition { mod_position: winner, file: path.clone() });
        }
    }

    for ((category, key), definitions) in &mut objects {
        definitions.sort_by(|a, b| (a.mod_position, &a.file).cmp(&(b.mod_position, &b.file)));
        let distinct: BTreeSet<usize> = definitions.iter().map(|d| d.mod_position).collect();
        if distinct.len() > 1 {
            report.objects.push(ObjectConflict { category: category.clone(), key: key.clone(), definitions: definitions.clone() });
        }
    }
    report.defined = objects;
    report
}

/// Collects every file below the mod's top-level folders, as `/`-separated
/// paths relative to `root`. Files at the root (descriptor, thumbnail) are
/// not game content.
pub(crate) fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Hidden entries (`.git`, editor files) are never game content.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &path, out)?;
        } else if kind.is_file() && dir != root {
            let rel = path.strip_prefix(root).unwrap();
            out.push(crate::platform::game_path(rel));
        }
    }
    Ok(())
}

pub(crate) fn is_under(path: &str, folder: &str) -> bool {
    let folder = folder.trim_matches('/');
    // By bytes: paths can hold any characters, and slicing a `str` inside one panics.
    let (path, folder) = (path.as_bytes(), folder.as_bytes());
    path.len() > folder.len() && path[..folder.len()].eq_ignore_ascii_case(folder) && path[folder.len()] == b'/'
}

/// The folder whose objects share a namespace, if the file defines objects
/// this module understands.
pub(crate) fn object_category(path: &str) -> Option<String> {
    let lower = path.to_lowercase();
    let (dir, file) = lower.rsplit_once('/')?;
    if lower.starts_with("localisation/") || lower.starts_with("localization/") {
        return file.ends_with(".yml").then(|| "localisation".to_owned());
    }
    if !file.ends_with(".txt") {
        return None;
    }
    if dir == "events" {
        return Some("events".to_owned());
    }
    if dir.starts_with("common/") && !MERGED_FOLDERS.iter().chain(FRAGMENT_FOLDERS).any(|m| dir == *m || is_under(dir, m)) {
        // The original spelling: lowercasing can change a name's length.
        return path.rsplit_once('/').map(|(original, _)| original.to_owned());
    }
    None
}

/// The unique object identifier for a Paradox script entry, if it defines an object.
///
/// Paradox Stellaris scripts define objects in two ways:
/// 1. Direct top-level name: `building_clinic = { ... }` or `tech_lasers_1 = { ... }`.
/// 2. Typed wrapper block with an inner identifier:
///    - `events`: `country_event = { id = "..." }`
///    - `common/component_sets`: `component_set = { key = "..." }`
///    - `common/component_templates`: `utility_component_template = { key = "..." }`
///    - `common/section_templates`: `ship_section_template = { key = "..." }`
///    - `common/ambient_objects`: `ambient_object = { name = "..." }`
///    - `common/ship_behaviors`: `ship_behavior = { name = "..." }`
///    - `common/global_ship_designs`: `ship_design = { name = "..." }`
pub fn object_key_for_entry(category: &str, entry: &Entry) -> Option<String> {
    let Entry::Assign { key, value, .. } = entry else { return None };
    if key.starts_with('@') || key.eq_ignore_ascii_case("namespace") || key.eq_ignore_ascii_case("inline_script") {
        return None;
    }
    let block = value.as_block();
    if let Some(b) = block && is_addition(category, b) {
        return None;
    }
    if category == "events" || key.ends_with("_event") {
        return block.and_then(|b| b.scalar("id")).map(str::to_owned);
    }
    if category.eq_ignore_ascii_case("common/ambient_objects") || is_under(category, "common/ambient_objects") || key.eq_ignore_ascii_case("ambient_object") {
        return block.and_then(|b| b.scalar("name")).map(str::to_owned);
    }
    if category.eq_ignore_ascii_case("common/ship_behaviors") || is_under(category, "common/ship_behaviors") || key.eq_ignore_ascii_case("ship_behavior") {
        return block.and_then(|b| b.scalar("name")).map(str::to_owned);
    }
    if category.eq_ignore_ascii_case("common/global_ship_designs") || is_under(category, "common/global_ship_designs") || key.eq_ignore_ascii_case("ship_design") {
        return block.and_then(|b| b.scalar("name")).map(str::to_owned);
    }
    if let Some(inner_key) = block.and_then(|b| b.scalar("key")) {
        return Some(inner_key.to_owned());
    }
    Some(key.clone())
}

fn object_keys(category: &str, src: &str) -> Result<Vec<String>, ScanError> {
    if category == "localisation" {
        return Ok(localisation_keys(src));
    }
    let entries = script::parse(src).map_err(|e| ScanError { message: e.message, line: Some(e.line) })?;
    let mut keys = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in &entries {
        if let Some(key) = object_key_for_entry(category, entry) {
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }
    }
    Ok(keys)
}

/// Whether `header` is a language header (`l_english`, `l_simp_chinese`).
/// It ends up in a file name, so nothing else is accepted.
pub fn is_language_header(header: &str) -> bool {
    header.strip_prefix("l_").is_some_and(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
}

/// Keys of a Paradox YAML-ish localisation file, prefixed by language so
/// `l_english` and `l_german` entries don't collide.
fn localisation_keys(src: &str) -> Vec<String> {
    let mut language = String::new();
    let mut keys = Vec::new();
    for line in src.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, rest)) = trimmed.split_once(':') else { continue };
        if !line.starts_with([' ', '\t']) && rest.trim().is_empty() {
            language = if is_language_header(key) { key.to_owned() } else { String::new() };
            continue;
        }
        if !language.is_empty() && !key.is_empty() && !key.contains([' ', '"']) {
            keys.push(format!("{language}:{key}"));
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, contents) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    fn input(root: &Path, name: &str, replace_path: &[&str]) -> ModInput {
        ModInput { root: Some(root.join(name)), replace_path: replace_path.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    #[ignore]
    fn scan_timing() {
        let root = std::env::temp_dir().join(format!("drukhal-scan-timing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut file_count = 0;
        let mut object_count = 0;
        let mut inputs = Vec::new();
        for m in 0..150 {
            let name = format!("mod{m}");
            let mut files = Vec::new();
            for f in 0..40 {
                let (folder, ext) = match f % 4 {
                    0 => ("common/buildings", "txt"),
                    1 => ("common/technology", "txt"),
                    2 => ("events", "txt"),
                    _ => ("localisation/english", "yml"),
                };
                let mut body = String::new();
                if ext == "yml" {
                    body.push_str("l_english:\n");
                    for l in 0..100 {
                        // Some keys are shared between mods.
                        let k = if l % 10 == 0 { format!("shared_{}", l / 10) } else { format!("m{m}_f{f}_l{l}") };
                        body.push_str(&format!(" {k}:0 \"Text {l}\"\n"));
                        object_count += 1;
                    }
                } else {
                    for o in 0..30 {
                        let k = if o % 10 == 0 { format!("shared_{f}_{o}") } else { format!("m{m}_f{f}_o{o}") };
                        let head = if folder == "events" { format!("country_event = {{ id = {k}") } else { format!("{k} = {{") };
                        body.push_str(&format!("{head}\n\tcost = {{ energy = {o} }}\n\tmodifier = {{ a = 1 b = 2 }}\n}}\n"));
                        object_count += 1;
                    }
                }
                let path = if ext == "yml" { format!("{folder}/m{m}_f{f}_l_english.yml") } else { format!("{folder}/m{m}_f{f}.txt") };
                files.push((path, body));
                file_count += 1;
            }
            let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            write(&root.join(&name), &refs);
            inputs.push(input(&root, &name, &[]));
        }
        let best = |cold: bool| {
            let mut warm_cache = ScanCache::default();
            if !cold {
                analyze_cached(&inputs, None, &mut warm_cache);
            }
            (0..3)
                .map(|_| {
                    let mut fresh = ScanCache::default();
                    let cache = if cold { &mut fresh } else { &mut warm_cache };
                    let start = std::time::Instant::now();
                    let report = analyze_cached(&inputs, None, cache);
                    assert!(!report.objects.is_empty());
                    start.elapsed().as_millis()
                })
                .min()
                .unwrap()
        };
        println!("TIMING cold scan={}", best(true));
        println!("TIMING warm scan={}", best(false));
        println!("TIMING files={file_count} objects={object_count}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_file_and_object_conflicts() {
        let root = std::env::temp_dir().join(format!("drukhal-conflicts-{}", std::process::id()));
        write(&root.join("a"), &[
            ("descriptor.mod", "name=a"),
            ("common/buildings/a_buildings.txt", "@cost = 10\nbuilding_lab = { cost = @cost }\nbuilding_a = {}"),
            ("common/buildings/shared.txt", "building_shared = {}"),
            ("common/ambient_objects/a.txt", "ambient_object = { name = x }"),
            (".git/config", "[core]"),
            ("common/on_actions/a.txt", "on_game_start = { events = { a.1 } }"),
            ("events/a.txt", "namespace = a\ncountry_event = { id = a.1 }\ncountry_event = { id = shared.1 }"),
            ("localisation/english/a_l_english.yml", "\u{feff}l_english:\n building_lab:0 \"Lab\"\n only_a: \"A\"\n"),
        ]);
        write(&root.join("b"), &[
            ("descriptor.mod", "name=b"),
            ("common/buildings/b_buildings.txt", "building_lab = { cost = 20 }"),
            ("common/buildings/Shared.txt", "building_shared = { b = yes }"),
            ("common/ambient_objects/b.txt", "ambient_object = { name = z }"),
            (".git/config", "[core]"),
            ("common/on_actions/b.txt", "on_game_start = { events = { b.1 } }"),
            ("events/b.txt", "namespace = b\nship_event = { id = shared.1 }"),
            ("localisation/b_l_english.yml", "l_english:\n building_lab:0 \"Better Lab\"\n"),
            ("localisation/b_l_german.yml", "l_german:\n only_a:0 \"A\"\n"),
        ]);

        let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &[])], None);

        assert_eq!(report.files, [FileConflict { path: "common/buildings/Shared.txt".into(), mods: vec![0, 1] }]);
        let objects: Vec<(&str, &str)> = report.objects.iter().map(|c| (c.category.as_str(), c.key.as_str())).collect();
        assert_eq!(objects, [("common/buildings", "building_lab"), ("events", "shared.1"), ("localisation", "l_english:building_lab")]);
        assert_eq!(report.objects[0].definitions[1], Definition { mod_position: 1, file: "common/buildings/b_buildings.txt".into() });
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.counts_by_mod(2), [4, 4]);

        std::fs::remove_dir_all(root).unwrap();
    }

    /// Measures what expanding `inline_script` calls would add to the scan, on
    /// real folders (read-only): `DRUKHAL_WORKSHOP` holds mod folders and
    /// `DRUKHAL_GAME` is the game folder. Every mod counts as enabled at once;
    /// load order, `replace_path` and file-name overrides are ignored, and `[[`
    /// conditional parameter blocks are not expanded.
    #[test]
    #[ignore]
    fn inline_script_survey() {
        use crate::paradox::script::{Entries, Value};

        let (Some(workshop), Some(game)) = (std::env::var_os("DRUKHAL_WORKSHOP"), std::env::var_os("DRUKHAL_GAME")) else {
            println!("SURVEY skipped: set DRUKHAL_WORKSHOP and DRUKHAL_GAME");
            return;
        };
        let (workshop, game) = (PathBuf::from(workshop), PathBuf::from(game));
        let read = |path: &Path| std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).trim_start_matches('\u{feff}').to_owned());

        // Every mod: its id, root and files.
        let mut mods: Vec<(String, PathBuf, Vec<String>)> = Vec::new();
        for entry in std::fs::read_dir(&workshop).unwrap().flatten() {
            if entry.file_type().is_ok_and(|k| k.is_dir()) {
                let mut files = Vec::new();
                let _ = walk(&entry.path(), &entry.path(), &mut files);
                mods.push((entry.file_name().to_string_lossy().into_owned(), entry.path(), files));
            }
        }
        mods.sort();
        // Lowercased path -> real path, per mod, for `common/inline_scripts`.
        let script_dir = "common/inline_scripts/";
        let index: Vec<BTreeMap<String, String>> = mods
            .iter()
            .map(|(_, _, files)| files.iter().filter(|f| f.to_lowercase().starts_with(script_dir)).map(|f| (f.to_lowercase(), f.clone())).collect())
            .collect();
        let mut game_files = Vec::new();
        let _ = walk(&game, &game, &mut game_files);
        let game_index: BTreeMap<String, String> = game_files.iter().filter(|f| f.to_lowercase().starts_with(script_dir)).map(|f| (f.to_lowercase(), f.clone())).collect();

        // Top-level keys that name objects: the rules of `object_keys`, but
        // counting nested calls instead of dropping them silently.
        let top_keys = |entries: &[Entry]| -> (Vec<String>, usize) {
            let mut seen = BTreeMap::<&str, usize>::new();
            let mut nested = 0;
            for e in entries {
                if let Entry::Assign { key, .. } = e {
                    if key.eq_ignore_ascii_case("inline_script") {
                        nested += 1;
                    } else if !key.starts_with('@') {
                        *seen.entry(key).or_default() += 1;
                    }
                }
            }
            (seen.into_iter().filter(|&(_, n)| n == 1).map(|(k, _)| k.to_owned()).collect(), nested)
        };

        let mut normal: BTreeMap<(String, String), BTreeSet<usize>> = BTreeMap::new();
        let mut expanded: BTreeMap<(String, String), BTreeSet<usize>> = BTreeMap::new();
        let (mut shipping, mut calling) = (0, 0);
        let (mut scalar, mut block, mut unresolved, mut nested, mut with_conditionals, mut parse_failures, mut calls) = (0, 0, 0, 0, 0, 0, 0);
        let mut bogus = 0;
        let mut event_calls = 0;
        let (mut source_parse_failures, mut source_failure_examples) = (0, Vec::new());
        let mut failure_examples = Vec::new();
        let mut unresolved_examples = Vec::new();

        for (m, (id, root, files)) in mods.iter().enumerate() {
            if !index[m].is_empty() {
                shipping += 1;
            }
            let mut mod_calls = 0;
            for file in files {
                let Some(category) = object_category(file) else { continue };
                let Some(src) = read(&root.join(file)) else { continue };
                if let Ok(keys) = object_keys(&category, &src) {
                    for key in keys {
                        normal.entry((category.clone(), key)).or_default().insert(m);
                    }
                }
                if category == "localisation" {
                    continue;
                }
                let entries = match script::parse(&src) {
                    Ok(entries) => entries,
                    Err(e) => {
                        // Files the scan itself can't parse yield no calls either.
                        source_parse_failures += 1;
                        if source_failure_examples.len() < 5 {
                            source_failure_examples.push(format!("{id}/{file}: {e}"));
                        }
                        continue;
                    }
                };
                let top_calls: Vec<&Value> = entries.values("inline_script").collect();
                // Event files never produced the bogus key (events are keyed by id).
                if top_calls.len() == 1 && category != "events" {
                    bogus += 1;
                }
                for call in top_calls {
                    calls += 1;
                    if category == "events" {
                        event_calls += 1;
                    }
                    mod_calls += 1;
                    let (name, params) = match call {
                        Value::Scalar(name) => {
                            scalar += 1;
                            (name.clone(), Vec::new())
                        }
                        Value::Block(b) => {
                            block += 1;
                            let params = b
                                .iter()
                                .filter_map(|e| match e {
                                    Entry::Assign { key, value: Value::Scalar(v), .. } if key != "script" => Some((key.clone(), v.clone())),
                                    _ => None,
                                })
                                .collect();
                            (b.scalar("script").unwrap_or_default().to_owned(), params)
                        }
                    };
                    let wanted = format!("{script_dir}{}.txt", name.to_lowercase());
                    let text = index[m]
                        .get(&wanted)
                        .map(|f| root.join(f))
                        .or_else(|| index.iter().enumerate().find_map(|(o, i)| i.get(&wanted).map(|f| mods[o].1.join(f))))
                        .or_else(|| game_index.get(&wanted).map(|f| game.join(f)))
                        .and_then(|path| read(&path));
                    let Some(mut text) = text else {
                        unresolved += 1;
                        if unresolved_examples.len() < 5 {
                            unresolved_examples.push(format!("{id}:{file}:{name}"));
                        }
                        continue;
                    };
                    for (k, v) in &params {
                        text = text.replace(&format!("${k}$"), v);
                    }
                    if text.contains("[[") {
                        with_conditionals += 1;
                    }
                    match script::parse(&text) {
                        Ok(parsed) => {
                            let (_, n) = top_keys(&parsed);
                            nested += n;
                            let keys: Vec<String> = parsed.iter().filter_map(|e| object_key_for_entry(&category, e)).collect();
                            for key in keys {
                                expanded.entry((category.clone(), key)).or_default().insert(m);
                            }
                        }
                        Err(e) => {
                            parse_failures += 1;
                            if failure_examples.len() < 5 {
                                failure_examples.push(format!("{wanted}: {e}"));
                            }
                        }
                    }
                }
            }
            if mod_calls > 0 {
                calling += 1;
            }
        }

        // (objects only from expansions, new conflicts, conflicts gaining a mod) over
        // the categories `wanted` accepts.
        let stats = |wanted: &dyn Fn(&str) -> bool| {
            let from_expansions = expanded.keys().filter(|k| wanted(&k.0) && !normal.contains_key(*k)).count();
            let mut new_conflicts = Vec::new();
            let mut gaining = 0;
            for (key, via) in expanded.iter().filter(|(k, _)| wanted(&k.0)) {
                let direct = normal.get(key).cloned().unwrap_or_default();
                let all: BTreeSet<usize> = direct.union(via).copied().collect();
                if direct.len() >= 2 {
                    if !via.is_subset(&direct) {
                        gaining += 1;
                    }
                } else if all.len() >= 2 {
                    new_conflicts.push((key, all));
                }
            }
            (from_expansions, new_conflicts, gaining)
        };
        let (from_expansions, new_conflicts, gaining) = stats(&|_| true);
        let (event_objects, event_new, event_gaining) = stats(&|c| c == "events");
        let mut categories = BTreeMap::<&str, usize>::new();
        for (category, _) in expanded.keys().filter(|k| !normal.contains_key(*k)) {
            *categories.entry(category).or_default() += 1;
        }
        let mut categories: Vec<_> = categories.into_iter().collect();
        categories.sort_by_key(|&(c, n)| (std::cmp::Reverse(n), c));

        println!("SURVEY mods={} mods_shipping_inline_scripts={shipping} mods_calling_inline_scripts={calling}", mods.len());
        println!("SURVEY calls={calls} scalar={scalar} block={block} unresolved={unresolved} nested={nested} with_conditionals={with_conditionals} parse_failures={parse_failures}");
        println!("SURVEY objects_from_expansions={from_expansions} new_conflicts={} conflicts_gaining_a_mod={gaining}", new_conflicts.len());
        println!("SURVEY events calls={event_calls} objects_from_expansions={event_objects} new_conflicts={} conflicts_gaining_a_mod={event_gaining}", event_new.len());
        println!("SURVEY bogus_inline_script_objects_before_fix={bogus}");
        for ((category, key), all) in new_conflicts.iter().take(15) {
            let ids: Vec<&str> = all.iter().map(|&m| mods[m].0.as_str()).collect();
            println!("SURVEY example {category} {key} mods={}", ids.join(","));
        }
        for (category, n) in categories.iter().take(10) {
            println!("SURVEY category {category} objects={n}");
        }
        println!("SURVEY calling_files_that_fail_to_parse={source_parse_failures}");
        for e in &source_failure_examples {
            println!("SURVEY source_parse_failure_example {e}");
        }
        for e in &failure_examples {
            println!("SURVEY parse_failure_example {e}");
        }
        for e in &unresolved_examples {
            println!("SURVEY unresolved_example {e}");
        }
    }

    #[test]
    fn inline_script_calls_are_not_objects() {
        let root = std::env::temp_dir().join(format!("drukhal-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("a"), &[
            ("common/technology/a.txt", "inline_script = \"tech/shared\"\ntech_a = { cost = 1 }"),
            ("common/inline_scripts/tech/shared.txt", "tech_from_script = { cost = 3 }"),
        ]);
        write(&root.join("b"), &[("common/technology/b.txt", "inline_script = { script = tech/shared NAME = b }\ntech_b = { cost = 2 }")]);

        let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &[])], None);
        assert!(report.objects.iter().all(|c| c.key != "inline_script"), "{:?}", report.objects);
        assert!(report.objects.is_empty(), "{:?}", report.objects);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn looks_up_objects_defined_by_one_mod_or_more() {
        let root = std::env::temp_dir().join(format!("drukhal-lookup-{}", std::process::id()));
        write(&root.join("a"), &[
            ("common/buildings/a.txt", "building_lab = {}\nbuilding_only_a = {}"),
            ("localisation/english/a_l_english.yml", "l_english:\n only_a: \"A\"\n"),
        ]);
        write(&root.join("b"), &[("common/buildings/b.txt", "building_lab = {}")]);

        let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &[])], None);

        let one = report.look_up("building_only_a", 10);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].definitions.iter().map(|d| d.mod_position).collect::<Vec<_>>(), [0]);
        assert!(one[0].winner().is_some());
        let both = report.look_up("BUILDING", 10);
        assert_eq!(both.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["building_lab", "building_only_a"]);
        let exact = report.look_up("building_lab", 10);
        assert_eq!(exact[0].key, "building_lab");
        assert_eq!(exact[0].definitions.len(), 2);
        assert!(report.look_up("only_a", 10).iter().any(|c| c.category == "localisation" && c.key == "l_english:only_a"));
        assert!(report.look_up("  ", 10).is_empty());
        assert_eq!(report.look_up("building", 1).len(), 1);
        assert!(report.objects.iter().any(|c| c.key == "building_lab"));
        assert!(!report.objects.iter().any(|c| c.key == "building_only_a"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replace_path_hides_earlier_files() {
        let root = std::env::temp_dir().join(format!("drukhal-replace-{}", std::process::id()));
        write(&root.join("a"), &[("common/ethics/x.txt", "ethic_a = {}"), ("events/broken.txt", "country_event = {")]);
        write(&root.join("b"), &[("common/ethics/x.txt", "ethic_a = {}")]);

        let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &["common/ethics"]), ModInput { root: None, replace_path: vec![] }], None);
        assert!(report.files.is_empty());
        assert!(report.objects.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].file, "events/broken.txt");
        assert_eq!(report.skipped, [(2, "Mod files not found".to_owned())]);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn counts_the_files_each_mod_ships() {
        let root = std::env::temp_dir().join(format!("dm-shipped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("a"), &[("gfx/1.dds", "1"), ("gfx/2.dds", "2")]);
        write(&root.join("b"), &[("gfx/1.dds", "x"), ("gfx/3.dds", "3"), ("gfx/4.dds", "4")]);
        let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &[])], None);
        assert_eq!(report.shipped, vec![2, 3]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn additions_define_nothing() {
        let swap = "auth_corporate = { advanced_authority_swap = { name = a } }\n";
        let full = "auth_corporate = { cost = 1 }\n";
        let file = "common/governments/authorities/x.txt";
        let run = |tag: &str, a: (&str, &str), b: (&str, &str)| {
            let root = std::env::temp_dir().join(format!("dm-additions-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            write(&root.join("a"), &[a]);
            write(&root.join("b"), &[b]);
            let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &[])], None);
            std::fs::remove_dir_all(&root).unwrap();
            report
        };
        let both_swaps = run("1", (file, swap), ("common/governments/authorities/y.txt", swap));
        assert!(both_swaps.objects.is_empty());
        assert!(both_swaps.defined.is_empty());
        let mixed = run("2", (file, full), ("common/governments/authorities/y.txt", swap));
        assert!(mixed.objects.is_empty());
        assert_eq!(mixed.defined.len(), 1);
        let both_full = run("3", (file, full), ("common/governments/authorities/y.txt", full));
        assert_eq!(both_full.objects.len(), 1);
        let with_other = "auth_corporate = { advanced_authority_swap = { } has_heir = yes }\n";
        let not_only = run("4", (file, with_other), ("common/governments/authorities/y.txt", with_other));
        assert_eq!(not_only.objects.len(), 1);
        let elsewhere = run("5", ("common/governments/civics/x.txt", swap), ("common/governments/civics/y.txt", swap));
        assert_eq!(elsewhere.objects.len(), 1);
        assert!(is_addition("Common/Governments/Authorities", &script::parse("ADVANCED_AUTHORITY_SWAP = {}").unwrap()));
        assert!(!is_addition("common/governments/authorities", &[]));
    }

    #[test]
    fn reads_only_the_chosen_languages() {
        assert_eq!(file_language("localisation/english/mod_l_english.yml"), Some("english"));
        assert_eq!(file_language("localisation/replace/x_L_SIMP_CHINESE.yml"), Some("simp_chinese"));
        assert_eq!(file_language("localisation/notes.yml"), None);
        assert_eq!(file_language("localisation/l_german.yml"), Some("german"));

        let root = std::env::temp_dir().join(format!("dm-languages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Each mod names its files after itself, so the keys conflict as
        // objects; both also ship one shared German file.
        for name in ["a", "b"] {
            for (path, src) in [
                (format!("localisation/{name}_l_english.yml"), "l_english:\n key:0 \"x\"\nl_german:\n stray:0 \"x\"\n"),
                (format!("localisation/{name}_l_german.yml"), "l_german:\n key:0 \"x\"\n"),
                ("localisation/shared_l_german.yml".to_owned(), "l_german:\n shared:0 \"x\"\n"),
                ("common/buildings/b.txt".to_owned(), "b = {}\n"),
            ] {
                let file = root.join(name).join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, src).unwrap();
            }
        }
        let mods = [input(&root, "a", &[]), input(&root, "b", &[])];
        let found = |languages: Option<&BTreeSet<String>>| {
            let report = analyze(&mods, languages);
            let mut found: Vec<String> = report.objects.iter().map(|c| c.key.clone()).collect();
            found.extend(report.files.iter().map(|c| c.path.clone()));
            found.sort();
            found
        };
        assert_eq!(found(None), ["common/buildings/b.txt", "l_english:key", "l_german:key", "l_german:stray", "localisation/shared_l_german.yml"]);
        let english = BTreeSet::from(["english".to_owned()]);
        assert_eq!(found(Some(&english)), ["common/buildings/b.txt", "l_english:key"], "German files and the stray German entries are left out");
        assert_eq!(found(Some(&BTreeSet::new())), ["common/buildings/b.txt"], "no localisation at all");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn finds_conflicts_meant_by_dependencies() {
        let names: Vec<String> = ["Base", "Patch", "Sub-patch", "Other"].map(String::from).into();
        let deps = vec![vec![], vec!["base".to_owned()], vec!["Patch".to_owned(), "Missing".to_owned()], vec![]];
        let depends = depends_on(&names, &deps);
        assert_eq!(depends[2], BTreeSet::from([0, 1]), "through the patch, too");
        assert!(depends[0].is_empty() && depends[3].is_empty());
        let file = |mods: Vec<usize>| FileConflict { path: "x".into(), mods };
        assert!(file(vec![0, 1]).builds_on_dependencies(&depends));
        assert!(file(vec![0, 1, 2]).builds_on_dependencies(&depends));
        assert!(!file(vec![0, 3]).builds_on_dependencies(&depends), "unrelated mods");
        assert!(!file(vec![0, 1, 3]).builds_on_dependencies(&depends), "one doesn't build on the rest");
        let object = ObjectConflict { category: "c".into(), key: "k".into(), definitions: vec![Definition { mod_position: 1, file: "a".into() }, Definition { mod_position: 1, file: "b".into() }] };
        assert!(!object.builds_on_dependencies(&depends), "one mod alone");
        // A dependency that loads later doesn't count: the patch loses then.
        let late = depends_on(&["Patch".into(), "Base".into()], &[vec!["Base".into()], vec![]]);
        assert!(!builds_on_dependencies([0, 1], &late));
        assert_eq!(depends_on(&["A".into(), "B".into()], &[vec!["B".into()], vec!["A".into()]]), vec![BTreeSet::from([1]), BTreeSet::from([0])], "cycles end");
    }

    #[test]
    fn picks_the_definition_the_game_uses() {
        let conflict = |category: &str, defs: &[(usize, &str)]| ObjectConflict {
            category: category.into(),
            key: "x".into(),
            definitions: defs.iter().map(|&(m, f)| Definition { mod_position: m, file: f.into() }).collect(),
        };
        // Later file names override earlier ones, whatever the load order.
        let c = conflict("common/buildings", &[(0, "common/buildings/zz_mine.txt"), (1, "common/buildings/00_theirs.txt")]);
        assert_eq!(c.winner(), Some((0, Reason::LastFile)));
        // Names compare without the extension, case-sensitively like the game.
        let c = conflict("common/buildings", &[(0, "common/buildings/a_x.txt"), (1, "common/buildings/a.txt"), (2, "common/buildings/B.txt")]);
        assert_eq!(c.winner(), Some((0, Reason::LastFile)));
        // Events keep the first definition read.
        let c = conflict("events", &[(0, "events/zz.txt"), (1, "events/aa.txt")]);
        assert_eq!(c.winner(), Some((1, Reason::FirstFile)));
        assert!(is_fios("common/governments/authorities") && is_fios("common/traits") && !is_fios("common/species_traits"));
        // Same names: the load order decides, either way round.
        let c = conflict("common/buildings", &[(0, "common/buildings/sub/x.txt"), (3, "common/buildings/x.txt")]);
        assert_eq!(c.winner(), Some((1, Reason::LoadOrder)));
        let c = conflict("events", &[(0, "events/a/x.txt"), (3, "events/x.txt")]);
        assert_eq!(c.winner(), Some((0, Reason::LoadOrder)));
        // Localisation in a replace folder beats the rest.
        let c = conflict("localisation", &[(0, "localisation/replace/a_l_english.yml"), (1, "localisation/english/z_l_english.yml")]);
        assert_eq!(c.winner(), Some((0, Reason::Replace)));
        let c = conflict("localisation", &[(0, "localisation/replace/z_l_english.yml"), (1, "localisation/english/replace/a_l_english.yml")]);
        assert_eq!(c.winner(), Some((0, Reason::LastFile)));
    }

    #[test]
    fn categorises_paths() {
        assert_eq!(object_category("common/governments/civics/x.txt").as_deref(), Some("common/governments/civics"));
        assert_eq!(object_category("common/on_actions/x.txt"), None);
        assert_eq!(object_category("common/ambient_objects/x.txt").as_deref(), Some("common/ambient_objects"));
        assert_eq!(object_category("events/x.txt").as_deref(), Some("events"));
        assert_eq!(object_category("common/inline_scripts/x/y.txt"), None);
        assert_eq!(object_category("localisation/replace/x_l_english.yml").as_deref(), Some("localisation"));
        assert_eq!(object_category("gfx/x.dds"), None);
        assert!(is_under("Common/Ethics/x.txt", "common/ethics/"));
        assert!(!is_under("common/ethics_extra/x.txt", "common/ethics"));
        // Non-ASCII names, where byte offsets fall inside characters.
        assert!(!is_under("common/需要的/x.txt", "common/ethics"));
        assert!(!is_under("需要/x.txt", "common/a"));
        assert_eq!(object_category("common/需要的/x.txt").as_deref(), Some("common/需要的"));
        assert_eq!(object_category("common/İtems/x.txt").as_deref(), Some("common/İtems"));
    }

    #[test]
    fn localisation_headers_that_are_not_languages_yield_no_keys() {
        let src = "l_english:\n a:0 \"x\"\n../../evil:\n b:0 \"y\"\nl_simp_chinese:\n c:0 \"z\"\nl_braz_por:\n d:0 \"w\"\n";

        assert_eq!(localisation_keys(src), ["l_english:a", "l_simp_chinese:c", "l_braz_por:d"]);
    }

    #[test]
    fn inner_key_categories_use_unique_identifiers_not_wrapper_keywords() {
        let set_a = "component_set = { key = \"SET_A\" icon = \"GFX_a\" }\n";
        let set_b = "component_set = { key = \"SET_B\" icon = \"GFX_b\" }\n";
        assert_eq!(object_keys("common/component_sets", set_a).unwrap(), vec!["SET_A"]);
        assert_eq!(object_keys("common/component_sets", set_b).unwrap(), vec!["SET_B"]);

        let tmpl_1 = "@p = 10\nutility_component_template = { key = \"UTILITY_1\" size = aux }\n";
        let tmpl_2 = "utility_component_template = { key = \"UTILITY_2\" size = aux }\n";
        assert_eq!(object_keys("common/component_templates", tmpl_1).unwrap(), vec!["UTILITY_1"]);
        assert_eq!(object_keys("common/component_templates", tmpl_2).unwrap(), vec!["UTILITY_2"]);

        let amb_1 = "ambient_object = { name = \"amb_1\" entity = \"ent_1\" }\n";
        let amb_2 = "ambient_object = { name = \"amb_2\" entity = \"ent_2\" }\n";
        assert_eq!(object_keys("common/ambient_objects", amb_1).unwrap(), vec!["amb_1"]);
        assert_eq!(object_keys("common/ambient_objects", amb_2).unwrap(), vec!["amb_2"]);

        let msg_1 = "message_type = { key = \"MSG_1\" icon = \"GFX_1\" }\n";
        let msg_2 = "message_type = { key = \"MSG_2\" icon = \"GFX_2\" }\n";
        assert_eq!(object_keys("common/message_types", msg_1).unwrap(), vec!["MSG_1"]);
        assert_eq!(object_keys("common/message_types", msg_2).unwrap(), vec!["MSG_2"]);

        let ship_1 = "ship_design = { name = \"DESIGN_1\" ship_size = corvette }\n";
        let ship_2 = "ship_design = { name = \"DESIGN_2\" ship_size = corvette }\n";
        assert_eq!(object_keys("common/global_ship_designs", ship_1).unwrap(), vec!["DESIGN_1"]);
        assert_eq!(object_keys("common/global_ship_designs", ship_2).unwrap(), vec!["DESIGN_2"]);

        let proj_1 = "special_project = { key = \"PROJ_1\" days_to_research = 180 }\n";
        let proj_2 = "special_project = { key = \"PROJ_2\" days_to_research = 180 }\n";
        assert_eq!(object_keys("common/special_projects", proj_1).unwrap(), vec!["PROJ_1"]);
        assert_eq!(object_keys("common/special_projects", proj_2).unwrap(), vec!["PROJ_2"]);

        let root = std::env::temp_dir().join(format!("dm-inner-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("mod1"), &[
            ("common/component_sets/a.txt", set_a),
            ("common/component_templates/t1.txt", tmpl_1),
            ("common/ambient_objects/m1.txt", amb_1),
            ("common/message_types/msg1.txt", msg_1),
            ("common/global_ship_designs/s1.txt", ship_1),
            ("common/special_projects/p1.txt", proj_1),
        ]);
        write(&root.join("mod2"), &[
            ("common/component_sets/b.txt", set_b),
            ("common/component_templates/t2.txt", tmpl_2),
            ("common/ambient_objects/m2.txt", amb_2),
            ("common/message_types/msg2.txt", msg_2),
            ("common/global_ship_designs/s2.txt", ship_2),
            ("common/special_projects/p2.txt", proj_2),
        ]);

        let report = analyze(&[input(&root, "mod1", &[]), input(&root, "mod2", &[])], None);
        assert!(report.objects.is_empty(), "different keys and names do not conflict: {:?}", report.objects);

        write(&root.join("mod2"), &[
            ("common/component_sets/b.txt", set_a),
            ("common/component_templates/t2.txt", tmpl_1),
            ("common/ambient_objects/m2.txt", amb_1),
            ("common/message_types/msg2.txt", msg_1),
            ("common/global_ship_designs/s2.txt", ship_1),
            ("common/special_projects/p2.txt", proj_1),
        ]);
        let report_conflict = analyze(&[input(&root, "mod1", &[]), input(&root, "mod2", &[])], None);
        assert_eq!(report_conflict.objects.len(), 6, "same keys and names do conflict");
        let mut keys: Vec<String> = report_conflict.objects.into_iter().map(|o| o.key).collect();
        keys.sort();
        assert_eq!(keys, vec!["DESIGN_1", "MSG_1", "PROJ_1", "SET_A", "UTILITY_1", "amb_1"]);

        std::fs::remove_dir_all(root).unwrap();
    }
}
