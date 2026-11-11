//! Multiplayer check: a report of the enabled mods' files and the base
//! game's checksummed files, with a hash of each, to compare with another
//! player's and find what differs before a game won't start (or desyncs).
//!
//! Follows Irony Mod Manager's hash report (MIT, © 2020 Mario,
//! https://github.com/bcssov/IronyModManager) in what it covers, in its own
//! format: only the base game folders the game's own checksum covers are
//! read, and load order is compared too.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Folders the game's multiplayer checksum covers.
pub const CHECKSUM_FOLDERS: &[&str] = &["common", "events", "map", "localisation_synced"];
/// Every folder of game content, for a report of all the game's files:
/// a modified or corrupted file outside the checksum still shows.
pub const CONTENT_FOLDERS: &[&str] = &[
    "common", "events", "map", "localisation_synced", "localisation", "interface", "gfx", "fonts", "flags", "prescripted_countries", "sound", "music",
];

const FORMAT: &str = "drukhal-multiplayer-report";
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    format: String,
    version: u32,
    pub game_version: Option<String>,
    /// Whether `game` covers all the game's content, not just the
    /// checksummed folders.
    #[serde(default)]
    pub all_game_files: bool,
    /// Which build of the game made it; `None` in reports from before
    /// builds were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<Build>,
    /// The base game's files: path -> hash.
    pub game: BTreeMap<String, String>,
    /// Enabled mods, in load order.
    pub mods: Vec<ModReport>,
}

/// A build of the game. On Linux, Steam installs either the native one or,
/// when a Proton version is forced, the Windows one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Build {
    Linux,
    Windows,
}

impl Build {
    /// The build whose executable is `exe`.
    pub fn of(exe: &Path) -> Self {
        if exe.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) { Self::Windows } else { Self::Linux }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Linux => "Native Linux build",
            Self::Windows => "Windows build",
        }
    }
}

/// Why players on different builds may not be able to play together, and
/// what helps.
pub const BUILDS_DIFFER: &str = "Players report that the native Linux build's checksum doesn't match the Windows build's, even with the same mods and files. Whoever runs the native Linux build can switch to the Windows build through Proton: in Steam, open Stellaris's Properties, and under Compatibility force a Proton version.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModReport {
    pub name: String,
    /// Workshop id, which identifies the mod better than its name.
    pub steam_id: Option<u64>,
    /// Content files: path -> hash.
    pub files: BTreeMap<String, String>,
}

/// One enabled mod to report.
pub struct ModInput {
    pub name: String,
    pub steam_id: Option<u64>,
    /// Its folder (a zipped mod's unpacked copy), if found.
    pub root: Option<PathBuf>,
}

/// Hashes of files already read, by path, size and modification time, so
/// reading tens of thousands of game files is slow only once.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct HashCache {
    files: HashMap<PathBuf, (u64, u128, String)>,
    #[serde(skip)]
    changed: bool,
}

impl HashCache {
    pub fn path() -> PathBuf {
        gtk::glib::user_cache_dir().join("DrukhalManager/file-hashes.json")
    }

    /// The cache at `path`; empty if there is none or it can't be read.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Writes the cache, if it changed, dropping files that are gone.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if !self.changed {
            return Ok(());
        }
        self.files.retain(|p, _| p.exists());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    fn hash(&mut self, path: &Path) -> std::io::Result<String> {
        let meta = std::fs::metadata(path)?;
        let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        if let Some((size, time, hash)) = self.files.get(path)
            && (*size, *time) == (meta.len(), modified)
        {
            return Ok(hash.clone());
        }
        let hash = hash_file(path)?;
        self.files.insert(path.to_owned(), (meta.len(), modified, hash.clone()));
        self.changed = true;
        Ok(hash)
    }
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// How far building a report has got, for a progress bar: files hashed
/// out of the total, which is 0 until the files are counted.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

impl Progress {
    /// From 0 to 1, or `None` while still counting files.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0))
    }
}

/// Every content file under `root`, or only those in `folders`.
fn list_tree(root: &Path, folders: Option<&[&str]>) -> Vec<String> {
    let mut files = Vec::new();
    match folders {
        Some(folders) => {
            for folder in folders {
                let dir = root.join(folder);
                if dir.is_dir() {
                    let _ = crate::merge::conflicts::walk(root, &dir, &mut files);
                }
            }
        }
        None => {
            let _ = crate::merge::conflicts::walk(root, root, &mut files);
        }
    }
    files
}

