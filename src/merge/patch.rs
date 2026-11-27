//! The merged patch: a local mod holding every merged object, written so
//! its definitions win over the mods it merges.
//!
//! Within a folder the game reads files from all mods in file-name order.
//! For most `common/` folders the last definition read wins, so merged
//! objects go in files that sort after everything else; for the folders
//! where the first one wins (see [`conflicts::is_fios`]) they sort first.
//! The folder list and the idea of non-ASCII prefixes that sort after `zzz_` come from
//! the patches Irony Mod Manager writes
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::conflicts;
use super::source::{Files as Sources, Source};
use crate::paradox::script::{self, Entry, Value};

/// The folder of the one patch older versions shared between every
/// collection; it becomes the active collection's.
pub const LEGACY_FOLDER: &str = "drukhal_merged_patch";
/// Other collections' patches are in folders named with this.
const FOLDER_PREFIX: &str = "drukhal_patch_";
const MANIFEST: &str = "drukhal_merges.json";

/// Which patch: each collection has its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Its folder, and the name of its descriptor in the user's `mod`
    /// folder.
    pub folder: String,
    /// The name the launcher and the mod list show.
    pub name: String,
    /// Where its folder is when not in the user's `mod` folder, such as
    /// the Extra Mod Folder.
    pub dir: Option<PathBuf>,
}

impl Target {
    /// The patch of the collection `collection`, kept in `folder`.
    pub fn new(folder: &str, collection: &str) -> Self {
        Self { folder: folder.to_owned(), name: format!("{collection} (Patch)"), dir: None }
    }

    /// The same patch, kept in `dir` (`None`: the user's `mod` folder).
    pub fn in_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.dir = dir;
        self
    }

    /// How `dlc_load.json` and the mod list refer to it.
    pub fn id(&self) -> String {
        format!("mod/{}.mod", self.folder)
    }

    pub fn root(&self, user_dir: &Path) -> PathBuf {
        self.dir.clone().unwrap_or_else(|| user_dir.join("mod")).join(&self.folder)
    }
}

/// A collection's patch, for tests.
#[cfg(test)]
pub fn test_target() -> Target {
    Target::new("drukhal_patch_main", "Main")
}

/// Whether the mod `id` is one of DrukhalManager's merged patches.
pub fn is_patch(id: &str) -> bool {
    id.strip_prefix("mod/").and_then(|f| f.strip_suffix(".mod")).is_some_and(|f| f == LEGACY_FOLDER || f.starts_with(FOLDER_PREFIX))
}

/// A folder for a new patch of the collection `collection`, unlike any
/// `taken` says is in use.
pub fn folder_for(collection: &str, taken: impl Fn(&str) -> bool) -> String {
    let slug: String = collection.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let slug = slug.split('_').filter(|w| !w.is_empty()).collect::<Vec<_>>().join("_");
    let slug: String = if slug.is_empty() { "collection".into() } else { slug.chars().take(40).collect() };
    let base = format!("{FOLDER_PREFIX}{slug}");
    (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}_{n}") }).find(|f| !taken(f)).unwrap()
}

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
    /// What each source looked like when merged; empty for merges saved
    /// before fingerprints existed.
    #[serde(default)]
    pub prints: Vec<Print>,
    /// The game version it was last checked against, to notice what an
    /// update adds; `None` for merges saved before versions were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_version: Option<String>,
    /// Inline scripts merged with this object and saved to the patch, by
    /// script name. Removing the merge removes them too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<String>,
    /// Fingerprint of `script` when the user last kept it after a content
    /// check, so an intentional edit is not reported again. `None` for
    /// merges saved before content checks existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<String>,
}

/// A fingerprint of one source of a merge, to notice when it changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Print {
    /// Descriptor id (`mod/ugc_1.mod`), or [`VANILLA`] for the base game.
    pub mod_id: String,
    pub name: String,
    pub file: String,
    pub hash: String,
}

pub const VANILLA: &str = "vanilla";

/// Every place the enabled mods define an object: descriptor id, name, file.
pub type ObjectDefinitions<'a> = &'a dyn Fn(&str, &str) -> Vec<(String, String, String)>;

/// The mods that ship a file: descriptor id, name.
pub type FileShippers<'a> = &'a dyn Fn(&str) -> Vec<(String, String)>;

/// A stable fingerprint of script text that ignores formatting changes.
pub fn fingerprint(text: &str) -> String {
    // FNV-1a over the whitespace-separated words.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for word in text.split_whitespace() {
        for byte in word.bytes().chain(*b" ") {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

impl Print {
    pub fn new(mod_id: &str, name: &str, file: &str, text: &str) -> Self {
        Self { mod_id: mod_id.into(), name: name.into(), file: file.into(), hash: fingerprint(text) }
    }
}

/// A merge whose sources changed since it was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    /// `Some((category, key))` for an object merge, `None` for a file.
    pub object: Option<(String, String)>,
    /// The file path, for file merges and fixes; the generated file for
    /// object merges.
    pub path: String,
    pub reasons: Vec<String>,
    /// Saved before fingerprints existed, so changes can't be noticed;
    /// [`Patch::track`] starts tracking it.
    pub untracked: bool,
    /// Whether the base game changed it, rather than only its mods.
    pub base_game: bool,
    /// Whether the saved script itself no longer matches its sources, as
    /// opposed to a source having changed since it was merged.
    pub content: bool,
    /// For a "Use This Version" choice where only the picked mod changed,
    /// that mod's name: its new version can replace the old one.
    pub pick: Option<String>,
}

/// A "Use This Version" choice to bring up to date: the patch keeps one
/// mod's version unchanged, that mod has changed it since, and nothing
/// else in the conflict has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// `Some((category, key))` for an object, `None` for a file.
    pub object: Option<(String, String)>,
    /// The file's path, for a file.
    pub path: String,
    /// The picked mod's name.
    pub name: String,
    /// The picked mod's version now.
    pub contents: Vec<u8>,
    /// The merge's fingerprints with the picked mod's brought up to date.
    pub prints: Vec<Print>,
}

impl Pick {
    /// Whether `stale` is this pick's entry.
    pub fn matches(&self, stale: &Stale) -> bool {
        match (&self.object, &stale.object) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.path.eq_ignore_ascii_case(&stale.path),
            _ => false,
        }
    }
}

/// Why an untracked merge is listed.
pub const UNTRACKED: &str = "Merged by an older version, so changes to its mods can't be noticed";

/// A mod a merge's sources can be read from: descriptor id, name, folder.
pub type SourceMod = (String, String, PathBuf);

/// Fingerprints of an object as `mods` (with the file defining it) and the
/// base game define it now. Mods that no longer define it are left out.
pub fn object_prints(category: &str, key: &str, game_dir: Option<&Path>, mods: &[(SourceMod, String)]) -> Vec<Print> {
    let vanilla = game_dir.and_then(|dir| super::source::vanilla(dir, category, key));
    let mut prints: Vec<Print> = vanilla.iter().map(|v| Print::new(VANILLA, "Vanilla", &v.file, &v.text)).collect();
    for ((id, name, root), file) in mods {
        let text = std::fs::read(root.join(file)).ok().and_then(|b| super::source::extract(category, key, &String::from_utf8_lossy(&b)));
        if let Some(text) = text {
            prints.push(Print::new(id, name, file, &text));
        }
    }
    prints
}

/// Fingerprints of the file at `path` as `mods` and the base game ship it
/// now.
pub fn file_prints(path: &str, game_dir: Option<&Path>, mods: &[SourceMod]) -> Vec<Print> {
    let read = |dir: &Path| std::fs::read(dir.join(path)).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
    let vanilla = game_dir.and_then(read);
    let mut prints: Vec<Print> = vanilla.iter().map(|v| Print::new(VANILLA, "Vanilla", path, v)).collect();
    for (id, name, root) in mods {
        if let Some(text) = read(root) {
            prints.push(Print::new(id, name, path, &text));
        }
    }
    prints
}

/// A merged copy of a file several mods ship, saved at the same path so
/// it replaces theirs. This only works while the patch loads last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMerge {
    pub path: String,
    /// The mods it was merged from, in load order.
    pub sources: Vec<String>,
    #[serde(default)]
    pub prints: Vec<Print>,
    /// As [`Merge::game_version`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_version: Option<String>,
    /// Inline scripts merged with objects in this file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    merges: Vec<Merge>,
    #[serde(default)]
    files: Vec<FileMerge>,
}

impl Manifest {
    /// Refuses a manifest whose paths could leave the patch folder: they are
    /// joined onto it to move and delete files.
    fn parse(src: &str) -> io::Result<Self> {
        let manifest: Self = serde_json::from_str(src).map_err(io::Error::other)?;
        let unsafe_path = manifest.merges.iter().flat_map(|m| [&m.file, &m.category]).chain(manifest.files.iter().map(|f| &f.path)).find(|p| !is_inside(p));
        if let Some(path) = unsafe_path {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the patch lists \"{path}\", which is outside its folder")));
        }
        let bad_language = manifest.merges.iter().filter(|m| m.category == "localisation").filter_map(|m| m.key.split_once(':')).find(|(language, _)| !conflicts::is_language_header(language));
        let unsafe_script = manifest
            .merges
            .iter()
            .flat_map(|m| &m.scripts)
            .chain(manifest.files.iter().flat_map(|f| &f.scripts))
            .find(|name| super::inline_scripts::file_path(name).is_none());
        if let Some(name) = unsafe_script {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the patch lists the inline script \"{name}\", which is not a safe path")));
        }
        match bad_language {
            Some((language, _)) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("the patch lists the language \"{language}\", which is not one"))),
            None => Ok(manifest),
        }
    }
}

/// Whether `path` is relative and stays inside the folder it is joined to,
/// whichever separator it uses.
fn is_inside(path: &str) -> bool {
    path.split(['/', '\\']).all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(':'))
}

/// Checks the manifest of the patch in `root`, if it has one, as
/// [`Patch::open`] does.
pub fn check_manifest(root: &Path) -> io::Result<()> {
    match std::fs::read_to_string(root.join(MANIFEST)) {
        Ok(src) => Manifest::parse(&src).map(drop),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone)]
pub struct Patch {
    root: PathBuf,
    target: Target,
    manifest: Manifest,
}

impl Patch {
    /// Reads the patch, or starts an empty one if it doesn't exist yet.
    pub fn open(user_dir: &Path, target: &Target) -> io::Result<Self> {
        let root = target.root(user_dir);
        let manifest = match std::fs::read_to_string(root.join(MANIFEST)) {
            Ok(src) => Manifest::parse(&src)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Manifest::default(),
            Err(e) => return Err(e),
        };
        let mut patch = Self { root, target: target.clone(), manifest };
        patch.rename_misplaced()?;
        Ok(patch)
    }

    /// Moves merges saved under a name that no longer sorts where it must,
    /// as older versions did for some first-wins folders. Events using
    /// `base =` are left to [`Patch::relocate_base_events`], which knows
    /// where the event they inherit from is defined.
    fn rename_misplaced(&mut self) -> io::Result<()> {
        let mut moved = false;
        for merge in &mut self.manifest.merges {
            if merge.category == "events" && merge.script.contains("base") && event_base(&merge.script).is_some() {
                continue;
            }
            let wanted = file_for(&merge.category, &merge.key);
            if merge.file == wanted {
                continue;
            }
            let (from, to) = (self.root.join(&merge.file), self.root.join(&wanted));
            if from.is_file() {
                std::fs::create_dir_all(to.parent().unwrap())?;
                crate::platform::rename(&from, &to)?;
            }
            merge.file = wanted;
            moved = true;
        }
        if moved { self.write_manifest() } else { Ok(()) }
    }

