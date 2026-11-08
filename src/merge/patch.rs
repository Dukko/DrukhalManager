//! The merged patch: a local mod holding every merged object, written so
//! its definitions win over the mods it merges.
//!
//! Within a folder the game reads files from all mods in file-name order.
//! For most `common/` folders the last definition read wins, so merged
//! objects go in files that sort after everything else; for the folders
//! where the first one wins (and for events) they sort first. The folder
//! list and the idea of non-ASCII prefixes that sort after `zzz_` come from
//! the patches Irony Mod Manager writes
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const NAME: &str = "DrukhalManager Merged Patch";
const FOLDER: &str = "drukhal_merged_patch";
/// How `dlc_load.json` refers to the patch.
pub const ID: &str = "mod/drukhal_merged_patch.mod";
const MANIFEST: &str = "drukhal_merges.json";

/// Folders where the first definition read wins.
const FIRST_WINS: &[&str] = &["events", "common/section_templates", "common/traits", "common/strategic_resources"];
/// UTF-8 sorts after every ASCII name, including `zzz_` and `~` prefixes
/// and Irony's own `伞伞zzz_` patches.
const SORT_LAST: &str = "伞伞伞_drukhal_";
const SORT_FIRST: &str = "!!!!!!_drukhal_";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merge {
    pub category: String,
    pub key: String,
    /// The merged definition as the user approved it.
    pub script: String,
    /// The mods it was merged from, in load order.
    pub sources: Vec<String>,
    /// Path of the generated file, relative to the patch folder.
    pub file: String,
}

/// A merged copy of a file several mods ship, saved at the same path so
/// it replaces theirs. This only works while the patch loads last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMerge {
    pub path: String,
    /// The mods it was merged from, in load order.
    pub sources: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    merges: Vec<Merge>,
    #[serde(default)]
    files: Vec<FileMerge>,
}

#[derive(Debug)]
pub struct Patch {
    root: PathBuf,
    manifest: Manifest,
}

impl Patch {
    pub fn root(user_dir: &Path) -> PathBuf {
        user_dir.join("mod").join(FOLDER)
    }

    /// Reads the patch, or starts an empty one if it doesn't exist yet.
    pub fn open(user_dir: &Path) -> io::Result<Self> {
        let root = Self::root(user_dir);
        let manifest = match std::fs::read_to_string(root.join(MANIFEST)) {
            Ok(src) => serde_json::from_str(&src).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Manifest::default(),
            Err(e) => return Err(e),
        };
        Ok(Self { root, manifest })
    }

    pub fn get(&self, category: &str, key: &str) -> Option<&Merge> {
        self.manifest.merges.iter().find(|m| m.category == category && m.key == key)
    }

    pub fn get_file(&self, path: &str) -> Option<&FileMerge> {
        self.manifest.files.iter().find(|f| f.path.eq_ignore_ascii_case(path))
    }

    /// Adds or replaces a merged copy of `path`.
    pub fn save_file(&mut self, user_dir: &Path, game_version: Option<&str>, path: &str, contents: &str, sources: Vec<String>) -> io::Result<()> {
        self.write_descriptors(user_dir, game_version)?;
        let target = self.root.join(path);
        std::fs::create_dir_all(target.parent().unwrap())?;
        write_atomic(&target, contents.as_bytes())?;
        let merge = FileMerge { path: path.to_owned(), sources };
        match self.manifest.files.iter_mut().find(|f| f.path.eq_ignore_ascii_case(path)) {
            Some(existing) => *existing = merge,
            None => self.manifest.files.push(merge),
        }
        self.write_manifest()
    }

    fn write_manifest(&self) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(&self.manifest).map_err(io::Error::other)?;
        write_atomic(&self.root.join(MANIFEST), &json)
    }

    /// Adds or replaces a merged object and writes it to disk, creating the
    /// mod's descriptors if needed. `game_version` (e.g. `v4.5.3`) sets the
    /// version the launcher shows the patch as supporting.
    pub fn save(&mut self, user_dir: &Path, game_version: Option<&str>, category: &str, key: &str, script: &str, sources: Vec<String>) -> io::Result<()> {
        self.write_descriptors(user_dir, game_version)?;
        let file = file_for(category, key);
        let path = self.root.join(&file);
        std::fs::create_dir_all(path.parent().unwrap())?;
        write_atomic(&path, render(category, key, script).as_bytes())?;

        let merge = Merge { category: category.into(), key: key.into(), script: script.into(), sources, file };
        match self.manifest.merges.iter_mut().find(|m| m.category == category && m.key == key) {
            Some(existing) => *existing = merge,
            None => self.manifest.merges.push(merge),
        }
        self.write_manifest()
    }

    fn write_descriptors(&self, user_dir: &Path, game_version: Option<&str>) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let version = game_version.and_then(|v| v.rsplit_once('.')).map_or("*".to_owned(), |(major_minor, _)| format!("{major_minor}.*"));
        let inner = format!("name=\"{NAME}\"\ntags={{\n\t\"Fixes\"\n}}\nsupported_version=\"{version}\"\n");
        let outer = format!("{inner}path=\"{}\"\n", self.root.display());
        write_atomic(&self.root.join("descriptor.mod"), inner.as_bytes())?;
        write_atomic(&user_dir.join(ID), outer.as_bytes())
    }
}