fn hash_tree(root: &Path, files: Vec<String>, progress: &Progress, cache: &mut HashCache) -> BTreeMap<String, String> {
    files
        .into_iter()
        .filter_map(|f| {
            let hash = cache.hash(&root.join(&f));
            progress.done.fetch_add(1, Ordering::Relaxed);
            // Paths compare case-insensitively, as the game loads them.
            Some((f.to_lowercase(), hash.ok()?))
        })
        .collect()
}

/// Builds the report, with all the game's content files or only the
/// checksummed ones. Reads every file not in `cache`, so call off the main
/// thread.
pub fn report(game_dir: Option<&Path>, game_version: Option<&str>, all_game_files: bool, mods: &[ModInput], progress: &Progress, cache: &mut HashCache) -> Report {
    // Count every file first, so progress has a total.
    let folders = if all_game_files { CONTENT_FOLDERS } else { CHECKSUM_FOLDERS };
    let game_files = game_dir.map(|dir| list_tree(dir, Some(folders))).unwrap_or_default();
    let mod_files: Vec<Vec<String>> = mods.iter().map(|m| m.root.as_deref().map(|r| list_tree(r, None)).unwrap_or_default()).collect();
    let total = game_files.len() + mod_files.iter().map(Vec::len).sum::<usize>();
    progress.total.store(total.max(1), Ordering::Relaxed);

    Report {
        format: FORMAT.into(),
        version: VERSION,
        game_version: game_version.map(str::to_owned),
        all_game_files,
        build: None,
        game: game_dir.map(|dir| hash_tree(dir, game_files, progress, cache)).unwrap_or_default(),
        mods: mods
            .iter()
            .zip(mod_files)
            .map(|(m, files)| ModReport { name: m.name.clone(), steam_id: m.steam_id, files: m.root.as_deref().map(|r| hash_tree(r, files, progress, cache)).unwrap_or_default() })
            .collect(),
    }
}

impl Report {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("serializable")
    }

    pub fn from_json(src: &str) -> Result<Self, String> {
        let report: Self = serde_json::from_str(src).map_err(|_| "This isn't a DrukhalManager multiplayer report".to_owned())?;
        if report.format != FORMAT {
            return Err("This isn't a DrukhalManager multiplayer report".into());
        }
        if report.version > VERSION {
            return Err("This report is from a newer version of DrukhalManager; update to compare it".into());
        }
        Ok(report)
    }
}

/// Whether the game's multiplayer checksum covers `path`.
pub fn in_checksum(path: &str) -> bool {
    CHECKSUM_FOLDERS.iter().any(|f| path.len() > f.len() && path.starts_with(f) && path.as_bytes()[f.len()] == b'/')
}

/// Whether a mod folder changes files the game checksums, which turns off
/// achievements (and must match to play together).
pub fn changes_checksum(root: &Path) -> bool {
    CHECKSUM_FOLDERS.iter().any(|f| has_files(&root.join(f)))
}