    /// Moves merged events that inherit with `base =` to a file that loads
    /// after the event they inherit from. Existing patches saved before
    /// this rule get fixed here, without merging them again.
    fn relocate_base_events(&mut self, game_dir: &Path) -> io::Result<()> {
        let mut moved = false;
        for merge in &mut self.manifest.merges {
            if merge.category != "events" || merge.prints.is_empty() || !merge.script.contains("base") {
                continue;
            }
            let wanted = file_for_script(&merge.category, &merge.key, &merge.script, &merge.prints, Some(game_dir));
            if merge.file == wanted {
                continue;
            }
            let (from, to) = (self.root.join(&merge.file), self.root.join(&wanted));
            if from.is_file() {
                std::fs::create_dir_all(to.parent().unwrap())?;
                crate::platform::rename(&from, &to)?;
            }
            merge.file = wanted;
            moved = true;
        }
        if moved { self.write_manifest() } else { Ok(()) }
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn get(&self, category: &str, key: &str) -> Option<&Merge> {
        self.manifest.merges.iter().find(|m| m.category == category && m.key == key)
    }

    pub fn get_file(&self, path: &str) -> Option<&FileMerge> {
        self.manifest.files.iter().find(|f| f.path.eq_ignore_ascii_case(path))
    }

    /// Reads the saved file from the patch at `path`, if it exists.
    pub fn read_file(&self, path: &str) -> io::Result<String> {
        std::fs::read_to_string(self.root.join(path))
    }

    /// Adds or replaces a merged copy of `path`, or a chosen copy of an
    /// image or other binary file.
    pub fn save_file(&mut self, user_dir: &Path, game_version: Option<&str>, path: &str, contents: impl AsRef<[u8]>, prints: Vec<Print>) -> io::Result<()> {
        let result = self.save_file_unlogged(user_dir, game_version, path, contents, prints);
        crate::diagnostics::outcome("patch", || format!("Saved {path} to {}", self.target.name), result)
    }

    fn save_file_unlogged(&mut self, user_dir: &Path, game_version: Option<&str>, path: &str, contents: impl AsRef<[u8]>, prints: Vec<Print>) -> io::Result<()> {
        if !is_inside(path) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{path} is outside the patch folder")));
        }
        let sources = prints.iter().filter(|p| p.mod_id != VANILLA).map(|p| p.name.clone()).collect();
        self.write_descriptors(user_dir, game_version)?;
        let target = self.root.join(path);
        std::fs::create_dir_all(target.parent().unwrap())?;
        write_atomic(&target, contents.as_ref())?;
        let scripts = self
            .manifest
            .files
            .iter()
            .find(|f| f.path.eq_ignore_ascii_case(path))
            .map(|f| f.scripts.clone())
            .unwrap_or_default();
        let merge = FileMerge { path: path.to_owned(), sources, prints, game_version: game_version.map(str::to_owned), scripts };
        match self.manifest.files.iter_mut().find(|f| f.path.eq_ignore_ascii_case(path)) {
            Some(existing) => *existing = merge,
            None => self.manifest.files.push(merge),
        }
        self.write_manifest()
    }

    /// Removes a merged object and its file.
    pub fn remove(&mut self, category: &str, key: &str) -> io::Result<()> {
        let result = self.remove_unlogged(category, key);
        crate::diagnostics::outcome("patch", || format!("Removed {category}/{key} from {}", self.target.name), result)
    }

