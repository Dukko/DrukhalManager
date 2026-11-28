//! A conflict scan of the load order, start to finish, without a UI: what
//! the Conflicts view shows comes from [`run`], and which conflicts the
//! player set aside from [`Ignores`].
//!
//! The steps are those the GTK app ran: the merged patch is left out (its
//! objects are resolutions, not conflicts), zipped mods are read from an
//! unpacked copy, files and verdicts come from the scan cache when they
//! haven't changed, and the conflicts that need no AI are worked out.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use super::conflicts::{self, FileConflict, ModInput, ObjectConflict, Report};
use super::ignore_rules::Rules;
use super::patch;
use super::scan_cache::ScanCache;
use crate::mods::{self, Mod, archive};

/// What a scan found, and what it was a scan of.
pub struct Analysis {
    /// The load order's revision when scanned, to tell when it's stale.
    pub revision: u64,
    /// The scanned mods' names, ids and folders (unpacked for zips), by
    /// scanned position: the load order without the merged patch.
    pub names: Vec<String>,
    pub ids: Vec<String>,
    pub roots: Vec<Option<PathBuf>>,
    pub report: Report,
    /// Object conflicts that need no AI, by index into `report.objects`,
    /// with why.
    pub simple: HashMap<usize, String>,
    /// For each scanned mod, the mods it depends on.
    pub depends_on: Vec<BTreeSet<usize>>,
    pub seconds: f32,
}

impl std::fmt::Debug for Analysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analysis").field("objects", &self.report.objects.len()).field("files", &self.report.files.len()).finish()
    }
}

impl Analysis {
    pub fn name(&self, position: usize) -> &str {
        &self.names[position]
    }
}

/// Scans the enabled mods of `mods` (indices in load order). Blocking, and
/// slow on a big load order: run it off the UI thread.
pub fn run(mods: &[Mod], enabled: &[usize], revision: u64, languages: &BTreeSet<String>, game_dir: Option<&std::path::Path>, game_version: Option<&str>) -> Analysis {
    let started = std::time::Instant::now();
    let scanned: Vec<&Mod> = enabled.iter().filter_map(|&i| mods.get(i)).filter(|m| !patch::is_patch(&m.id)).collect();
    let names: Vec<String> = scanned.iter().map(|m| mods::display_name(m).to_owned()).collect();
    let ids = scanned.iter().map(|m| m.id.clone()).collect();
    let descriptor_names: Vec<String> = scanned.iter().map(|m| m.descriptor.name.clone()).collect();
    let dependencies: Vec<Vec<String>> = scanned.iter().map(|m| m.descriptor.dependencies.clone()).collect();
    let depends_on = conflicts::depends_on(&descriptor_names, &dependencies);
    let mut inputs: Vec<ModInput> = scanned.iter().map(|m| ModInput { root: m.content.clone(), replace_path: m.descriptor.replace_path.clone() }).collect();
    // Zipped mods are read from an unpacked copy; one that can't be
    // unpacked stays a zip, which the scan reports.
    for input in &mut inputs {
        if let Some(root) = &input.root
            && let Ok(files) = archive::files(root)
        {
            input.root = Some(files);
        }
    }
    let roots: Vec<Option<PathBuf>> = inputs.iter().map(|i| i.root.clone().filter(|r| r.is_dir())).collect();
    crate::diagnostics::log!("scan", "Scanning {} mods for conflicts", inputs.len());
    let cache_path = ScanCache::path();
    let mut cache = ScanCache::load(&cache_path);
    let report = conflicts::analyze_cached(&inputs, Some(languages), &mut cache);
    let simple = super::trivial::simple_conflicts_cached(&report, &roots, &names, game_dir, game_version, &mut cache);
    if let Err(e) = cache.save(&cache_path) {
        crate::diagnostics::log!("scan", "Could not save the conflict scan cache: {e}");
    }
    let seconds = started.elapsed().as_secs_f32();
    crate::diagnostics::log!("scan", "Scan done in {seconds:.1}s: {} file and {} object conflicts, {} files with errors, {} mods skipped", report.files.len(), report.objects.len(), report.errors.len(), report.skipped.len());
    Analysis { revision, names, ids, roots, report, simple, depends_on, seconds }
}

// Keys for ignored conflicts, stored in the settings' `ignored` set. Kept
// as the GTK app wrote them, so existing settings still apply.
pub fn file_key(path: &str) -> String {
    format!("file:{}", path.to_lowercase())
}
pub fn object_key(category: &str, key: &str) -> String {
    format!("object:{category}:{key}")
}
pub fn files_folder_key(folder: &str) -> String {
    format!("files-in:{}", folder.to_lowercase())
}
pub fn objects_folder_key(category: &str) -> String {
    format!("objects-in:{category}")
}

pub fn file_folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// What hides conflicts: keys ignored one at a time or by folder, and
/// the ignore rules, which need the mod names by scanned position.
pub struct Ignores<'a> {
    pub keys: &'a BTreeSet<String>,
    pub rules: &'a Rules,
    pub names: &'a [String],
    /// Each mod's dependencies, when conflicts with them are hidden.
    pub dependencies: Option<&'a [BTreeSet<usize>]>,
}

impl Ignores<'_> {
    /// Whether a file override is ignored, directly, through its folder,
    /// or by a rule.
    pub fn file(&self, c: &FileConflict) -> bool {
        self.keys.contains(&file_key(&c.path))
            || self.keys.contains(&files_folder_key(file_folder(&c.path)))
            || self.rules.hiding_file(c, self.names).is_some()
            || self.dependencies.is_some_and(|d| c.builds_on_dependencies(d))
    }

    pub fn object(&self, c: &ObjectConflict) -> bool {
        self.keys.contains(&object_key(&c.category, &c.key))
            || self.keys.contains(&objects_folder_key(&c.category))
            || self.rules.hiding_object(c, self.names).is_some()
            || self.dependencies.is_some_and(|d| c.builds_on_dependencies(d))
    }
}

/// A rule line hiding every conflict between exactly these mods, for the
/// settings' ignore rules: `** | Mod A | Mod B`.
pub fn mods_rule(names: &[&str]) -> String {
    let mut names: Vec<&str> = names.to_vec();
    names.sort_unstable();
    names.dedup();
    format!("** | {}", names.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::conflicts::Definition;

    #[test]
    fn keys_and_rules_hide_conflicts() {
        let object = ObjectConflict { category: "common/buildings".into(), key: "lab".into(), definitions: vec![Definition { mod_position: 0, file: "common/buildings/a.txt".into() }, Definition { mod_position: 1, file: "common/buildings/b.txt".into() }] };
        let names = vec!["Mod A".to_owned(), "Mod B".to_owned()];
        let rules = Rules::parse("");
        let mut keys = BTreeSet::new();
        assert!(!Ignores { keys: &keys, rules: &rules, names: &names, dependencies: None }.object(&object));
        keys.insert(objects_folder_key("common/buildings"));
        assert!(Ignores { keys: &keys, rules: &rules, names: &names, dependencies: None }.object(&object));
        let rules = Rules::parse(&mods_rule(&["Mod B", "Mod A"]));
        assert!(Ignores { keys: &BTreeSet::new(), rules: &rules, names: &names, dependencies: None }.object(&object), "the rule names exactly these mods");
        let others = Rules::parse(&mods_rule(&["Mod A", "Mod C"]));
        assert!(!Ignores { keys: &BTreeSet::new(), rules: &others, names: &names, dependencies: None }.object(&object));
    }
}