fn has_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| match e.file_type() {
        Ok(t) if t.is_dir() => has_files(&e.path()),
        Ok(_) => true,
        Err(_) => false,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiff {
    Changed(String),
    OnlyMine(String),
    OnlyTheirs(String),
}

impl FileDiff {
    pub fn path(&self) -> &str {
        match self {
            Self::Changed(p) | Self::OnlyMine(p) | Self::OnlyTheirs(p) => p,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDiff {
    pub name: String,
    pub files: Vec<FileDiff>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// `(mine, theirs)`, if the game versions differ.
    pub game_version: Option<(String, String)>,
    /// `(mine, theirs)`, if the reports come from different builds.
    pub builds: Option<(Build, Build)>,
    pub game_files: Vec<FileDiff>,
    pub only_mine: Vec<String>,
    pub only_theirs: Vec<String>,
    /// Mods both have, if their order differs: names in my order, with
    /// their position (1-based) in each list.
    pub order: Vec<(String, usize, usize)>,
    pub mods: Vec<ModDiff>,
}

impl Comparison {
    pub fn matches(&self) -> bool {
        *self == Self::default()
    }
}

fn diff_files(mine: &BTreeMap<String, String>, theirs: &BTreeMap<String, String>) -> Vec<FileDiff> {
    let mut out = Vec::new();
    for (path, hash) in mine {
        match theirs.get(path) {
            None => out.push(FileDiff::OnlyMine(path.clone())),
            Some(h) if h != hash => out.push(FileDiff::Changed(path.clone())),
            Some(_) => {}
        }
    }
    out.extend(theirs.keys().filter(|p| !mine.contains_key(*p)).map(|p| FileDiff::OnlyTheirs(p.clone())));
    out.sort_by(|a, b| a.path().cmp(b.path()));
    out
}

/// What differs between my report and another player's.
pub fn compare(mine: &Report, theirs: &Report) -> Comparison {
    // Mods match by Workshop id, else by name.
    let key = |m: &ModReport| m.steam_id.map_or_else(|| format!("name:{}", m.name.to_lowercase()), |id| format!("id:{id}"));
    let their_mods: HashMap<String, (usize, &ModReport)> = theirs.mods.iter().enumerate().map(|(i, m)| (key(m), (i, m))).collect();
    let my_keys: std::collections::HashSet<String> = mine.mods.iter().map(key).collect();

    let mut c = Comparison::default();
    if let (Some(a), Some(b)) = (&mine.game_version, &theirs.game_version)
        && a != b
    {
        c.game_version = Some((a.clone(), b.clone()));
    }
    if let (Some(a), Some(b)) = (mine.build, theirs.build)
        && a != b
    {
        c.builds = Some((a, b));
    }
    // A report without game files (game not found) can't be compared there.
    if !mine.game.is_empty() && !theirs.game.is_empty() {
        // Only what both reports cover.
        if mine.all_game_files == theirs.all_game_files {
            c.game_files = diff_files(&mine.game, &theirs.game);
        } else {
            let checksummed = |r: &Report| r.game.iter().filter(|(p, _)| in_checksum(p)).map(|(p, h)| (p.clone(), h.clone())).collect::<BTreeMap<_, _>>();
            c.game_files = diff_files(&checksummed(mine), &checksummed(theirs));
        }
    }

    let mut shared = Vec::new();
    for m in &mine.mods {
        match their_mods.get(&key(m)) {
            None => c.only_mine.push(m.name.clone()),
            Some((their_pos, theirs)) => {
                shared.push((m.name.clone(), *their_pos));
                let files = diff_files(&m.files, &theirs.files);
                if !files.is_empty() {
                    c.mods.push(ModDiff { name: m.name.clone(), files });
                }
            }
        }
    }
    c.only_theirs = theirs.mods.iter().filter(|m| !my_keys.contains(&key(m))).map(|m| m.name.clone()).collect();

    // Order among the mods both have.
    let mut their_order: Vec<usize> = shared.iter().map(|(_, p)| *p).collect();
    their_order.sort_unstable();
    let their_rank: HashMap<usize, usize> = their_order.iter().enumerate().map(|(rank, p)| (*p, rank)).collect();
    if shared.iter().enumerate().any(|(i, (_, p))| their_rank[p] != i) {
        c.order = shared.iter().enumerate().map(|(i, (name, p))| (name.clone(), i + 1, their_rank[p] + 1)).collect();
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_builds_apart() {
        assert_eq!(Build::of(Path::new("/g/stellaris")), Build::Linux);
        assert_eq!(Build::of(Path::new("/g/stellaris.exe")), Build::Windows);
        let base = Report { format: FORMAT.into(), version: VERSION, game_version: None, all_game_files: false, build: Some(Build::Linux), game: BTreeMap::new(), mods: vec![] };
        let proton = Report { build: Some(Build::Windows), ..base.clone() };
        assert_eq!(compare(&base, &proton).builds, Some((Build::Linux, Build::Windows)));
        assert!(compare(&base, &Report { build: None, ..base.clone() }).matches(), "older reports don't say");
        let json = proton.to_json();
        assert!(json.contains("\"build\": \"windows\""));
        assert_eq!(Report::from_json(&json).unwrap().build, Some(Build::Windows));
    }

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, body) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    #[test]
    fn reports_and_compares() {
        let root = std::env::temp_dir().join(format!("drukhal-mp-{}", std::process::id()));
        let (game, a, b, b2) = (root.join("game"), root.join("a"), root.join("b"), root.join("b2"));
        write(&game, &[("common/x.txt", "x"), ("gfx/big.dds", "not read"), ("events/e.txt", "e")]);
        write(&a, &[("common/A.txt", "a"), ("gfx/a.dds", "pic"), ("descriptor.mod", "ignored")]);
        write(&b, &[("events/b.txt", "b"), ("gfx/b.dds", "1")]);
        // Their copy of mod B: an event changed, a texture changed, a file added.
        write(&b2, &[("events/b.txt", "b, newer"), ("gfx/b.dds", "2"), ("common/new.txt", "n")]);

        let input = |name: &str, id: Option<u64>, root: &Path| ModInput { name: name.into(), steam_id: id, root: Some(root.to_path_buf()) };
        let progress = Progress::default();
        assert_eq!(progress.fraction(), None, "still counting");
        let mut cache = HashCache::default();
        let mine = report(Some(&game), Some("v4.5.3"), false, &[input("A", Some(1), &a), input("B", Some(2), &b), input("Mine Only", None, &a)], &progress, &mut cache);
        // 2 game files, then A's 2, B's 2 and A's 2 again.
        assert_eq!((progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed)), (8, 8));
        assert_eq!(progress.fraction(), Some(1.0));
        assert_eq!(mine.game.keys().collect::<Vec<_>>(), ["common/x.txt", "events/e.txt"], "only checksummed folders");
        assert_eq!(mine.mods[0].files.keys().collect::<Vec<_>>(), ["common/a.txt", "gfx/a.dds"], "lowercased; root files skipped");

        let same = Report::from_json(&mine.to_json()).unwrap();
        assert!(compare(&mine, &same).matches());

        // They renamed A (same Workshop id), load B first, have another mod
        // and a different game version.
        let mut theirs = report(Some(&game), Some("v4.5.4"), false, &[input("B", Some(2), &b2), input("A renamed", Some(1), &a), input("Theirs Only", Some(9), &a)], &Progress::default(), &mut cache);
        theirs.game.insert("map/setup.txt".into(), "h".into());
        let c = compare(&mine, &theirs);
        assert!(!c.matches());
        assert_eq!(c.game_version, Some(("v4.5.3".into(), "v4.5.4".into())));
        assert_eq!(c.game_files, [FileDiff::OnlyTheirs("map/setup.txt".into())]);
        assert_eq!(c.only_mine, ["Mine Only"]);
        assert_eq!(c.only_theirs, ["Theirs Only"]);
        assert_eq!(c.order, [("A".into(), 1, 2), ("B".into(), 2, 1)]);
        assert_eq!(c.mods.len(), 1);
        assert_eq!(c.mods[0].name, "B");
        assert_eq!(c.mods[0].files, [FileDiff::OnlyTheirs("common/new.txt".into()), FileDiff::Changed("events/b.txt".into()), FileDiff::Changed("gfx/b.dds".into())]);

        // All the game's files: graphics too, compared with a report that
        // has them, and only the checksummed ones against one that hasn't.
        let full = report(Some(&game), Some("v4.5.3"), true, &[], &Progress::default(), &mut cache);
        assert_eq!(full.game.keys().collect::<Vec<_>>(), ["common/x.txt", "events/e.txt", "gfx/big.dds"]);
        let mut modified = full.clone();
        modified.game.insert("gfx/big.dds".into(), "other".into());
        assert_eq!(compare(&full, &modified).game_files, [FileDiff::Changed("gfx/big.dds".into())]);
        let checksum_only = Report { mods: vec![], ..mine.clone() };
        assert!(compare(&modified, &checksum_only).game_files.is_empty(), "gfx isn't in theirs to compare");

        // Cached hashes are used while a file's size and time are unchanged.
        let path = root.join("cache.json");
        cache.save(&path).unwrap();
        let mut loaded = HashCache::load(&path);
        assert_eq!(loaded.files.len(), cache.files.len());
        let file = game.join("common/x.txt");
        let first = loaded.hash(&file).unwrap();
        std::fs::write(&file, "changed!").unwrap();
        assert_ne!(loaded.hash(&file).unwrap(), first, "a new size is read again");

        assert!(changes_checksum(&a) && changes_checksum(&b) && !changes_checksum(&root.join("missing")));
        write(&root.join("gfx_only"), &[("gfx/x.dds", "x"), ("descriptor.mod", "d")]);
        std::fs::create_dir_all(root.join("gfx_only/common/empty")).unwrap();
        assert!(!changes_checksum(&root.join("gfx_only")), "only graphics, and an empty folder");
        assert!(in_checksum("common/new.txt") && !in_checksum("gfx/b.dds") && !in_checksum("common"));
        assert!(Report::from_json("{}").is_err());
        assert!(Report::from_json(&mine.to_json().replace("\"version\": 1", "\"version\": 99")).unwrap_err().contains("newer"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