fn file_for(category: &str, key: &str) -> String {
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).collect();
    if category == "localisation" {
        // `l_english:key` goes to `localisation/replace/english/…_l_english.yml`;
        // the game requires the language suffix in the file name.
        let (language, name) = key.split_once(':').unwrap_or(("l_english", key));
        let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
        let folder = language.strip_prefix("l_").unwrap_or(language);
        return format!("localisation/replace/{folder}/drukhal_{name}_{language}.yml");
    }
    let prefix = if FIRST_WINS.contains(&category) { SORT_FIRST } else { SORT_LAST };
    format!("{category}/{prefix}{safe}.txt")
}

fn render(category: &str, key: &str, script: &str) -> String {
    let script = script.trim();
    match category {
        "localisation" => {
            let language = key.split_once(':').map_or("l_english", |(l, _)| l);
            // Localisation must be UTF-8 with a BOM.
            format!("\u{feff}{language}:\n {script}\n")
        }
        "events" => {
            let namespace = key.split_once('.').map_or(key, |(ns, _)| ns);
            format!("# Generated by DrukhalManager.\nnamespace = {namespace}\n\n{script}\n")
        }
        _ => format!("# Generated by DrukhalManager.\n{script}\n"),
    }
}

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("drukhal-tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_files_to_win() {
        assert_eq!(file_for("common/buildings", "building_lab"), "common/buildings/伞伞伞_drukhal_building_lab.txt");
        assert_eq!(file_for("common/traits", "trait_x"), "common/traits/!!!!!!_drukhal_trait_x.txt");
        assert_eq!(file_for("events", "ns.12"), "events/!!!!!!_drukhal_ns.12.txt");
        assert_eq!(file_for("localisation", "l_german:lab"), "localisation/replace/german/drukhal_lab_l_german.yml");
        assert!(SORT_LAST > "zzzz_" && SORT_LAST > "~ste" && SORT_LAST > "伞伞zzz_");
        assert!(SORT_FIRST < "!_pd" && SORT_FIRST < "00_");
    }

    #[test]
    fn saves_and_reopens() {
        let user = std::env::temp_dir().join(format!("drukhal-patch-{}", std::process::id()));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user).unwrap();
        patch.save(&user, Some("v4.5.3"), "events", "ns.1", "country_event = { id = ns.1 }", vec!["A".into(), "B".into()]).unwrap();
        patch.save(&user, Some("v4.5.3"), "localisation", "l_english:lab", "lab:0 \"Lab\"", vec![]).unwrap();
        patch.save(&user, Some("v4.5.3"), "events", "ns.1", "country_event = { id = ns.1 hide_window = yes }", vec!["A".into()]).unwrap();

        let patch = Patch::open(&user).unwrap();
        assert_eq!(patch.get("events", "ns.1").unwrap().sources, ["A"]);
        let event = std::fs::read_to_string(Patch::root(&user).join("events/!!!!!!_drukhal_ns.1.txt")).unwrap();
        assert!(event.contains("namespace = ns\n\ncountry_event = { id = ns.1 hide_window = yes }"));
        let loc = std::fs::read_to_string(Patch::root(&user).join("localisation/replace/english/drukhal_lab_l_english.yml")).unwrap();
        assert_eq!(loc, "\u{feff}l_english:\n lab:0 \"Lab\"\n");
        let descriptor = std::fs::read_to_string(user.join(ID)).unwrap();
        assert!(descriptor.contains("supported_version=\"v4.5.*\"") && descriptor.contains("path=\""));
        let parsed = crate::mods::Descriptor::parse(&descriptor).unwrap();
        assert_eq!(parsed.name, NAME);

        let mut patch = Patch::open(&user).unwrap();
        patch.save_file(&user, None, "common/buildings/00_x.txt", "a = {}\n", vec!["A".into(), "B".into()]).unwrap();
        let patch = Patch::open(&user).unwrap();
        assert!(patch.get("events", "ns.1").is_some(), "object merges survive");
        assert_eq!(patch.get_file("common/buildings/00_X.txt").unwrap().sources, ["A", "B"]);
        assert_eq!(std::fs::read_to_string(Patch::root(&user).join("common/buildings/00_x.txt")).unwrap(), "a = {}\n");

        std::fs::remove_dir_all(user).unwrap();
    }
}