    fn remove_unlogged(&mut self, category: &str, key: &str) -> io::Result<()> {
        if let Some(at) = self.manifest.merges.iter().position(|m| m.category == category && m.key == key) {
            let file = &self.manifest.merges[at].file;
            if !is_inside(file) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{file} is outside the patch folder")));
            }
            let merge = self.manifest.merges.remove(at);
            remove_if_present(&self.root.join(&merge.file))?;
            for name in &merge.scripts {
                let shared = self
                    .manifest
                    .merges
                    .iter()
                    .any(|m| m.scripts.iter().any(|n| n.eq_ignore_ascii_case(name)))
                    || self
                        .manifest
                        .files
                        .iter()
                        .any(|f| f.scripts.iter().any(|n| n.eq_ignore_ascii_case(name)));
                if !shared {
                    self.remove_script(name)?;
                }
            }
        }
        self.write_manifest()
    }

    /// Records the inline scripts merged with an object, removing script
    /// files the object no longer needs.
    pub fn set_scripts(&mut self, category: &str, key: &str, scripts: Vec<String>) -> io::Result<()> {
        let safe: Vec<String> = scripts
            .into_iter()
            .filter(|name| super::inline_scripts::file_path(name).is_some())
            .collect();
        if let Some(at) = self
            .manifest
            .merges
            .iter()
            .position(|m| m.category == category && m.key == key)
        {
            let old = self.manifest.merges[at].scripts.clone();
            for name in old {
                if safe.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    continue;
                }
                let shared = self
                    .manifest
                    .merges
                    .iter()
                    .enumerate()
                    .any(|(i, m)| i != at && m.scripts.iter().any(|n| n.eq_ignore_ascii_case(&name)))
                    || self
                        .manifest
                        .files
                        .iter()
                        .any(|f| f.scripts.iter().any(|n| n.eq_ignore_ascii_case(&name)));
                if !shared {
                    self.remove_script(&name)?;
                }
            }
            self.manifest.merges[at].scripts = safe;
        }
        self.write_manifest()
    }

    fn remove_script(&mut self, name: &str) -> io::Result<()> {
        let Some(path) = super::inline_scripts::file_path(name) else {
            return Ok(());
        };
        remove_if_present(&self.root.join(&path))?;
        self.manifest
            .files
            .retain(|f| !f.path.eq_ignore_ascii_case(&path));
        for file in &mut self.manifest.files {
            file.scripts.retain(|n| {
                !super::inline_scripts::file_path(n)
                    .is_some_and(|script| script.eq_ignore_ascii_case(&path))
            });
        }
        Ok(())
    }

    /// Records the inline scripts merged with objects in a file.
    pub fn set_file_scripts(&mut self, path: &str, scripts: Vec<String>) -> io::Result<()> {
        let safe: Vec<String> = scripts
            .into_iter()
            .filter(|name| super::inline_scripts::file_path(name).is_some())
            .collect();
        if let Some(at) = self
            .manifest
            .files
            .iter()
            .position(|f| f.path.eq_ignore_ascii_case(path))
        {
            let old = self.manifest.files[at].scripts.clone();
            for name in old {
                if safe.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    continue;
                }
                let shared = self
                    .manifest
                    .merges
                    .iter()
                    .any(|m| m.scripts.iter().any(|n| n.eq_ignore_ascii_case(&name)))
                    || self
                        .manifest
                        .files
                        .iter()
                        .enumerate()
                        .any(|(i, f)| i != at && f.scripts.iter().any(|n| n.eq_ignore_ascii_case(&name)));
                if !shared {
                    self.remove_script(&name)?;
                }
            }
            self.manifest.files[at].scripts = safe;
        }
        self.write_manifest()
    }

    /// Removes a merged or fixed file.
    pub fn remove_file(&mut self, path: &str) -> io::Result<()> {
        let result = self.remove_file_unlogged(path);
        crate::diagnostics::outcome("patch", || format!("Removed {path} from {}", self.target.name), result)
    }

    fn remove_file_unlogged(&mut self, path: &str) -> io::Result<()> {
        if let Some(at) = self.manifest.files.iter().position(|f| f.path.eq_ignore_ascii_case(path)) {
            let merge = self.manifest.files.remove(at);
            remove_if_present(&self.root.join(&merge.path))?;
            for name in &merge.scripts {
                let shared = self
                    .manifest
                    .merges
                    .iter()
                    .any(|m| m.scripts.iter().any(|n| n.eq_ignore_ascii_case(name)))
                    || self
                        .manifest
                        .files
                        .iter()
                        .any(|f| f.scripts.iter().any(|n| n.eq_ignore_ascii_case(name)));
                if !shared {
                    self.remove_script(name)?;
                }
            }
        }
        for merge in &mut self.manifest.merges {
            merge.scripts.retain(|name| {
                !super::inline_scripts::file_path(name)
                    .is_some_and(|script| script.eq_ignore_ascii_case(path))
            });
        }
        self.write_manifest()
    }

    /// The "Use This Version" choices whose mod has changed the version
    /// they kept, while nothing else in the conflict changed: those can
    /// take the mod's new version without asking again. `roots` is as for
    /// [`Patch::check`].
    pub fn picks_to_update(&self, roots: &HashMap<String, PathBuf>) -> Vec<Pick> {
        self.picks_with(roots, &Sources::default())
    }

    /// [`Patch::picks_to_update`], reading source files through `sources`.
    fn picks_with(&self, roots: &HashMap<String, PathBuf>, sources: &Sources) -> Vec<Pick> {
        let game_dir = roots.get(VANILLA).map(PathBuf::as_path);
        let mut picks = Vec::new();
        for m in &self.manifest.merges {
            let read = |root: &Path, file: &str| sources.object(root, file, &m.category, &m.key);
            if let Some((i, now, prints)) = picked_update(&m.script, &m.prints, roots, game_dir, &read) {
                picks.push(Pick { object: Some((m.category.clone(), m.key.clone())), path: m.file.clone(), name: m.prints[i].name.clone(), contents: now.into_bytes(), prints });
            }
        }
        for f in &self.manifest.files {
            let Some(kept) = read_text(&self.root, &f.path) else { continue };
            let read = |root: &Path, file: &str| sources.text(root, file).map(|t| t.as_str().to_owned());
            let Some((i, _, prints)) = picked_update(&kept, &f.prints, roots, game_dir, &read) else { continue };
            // The file as the mod ships it, byte for byte.
            let Some(contents) = roots.get(&f.prints[i].mod_id).and_then(|root| std::fs::read(root.join(&f.prints[i].file)).ok()) else { continue };
            picks.push(Pick { object: None, path: f.path.clone(), name: f.prints[i].name.clone(), contents, prints });
        }
        picks
    }

    /// Notes on `stale` which entries are among `picks`.
    pub fn mark_picks(stale: &mut [Stale], picks: &[Pick]) {
        for s in stale {
            s.pick = picks.iter().find(|p| p.matches(s)).map(|p| p.name.clone());
        }
    }

    /// Saves each pick's new version in place of the old one.
    pub fn update_picks(&mut self, user_dir: &Path, game_version: Option<&str>, picks: Vec<Pick>) -> io::Result<()> {
        for pick in picks {
            match &pick.object {
                Some((category, key)) => self.save(user_dir, game_version, category, key, &String::from_utf8_lossy(&pick.contents), pick.prints)?,
                None => self.save_file(user_dir, game_version, &pick.path, &pick.contents, pick.prints)?,
            }
        }
        Ok(())
    }

    /// Compares every merge with its sources as they are now and returns the
    /// ones that changed since they were saved. `roots` maps the enabled
    /// mods' descriptor ids, and [`VANILLA`], to their folders; without the
    /// base game's folder its versions aren't checked. `game_version` is the
    /// installed game's (e.g. `v4.5.3`).
    ///
    /// Along the way it records the game version each merge was checked
    /// against, so that after an update it also notices definitions the base
    /// game added, and follows base-game definitions that moved to another
    /// file. That bookkeeping is saved if it changed.
    #[cfg(test)]
    pub fn recheck(&mut self, roots: &HashMap<String, PathBuf>, game_version: Option<&str>) -> Vec<Stale> {
        self.recheck_with(roots, game_version, &Sources::default(), &|_, _| Vec::new(), &|_, _| Vec::new(), &|_| Vec::new())
    }

    /// [`Patch::check`], with each entry's pick marked as
    /// [`Patch::mark_picks`] does, reading each source file once for both.
    #[cfg(test)]
    pub fn check(&mut self, roots: &HashMap<String, PathBuf>, game_version: Option<&str>) -> Vec<Stale> {
        self.check_with(roots, game_version, &|_, _| Vec::new(), &|_| Vec::new())
    }

    /// [`Patch::check`], also noticing definitions the enabled mods *added*
    /// since a merge was made. `definitions` returns where every enabled mod
    /// currently defines `(category, key)`, as `(descriptor id, name, file)`;
    /// the conflicts scan already knows this. Without it a merge only
    /// rechecks the mods it was made from, so a mod that starts defining the
    /// object later (or was enabled later) is missed and the patch keeps
    /// dropping its version.
    /// `shippers` does the same for a merged file: the mods that currently
    /// ship `path`, as `(descriptor id, name)`. A file merge only rechecks the
    /// mods it was made from too, so a mod that starts shipping the file is
    /// missed the same way.
    pub fn check_with(
        &mut self,
        roots: &HashMap<String, PathBuf>,
        game_version: Option<&str>,
        definitions: ObjectDefinitions<'_>,
        shippers: FileShippers<'_>,
    ) -> Vec<Stale> {
        // Sharing one reader is safe: `recheck_with` writes only the patch's
        // manifest, never a source file the reader has cached.
        let sources = Sources::default();
        let previous = std::cell::OnceCell::new();
        let previous_game = |category: &str, key: &str| {
            let snapshots = previous.get_or_init(|| game_version.map(|v| super::base_game::older(&super::base_game::dir(), v).collect::<Vec<_>>()).unwrap_or_default());
            snapshots
                .iter()
                .filter_map(|s| s.get(category, key).map(|saved| Source { label: s.version.clone(), file: saved.file.clone(), text: saved.text.clone() }))
                .collect()
        };
        let mut stale = self.recheck_with(roots, game_version, &sources, &previous_game, definitions, shippers);
        Self::mark_picks(&mut stale, &self.picks_with(roots, &sources));
        stale
    }

    /// [`Patch::check`], reading source files through `sources`.
    fn recheck_with(
        &mut self,
        roots: &HashMap<String, PathBuf>,
        game_version: Option<&str>,
        sources: &Sources,
        previous_game: &dyn Fn(&str, &str) -> Vec<Source>,
        definitions: ObjectDefinitions<'_>,
        shippers: FileShippers<'_>,
    ) -> Vec<Stale> {
        let game_dir = roots.get(VANILLA).map(PathBuf::as_path);
        if let Some(dir) = game_dir
            && let Err(e) = self.relocate_base_events(dir)
        {
            crate::diagnostics::log!("patch", "Could not move an event that inherits with base =: {e}");
        }
        let updated = |checked: Option<&str>| matches!((checked, game_version), (Some(was), Some(now)) if was != now);
        let changed_in = |checked: Option<&str>| match (updated(checked), game_version) {
            (true, Some(now)) => tf!("The base game changed it in {version}", version = now),
            _ => t!("The base game changed it in an update"),
        };
        let has_vanilla = |prints: &[Print]| prints.iter().any(|p| p.mod_id == VANILLA);

        // Objects the base game didn't define when merged, but might since an
        // update: looked up per folder, reading each game file once.
        let mut added: HashSet<(String, String)> = HashSet::new();
        if let (Some(dir), Some(_)) = (game_dir, game_version) {
            let mut wanted: HashMap<&str, Vec<String>> = HashMap::new();
            for m in &self.manifest.merges {
                if !m.prints.is_empty() && !has_vanilla(&m.prints) && updated(m.game_version.as_deref()) {
                    wanted.entry(&m.category).or_default().push(m.key.clone());
                }
            }
            for (category, keys) in wanted {
                added.extend(super::source::vanilla_many(dir, category, &keys).into_keys().map(|k| (category.to_owned(), k)));
            }
        }

        let mut stale = Vec::new();
        let mut dirty = false;
        for m in &mut self.manifest.merges {
            let (category, key) = (m.category.clone(), m.key.clone());
            if m.prints.is_empty() {
                stale.push(Stale { object: Some((category, key)), path: m.file.clone(), reasons: vec![UNTRACKED.to_owned()], untracked: true, base_game: false, content: false, pick: None });
                continue;
            }
            let read = |root: &Path, file: &str| sources.object(root, file, &category, &key);
            let mut check = Check::default();
            for p in &mut m.prints {
                if p.mod_id != VANILLA {
                    let Some(root) = roots.get(&p.mod_id) else {
                        check.reasons.push(tf!("{name} isn't enabled any more", name = &p.name));
                        continue;
                    };
                    let mut now = read(root, &p.file);
                    if now.is_none() {
                        // Mod updates often move definitions between files.
                        let moved = super::source::vanilla_files(root, &category).into_iter().find_map(|path| {
                            let file = crate::platform::game_path(path.strip_prefix(root).ok()?);
                            let text = read(root, &file)?;
                            Some((file, text))
                        });
                        if let Some((file, text)) = moved {
                            if fingerprint(&text) == p.hash {
                                p.file = file;
                                dirty = true;
                            }
                            now = Some(text);
                        }
                    }
                    match now {
                        None => check.reasons.push(tf!("{name} no longer defines it", name = &p.name)),
                        Some(text) if fingerprint(&text) != p.hash => check.reasons.push(tf!("{name} changed it", name = &p.name)),
                        Some(_) => {}
                    }
                    continue;
                }
                let Some(dir) = game_dir else { continue };
                let mut now = read(dir, &p.file);
                if now.is_none() {
                    // Updates often move definitions between files.
                    let moved = super::source::vanilla_files(dir, &category).into_iter().find_map(|path| {
                        let file = crate::platform::game_path(path.strip_prefix(dir).ok()?);
                        let text = read(dir, &file)?;
                        Some((file, text))
                    });
                    if let Some((file, text)) = moved {
                        if fingerprint(&text) == p.hash {
                            p.file = file;
                            dirty = true;
                        }
                        now = Some(text);
                    }
                }
                match now {
                    None => check.base_game(t!("The base game no longer defines it")),
                    Some(text) if fingerprint(&text) != p.hash => check.base_game(changed_in(m.game_version.as_deref())),
                    Some(_) => {}
                }
            }
            if added.contains(&(category.clone(), key.clone())) {
                check.base_game(tf!("The base game added it in {version}", version = game_version.unwrap_or_default()));
            }
            // A mod that wasn't part of the merge may define the object now
            // (enabled later, or it gained the definition in an update). The
            // saved merge has never seen its version, so it is out of date
            // even when every recorded source is unchanged.
            let known: std::collections::HashSet<&str> = m.prints.iter().map(|p| p.mod_id.as_str()).collect();
            let mut reported = std::collections::BTreeSet::new();
            for (id, name, _) in definitions(&category, &key) {
                if id != VANILLA && !known.contains(id.as_str()) && reported.insert(id) {
                    check.reasons.push(tf!("{name} also defines it, and was not merged", name = &name));
                }
            }
            dirty |= check.confirm(&mut m.game_version, game_dir, game_version);
            let mut content = false;
            if check.reasons.is_empty() {
                let hash = fingerprint(&m.script);
                if m.accepted.as_deref() != Some(hash.as_str()) {
                    // One source's version kept whole is a deliberate choice.
                    let kept_whole = m.prints.iter().any(|p| p.mod_id != VANILLA && p.hash == hash);
                    let outcome = if kept_whole { MergeContent::Clean } else { merge_content(m, roots, sources, previous_game) };
                    match outcome {
                        MergeContent::Problems(problems) => {
                            check.reasons.push(tf!(
                                "The saved merge no longer combines its sources cleanly: {problems}",
                                problems = problems.join("; ")
                            ));
                            content = true;
                        }
                        MergeContent::Clean => {
                            m.accepted = Some(hash);
                            dirty = true;
                        }
                        MergeContent::Unavailable => {}
                    }
                }
            }
            if !check.reasons.is_empty() {
                stale.push(Stale { object: Some((category, key)), path: m.file.clone(), base_game: check.base_game, reasons: check.reasons, untracked: false, content, pick: None });
            }
        }
        for f in &mut self.manifest.files {
            if f.prints.is_empty() {
                stale.push(Stale { object: None, path: f.path.clone(), reasons: vec![UNTRACKED.to_owned()], untracked: true, base_game: false, content: false, pick: None });
                continue;
            }
            let read = |root: &Path, file: &str| sources.text(root, file).map(|t| t.as_str().to_owned());
            let mut check = Check::default();
            for p in &f.prints {
                if p.mod_id != VANILLA {
                    check.mod_print(p, roots, &read, &t!("no longer ships this file"));
                    continue;
                }
                let Some(dir) = game_dir else { continue };
                match read(dir, &p.file) {
                    None => check.base_game(t!("The base game no longer ships this file")),
                    Some(text) if fingerprint(&text) != p.hash => check.base_game(changed_in(f.game_version.as_deref())),
                    Some(_) => {}
                }
            }
            if let Some(dir) = game_dir
                && !has_vanilla(&f.prints)
                && updated(f.game_version.as_deref())
                && dir.join(&f.path).is_file()
            {
                check.base_game(tf!("The base game added this file in {version}", version = game_version.unwrap_or_default()));
            }
            // As with objects: a mod the merge was not made from may ship the
            // file now, and its version would be dropped silently.
            let known: std::collections::HashSet<&str> = f.prints.iter().map(|p| p.mod_id.as_str()).collect();
            let mut reported = std::collections::BTreeSet::new();
            for (id, name) in shippers(&f.path) {
                if id != VANILLA && !known.contains(id.as_str()) && reported.insert(id) {
                    check.reasons.push(tf!("{name} also ships it, and was not merged", name = &name));
                }
            }
            dirty |= check.confirm(&mut f.game_version, game_dir, game_version);
            if !check.reasons.is_empty() {
                stale.push(Stale { object: None, path: f.path.clone(), base_game: check.base_game, reasons: check.reasons, untracked: false, content: false, pick: None });
            }
        }
        if dirty && let Err(e) = self.write_manifest() {
            // Only bookkeeping: the next check redoes it.
            crate::diagnostics::log!("patch", "Could not update the merged patch's records: {e}");
        }
        stale
    }

    /// The mod names a merge was made from, for `object` (`(category,
    /// key)`) or, if `None`, the merged file at `path`.
    pub fn sources(&self, object: Option<&(String, String)>, path: &str) -> Option<&[String]> {
        match object {
            Some((category, key)) => self.get(category, key).map(|m| m.sources.as_slice()),
            None => self.get_file(path).map(|f| f.sources.as_slice()),
        }
    }

    /// Records `prints` as what a merge's sources look like in game version
    /// `game_version`, so later changes to them are noticed. For merges
    /// saved before fingerprints.
    /// Accepts a saved merge's script as it is, so the content check does
    /// not report it again until the script or a source changes.
    pub fn accept(&mut self, category: &str, key: &str) -> io::Result<()> {
        let Some(merge) = self.manifest.merges.iter_mut().find(|m| m.category == category && m.key == key) else {
            return Err(io::Error::new(io::ErrorKind::NotFound, t!("It is no longer in the patch")));
        };
        merge.accepted = Some(fingerprint(&merge.script));
        self.write_manifest()
    }

    pub fn track(&mut self, object: Option<&(String, String)>, path: &str, prints: Vec<Print>, game_version: Option<&str>) -> io::Result<()> {
        // The tracked prints are what the merge now counts as its sources;
        // keep the recorded names in step with them.
        let sources: Vec<String> = prints.iter().filter(|p| p.mod_id != VANILLA).map(|p| p.name.clone()).collect();
        match object {
            Some((category, key)) => {
                let Some(merge) = self.manifest.merges.iter_mut().find(|m| &m.category == category && &m.key == key) else {
                    return Err(io::Error::new(io::ErrorKind::NotFound, t!("It is no longer in the patch")));
                };
                merge.prints = prints;
                merge.sources = sources;
                merge.game_version = game_version.map(str::to_owned);
                merge.accepted = Some(fingerprint(&merge.script));
            }
            None => {
                let Some(merge) = self.manifest.files.iter_mut().find(|f| f.path.eq_ignore_ascii_case(path)) else {
                    return Err(io::Error::new(io::ErrorKind::NotFound, t!("It is no longer in the patch")));
                };
                merge.prints = prints;
                merge.sources = sources;
                merge.game_version = game_version.map(str::to_owned);
            }
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
    #[allow(clippy::too_many_arguments)]
    pub fn save(&mut self, user_dir: &Path, game_version: Option<&str>, category: &str, key: &str, script: &str, prints: Vec<Print>) -> io::Result<()> {
        let file = file_for(category, key);
        self.save_at(user_dir, game_version, category, key, &file, script, prints)
    }

    /// Like [`Patch::save`], placing an event after the event it inherits
    /// from with `base =`, so the inheritance is defined when it is read.
    #[allow(clippy::too_many_arguments)]
    pub fn save_for_game(
        &mut self,
        user_dir: &Path,
        game_version: Option<&str>,
        game_dir: Option<&Path>,
        category: &str,
        key: &str,
        script: &str,
        prints: Vec<Print>,
    ) -> io::Result<()> {
        let file = file_for_script(category, key, script, &prints, game_dir);
        self.save_at(user_dir, game_version, category, key, &file, script, prints)
    }

    #[allow(clippy::too_many_arguments)]
    fn save_at(&mut self, user_dir: &Path, game_version: Option<&str>, category: &str, key: &str, file: &str, script: &str, prints: Vec<Print>) -> io::Result<()> {
        let result = self.save_unlogged(user_dir, game_version, category, key, file, script, prints);
        crate::diagnostics::outcome("patch", || format!("Saved {category}/{key} to {}", self.target.name), result)
    }

    #[allow(clippy::too_many_arguments)]
    fn save_unlogged(&mut self, user_dir: &Path, game_version: Option<&str>, category: &str, key: &str, file: &str, script: &str, prints: Vec<Print>) -> io::Result<()> {
        let sources = prints.iter().filter(|p| p.mod_id != VANILLA).map(|p| p.name.clone()).collect();
        self.write_descriptors(user_dir, game_version)?;
        let file = file.to_owned();
        let path = self.root.join(&file);
        std::fs::create_dir_all(path.parent().unwrap())?;
        write_atomic(&path, render(category, key, script).as_bytes())?;

        let scripts = self
            .manifest
            .merges
            .iter()
            .find(|m| m.category == category && m.key == key)
            .map(|m| m.scripts.clone())
            .unwrap_or_default();
        let accepted = self
            .manifest
            .merges
            .iter()
            .find(|m| m.category == category && m.key == key && m.script == script)
            .and_then(|m| m.accepted.clone());
        let merge = Merge { category: category.into(), key: key.into(), script: script.into(), sources, file, prints, game_version: game_version.map(str::to_owned), scripts, accepted };
        match self.manifest.merges.iter_mut().find(|m| m.category == category && m.key == key) {
            Some(existing) => {
                if existing.file != merge.file {
                    remove_if_present(&self.root.join(&existing.file))?;
                }
                *existing = merge;
            }
            None => self.manifest.merges.push(merge),
        }
        self.write_manifest()
    }

    fn write_descriptors(&self, user_dir: &Path, game_version: Option<&str>) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let version = supported_version(game_version);
        let inner = format!("name=\"{}\"\ntags={{\n\t\"Fixes\"\n}}\nsupported_version=\"{version}\"\n", descriptor_text(&self.target.name));
        write_atomic(&self.root.join("descriptor.mod"), inner.as_bytes())?;
        write_outer(user_dir, &self.target, &inner)
    }
}

