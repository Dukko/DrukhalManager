//! Finding where enabled mods step on each other.
//!
//! Two kinds of conflict are reported:
//!
//! - **File overrides**: several mods ship the same file path. The game loads
//!   only the copy from the mod that comes last in the load order.
//! - **Object conflicts**: different files define the same object, e.g. the
//!   same building in `common/buildings`, the same event id, or the same
//!   localisation key. Which definition wins depends on the folder's override
//!   rules and on file names, so no winner is claimed here.
//!
//! Only mod-vs-mod conflicts are reported; overriding vanilla is normal.
//! The approach (per-file indexing, `replace_path` dropping earlier files,
//! grouping objects by folder) follows Irony Mod Manager's conflict solver
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub mod_position: usize,
    pub file: String,
    pub message: String,
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

/// Folders whose objects the game merges rather than overrides.
const MERGED_FOLDERS: &[&str] = &["common/on_actions", "common/defines"];

/// Folders of script fragments pasted into other scripts, not objects.
pub const FRAGMENT_FOLDERS: &[&str] = &["common/inline_scripts"];

pub fn analyze(mods: &[ModInput]) -> Report {
    let mut report = Report::default();

    // Lowercased relative path -> (load position, path as spelled in that
    // mod) for every mod shipping it.
    let mut files: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    for (position, m) in mods.iter().enumerate() {
        let Some(root) = &m.root else {
            report.skipped.push((position, "Mod files not found".into()));
            continue;
        };
        if root.is_file() {
            // Zips are unpacked before scanning; this one couldn't be.
            report.skipped.push((position, "Could not unpack this zipped mod".into()));
            continue;
        }
        let mut found = Vec::new();
        if let Err(e) = walk(root, root, &mut found) {
            report.skipped.push((position, format!("Could not read mod files: {e}")));
            continue;
        }
        // A later `replace_path` makes the game ignore earlier mods' files in
        // that folder, so they cannot conflict.
        files.retain(|path, _| !m.replace_path.iter().any(|r| is_under(path, r)));
        for path in found {
            files.entry(path.to_lowercase()).or_default().push((position, path));
        }
    }

    // Objects come only from the copy of each file that actually loads.
    let mut objects: BTreeMap<(String, String), Vec<Definition>> = BTreeMap::new();
    for shipped in files.values() {
        let (winner, path) = shipped.last().unwrap();
        let winner = *winner;
        if shipped.len() > 1 {
            report.files.push(FileConflict { path: path.clone(), mods: shipped.iter().map(|(m, _)| *m).collect() });
        }
        let Some(category) = object_category(path) else { continue };
        let full = mods[winner].root.as_ref().unwrap().join(path);
        let src = match std::fs::read(&full) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                report.errors.push(FileError { mod_position: winner, file: path.clone(), message: e.to_string() });
                continue;
            }
        };
        let keys = match object_keys(&category, &src) {
            Ok(keys) => keys,
            Err(message) => {
                report.errors.push(FileError { mod_position: winner, file: path.clone(), message });
                continue;
            }
        };
        for key in keys {
            objects.entry((category.clone(), key)).or_default().push(Definition { mod_position: winner, file: path.clone() });
        }
    }

    for ((category, key), mut definitions) in objects {
        let distinct: BTreeSet<usize> = definitions.iter().map(|d| d.mod_position).collect();
        if distinct.len() > 1 {
            definitions.sort_by(|a, b| (a.mod_position, &a.file).cmp(&(b.mod_position, &b.file)));
            report.objects.push(ObjectConflict { category, key, definitions });
        }
    }
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
            out.push(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
        }
    }
    Ok(())
}

fn is_under(path: &str, folder: &str) -> bool {
    let folder = folder.trim_matches('/');
    path.len() > folder.len() && path[..folder.len()].eq_ignore_ascii_case(folder) && path.as_bytes()[folder.len()] == b'/'
}

/// The folder whose objects share a namespace, if the file defines objects
/// this module understands.
fn object_category(path: &str) -> Option<String> {
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
        return Some(path[..dir.len()].to_owned());
    }
    None
}

