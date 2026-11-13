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

use super::conflicts;

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

/// A stable fingerprint of script text that ignores formatting changes.
pub fn fingerprint(text: &str) -> String {
    // FNV-1a over the whitespace-separated words.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for word in text.split_whitespace() {
        for byte in word.bytes().chain([b' ']) {
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    merges: Vec<Merge>,
    #[serde(default)]
    files: Vec<FileMerge>,
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
            Ok(src) => serde_json::from_str(&src).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Manifest::default(),
            Err(e) => return Err(e),
        };
        let mut patch = Self { root, target: target.clone(), manifest };
        patch.rename_misplaced()?;
        Ok(patch)
    }

    /// Moves merges saved under a name that no longer sorts where it must,
    /// as older versions did for some first-wins folders.
    fn rename_misplaced(&mut self) -> io::Result<()> {
        let mut moved = false;
        for merge in &mut self.manifest.merges {
            let wanted = file_for(&merge.category, &merge.key);
            if merge.file == wanted {
                continue;
            }
            let (from, to) = (self.root.join(&merge.file), self.root.join(&wanted));
            if from.is_file() {
                std::fs::create_dir_all(to.parent().unwrap())?;
                std::fs::rename(&from, &to)?;
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

    /// Adds or replaces a merged copy of `path`, or a chosen copy of an
    /// image or other binary file.
    pub fn save_file(&mut self, user_dir: &Path, game_version: Option<&str>, path: &str, contents: impl AsRef<[u8]>, prints: Vec<Print>) -> io::Result<()> {
        let result = self.save_file_unlogged(user_dir, game_version, path, contents, prints);
        crate::diagnostics::outcome("patch", || format!("Saved {path} to {}", self.target.name), result)
    }

    fn save_file_unlogged(&mut self, user_dir: &Path, game_version: Option<&str>, path: &str, contents: impl AsRef<[u8]>, prints: Vec<Print>) -> io::Result<()> {
        let sources = prints.iter().filter(|p| p.mod_id != VANILLA).map(|p| p.name.clone()).collect();
        self.write_descriptors(user_dir, game_version)?;
        let target = self.root.join(path);
        std::fs::create_dir_all(target.parent().unwrap())?;
        write_atomic(&target, contents.as_ref())?;
        let merge = FileMerge { path: path.to_owned(), sources, prints, game_version: game_version.map(str::to_owned) };
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
            let merge = self.manifest.merges.remove(at);
            remove_if_present(&self.root.join(&merge.file))?;
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
        }
        self.write_manifest()
    }

    /// The "Use This Version" choices whose mod has changed the version
    /// they kept, while nothing else in the conflict changed: those can
    /// take the mod's new version without asking again. `roots` is as for
    /// [`Patch::recheck`].
    pub fn picks_to_update(&self, roots: &HashMap<String, PathBuf>) -> Vec<Pick> {
        let game_dir = roots.get(VANILLA).map(PathBuf::as_path);
        let mut picks = Vec::new();
        for m in &self.manifest.merges {
            let text_of = |src: &str| super::source::extract(&m.category, &m.key, src);
            if let Some((i, now, prints)) = picked_update(&m.script, &m.prints, roots, game_dir, &text_of) {
                picks.push(Pick { object: Some((m.category.clone(), m.key.clone())), path: m.file.clone(), name: m.prints[i].name.clone(), contents: now.into_bytes(), prints });
            }
        }
        for f in &self.manifest.files {
            let Some(kept) = read_text(&self.root, &f.path) else { continue };
            let text_of = |src: &str| Some(src.to_owned());
            let Some((i, _, prints)) = picked_update(&kept, &f.prints, roots, game_dir, &text_of) else { continue };
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
    pub fn recheck(&mut self, roots: &HashMap<String, PathBuf>, game_version: Option<&str>) -> Vec<Stale> {
        let game_dir = roots.get(VANILLA).map(PathBuf::as_path);
        let updated = |checked: Option<&str>| matches!((checked, game_version), (Some(was), Some(now)) if was != now);
        let changed_in = |checked: Option<&str>| match (updated(checked), game_version) {
            (true, Some(now)) => format!("The base game changed it in {now}"),
            _ => "The base game changed it in an update".to_owned(),
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
                stale.push(Stale { object: Some((category, key)), path: m.file.clone(), reasons: vec![UNTRACKED.to_owned()], untracked: true, base_game: false, pick: None });
                continue;
            }
            let text_of = |src: &str| super::source::extract(&category, &key, src);
            let mut check = Check::default();
            for p in &mut m.prints {
                if p.mod_id != VANILLA {
                    check.mod_print(p, roots, &text_of, "no longer defines it");
                    continue;
                }
                let Some(dir) = game_dir else { continue };
                let mut now = read_text(dir, &p.file).and_then(|s| text_of(&s));
                if now.is_none() {
                    // Updates often move definitions between files.
                    if let Some(v) = super::source::vanilla(dir, &category, &key) {
                        if fingerprint(&v.text) == p.hash {
                            p.file = v.file;
                            dirty = true;
                        }
                        now = Some(v.text);
                    }
                }
                match now {
                    None => check.base_game("The base game no longer defines it".to_owned()),
                    Some(text) if fingerprint(&text) != p.hash => check.base_game(changed_in(m.game_version.as_deref())),
                    Some(_) => {}
                }
            }
            if added.contains(&(category.clone(), key.clone())) {
                check.base_game(format!("The base game added it in {}", game_version.unwrap_or_default()));
            }
            dirty |= check.confirm(&mut m.game_version, game_dir, game_version);
            if !check.reasons.is_empty() {
                stale.push(Stale { object: Some((category, key)), path: m.file.clone(), base_game: check.base_game, reasons: check.reasons, untracked: false, pick: None });
            }
        }
        for f in &mut self.manifest.files {
            if f.prints.is_empty() {
                stale.push(Stale { object: None, path: f.path.clone(), reasons: vec![UNTRACKED.to_owned()], untracked: true, base_game: false, pick: None });
                continue;
            }
            let text_of = |src: &str| Some(src.to_owned());
            let mut check = Check::default();
            for p in &f.prints {
                if p.mod_id != VANILLA {
                    check.mod_print(p, roots, &text_of, "no longer ships this file");
                    continue;
                }
                let Some(dir) = game_dir else { continue };
                match read_text(dir, &p.file) {
                    None => check.base_game("The base game no longer ships this file".to_owned()),
                    Some(text) if fingerprint(&text) != p.hash => check.base_game(changed_in(f.game_version.as_deref())),
                    Some(_) => {}
                }
            }
            if let Some(dir) = game_dir
                && !has_vanilla(&f.prints)
                && updated(f.game_version.as_deref())
                && dir.join(&f.path).is_file()
            {
                check.base_game(format!("The base game added this file in {}", game_version.unwrap_or_default()));
            }
            dirty |= check.confirm(&mut f.game_version, game_dir, game_version);
            if !check.reasons.is_empty() {
                stale.push(Stale { object: None, path: f.path.clone(), base_game: check.base_game, reasons: check.reasons, untracked: false, pick: None });
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
    pub fn track(&mut self, object: Option<&(String, String)>, path: &str, prints: Vec<Print>, game_version: Option<&str>) -> io::Result<()> {
        let slot = match object {
            Some((category, key)) => self.manifest.merges.iter_mut().find(|m| &m.category == category && &m.key == key).map(|m| (&mut m.prints, &mut m.game_version)),
            None => self.manifest.files.iter_mut().find(|f| f.path.eq_ignore_ascii_case(path)).map(|f| (&mut f.prints, &mut f.game_version)),
        };
        let Some((slot, version)) = slot else { return Err(io::Error::new(io::ErrorKind::NotFound, "It is no longer in the patch")) };
        *slot = prints;
        *version = game_version.map(str::to_owned);
        self.write_manifest()
    }

    fn write_manifest(&self) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(&self.manifest).map_err(io::Error::other)?;
        write_atomic(&self.root.join(MANIFEST), &json)
    }

    /// Adds or replaces a merged object and writes it to disk, creating the
    /// mod's descriptors if needed. `game_version` (e.g. `v4.5.3`) sets the
    /// version the launcher shows the patch as supporting.
    pub fn save(&mut self, user_dir: &Path, game_version: Option<&str>, category: &str, key: &str, script: &str, prints: Vec<Print>) -> io::Result<()> {
        let result = self.save_unlogged(user_dir, game_version, category, key, script, prints);
        crate::diagnostics::outcome("patch", || format!("Saved {category}/{key} to {}", self.target.name), result)
    }

    fn save_unlogged(&mut self, user_dir: &Path, game_version: Option<&str>, category: &str, key: &str, script: &str, prints: Vec<Print>) -> io::Result<()> {
        let sources = prints.iter().filter(|p| p.mod_id != VANILLA).map(|p| p.name.clone()).collect();
        self.write_descriptors(user_dir, game_version)?;
        let file = file_for(category, key);
        let path = self.root.join(&file);
        std::fs::create_dir_all(path.parent().unwrap())?;
        write_atomic(&path, render(category, key, script).as_bytes())?;

        let merge = Merge { category: category.into(), key: key.into(), script: script.into(), sources, file, prints, game_version: game_version.map(str::to_owned) };
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
    if std::fs::rename(&old, &new).is_err() {
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
    let outer = format!("{}\npath=\"{}\"\n", inner.trim_end(), target.root(user_dir).display());
    write_atomic(&user_dir.join(target.id()), outer.as_bytes())
}

/// How many merged objects and files the patch copy at `dir` holds.
pub fn summary(dir: &Path) -> Option<(usize, usize)> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).ok()?).ok()?;
    Some((manifest.merges.len(), manifest.files.len()))
}

/// What [`Patch::recheck`] found about one merge's sources.
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
    fn mod_print(&mut self, p: &Print, roots: &HashMap<String, PathBuf>, text_of: &dyn Fn(&str) -> Option<String>, missing: &str) {
        let Some(root) = roots.get(&p.mod_id) else {
            self.reasons.push(format!("{} isn't enabled any more", p.name));
            return;
        };
        match read_text(root, &p.file).and_then(|s| text_of(&s)) {
            None => self.reasons.push(format!("{} {missing}", p.name)),
            Some(text) if fingerprint(&text) != p.hash => self.reasons.push(format!("{} changed it", p.name)),
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
/// its new version and the updated prints. `text_of` reads a source's
/// version from a file's text; the base game is only checked when
/// `game_dir` is known, as [`Patch::recheck`] does.
fn picked_update(kept: &str, prints: &[Print], roots: &HashMap<String, PathBuf>, game_dir: Option<&Path>, text_of: &dyn Fn(&str) -> Option<String>) -> Option<(usize, String, Vec<Print>)> {
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
        let text = read_text(root, &p.file).and_then(|s| text_of(&s))?;
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
    let prefix = if conflicts::is_fios(category) { SORT_FIRST } else { SORT_LAST };
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
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        patch.save(&user, None, "common/buildings", "lab", "lab = { cost = 3 }", prints).unwrap();
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
        assert!(outer.contains(&format!("path=\"{}\"", away.root(&user).display())), "{outer}");
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
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_files_to_win() {
        assert_eq!(file_for("common/buildings", "building_lab"), "common/buildings/伞伞伞_drukhal_building_lab.txt");
        assert_eq!(file_for("common/traits", "trait_x"), "common/traits/!!!!!!_drukhal_trait_x.txt");
        assert_eq!(file_for("events", "ns.12"), "events/!!!!!!_drukhal_ns.12.txt");
        assert_eq!(file_for("common/component_templates", "c"), "common/component_templates/!!!!!!_drukhal_c.txt");
        assert_eq!(file_for("localisation", "l_german:lab"), "localisation/replace/german/drukhal_lab_l_german.yml");
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
}