/// Writes the descriptor the launcher finds the patch by from the one in
/// its folder, as after restoring or importing it, named for its
/// collection.
pub fn link(user_dir: &Path, target: &Target) -> io::Result<()> {
    let root = target.root(user_dir);
    match std::fs::read_to_string(root.join("descriptor.mod")) {
        Ok(inner) => {
            let named = format!("name=\"{}\"", descriptor_text(&target.name));
            let inner: String = inner.lines().map(|l| if l.trim_start().starts_with("name=") || l.trim_start().starts_with("name =") { named.as_str() } else { l }).collect::<Vec<_>>().join("\n") + "\n";
            write_atomic(&root.join("descriptor.mod"), inner.as_bytes())?;
            write_outer(user_dir, target, &inner)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Moves the patch `from` to where `to` keeps it, and points the
/// descriptor the launcher finds it by there. Moving between drives
/// copies, then deletes the old folder.
pub fn relocate(user_dir: &Path, from: &Target, to: &Target) -> io::Result<()> {
    let result = relocate_unlogged(user_dir, from, to);
    crate::diagnostics::outcome("patch", || format!("Moved {} to {}", from.name, to.root(user_dir).display()), result)
}

fn relocate_unlogged(user_dir: &Path, from: &Target, to: &Target) -> io::Result<()> {
    let (old, new) = (from.root(user_dir), to.root(user_dir));
    if new.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", new.display())));
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if crate::platform::rename(&old, &new).is_err() {
        // Another drive: copy, and only then delete.
        let copied = super::snapshot::copy_dir(&old, &new);
        if let Err(e) = copied {
            let _ = std::fs::remove_dir_all(&new);
            return Err(e);
        }
        std::fs::remove_dir_all(&old)?;
    }
    link(user_dir, to)
}

/// Deletes the patch `target` and the descriptor the launcher finds it by.
pub fn delete(user_dir: &Path, target: &Target) -> io::Result<()> {
    let result = remove_if_present(&user_dir.join(target.id())).and_then(|()| match std::fs::remove_dir_all(target.root(user_dir)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    });
    crate::diagnostics::outcome("patch", || format!("Deleted {}", target.name), result)
}

/// `text` as a descriptor's quoted value.
fn descriptor_text(text: &str) -> String {
    text.replace('"', "'")
}

fn write_outer(user_dir: &Path, target: &Target, inner: &str) -> io::Result<()> {
    let outer = format!("{}\npath=\"{}\"\n", inner.trim_end(), crate::platform::descriptor_path(&target.root(user_dir)));
    write_atomic(&user_dir.join(target.id()), outer.as_bytes())
}

/// How many merged objects and files the patch copy at `dir` holds.
pub fn summary(dir: &Path) -> Option<(usize, usize)> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).ok()?).ok()?;
    Some((manifest.merges.len(), manifest.files.len()))
}

/// What [`Patch::check`] found about one merge's sources.
#[derive(Default)]
struct Check {
    reasons: Vec<String>,
    /// Whether the base game's version changed, so the merge can't be
    /// counted as checked against the current game.
    base_game: bool,
}

impl Check {
    fn base_game(&mut self, reason: String) {
        self.reasons.push(reason);
        self.base_game = true;
    }

    /// Compares a mod's source with `p`, `missing` saying what it means when
    /// the mod no longer has it.
    fn mod_print(&mut self, p: &Print, roots: &HashMap<String, PathBuf>, read: &dyn Fn(&Path, &str) -> Option<String>, missing: &str) {
        let Some(root) = roots.get(&p.mod_id) else {
            self.reasons.push(tf!("{name} isn't enabled any more", name = &p.name));
            return;
        };
        match read(root, &p.file) {
            None => self.reasons.push(tf!("{name} {missing}", name = &p.name, missing = missing)),
            Some(text) if fingerprint(&text) != p.hash => self.reasons.push(tf!("{name} changed it", name = &p.name)),
            Some(_) => {}
        }
    }

    /// Records the merge as checked against `game_version`, unless the base
    /// game changed it or wasn't looked at; whether that changed `checked`.
    /// Merges from before versions were recorded are taken as made against
    /// the current game, as [`Patch::track`] does for their mods.
    fn confirm(&self, checked: &mut Option<String>, game_dir: Option<&Path>, game_version: Option<&str>) -> bool {
        match (game_dir, game_version) {
            (Some(_), Some(now)) if !self.base_game && checked.as_deref() != Some(now) => {
                *checked = Some(now.to_owned());
                true
            }
            _ => false,
        }
    }
}

/// Of `prints`, the mod one `kept` (the text the patch holds) is a copy
/// of, if that mod changed it since while every other source is as it was:
/// its new version and the updated prints. `read` gives a source's
/// version from its folder and file; the base game is only checked when
/// `game_dir` is known, as [`Patch::check`] does.
fn picked_update(kept: &str, prints: &[Print], roots: &HashMap<String, PathBuf>, game_dir: Option<&Path>, read: &dyn Fn(&Path, &str) -> Option<String>) -> Option<(usize, String, Vec<Print>)> {
    let kept = fingerprint(kept);
    let picked = prints.iter().position(|p| p.mod_id != VANILLA && p.hash == kept)?;
    let mut now = None;
    for (i, p) in prints.iter().enumerate() {
        let root = if p.mod_id == VANILLA {
            match game_dir {
                Some(dir) => dir,
                None => continue,
            }
        } else {
            roots.get(&p.mod_id)?
        };
        let text = read(root, &p.file)?;
        let same = fingerprint(&text) == p.hash;
        match (i == picked, same) {
            (true, false) => now = Some(text),
            (false, true) => {}
            // The picked mod didn't change, or another source did.
            _ => return None,
        }
    }
    let now = now?;
    let mut updated = prints.to_vec();
    updated[picked].hash = fingerprint(&now);
    Some((picked, now, updated))
}

fn read_text(root: &Path, file: &str) -> Option<String> {
    std::fs::read(root.join(file)).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A descriptor's `supported_version` for the installed game: `v4.5.3`
/// gives `v4.5.*`, unknown gives `*`.
pub fn supported_version(game_version: Option<&str>) -> String {
    game_version.and_then(|v| v.rsplit_once('.')).map_or("*".to_owned(), |(major_minor, _)| format!("{major_minor}.*"))
}

fn safe_name(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).collect()
}

fn file_for(category: &str, key: &str) -> String {
    let safe = safe_name(key);
    if category == "localisation" {
        // `l_english:key` goes to `localisation/replace/english/…_l_english.yml`;
        // the game requires the language suffix in the file name.
        let (language, name) = key.split_once(':').unwrap_or(("l_english", key));
        let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
        let language: String = language.chars().map(|c| if c.is_ascii_lowercase() || c == '_' { c } else { '_' }).collect();
        let folder = language.strip_prefix("l_").unwrap_or(&language);
        let (raw_language, raw_name) = key.split_once(':').unwrap_or(("l_english", key));
        let tag = if raw_language == language && raw_name == name { String::new() } else { key_hash(key) };
        return format!("localisation/replace/{folder}/drukhal_{name}{tag}_{language}.yml");
    }
    let prefix = if conflicts::is_fios(category) { SORT_FIRST } else { SORT_LAST };
    let tag = if safe == key { String::new() } else { key_hash(key) };
    format!("{category}/{prefix}{safe}{tag}.txt")
}

/// `_` plus the first 8 hex digits of the key's SHA-256: tells apart keys
/// whose sanitized names are equal.
fn key_hash(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!("_{:02x}{:02x}{:02x}{:02x}", digest[0], digest[1], digest[2], digest[3])
}

/// The event a merged event inherits from with `base =`, if it has one.
/// `base` is only read from the event's own top level, not from blocks
/// inside its options or effects.
fn event_base(script: &str) -> Option<String> {
    let scalar = |value: &Value| match value {
        Value::Scalar(text) => {
            let text = text.trim().trim_matches('"');
            (!text.is_empty()).then(|| text.to_owned())
        }
        Value::Block(_) => None,
    };
    let entries = script::parse(script).ok()?;
    for entry in &entries {
        if let Entry::Assign { key, value, .. } = entry {
            if key.eq_ignore_ascii_case("base") {
                return scalar(value);
            }
            if let Value::Block(inner) = value {
                for entry in inner {
                    if let Entry::Assign { key, value, .. } = entry
                        && key.eq_ignore_ascii_case("base")
                    {
                        return scalar(value);
                    }
                }
            }
        }
    }
    None
}

/// The path a merged object is written to. Events are normally written
/// first so they win the game's first-in-only-serve load order, but an
/// event using `base = X` must load *after* `X` is defined. When `X` is in
/// an earlier file and the merged copy still has to sort before the event
/// it replaces, put it directly after `X`'s file instead of at the very
/// front. `prints` say which files currently define the event.
fn file_for_script(category: &str, key: &str, script: &str, prints: &[Print], game_dir: Option<&Path>) -> String {
    let default = file_for(category, key);
    if category != "events" {
        return default;
    }
    let Some(base) = event_base(script) else { return default };
    let Some(game_dir) = game_dir else { return default };
    let Some(base_source) = super::source::vanilla(game_dir, "events", &base) else { return default };
    // The copy has to sort before every definition it replaces.
    let Some(target) = prints.iter().map(|p| p.file.as_str()).min_by_key(|f| f.to_ascii_lowercase()) else {
        return default;
    };
    let base_file = base_source.file;
    if base_file.to_ascii_lowercase() >= target.to_ascii_lowercase() {
        // The inherited event is defined at or after the event being
        // replaced, so no name can be both after the base and before it.
        return default;
    }
    let stem = base_file
        .rsplit_once('/')
        .map_or(base_file.as_str(), |(_, name)| name)
        .strip_suffix(".txt")
        .unwrap_or(base_file.rsplit_once('/').map_or(base_file.as_str(), |(_, name)| name));
    let candidate = format!("events/{stem}_drukhal_{}.txt", safe_name(key));
    let (candidate_lower, target_lower) = (candidate.to_ascii_lowercase(), target.to_ascii_lowercase());
    if candidate_lower > base_file.to_ascii_lowercase() && candidate_lower < target_lower {
        candidate
    } else {
        default
    }
}

/// The outcome of checking a saved merge against the sources it was made
/// from.
enum MergeContent {
    /// The sources aren't all available, so there is nothing to compare.
    Unavailable,
    /// The merge still covers every source's additions.
    Clean,
    /// The merge no longer matches its sources, with what is missing.
    Problems(Vec<String>),
}