fn object_keys(category: &str, src: &str) -> Result<Vec<String>, String> {
    if category == "localisation" {
        return Ok(localisation_keys(src));
    }
    let entries = script::parse(src).map_err(|e| e.to_string())?;
    let keys = if category == "events" {
        // Events are `country_event = { id = ns.1 ... }`; the id is the key.
        entries
            .iter()
            .filter_map(|e| match e {
                Entry::Assign { key, value, .. } if !key.eq_ignore_ascii_case("namespace") && !key.starts_with('@') => value.as_block()?.scalar("id").map(str::to_owned),
                _ => None,
            })
            .collect()
    } else {
        let mut seen = BTreeMap::<&str, usize>::new();
        for e in &entries {
            // `@name = value` variables are file-local.
            if let Entry::Assign { key, .. } = e
                && !key.starts_with('@')
            {
                *seen.entry(key).or_default() += 1;
            }
        }
        // A key repeated within one file is a type keyword (e.g.
        // `ambient_object = { name = ... }`), not an object's name.
        seen.into_iter().filter(|&(_, n)| n == 1).map(|(k, _)| k.to_owned()).collect()
    };
    Ok(keys)
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
            language = key.to_owned();
            continue;
        }
        if !key.is_empty() && !key.contains([' ', '"']) {
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
    fn finds_file_and_object_conflicts() {
        let root = std::env::temp_dir().join(format!("drukhal-conflicts-{}", std::process::id()));
        write(&root.join("a"), &[
            ("descriptor.mod", "name=a"),
            ("common/buildings/a_buildings.txt", "@cost = 10\nbuilding_lab = { cost = @cost }\nbuilding_a = {}"),
            ("common/buildings/shared.txt", "building_shared = {}"),
            ("common/ambient_objects/a.txt", "ambient_object = { name = x }\nambient_object = { name = y }"),
            (".git/config", "[core]"),
            ("common/on_actions/a.txt", "on_game_start = { events = { a.1 } }"),
            ("events/a.txt", "namespace = a\ncountry_event = { id = a.1 }\ncountry_event = { id = shared.1 }"),
            ("localisation/english/a_l_english.yml", "\u{feff}l_english:\n building_lab:0 \"Lab\"\n only_a: \"A\"\n"),
        ]);
        write(&root.join("b"), &[
            ("descriptor.mod", "name=b"),
            ("common/buildings/b_buildings.txt", "building_lab = { cost = 20 }"),
            ("common/buildings/Shared.txt", "building_shared = { b = yes }"),
            ("common/ambient_objects/b.txt", "ambient_object = { name = z }\nambient_object = { name = w }"),
            (".git/config", "[core]"),
            ("common/on_actions/b.txt", "on_game_start = { events = { b.1 } }"),
            ("events/b.txt", "namespace = b\nship_event = { id = shared.1 }"),
            ("localisation/b_l_english.yml", "l_english:\n building_lab:0 \"Better Lab\"\n"),
            ("localisation/b_l_german.yml", "l_german:\n only_a:0 \"A\"\n"),
        ]);

        let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &[])]);

        assert_eq!(report.files, [FileConflict { path: "common/buildings/Shared.txt".into(), mods: vec![0, 1] }]);
        let objects: Vec<(&str, &str)> = report.objects.iter().map(|c| (c.category.as_str(), c.key.as_str())).collect();
        assert_eq!(objects, [("common/buildings", "building_lab"), ("events", "shared.1"), ("localisation", "l_english:building_lab")]);
        assert_eq!(report.objects[0].definitions[1], Definition { mod_position: 1, file: "common/buildings/b_buildings.txt".into() });
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.counts_by_mod(2), [4, 4]);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replace_path_hides_earlier_files() {
        let root = std::env::temp_dir().join(format!("drukhal-replace-{}", std::process::id()));
        write(&root.join("a"), &[("common/ethics/x.txt", "ethic_a = {}"), ("events/broken.txt", "country_event = {")]);
        write(&root.join("b"), &[("common/ethics/x.txt", "ethic_a = {}")]);

        let report = analyze(&[input(&root, "a", &[]), input(&root, "b", &["common/ethics"]), ModInput { root: None, replace_path: vec![] }]);
        assert!(report.files.is_empty());
        assert!(report.objects.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].file, "events/broken.txt");
        assert_eq!(report.skipped, [(2, "Mod files not found".to_owned())]);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn categorises_paths() {
        assert_eq!(object_category("common/governments/civics/x.txt").as_deref(), Some("common/governments/civics"));
        assert_eq!(object_category("common/on_actions/x.txt"), None);
        assert_eq!(object_category("events/x.txt").as_deref(), Some("events"));
        assert_eq!(object_category("common/inline_scripts/x/y.txt"), None);
        assert_eq!(object_category("localisation/replace/x_l_english.yml").as_deref(), Some("localisation"));
        assert_eq!(object_category("gfx/x.dds"), None);
        assert!(is_under("Common/Ethics/x.txt", "common/ethics/"));
        assert!(!is_under("common/ethics_extra/x.txt", "common/ethics"));
    }
}
