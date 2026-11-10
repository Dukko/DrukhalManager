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
    /// What each source looked like when merged; empty for merges saved
    /// before fingerprints existed.
    #[serde(default)]
    pub prints: Vec<Print>,
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
    pub fn save_file(&mut self, user_dir: &Path, game_version: Option<&str>, path: &str, contents: &str, prints: Vec<Print>) -> io::Result<()> {
        let sources = prints.iter().filter(|p| p.mod_id != VANILLA).map(|p| p.name.clone()).collect();
        self.write_descriptors(user_dir, game_version)?;
        let target = self.root.join(path);
        std::fs::create_dir_all(target.parent().unwrap())?;
        write_atomic(&target, contents.as_bytes())?;
        let merge = FileMerge { path: path.to_owned(), sources, prints };
        match self.manifest.files.iter_mut().find(|f| f.path.eq_ignore_ascii_case(path)) {
            Some(existing) => *existing = merge,
            None => self.manifest.files.push(merge),
        }
        self.write_manifest()
    }

    /// Removes a merged object and its file.
    pub fn remove(&mut self, category: &str, key: &str) -> io::Result<()> {
        if let Some(at) = self.manifest.merges.iter().position(|m| m.category == category && m.key == key) {
            let merge = self.manifest.merges.remove(at);
            remove_if_present(&self.root.join(&merge.file))?;
        }
        self.write_manifest()
    }

    /// Removes a merged or fixed file.
    pub fn remove_file(&mut self, path: &str) -> io::Result<()> {
        if let Some(at) = self.manifest.files.iter().position(|f| f.path.eq_ignore_ascii_case(path)) {
            let merge = self.manifest.files.remove(at);
            remove_if_present(&self.root.join(&merge.path))?;
        }
        self.write_manifest()
    }

    /// Merges whose sources changed since they were saved. `roots` maps the
    /// enabled mods' descriptor ids, and [`VANILLA`], to their folders.
    pub fn stale(&self, roots: &std::collections::HashMap<String, PathBuf>) -> Vec<Stale> {
        let check = |prints: &[Print], text_of: &dyn Fn(&str) -> Option<String>, missing: &str| -> Vec<String> {
            prints
                .iter()
                .filter_map(|p| {
                    let Some(root) = roots.get(&p.mod_id) else {
                        return Some(format!("{} isn't enabled any more", p.name));
                    };
                    let src = std::fs::read(root.join(&p.file)).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
                    match src.as_deref().and_then(|s| text_of(s)) {
                        None => Some(format!("{} {missing}", p.name)),
                        Some(text) if fingerprint(&text) != p.hash => Some(if p.mod_id == VANILLA { "The base game changed it in an update".to_owned() } else { format!("{} changed it", p.name) }),
                        Some(_) => None,
                    }
                })
                .collect()
        };
        let mut stale = Vec::new();
        for m in &self.manifest.merges {
            let (category, key) = (m.category.clone(), m.key.clone());
            if m.prints.is_empty() {
                stale.push(Stale { object: Some((category, key)), path: m.file.clone(), reasons: vec![UNTRACKED.to_owned()], untracked: true });
                continue;
            }
            let reasons = check(&m.prints, &|src| super::source::extract(&category, &key, src), "no longer defines it");
            if !reasons.is_empty() {
                stale.push(Stale { object: Some((m.category.clone(), m.key.clone())), path: m.file.clone(), reasons, untracked: false });
            }
        }
        for f in &self.manifest.files {
            if f.prints.is_empty() {
                stale.push(Stale { object: None, path: f.path.clone(), reasons: vec![UNTRACKED.to_owned()], untracked: true });
                continue;
            }
            let reasons = check(&f.prints, &|src| Some(src.to_owned()), "no longer ships this file");
            if !reasons.is_empty() {
                stale.push(Stale { object: None, path: f.path.clone(), reasons, untracked: false });
            }
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

    /// Records `prints` as what a merge's sources look like, so later
    /// changes to them are noticed. For merges saved before fingerprints.
    pub fn track(&mut self, object: Option<&(String, String)>, path: &str, prints: Vec<Print>) -> io::Result<()> {
        let slot = match object {
            Some((category, key)) => self.manifest.merges.iter_mut().find(|m| &m.category == category && &m.key == key).map(|m| &mut m.prints),
            None => self.manifest.files.iter_mut().find(|f| f.path.eq_ignore_ascii_case(path)).map(|f| &mut f.prints),
        };
        let Some(slot) = slot else { return Err(io::Error::new(io::ErrorKind::NotFound, "It is no longer in the patch")) };
        *slot = prints;
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
        let sources = prints.iter().filter(|p| p.mod_id != VANILLA).map(|p| p.name.clone()).collect();
        self.write_descriptors(user_dir, game_version)?;
        let file = file_for(category, key);
        let path = self.root.join(&file);
        std::fs::create_dir_all(path.parent().unwrap())?;
        write_atomic(&path, render(category, key, script).as_bytes())?;

        let merge = Merge { category: category.into(), key: key.into(), script: script.into(), sources, file, prints };
        match self.manifest.merges.iter_mut().find(|m| m.category == category && m.key == key) {
            Some(existing) => *existing = merge,
            None => self.manifest.merges.push(merge),
        }
        self.write_manifest()
    }

    fn write_descriptors(&self, user_dir: &Path, game_version: Option<&str>) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let version = supported_version(game_version);
        let inner = format!("name=\"{NAME}\"\ntags={{\n\t\"Fixes\"\n}}\nsupported_version=\"{version}\"\n");
        let outer = format!("{inner}path=\"{}\"\n", self.root.display());
        write_atomic(&self.root.join("descriptor.mod"), inner.as_bytes())?;
        write_atomic(&user_dir.join(ID), outer.as_bytes())
    }
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
    fn notices_when_sources_change() {
        let root = std::env::temp_dir().join(format!("drukhal-stale-{}", std::process::id()));
        let (user, a, b, game) = (root.join("user"), root.join("a"), root.join("b"), root.join("game"));
        for (dir, file, text) in [(&a, "common/buildings/a.txt", "lab = { cost = 1 }\nfarm = {}"), (&b, "common/buildings/b.txt", "lab = { cost = 2 }"), (&game, "common/buildings/00.txt", "lab = { cost = 0 }")] {
            std::fs::create_dir_all(dir.join("common/buildings")).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
        }
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mut patch = Patch::open(&user).unwrap();
        let prints = vec![
            Print::new(VANILLA, "Vanilla", "common/buildings/00.txt", "lab = { cost = 0 }"),
            Print::new("mod/a.mod", "A", "common/buildings/a.txt", "lab = {\n\tcost = 1\n}"),
            Print::new("mod/b.mod", "B", "common/buildings/b.txt", "lab = { cost = 2 }"),
        ];
        patch.save(&user, None, "common/buildings", "lab", "lab = { cost = 3 }", prints).unwrap();
        let mut roots: std::collections::HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a.clone()), ("mod/b.mod".to_owned(), b.clone()), (VANILLA.to_owned(), game.clone())].into();
        assert!(patch.stale(&roots).is_empty(), "reformatting and unrelated objects don't count");

        std::fs::write(b.join("common/buildings/b.txt"), "lab = { cost = 5 }").unwrap();
        std::fs::write(game.join("common/buildings/00.txt"), "").unwrap();
        roots.remove("mod/a.mod");
        let stale = patch.stale(&roots);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].object, Some(("common/buildings".into(), "lab".into())));
        assert_eq!(stale[0].reasons, ["Vanilla no longer defines it", "A isn't enabled any more", "B changed it"]);

        patch.remove("common/buildings", "lab").unwrap();
        assert!(Patch::open(&user).unwrap().get("common/buildings", "lab").is_none());
        assert!(!Patch::root(&user).join(file_for("common/buildings", "lab")).exists());
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
        let dir = Patch::root(&user);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = r#"{"merges":[{"category":"common/buildings","key":"lab","script":"lab = { cost = 3 }","sources":["A","B"],"file":"common/buildings/伞伞伞_drukhal_lab.txt"}],
                         "files":[{"path":"common/buildings/shared.txt","sources":["A","B"]}]}"#;
        std::fs::write(dir.join(MANIFEST), legacy).unwrap();
        let mut patch = Patch::open(&user).unwrap();
        let mut roots: std::collections::HashMap<String, PathBuf> = [("mod/a.mod".to_owned(), a.clone()), ("mod/b.mod".to_owned(), b.clone()), (VANILLA.to_owned(), game.clone())].into();

        let stale = patch.stale(&roots);
        assert_eq!(stale.len(), 2);
        assert!(stale.iter().all(|s| s.untracked && s.reasons == [UNTRACKED]));
        assert_eq!(patch.sources(stale[0].object.as_ref(), &stale[0].path).unwrap(), ["A", "B"]);

        // Keeping them records the mods' (and the game's) current versions.
        let (ma, mb) = (("mod/a.mod".to_owned(), "A".to_owned(), a.clone()), ("mod/b.mod".to_owned(), "B".to_owned(), b.clone()));
        let prints = object_prints("common/buildings", "lab", Some(&game), &[(ma.clone(), "common/buildings/a.txt".into()), (mb.clone(), "common/buildings/b.txt".into())]);
        assert_eq!(prints.iter().map(|p| p.mod_id.as_str()).collect::<Vec<_>>(), [VANILLA, "mod/a.mod", "mod/b.mod"]);
        patch.track(stale[0].object.as_ref(), &stale[0].path, prints).unwrap();
        patch.track(None, "common/buildings/shared.txt", file_prints("common/buildings/shared.txt", Some(&game), &[ma, mb])).unwrap();
        let mut patch = Patch::open(&user).unwrap();
        assert!(patch.stale(&roots).is_empty(), "tracked and unchanged");

        // From now on, changes are noticed.
        std::fs::write(b.join("common/buildings/b.txt"), "lab = { cost = 9 }").unwrap();
        roots.remove("mod/a.mod");
        let stale = patch.stale(&roots);
        assert_eq!(stale.iter().map(|s| (s.untracked, s.reasons.clone())).collect::<Vec<_>>(), [
            (false, vec!["A isn't enabled any more".to_owned(), "B changed it".to_owned()]),
            (false, vec!["A isn't enabled any more".to_owned()]),
        ]);
        assert!(patch.track(Some(&("events".into(), "gone".into())), "", vec![]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

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
        patch.save(&user, Some("v4.5.3"), "events", "ns.1", "country_event = { id = ns.1 }", vec![Print::new("mod/a.mod", "A", "events/a.txt", "x"), Print::new("mod/b.mod", "B", "events/b.txt", "y")]).unwrap();
        patch.save(&user, Some("v4.5.3"), "localisation", "l_english:lab", "lab:0 \"Lab\"", vec![]).unwrap();
        patch.save(&user, Some("v4.5.3"), "events", "ns.1", "country_event = { id = ns.1 hide_window = yes }", vec![Print::new("mod/a.mod", "A", "events/a.txt", "x")]).unwrap();

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
        patch.save_file(&user, None, "common/buildings/00_x.txt", "a = {}\n", vec![Print::new("mod/a.mod", "A", "common/buildings/00_x.txt", "a"), Print::new("mod/b.mod", "B", "common/buildings/00_x.txt", "b")]).unwrap();
        let patch = Patch::open(&user).unwrap();
        assert!(patch.get("events", "ns.1").is_some(), "object merges survive");
        assert_eq!(patch.get_file("common/buildings/00_X.txt").unwrap().sources, ["A", "B"]);
        assert_eq!(std::fs::read_to_string(Patch::root(&user).join("common/buildings/00_x.txt")).unwrap(), "a = {}\n");

        std::fs::remove_dir_all(user).unwrap();
    }
}