/// Rechecks a saved merge against its sources. The fingerprint check only
/// notices when a source changes; this notices when the saved merge itself
/// dropped or broke something, such as a rebuild from an older version or
/// a manual edit.
///
/// A mod line that equals the previous game version's is that version's
/// value, left over in the mod, so `previous_game` (the object as the
/// game had it before its last update) is consulted when the merge looks
/// incomplete.
fn merge_content(merge: &Merge, roots: &HashMap<String, PathBuf>, sources: &Sources, previous_game: &dyn Fn(&str, &str) -> Vec<Source>) -> MergeContent {
    if merge.prints.is_empty() {
        return MergeContent::Unavailable;
    }
    let mut vanilla = None;
    let mut mods = Vec::new();
    for print in &merge.prints {
        let Some(root) = roots.get(&print.mod_id) else { return MergeContent::Unavailable };
        let Some(text) = sources.object(root, &print.file, &merge.category, &merge.key) else {
            return MergeContent::Unavailable;
        };
        let source = Source {
            label: if print.mod_id == VANILLA { "Vanilla".to_owned() } else { print.name.clone() },
            file: print.file.clone(),
            text,
        };
        if print.mod_id == VANILLA {
            vanilla = Some(source);
        } else {
            mods.push(source);
        }
    }
    let problems_with = |previous: &[Source]| -> Vec<String> {
        let validation = super::validate::validate_object_since(&merge.category, &merge.key, &merge.script, vanilla.as_ref(), previous, &mods, None);
        validation
            .problems
            .iter()
            .filter(|p| p.severity == super::validate::Severity::Error)
            .filter(|p| p.kind != super::validate::ProblemKind::UnknownScriptName)
            .map(|p| p.message.clone())
            .collect()
    };
    let mut problems = problems_with(&[]);
    if !problems.is_empty() {
        let previous = previous_game(&merge.category, &merge.key);
        if !previous.is_empty() {
            problems = problems_with(&previous);
        }
    }
    if problems.is_empty() {
        MergeContent::Clean
    } else {
        MergeContent::Problems(problems)
    }
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

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("drukhal-tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    crate::platform::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<_> = read.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                script_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "txt") {
                out.push(path);
            }
        }
    }

    /// Runs the save validators over a patch folder's saved merges, read
    /// only, and prints what they reject.
    /// `DRUKHAL_PATCH_DIR=<patch folder> cargo test saved_patch_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn saved_patch_survey() {
        let Some(dir) = std::env::var_os("DRUKHAL_PATCH_DIR").map(PathBuf::from) else {
            println!("SURVEY skipped: set DRUKHAL_PATCH_DIR");
            return;
        };
        let manifest = Manifest::parse(&std::fs::read_to_string(dir.join(MANIFEST)).unwrap()).unwrap();
        let mut rejected = 0;
        for merge in &manifest.merges {
            if let Err(e) = crate::merge_dialog::validate(&merge.category, &merge.key, &merge.script) {
                rejected += 1;
                println!("OBJECT {} {}: {e}", merge.category, merge.key);
            }
        }
        let mut files: Vec<PathBuf> = manifest.files.iter().map(|f| dir.join(&f.path)).collect();
        script_files(&dir.join("common/inline_scripts"), &mut files);
        let is_script = |p: &Path| p.extension().is_some_and(|e| ["txt", "gfx", "gui", "asset", "sfx", "yml"].iter().any(|x| e.eq_ignore_ascii_case(x)));
        files.retain(|p| is_script(p));
        for path in &files {
            let relative = path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
            let text = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
            if let Err(e) = crate::merge::file_merge::validate(&crate::merge::file_merge::category(&relative), &text) {
                rejected += 1;
                println!("FILE {relative}: {e}");
            }
        }
        println!("SURVEY {} objects, {} files, {rejected} rejected", manifest.merges.len(), files.len());
    }

    #[test]
    fn names_each_collections_patch() {
        assert_eq!(folder_for("Main Campaign!", |_| false), "drukhal_patch_main_campaign");
        assert_eq!(folder_for("Main Campaign", |f| f == "drukhal_patch_main_campaign"), "drukhal_patch_main_campaign_2");
        assert_eq!(folder_for("Ψ", |_| false), "drukhal_patch_collection");
        let t = Target::new("drukhal_patch_main", "Main");
        assert_eq!((t.id().as_str(), t.name.as_str()), ("mod/drukhal_patch_main.mod", "Main (Patch)"));
        assert!(is_patch(&t.id()) && is_patch("mod/drukhal_merged_patch.mod"));
        assert!(!is_patch("mod/ugc_1.mod") && !is_patch("mod/drukhal_merged_patch_fork.mod"));
    }

    #[test]
    fn refuses_writes_outside_the_patch_folder() {
        let user = std::env::temp_dir().join(format!("drukhal-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();

        let err = patch.save_file(&user, None, "../x", "x", vec![]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!test_target().root(&user).parent().unwrap().join("x").exists());
        assert!(!test_target().root(&user).exists(), "nothing was created");
        patch.save_file(&user, None, "common/x.txt", "x", vec![]).unwrap();
        assert!(test_target().root(&user).join("common/x.txt").is_file());
        std::fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn keys_that_sanitize_alike_get_separate_files() {
        let user = std::env::temp_dir().join(format!("drukhal-collide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();

        patch.save(&user, None, "common/buildings", "a b", "a = {}", vec![]).unwrap();
        patch.save(&user, None, "common/buildings", "a:b", "b = {}", vec![]).unwrap();

        assert_eq!(patch.manifest.merges.len(), 2);
        assert_ne!(patch.get("common/buildings", "a b").unwrap().file, patch.get("common/buildings", "a:b").unwrap().file);
        let root = test_target().root(&user);
        assert!(std::fs::read_to_string(root.join(&patch.get("common/buildings", "a b").unwrap().file)).unwrap().contains("a = {}"));
        assert_eq!(file_for("localisation", "l_english:a b"), "localisation/replace/english/drukhal_a_b_c61402c1_l_english.yml");
        assert_ne!(file_for("localisation", "l_english:a b"), file_for("localisation", "l_english:a:b"));
        std::fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn moves_merges_saved_under_a_name_that_loses() {
        let user = std::env::temp_dir().join(format!("drukhal-misplaced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save(&user, None, "common/component_templates", "c", "c = {}", vec![]).unwrap();
        // As an older version saved it: sorting last, where the first wins.
        let old = "common/component_templates/伞伞伞_drukhal_c.txt";
        let root = test_target().root(&user);
        std::fs::rename(root.join(file_for("common/component_templates", "c")), root.join(old)).unwrap();
        patch.manifest.merges[0].file = old.into();
        patch.write_manifest().unwrap();

        let patch = Patch::open(&user, &test_target()).unwrap();
        let wanted = file_for("common/component_templates", "c");
        assert_eq!(patch.get("common/component_templates", "c").unwrap().file, wanted);
        assert!(root.join(&wanted).is_file() && !root.join(old).exists());
        assert_eq!(Patch::open(&user, &test_target()).unwrap().get("common/component_templates", "c").unwrap().file, wanted, "saved");
        std::fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn notices_a_saved_merge_that_dropped_a_source_addition() {
        let root = std::env::temp_dir().join(format!("drukhal-merge-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, a, game) = (root.join("user"), root.join("a"), root.join("game"));
        for dir in [&a, &game] {
            std::fs::create_dir_all(dir.join("common/buildings")).unwrap();
        }
        std::fs::write(game.join("common/buildings/00.txt"), "lab = { cost = { minerals = 100 } }\n").unwrap();
        std::fs::write(a.join("common/buildings/a.txt"), "lab = { cost = { minerals = 50 } planet_modifier = { job_researcher_add = 1 } }\n").unwrap();
        std::fs::create_dir_all(user.join("mod")).unwrap();

        let prints = object_prints(
            "common/buildings",
            "lab",
            Some(&game),
            &[(
                ("mod/a.mod".to_owned(), "A".to_owned(), a.clone()),
                "common/buildings/a.txt".to_owned(),
            )],
        );
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        // The mod's planet_modifier is missing.
        patch.save(&user, Some("v4.5.3"), "common/buildings", "lab", "lab = { cost = { minerals = 50 } }", prints.clone()).unwrap();
        let roots: std::collections::HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a), (VANILLA.to_owned(), game)].into();
        let stale = patch.check(&roots, Some("v4.5.3"));
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].reasons.iter().any(|r| r.contains("Missing additions from A")), "{:?}", stale[0].reasons);
        assert_eq!(patch.get("common/buildings", "lab").unwrap().accepted, None);

        // Accepting the merge keeps its current content without re-merging.
        patch.accept("common/buildings", "lab").unwrap();
        assert!(patch.check(&roots, Some("v4.5.3")).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notices_a_mod_that_started_defining_a_merged_object() {
        let root = std::env::temp_dir().join(format!("drukhal-new-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, a, b, game) = (root.join("user"), root.join("a"), root.join("b"), root.join("game"));
        for dir in [&a, &b, &game] {
            std::fs::create_dir_all(dir.join("common/buildings")).unwrap();
        }
        std::fs::write(game.join("common/buildings/00.txt"), "lab = { cost = { minerals = 100 } }\n").unwrap();
        std::fs::write(a.join("common/buildings/a.txt"), "lab = { cost = { minerals = 50 } }\n").unwrap();
        std::fs::write(b.join("common/buildings/b.txt"), "lab = { cost = { minerals = 25 } }\n").unwrap();
        std::fs::create_dir_all(user.join("mod")).unwrap();

        let prints = object_prints(
            "common/buildings",
            "lab",
            Some(&game),
            &[(
                ("mod/a.mod".to_owned(), "A".to_owned(), a.clone()),
                "common/buildings/a.txt".to_owned(),
            )],
        );
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save(&user, Some("v4.5.3"), "common/buildings", "lab", "lab = { cost = { minerals = 50 } }", prints).unwrap();
        let roots: std::collections::HashMap<String, PathBuf> = [
            ("mod/a.mod".to_owned(), a),
            ("mod/b.mod".to_owned(), b),
            (VANILLA.to_owned(), game),
        ]
        .into();

        // Rechecking only the recorded source can't see B, which was not part
        // of the merge, even though the enabled mods define the object twice.
        assert!(patch.check(&roots, Some("v4.5.3")).is_empty());

        let definitions = |category: &str, key: &str| {
            if (category, key) == ("common/buildings", "lab") {
                vec![
                    ("mod/a.mod".to_owned(), "A".to_owned(), "common/buildings/a.txt".to_owned()),
                    ("mod/b.mod".to_owned(), "B".to_owned(), "common/buildings/b.txt".to_owned()),
                ]
            } else {
                Vec::new()
            }
        };
        let stale = patch.check_with(&roots, Some("v4.5.3"), &definitions, &|_| Vec::new());
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].reasons.iter().any(|r| r.contains("B also defines it, and was not merged")), "{:?}", stale[0].reasons);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notices_a_mod_that_started_shipping_a_merged_file() {
        let root = std::env::temp_dir().join(format!("drukhal-new-file-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, a, b) = (root.join("user"), root.join("a"), root.join("b"));
        for dir in [&a, &b] {
            std::fs::create_dir_all(dir.join("common/defines")).unwrap();
        }
        std::fs::write(a.join("common/defines/00_defines.txt"), "NAI = { X = 1 }\n").unwrap();
        std::fs::write(b.join("common/defines/00_defines.txt"), "NAI = { X = 2 }\n").unwrap();
        std::fs::create_dir_all(user.join("mod")).unwrap();

        let path = "common/defines/00_defines.txt";
        let prints = file_prints(path, None, &[("mod/a.mod".to_owned(), "A".to_owned(), a.clone())]);
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save_file(&user, None, path, b"NAI = { X = 1 }\n", prints).unwrap();
        let roots: std::collections::HashMap<String, PathBuf> =
            [("mod/a.mod".to_owned(), a), ("mod/b.mod".to_owned(), b)].into();

        assert!(patch.check(&roots, None).is_empty());

        let shippers = |shipped: &str| {
            if shipped.eq_ignore_ascii_case(path) {
                vec![("mod/a.mod".to_owned(), "A".to_owned()), ("mod/b.mod".to_owned(), "B".to_owned())]
            } else {
                Vec::new()
            }
        };
        let stale = patch.check_with(&roots, None, &|_, _| Vec::new(), &shippers);
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].reasons.iter().any(|r| r.contains("B also ships it, and was not merged")), "{:?}", stale[0].reasons);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Saves `merged` over a mod copy and the current game's copy, then
    /// rechecks it with `previous` as the previous game version's copy.
    fn recheck_against_previous(tag: &str, mod_text: &str, merged: &str, previous: &[&str]) -> (Vec<Stale>, Option<String>) {
        let root = std::env::temp_dir().join(format!("drukhal-previous-game-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, a, game) = (root.join("user"), root.join("a"), root.join("game"));
        for dir in [&a, &game] {
            std::fs::create_dir_all(dir.join("common/ethics")).unwrap();
        }
        std::fs::write(game.join("common/ethics/00.txt"), "ethic = { category_value = 5 }\n").unwrap();
        std::fs::write(a.join("common/ethics/a.txt"), mod_text).unwrap();
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mod_source = (("mod/a.mod".to_owned(), "A".to_owned(), a.clone()), "common/ethics/a.txt".to_owned());
        let prints = object_prints("common/ethics", "ethic", Some(&game), &[mod_source]);
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save(&user, Some("v4.6.0"), "common/ethics", "ethic", merged, prints).unwrap();
        let roots: std::collections::HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a), (VANILLA.to_owned(), game)].into();
        let previous: Vec<Source> = previous.iter().map(|text| Source { label: "Previous".into(), file: "common/ethics/00.txt".into(), text: (*text).into() }).collect();
        let stale = patch.recheck_with(&roots, Some("v4.6.0"), &Sources::default(), &|_, _| previous.clone(), &|_, _| Vec::new(), &|_| Vec::new());
        let accepted = patch.get("common/ethics", "ethic").unwrap().accepted.clone();
        std::fs::remove_dir_all(root).unwrap();
        (stale, accepted)
    }

    #[test]
    fn a_mods_leftover_copy_of_the_previous_game_version_is_not_a_missing_addition() {
        let (stale, accepted) = recheck_against_previous(
            "leftover",
            "ethic = { category_value = 4 }\n",
            "ethic = { category_value = 5 }",
            &["ethic = { category_value = 4 }"],
        );
        assert!(stale.is_empty(), "{stale:?}");
        assert!(accepted.is_some());
    }

    #[test]
    fn a_mod_written_two_game_versions_ago_is_compared_against_the_older_snapshot_too() {
        let (stale, accepted) = recheck_against_previous(
            "two-updates",
            "ethic = { category_value = 3 }\n",
            "ethic = { category_value = 5 }",
            &["ethic = { category_value = 4 }", "ethic = { category_value = 3 }"],
        );
        assert!(stale.is_empty(), "{stale:?}");
        assert!(accepted.is_some());
    }

    #[test]
    fn a_mod_change_to_the_previous_game_version_still_has_to_be_kept() {
        let (stale, accepted) = recheck_against_previous(
            "genuine",
            "ethic = { category_value = 4 modifier = { army_damage_mult = 0.1 } }\n",
            "ethic = { category_value = 5 }",
            &["ethic = { category_value = 4 }"],
        );
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].reasons.iter().any(|r| r.contains("Missing additions from A") && r.contains("army_damage_mult")), "{:?}", stale[0].reasons);
        assert_eq!(accepted, None);
    }

    #[test]
    fn places_events_after_the_event_they_inherit_from() {
        let root = std::env::temp_dir().join(format!("drukhal-base-event-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, game) = (root.join("user"), root.join("game"));
        std::fs::create_dir_all(game.join("events")).unwrap();
        std::fs::create_dir_all(user.join("mod")).unwrap();
        std::fs::write(game.join("events/overlord_enclaves_events.txt"), "country_event = { id = enclave.405 is_triggered_only = yes }\n").unwrap();
        std::fs::write(game.join("events/shroud_events.txt"), "country_event = { id = shroud.5310 base = enclave.405 }\n").unwrap();

        let script = "country_event = {\n\tid = shroud.5310\n\tbase = enclave.405\n}\n";
        let prints = vec![
            Print::new(VANILLA, "Vanilla", "events/shroud_events.txt", "country_event = { id = shroud.5310 base = enclave.405 }"),
            Print::new("mod/bts.mod", "BTS", "events/p_bts_shroudwalker_attunement_override.txt", "country_event = { id = shroud.5310 base = enclave.405 }"),
        ];
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save_for_game(&user, None, Some(&game), "events", "shroud.5310", script, prints).unwrap();
        let wanted = patch.get("events", "shroud.5310").unwrap().file.clone();
        // After the base's file, but still before the event it replaces.
        assert!(wanted.as_str() > "events/overlord_enclaves_events.txt", "{wanted}");
        assert!(wanted.as_str() < "events/shroud_events.txt", "{wanted}");
        assert!(test_target().root(&user).join(&wanted).is_file());

        // A patch saved before this rule is moved on the next check.
        let old = "events/!!!!!!_drukhal_shroud.5310.txt";
        let patch_root = test_target().root(&user);
        std::fs::rename(patch_root.join(&wanted), patch_root.join(old)).unwrap();
        patch.manifest.merges[0].file = old.into();
        patch.write_manifest().unwrap();
        let roots: std::collections::HashMap<String, PathBuf> = [(VANILLA.to_owned(), game.clone())].into();
        let _ = patch.recheck(&roots, None);
        assert_eq!(patch.get("events", "shroud.5310").unwrap().file, wanted);
        assert!(patch_root.join(&wanted).is_file() && !patch_root.join(old).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Times checking a patch of many merges after each save, as every
    /// resolved conflict does. `cargo test checks_many_merges -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn checks_many_merges() {
        let merges: usize = std::env::var("MERGES").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
        let per_file: usize = std::env::var("OBJECTS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
        let root = std::env::temp_dir().join(format!("drukhal-bench-{}", std::process::id()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let (user, a, game) = (root.join("user"), root.join("a"), root.join("game"));
        let file_text = |m: usize, cost: usize| (0..per_file).map(|i| format!("obj_{m}_{i} = {{\n\tcost = {cost}\n\tupkeep = 2\n}}\n")).collect::<String>();
        for dir in [&a, &game] {
            std::fs::create_dir_all(dir.join("common/buildings")).unwrap();
        }
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        for m in 0..merges {
            std::fs::write(a.join(format!("common/buildings/a{m}.txt")), file_text(m, 1)).unwrap();
            std::fs::write(game.join(format!("common/buildings/g{m}.txt")), file_text(m, 0)).unwrap();
            let prints = vec![
                Print::new(VANILLA, "Vanilla", &format!("common/buildings/g{m}.txt"), &file_text(m, 0)),
                Print::new("mod/a.mod", "A", &format!("common/buildings/a{m}.txt"), &file_text(m, 1)),
            ];
            patch.save(&user, None, "common/buildings", &format!("obj_{m}_0"), "obj = {}", prints).unwrap();
        }
        let roots: std::collections::HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a), (VANILLA.to_owned(), game)].into();
        let start = std::time::Instant::now();
        let stale = patch.check(&roots, Some("v4.5.3"));
        println!("check of {merges} merges of {} line files: {:?} ({} stale)", per_file * 4, start.elapsed(), stale.len());
        let start = std::time::Instant::now();
        patch.save(&user, None, "common/buildings", "obj_0_0", "obj = {}", patch.manifest.merges[0].prints.clone()).unwrap();
        println!("one more save: {:?}", start.elapsed());
    }

    #[test]
    fn notices_when_sources_change() {
        let root = std::env::temp_dir().join(format!("drukhal-stale-{}", std::process::id()));
        let (user, a, b, game) = (root.join("user"), root.join("a"), root.join("b"), root.join("game"));
        for (dir, file, text) in [(&a, "common/buildings/a.txt", "lab = { cost = 1 }\nfarm = {}"), (&b, "common/buildings/b.txt", "lab = { cost = 2 }"), (&game, "common/buildings/00.txt", "lab = { cost = 0 }")] {
            std::fs::create_dir_all(dir.join("common/buildings")).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
        }
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        let prints = vec![
            Print::new(VANILLA, "Vanilla", "common/buildings/00.txt", "lab = { cost = 0 }"),
            Print::new("mod/a.mod", "A", "common/buildings/a.txt", "lab = {\n\tcost = 1\n}"),
            Print::new("mod/b.mod", "B", "common/buildings/b.txt", "lab = { cost = 2 }"),
        ];
        patch.save(&user, None, "common/buildings", "lab", "lab = { cost = 3 }", prints.clone()).unwrap();
        // The test merge deliberately chooses a value neither mod ships;
        // keep it so the change detection below is what's under test.
        let file = patch.get("common/buildings", "lab").unwrap().file.clone();
        patch.track(Some(&("common/buildings".to_owned(), "lab".to_owned())), &file, prints, None).unwrap();
        let mut roots: std::collections::HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a.clone()), ("mod/b.mod".to_owned(), b.clone()), (VANILLA.to_owned(), game.clone())].into();
        assert!(patch.recheck(&roots, None).is_empty(), "reformatting and unrelated objects don't count");

        std::fs::write(b.join("common/buildings/b.txt"), "lab = { cost = 5 }").unwrap();
        std::fs::write(game.join("common/buildings/00.txt"), "").unwrap();
        roots.remove("mod/a.mod");
        let stale = patch.recheck(&roots, None);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].object, Some(("common/buildings".into(), "lab".into())));
        assert_eq!(stale[0].reasons, ["The base game no longer defines it", "A isn't enabled any more", "B changed it"]);

        patch.remove("common/buildings", "lab").unwrap();
        assert!(Patch::open(&user, &test_target()).unwrap().get("common/buildings", "lab").is_none());
        assert!(!test_target().root(&user).join(file_for("common/buildings", "lab")).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tracks_merges_saved_before_fingerprints() {
        let root = std::env::temp_dir().join(format!("drukhal-untracked-{}", std::process::id()));
        let (user, a, b, game) = (root.join("user"), root.join("a"), root.join("b"), root.join("game"));
        for (dir, file, text) in [(&a, "common/buildings/a.txt", "lab = { cost = 1 }"), (&b, "common/buildings/b.txt", "lab = { cost = 2 }"), (&game, "common/buildings/00.txt", "lab = { cost = 0 }")] {
            std::fs::create_dir_all(dir.join("common/buildings")).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
        }
        std::fs::write(a.join("common/buildings/shared.txt"), "farm = {}").unwrap();
        std::fs::write(b.join("common/buildings/shared.txt"), "farm = { cost = 1 }").unwrap();
        // As written before fingerprints existed: sources, but no prints.
        let dir = test_target().root(&user);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = r#"{"merges":[{"category":"common/buildings","key":"lab","script":"lab = { cost = 3 }","sources":["A","B"],"file":"common/buildings/伞伞伞_drukhal_lab.txt"}],
                         "files":[{"path":"common/buildings/shared.txt","sources":["A","B"]}]}"#;
        std::fs::write(dir.join(MANIFEST), legacy).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        let mut roots: std::collections::HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a.clone()), ("mod/b.mod".to_owned(), b.clone()), (VANILLA.to_owned(), game.clone())].into();

        let stale = patch.recheck(&roots, None);
        assert_eq!(stale.len(), 2);
        assert!(stale.iter().all(|s| s.untracked && s.reasons == [UNTRACKED]));
        assert_eq!(patch.sources(stale[0].object.as_ref(), &stale[0].path).unwrap(), ["A", "B"]);

        // Keeping them records the mods' (and the game's) current versions.
        let (ma, mb) = (("mod/a.mod".to_owned(), "A".to_owned(), a.clone()), ("mod/b.mod".to_owned(), "B".to_owned(), b.clone()));
        let prints = object_prints("common/buildings", "lab", Some(&game), &[(ma.clone(), "common/buildings/a.txt".into()), (mb.clone(), "common/buildings/b.txt".into())]);
        assert_eq!(prints.iter().map(|p| p.mod_id.as_str()).collect::<Vec<_>>(), [VANILLA, "mod/a.mod", "mod/b.mod"]);
        patch.track(stale[0].object.as_ref(), &stale[0].path, prints, None).unwrap();
        patch.track(None, "common/buildings/shared.txt", file_prints("common/buildings/shared.txt", Some(&game), &[ma, mb]), None).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        assert!(patch.recheck(&roots, None).is_empty(), "tracked and unchanged");

        // From now on, changes are noticed.
        std::fs::write(b.join("common/buildings/b.txt"), "lab = { cost = 9 }").unwrap();
        roots.remove("mod/a.mod");
        let stale = patch.recheck(&roots, None);
        assert_eq!(stale.iter().map(|s| (s.untracked, s.reasons.clone())).collect::<Vec<_>>(), [
            (false, vec!["A isn't enabled any more".to_owned(), "B changed it".to_owned()]),
            (false, vec!["A isn't enabled any more".to_owned()]),
        ]);
        assert!(patch.track(Some(&("events".into(), "gone".into())), "", vec![], None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moves_to_another_folder_and_back() {
        let root = std::env::temp_dir().join(format!("drukhal-relocate-{}", std::process::id()));
        let (user, extra) = (root.join("user"), root.join("extra"));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let home = test_target();
        let mut patch = Patch::open(&user, &home).unwrap();
        patch.save(&user, None, "common/buildings", "lab", "lab = {}", vec![]).unwrap();
        let away = home.clone().in_dir(Some(extra.clone()));
        assert_eq!(away.id(), home.id(), "the launcher finds it by the same descriptor");

        relocate(&user, &home, &away).unwrap();
        assert!(!home.root(&user).exists());
        assert!(away.root(&user).join(MANIFEST).is_file());
        let outer = std::fs::read_to_string(user.join(home.id())).unwrap();
        assert!(outer.contains(&format!("path=\"{}\"", crate::platform::descriptor_path(&away.root(&user)))), "{outer}");
        assert_eq!(Patch::open(&user, &away).unwrap().manifest.merges.len(), 1);

        std::fs::create_dir_all(home.root(&user)).unwrap();
        assert!(relocate(&user, &away, &home).is_err(), "won't overwrite a folder in the way");
        std::fs::remove_dir(home.root(&user)).unwrap();
        relocate(&user, &away, &home).unwrap();
        assert!(home.root(&user).join(MANIFEST).is_file() && !away.root(&user).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn updates_picked_versions_when_only_their_mod_changed() {
        let root = std::env::temp_dir().join(format!("drukhal-picks-{}", std::process::id()));
        let (user, a, b, game) = (root.join("user"), root.join("a"), root.join("b"), root.join("game"));
        let write = |dir: &Path, file: &str, text: &str| {
            std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
        };
        write(&a, "common/buildings/m.txt", "lab = { cost = 1 }\nfarm = { cost = 1 }");
        write(&b, "common/buildings/m.txt", "lab = { cost = 2 }\nfarm = { cost = 2 }");
        write(&a, "interface/x.gui", "a's gui");
        write(&b, "interface/x.gui", "b's gui");
        write(&game, "common/buildings/00.txt", "lab = { cost = 0 }");
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mods = [(("mod/a.mod".to_owned(), "A".to_owned(), a.clone()), "common/buildings/m.txt".to_owned()), (("mod/b.mod".to_owned(), "B".to_owned(), b.clone()), "common/buildings/m.txt".to_owned())];
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        // Picked A's lab, merged farm by hand, picked B's file.
        patch.save(&user, None, "common/buildings", "lab", "lab = {\n\tcost = 1\n}", object_prints("common/buildings", "lab", Some(&game), &mods)).unwrap();
        patch.save(&user, None, "common/buildings", "farm", "farm = { cost = 3 }", object_prints("common/buildings", "farm", Some(&game), &mods)).unwrap();
        let file_mods: Vec<SourceMod> = mods.iter().map(|(m, _)| m.clone()).collect();
        patch.save_file(&user, None, "interface/x.gui", "b's gui", file_prints("interface/x.gui", Some(&game), &file_mods)).unwrap();
        let roots: HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a.clone()), ("mod/b.mod".to_owned(), b.clone()), (VANILLA.to_owned(), game.clone())].into();
        assert!(patch.picks_to_update(&roots).is_empty(), "nothing changed");

        // A updates lab and farm, B updates its file.
        write(&a, "common/buildings/m.txt", "lab = { cost = 5 }\nfarm = { cost = 5 }");
        write(&b, "interface/x.gui", "b's new gui");
        let picks = patch.picks_to_update(&roots);
        let found: Vec<(Option<(String, String)>, &str)> = picks.iter().map(|p| (p.object.clone(), p.name.as_str())).collect();
        assert_eq!(found, [(Some(("common/buildings".into(), "lab".into())), "A"), (None, "B")], "the hand merge of farm isn't a pick");
        // Nothing changes until asked; the outdated list says which are picks.
        let mut stale = patch.recheck(&roots, None);
        Patch::mark_picks(&mut stale, &picks);
        let marked: Vec<(String, Option<String>)> = stale.iter().map(|s| (s.path.rsplit('/').next().unwrap().to_owned(), s.pick.clone())).collect();
        assert_eq!(marked.len(), 3);
        assert!(marked.iter().any(|(p, pick)| p == "x.gui" && pick.as_deref() == Some("B")), "{marked:?}");
        assert_eq!(marked.iter().filter(|(_, pick)| pick.is_none()).count(), 1, "farm is a real merge");
        patch.update_picks(&user, None, picks).unwrap();
        assert!(patch.picks_to_update(&roots).is_empty());
        assert_eq!(std::fs::read_to_string(patch.root.join("interface/x.gui")).unwrap(), "b's new gui");
        let stale: Vec<Option<(String, String)>> = patch.recheck(&roots, None).into_iter().map(|s| s.object).collect();
        assert_eq!(stale, [Some(("common/buildings".into(), "farm".into()))], "only the real merge still asks");

        // When another mod changes too, the pick asks again.
        write(&a, "common/buildings/m.txt", "lab = { cost = 6 }\nfarm = { cost = 5 }");
        write(&b, "common/buildings/m.txt", "lab = { cost = 7 }\nfarm = { cost = 2 }");
        assert!(patch.picks_to_update(&roots).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A patch whose lab was saved as `merged` over mods A and B, which each
    /// add something to the game's lab.
    fn recheck_two_additions(tag: &str, merged: &str, after: impl FnOnce(&Path, &Path)) -> (Vec<Stale>, Option<String>) {
        let root = std::env::temp_dir().join(format!("drukhal-kept-whole-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, a, b, game) = (root.join("user"), root.join("a"), root.join("b"), root.join("game"));
        for dir in [&a, &b, &game] {
            std::fs::create_dir_all(dir.join("common/buildings")).unwrap();
        }
        std::fs::write(game.join("common/buildings/00.txt"), "lab = { cost = 0 }\n").unwrap();
        std::fs::write(a.join("common/buildings/m.txt"), "lab = { cost = 0 a_extra = 1 }\n").unwrap();
        std::fs::write(b.join("common/buildings/m.txt"), "lab = { cost = 0 b_extra = 1 }\n").unwrap();
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mods = [(("mod/a.mod".to_owned(), "A".to_owned(), a.clone()), "common/buildings/m.txt".to_owned()), (("mod/b.mod".to_owned(), "B".to_owned(), b.clone()), "common/buildings/m.txt".to_owned())];
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save(&user, None, "common/buildings", "lab", merged, object_prints("common/buildings", "lab", Some(&game), &mods)).unwrap();
        after(&a, &b);
        let roots: HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a), ("mod/b.mod".to_owned(), b), (VANILLA.to_owned(), game)].into();
        let stale = patch.check(&roots, None);
        let accepted = patch.get("common/buildings", "lab").unwrap().accepted.clone();
        std::fs::remove_dir_all(root).unwrap();
        (stale, accepted)
    }

    #[test]
    fn keeping_one_mods_version_whole_is_not_audited_for_the_others_additions() {
        let (stale, accepted) = recheck_two_additions("pick", "lab = {\n\tcost = 0\n\ta_extra = 1\n}", |_, _| {});
        assert!(stale.is_empty(), "{stale:?}");
        assert!(accepted.is_some());
    }

    #[test]
    fn a_hand_edit_that_drops_a_mods_addition_is_still_audited() {
        let (stale, accepted) = recheck_two_additions("edit", "lab = { cost = 0 a_extra = 1 c_extra = 1 }", |_, _| {});
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].content, "{stale:?}");
        assert_eq!(accepted, None);
    }

    #[test]
    fn a_kept_whole_version_whose_mod_changed_is_reported_as_a_pick_to_update() {
        let (stale, _) = recheck_two_additions("changed", "lab = { cost = 0 a_extra = 1 }", |a, _| {
            std::fs::write(a.join("common/buildings/m.txt"), "lab = { cost = 0 a_extra = 2 }\n").unwrap();
        });
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].reasons.iter().any(|r| r.contains("A changed it")), "{:?}", stale[0].reasons);
        assert_eq!(stale[0].pick.as_deref(), Some("A"));
    }

    #[test]
    fn rechecks_after_a_game_update() {
        let root = std::env::temp_dir().join(format!("drukhal-update-{}", std::process::id()));
        let (user, a, b, game) = (root.join("user"), root.join("a"), root.join("b"), root.join("game"));
        let write = |dir: &Path, file: &str, text: &str| {
            std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
        };
        for dir in [&a, &b] {
            write(dir, "common/buildings/m.txt", "lab = {}\nfarm = {}\nmine = {}");
            write(dir, "interface/x.gui", "guiTypes = {}");
        }
        write(&game, "common/buildings/00.txt", "lab = { cost = 0 }");
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mods = [(("mod/a.mod".to_owned(), "A".to_owned(), a.clone()), "common/buildings/m.txt".to_owned()), (("mod/b.mod".to_owned(), "B".to_owned(), b.clone()), "common/buildings/m.txt".to_owned())];
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        for key in ["lab", "farm", "mine"] {
            let prints = object_prints("common/buildings", key, Some(&game), &mods);
            patch.save(&user, Some("v4.5.3"), "common/buildings", key, &format!("{key} = {{}}"), prints).unwrap();
        }
        let file_mods: Vec<SourceMod> = mods.iter().map(|(m, _)| m.clone()).collect();
        patch.save_file(&user, Some("v4.5.3"), "interface/x.gui", "guiTypes = {}", file_prints("interface/x.gui", Some(&game), &file_mods)).unwrap();
        let roots: HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a.clone()), ("mod/b.mod".to_owned(), b.clone()), (VANILLA.to_owned(), game.clone())].into();
        assert!(patch.recheck(&roots, Some("v4.5.3")).is_empty());

        // Without the base game's folder, its versions aren't checked.
        let mods_only: HashMap<String, PathBuf> = roots.iter().filter(|(id, _)| *id != VANILLA).map(|(k, v)| (k.clone(), v.clone())).collect();
        assert!(patch.recheck(&mods_only, Some("v4.6.0")).is_empty(), "nothing to blame on an unknown game");

        // The update moves lab to another file unchanged, changes nothing
        // about mine, and adds farm and the interface file.
        std::fs::remove_file(game.join("common/buildings/00.txt")).unwrap();
        write(&game, "common/buildings/01.txt", "lab = { cost = 0 }\nfarm = { cost = 7 }");
        write(&game, "interface/x.gui", "guiTypes = { new = yes }");
        let stale = patch.recheck(&roots, Some("v4.6.0"));
        assert_eq!(stale.iter().map(|s| (s.path.as_str(), s.reasons.clone(), s.base_game)).collect::<Vec<_>>(), [
            (file_for("common/buildings", "farm").as_str(), vec!["The base game added it in v4.6.0".to_owned()], true),
            ("interface/x.gui", vec!["The base game added this file in v4.6.0".to_owned()], true),
        ]);

        // The unchanged merges count as checked against the new version,
        // and lab's fingerprint follows it to its new file.
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        assert_eq!(patch.get("common/buildings", "lab").unwrap().game_version.as_deref(), Some("v4.6.0"));
        assert_eq!(patch.get("common/buildings", "lab").unwrap().prints[0].file, "common/buildings/01.txt");
        assert_eq!(patch.get("common/buildings", "mine").unwrap().game_version.as_deref(), Some("v4.6.0"));
        assert_eq!(patch.get("common/buildings", "farm").unwrap().game_version.as_deref(), Some("v4.5.3"), "still outdated");
        assert_eq!(patch.recheck(&roots, Some("v4.6.0")).len(), 2, "until re-merged");

        // A later change to lab names the update it came with.
        write(&game, "common/buildings/01.txt", "lab = { cost = 1 }\nfarm = { cost = 7 }");
        let stale = patch.recheck(&roots, Some("v4.6.1"));
        assert_eq!(stale[0].reasons, ["The base game changed it in v4.6.1"]);

        // Merges from before versions were recorded are taken as made
        // against the installed game.
        patch.manifest.merges.iter_mut().for_each(|m| m.game_version = None);
        patch.recheck(&roots, Some("v4.6.1"));
        assert_eq!(patch.get("common/buildings", "mine").unwrap().game_version.as_deref(), Some("v4.6.1"));
        // A mod update moves lab from m.txt to m_renamed.txt unchanged.
        std::fs::remove_file(a.join("common/buildings/m.txt")).unwrap();
        write(&a, "common/buildings/m_renamed.txt", "lab = {}");
        let stale = patch.recheck(&roots, Some("v4.6.1"));
        // lab is still stale from the base game update, but not because mod A moved it.
        let lab_stale = stale.iter().find(|s| s.object.as_ref().is_some_and(|(_, k)| k == "lab")).unwrap();
        assert_eq!(lab_stale.reasons, ["The base game changed it in an update"]);
        let reopened = Patch::open(&user, &test_target()).unwrap();
        let a_print = reopened.get("common/buildings", "lab").unwrap().prints.iter().find(|p| p.mod_id == "mod/a.mod").unwrap();
        assert_eq!(a_print.file, "common/buildings/m_renamed.txt");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_files_to_win() {
        assert_eq!(file_for("common/buildings", "building_lab"), "common/buildings/伞伞伞_drukhal_building_lab.txt");
        assert_eq!(file_for("common/traits", "trait_x"), "common/traits/!!!!!!_drukhal_trait_x.txt");
        assert_eq!(file_for("events", "ns.12"), "events/!!!!!!_drukhal_ns.12.txt");
        assert_eq!(file_for("common/component_templates", "c"), "common/component_templates/!!!!!!_drukhal_c.txt");
        assert_eq!(file_for("localisation", "l_german:lab"), "localisation/replace/german/drukhal_lab_l_german.yml");
        assert_eq!(file_for("localisation", "l_braz_por:lab"), "localisation/replace/braz_por/drukhal_lab_l_braz_por.yml");
        assert_eq!(file_for("localisation", "../../x:lab"), "localisation/replace/______x/drukhal_lab_b2e89022_______x.yml", "a crafted language stays in the folder");
        assert!(SORT_LAST > "zzzz_" && SORT_LAST > "~ste" && SORT_LAST > "伞伞zzz_");
        assert!(SORT_FIRST < "!_pd" && SORT_FIRST < "00_");
    }

    #[test]
    fn saves_and_reopens() {
        let user = std::env::temp_dir().join(format!("drukhal-patch-{}", std::process::id()));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save(&user, Some("v4.5.3"), "events", "ns.1", "country_event = { id = ns.1 }", vec![Print::new("mod/a.mod", "A", "events/a.txt", "x"), Print::new("mod/b.mod", "B", "events/b.txt", "y")]).unwrap();
        patch.save(&user, Some("v4.5.3"), "localisation", "l_english:lab", "lab:0 \"Lab\"", vec![]).unwrap();
        patch.save(&user, Some("v4.5.3"), "events", "ns.1", "country_event = { id = ns.1 hide_window = yes }", vec![Print::new("mod/a.mod", "A", "events/a.txt", "x")]).unwrap();

        let patch = Patch::open(&user, &test_target()).unwrap();
        assert_eq!(patch.get("events", "ns.1").unwrap().sources, ["A"]);
        let event = std::fs::read_to_string(test_target().root(&user).join("events/!!!!!!_drukhal_ns.1.txt")).unwrap();
        assert!(event.contains("namespace = ns\n\ncountry_event = { id = ns.1 hide_window = yes }"));
        let loc = std::fs::read_to_string(test_target().root(&user).join("localisation/replace/english/drukhal_lab_l_english.yml")).unwrap();
        assert_eq!(loc, "\u{feff}l_english:\n lab:0 \"Lab\"\n");
        let descriptor = std::fs::read_to_string(user.join(test_target().id())).unwrap();
        assert!(descriptor.contains("supported_version=\"v4.5.*\"") && descriptor.contains("path=\""));
        let parsed = crate::mods::Descriptor::parse(&descriptor).unwrap();
        assert_eq!(parsed.name, "Main (Patch)");

        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch.save_file(&user, None, "common/buildings/00_x.txt", "a = {}\n", vec![Print::new("mod/a.mod", "A", "common/buildings/00_x.txt", "a"), Print::new("mod/b.mod", "B", "common/buildings/00_x.txt", "b")]).unwrap();
        let patch = Patch::open(&user, &test_target()).unwrap();
        assert!(patch.get("events", "ns.1").is_some(), "object merges survive");
        assert_eq!(patch.get_file("common/buildings/00_X.txt").unwrap().sources, ["A", "B"]);
        assert_eq!(std::fs::read_to_string(test_target().root(&user).join("common/buildings/00_x.txt")).unwrap(), "a = {}\n");

        std::fs::remove_dir_all(user).unwrap();
    }

    /// A heavily modded game on disk, with a patch merging much of it.
    struct TimingFixture {
        root: PathBuf,
        game: PathBuf,
        mods_dir: PathBuf,
        patch: Patch,
        roots: HashMap<String, PathBuf>,
    }

    /// The building a merge `idx` is about: 150 from `00_a.txt`, 150 from `01_b.txt`.
    fn timing_building(idx: usize) -> String {
        if idx < 150 { format!("bld_a_{idx}") } else { format!("bld_b_{}", idx - 150) }
    }

    fn timing_object(name: &str, buildtime: u32) -> String {
        format!("{name} = {{\n\tbase_buildtime = {buildtime}\n\tcost = {{ minerals = 50 }}\n\tallow = {{ always = yes }}\n\tai_weight = {{ factor = 1 }}\n}}\n")
    }

    fn timing_write(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Mod `n`'s buildings: 20 of the 300 merged ones, so each is defined by 2 mods.
    fn timing_mod_buildings(n: usize) -> Vec<String> {
        (0..20).map(|j| timing_building((n * 20 + j) % 300)).collect()
    }

    /// Mod `n`'s localisation keys: 10 of the first 100.
    fn timing_mod_keys(n: usize) -> Vec<usize> {
        (0..10).map(|j| (n * 10 + j) % 100).collect()
    }

    fn timing_fixture(name: &str) -> TimingFixture {
        let root = std::env::temp_dir().join(format!("drukhal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, game, mods_dir) = (root.join("user"), root.join("game"), root.join("mods"));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        for f in ["a", "b", "c", "d"] {
            let text: String = (0..500).map(|i| timing_object(&format!("bld_{f}_{i}"), 100)).collect();
            let file = format!("common/buildings/{}_{f}.txt", match f { "a" => "00", "b" => "01", "c" => "02", _ => "03" });
            timing_write(&game, &file, &text);
        }
        let mut loc = String::from("l_english:\n");
        for i in 0..20_000 {
            loc.push_str(&format!(" key_{i}:0 \"Text {i}\"\n"));
        }
        timing_write(&game, "localisation/english/big_l_english.yml", &loc);

        let mut defining: HashMap<String, Vec<(SourceMod, String)>> = HashMap::new();
        let mut loc_defining: HashMap<usize, Vec<(SourceMod, String)>> = HashMap::new();
        let mut roots: HashMap<String, PathBuf> = HashMap::new();
        for n in 0..30 {
            let dir = mods_dir.join(format!("m{n}"));
            let source: SourceMod = (format!("mod/m{n}.mod"), format!("Mod {n}"), dir.clone());
            let buildings = timing_mod_buildings(n);
            let text: String = buildings.iter().map(|b| timing_object(b, 200)).collect();
            let file = format!("common/buildings/zz_m{n}.txt");
            timing_write(&dir, &file, &text);
            for b in buildings {
                defining.entry(b).or_default().push((source.clone(), file.clone()));
            }
            let keys = timing_mod_keys(n);
            let mut loc = String::from("l_english:\n");
            for k in &keys {
                loc.push_str(&format!(" key_{k}:0 \"Mod {n} {k}\"\n"));
            }
            let file = format!("localisation/english/m{n}_l_english.yml");
            timing_write(&dir, &file, &loc);
            for k in keys {
                loc_defining.entry(k).or_default().push((source.clone(), file.clone()));
            }
            roots.insert(source.0, dir);
        }
        roots.insert(VANILLA.to_owned(), game.clone());

        let mut patch = Patch::open(&user, &test_target()).unwrap();
        for idx in 0..300 {
            let key = timing_building(idx);
            let mods = &defining[&key];
            assert!(mods.len() >= 2, "{key} is defined by {} mod(s)", mods.len());
            let prints = object_prints("common/buildings", &key, Some(&game), mods);
            // As if the first mod's version was picked.
            let kept = super::super::source::extract("common/buildings", &key, &std::fs::read_to_string(mods[0].0.2.join(&mods[0].1)).unwrap()).unwrap();
            patch.save(&user, Some("v4.0.0"), "common/buildings", &key, &kept, prints).unwrap();
        }
        for k in 0..50 {
            let mods = &loc_defining[&k];
            assert!(mods.len() >= 2);
            let key = format!("l_english:key_{k}");
            let prints = object_prints("localisation", &key, Some(&game), mods);
            let kept = super::super::source::extract("localisation", &key, &std::fs::read_to_string(mods[0].0.2.join(&mods[0].1)).unwrap()).unwrap();
            patch.save(&user, Some("v4.0.0"), "localisation", &key, &kept, prints).unwrap();
        }
        TimingFixture { root, game, mods_dir, patch, roots }
    }

    /// A game update and two mods' edits: 20 merges go out of date and 50
    /// definitions move to a new file, unchanged.
    fn timing_update(f: &TimingFixture) {
        for n in 0..2 {
            let text: String = timing_mod_buildings(n).iter().enumerate().map(|(j, b)| timing_object(b, if j < 10 { 999 } else { 200 })).collect();
            timing_write(&f.mods_dir.join(format!("m{n}")), &format!("common/buildings/zz_m{n}.txt"), &text);
        }
        let (kept, moved): (String, String) = ((50..500).map(|i| timing_object(&format!("bld_a_{i}"), 100)).collect(), (0..50).map(|i| timing_object(&format!("bld_a_{i}"), 100)).collect());
        timing_write(&f.game, "common/buildings/00_a.txt", &kept);
        timing_write(&f.game, "common/buildings/04_e.txt", &moved);
    }

    /// How long the merged patch's freshness check takes on a big setup:
    /// `cargo test --release --locked recheck_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn recheck_timing() {
        use std::time::Instant;
        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
        for (case, version) in [("steady", "v4.0.0"), ("updated", "v4.1.0")] {
            let (mut recheck, mut picks) = (f64::MAX, f64::MAX);
            let mut stale = 0;
            for _ in 0..3 {
                let mut f = timing_fixture("timing");
                if case == "updated" {
                    timing_update(&f);
                }
                let t = Instant::now();
                stale = f.patch.recheck(&f.roots, Some(version)).len();
                recheck = recheck.min(ms(t));
                let t = Instant::now();
                let n = f.patch.picks_to_update(&f.roots).len();
                picks = picks.min(ms(t));
                let _ = n;
                std::fs::remove_dir_all(&f.root).unwrap();
            }
            println!("TIMING {case} recheck={recheck:.1} picks={picks:.1} (stale={stale})");
            // The combined check, on a fresh fixture each run as above.
            let mut check = f64::MAX;
            let mut marked = 0;
            for _ in 0..3 {
                let mut f = timing_fixture("timing");
                if case == "updated" {
                    timing_update(&f);
                }
                let t = Instant::now();
                marked = f.patch.check(&f.roots, Some(version)).len();
                check = check.min(ms(t));
                std::fs::remove_dir_all(&f.root).unwrap();
            }
            println!("TIMING {case} check={check:.1} (stale={marked})");
        }
    }

    #[test]
    fn sources_match_extract() {
        let dir = std::env::temp_dir().join(format!("drukhal-sources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = "@cost = 10\n@unused = 1\nbuilding_lab = {\n\tcost = @cost\n}\nbuilding_other = { cost = 2 }\n";
        std::fs::write(dir.join("a.txt"), text).unwrap();
        let loc = "l_english:\n lab:0 \"Lab\"\n";
        std::fs::write(dir.join("a.yml"), loc).unwrap();
        let sources = Sources::default();
        for key in ["building_lab", "building_other", "building_missing"] {
            assert_eq!(sources.object(&dir, "a.txt", "common/buildings", key), super::super::source::extract("common/buildings", key, text), "{key}");
        }
        assert!(sources.object(&dir, "a.txt", "common/buildings", "building_lab").unwrap().contains("@cost = 10"));
        assert_eq!(sources.object(&dir, "a.yml", "localisation", "l_english:lab"), super::super::source::extract("localisation", "l_english:lab", loc));
        assert!(sources.object(&dir, "a.yml", "localisation", "l_english:lab").is_some());
        assert_eq!(sources.object(&dir, "nope.txt", "common/buildings", "building_lab"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sources_read_each_file_once() {
        let dir = std::env::temp_dir().join(format!("drukhal-sources-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x = {}").unwrap();
        let sources = Sources::default();
        assert_eq!(sources.text(&dir, "a.txt").as_deref().map(String::as_str), Some("x = {}"));
        assert!(sources.text(&dir, "a.txt").is_some());
        std::fs::remove_file(dir.join("a.txt")).unwrap();
        assert_eq!(sources.text(&dir, "a.txt").as_deref().map(String::as_str), Some("x = {}"), "served from the cache");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_a_manifest_whose_paths_leave_the_patch() {
        let user = std::env::temp_dir().join(format!("drukhal-patch-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        let victim = user.join("victim.txt");
        let absolute = victim.to_string_lossy().replace('\\', "\\\\");
        let merge_keyed = |category: &str, key: &str, file: &str| format!(r#"{{"merges":[{{"category":"{category}","key":"{key}","script":"","sources":[],"file":"{file}"}}]}}"#);
        let merge = |category: &str, file: &str| merge_keyed(category, "lab", file);
        let manifests = [
            merge("common/buildings", &absolute),
            merge("common/buildings", "../../victim.txt"),
            merge("../..", "common/buildings/a.txt"),
            merge("", "common/buildings/a.txt"),
            format!(r#"{{"merges":[],"files":[{{"path":"{absolute}","sources":[]}}]}}"#),
            r#"{"merges":[],"files":[{"path":"common/../../victim.txt","sources":[]}]}"#.to_owned(),
            merge_keyed("localisation", "../../x:lab", "localisation/replace/english/a.yml"),
            merge_keyed("localisation", "l_english/..:lab", "localisation/replace/english/a.yml"),
            r#"{"merges":[{"category":"common/buildings","key":"lab","script":"","sources":[],"file":"common/buildings/a.txt","scripts":["../../victim"]}]}"#.to_owned(),
        ];
        for manifest in manifests {
            let root = test_target().root(&user);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(&victim, "keep").unwrap();
            std::fs::write(root.join(MANIFEST), &manifest).unwrap();

            let error = Patch::open(&user, &test_target()).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{manifest}");
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep", "{manifest}");
            assert_eq!(std::fs::read_to_string(root.join(MANIFEST)).unwrap(), manifest, "left as it was");
        }
        std::fs::remove_dir_all(&user).unwrap();
    }

    #[test]
    fn removing_a_merge_removes_its_inline_scripts() {
        let user = std::env::temp_dir().join(format!("drukhal-patch-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        patch
            .save(&user, None, "common/pop_faction_types", "imperialist", "imperialist = {}", vec![])
            .unwrap();
        let script = "common/inline_scripts/MECR/factions/imperialist_460.txt";
        patch
            .save_file(&user, None, script, "demand = {}", vec![])
            .unwrap();
        patch
            .set_scripts(
                "common/pop_faction_types",
                "imperialist",
                vec!["MECR/factions/imperialist_460".into()],
            )
            .unwrap();
        let root = test_target().root(&user);
        assert!(root.join(script).is_file());
        // Saving the object again keeps its script record.
        patch
            .save(&user, None, "common/pop_faction_types", "imperialist", "imperialist = { a = 1 }", vec![])
            .unwrap();
        assert_eq!(
            patch.get("common/pop_faction_types", "imperialist").unwrap().scripts,
            vec!["MECR/factions/imperialist_460".to_owned()]
        );
        patch
            .remove("common/pop_faction_types", "imperialist")
            .unwrap();
        assert!(!root.join(script).is_file());
        assert!(patch.get_file(script).is_none());
        std::fs::remove_dir_all(&user).unwrap();
    }

    #[test]
    fn a_script_shared_by_two_merges_survives_until_the_last_one() {
        let user = std::env::temp_dir().join(format!("drukhal-patch-shared-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        for key in ["imperialist", "isolationist"] {
            patch.save(&user, None, "common/pop_faction_types", key, &format!("{key} = {{}}"), vec![]).unwrap();
        }
        let script = "common/inline_scripts/MECR/factions/imperialist_460.txt";
        patch.save_file(&user, None, script, "demand = {}", vec![]).unwrap();
        for key in ["imperialist", "isolationist"] {
            patch
                .set_scripts("common/pop_faction_types", key, vec!["MECR/factions/imperialist_460".into()])
                .unwrap();
        }
        let root = test_target().root(&user);
        patch.remove("common/pop_faction_types", "imperialist").unwrap();
        assert!(root.join(script).is_file(), "the other merge still uses it");
        patch.remove("common/pop_faction_types", "isolationist").unwrap();
        assert!(!root.join(script).is_file(), "the last merge removes it");
        std::fs::remove_dir_all(&user).unwrap();
    }

    #[test]
    fn removing_a_file_merge_removes_its_inline_scripts() {
        let user = std::env::temp_dir().join(format!("drukhal-patch-file-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        let mut patch = Patch::open(&user, &test_target()).unwrap();
        let merged = "common/pop_faction_types/00_imperialist.txt";
        let script = "common/inline_scripts/MECR/factions/imperialist_460.txt";
        patch.save_file(&user, None, merged, "imperialist = {}", vec![]).unwrap();
        patch.save_file(&user, None, script, "demand = {}", vec![]).unwrap();
        patch
            .set_file_scripts(merged, vec!["MECR/factions/imperialist_460".into()])
            .unwrap();
        let root = test_target().root(&user);
        assert!(root.join(script).is_file());
        patch.remove_file(merged).unwrap();
        assert!(!root.join(script).is_file());
        assert!(patch.get_file(script).is_none());
        std::fs::remove_dir_all(&user).unwrap();
    }
}
