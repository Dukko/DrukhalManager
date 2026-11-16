//! Copies of the base game's script objects, one per game version, so the
//! objects mods replace can be checked after a game update: when Paradox
//! changes an object a mod ships its own copy of, the mod keeps loading the
//! old one and the game's change is silently lost.
//!
//! The copies hold full object texts, not hashes, because showing what the
//! game changed (and merging it into the mod's copy later) needs the old
//! text.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use super::conflicts::{self, Definition, ObjectConflict};
use super::source;
use crate::paradox::script;

/// How many versions are kept on disk; older ones are deleted.
const KEEP: usize = 3;

/// The base game's script objects in one version.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: String,
    /// By "category\tkey": where it's defined and its text.
    objects: BTreeMap<String, Saved>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// Relative to the game folder, `/`-separated.
    pub file: String,
    pub text: String,
}

fn id(category: &str, key: &str) -> String {
    format!("{category}\t{key}")
}

impl Snapshot {
    pub fn get(&self, category: &str, key: &str) -> Option<&Saved> {
        self.objects.get(&id(category, key))
    }

    /// The text of every object, for collecting the game's keywords.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.objects.values().map(|s| s.text.as_str())
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Writes the snapshot, gzip-compressed, replacing any of the same version.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = path(dir, &self.version);
        let tmp = path.with_extension("tmp");
        let mut encoder = GzEncoder::new(std::fs::File::create(&tmp)?, Compression::default());
        encoder.write_all(&serde_json::to_vec(self).map_err(io::Error::other)?)?;
        encoder.finish()?;
        crate::platform::rename(tmp, path)
    }
}

/// Reads the script objects of the game in `game_dir`. Localisation, and
/// the folders the game merges instead of overriding, are left out.
pub fn take(game_dir: &Path, version: &str) -> Snapshot {
    take_counting(game_dir, version).0
}

/// As [`take`], also giving how many files were read.
pub fn take_counting(game_dir: &Path, version: &str) -> (Snapshot, usize) {
    let mut files = Vec::new();
    for folder in ["common", "events"] {
        let dir = game_dir.join(folder);
        if dir.is_dir() {
            let _ = conflicts::walk(game_dir, &dir, &mut files);
        }
    }
    // The game reads a folder's files in name order; which definition of a
    // key it keeps depends on that order.
    let stem = |p: &str| {
        let name = p.rsplit('/').next().unwrap_or(p);
        name.rsplit_once('.').map_or(name, |(s, _)| s).to_owned()
    };
    let mut by_category: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        match conflicts::object_category(&file) {
            Some(c) if c != "localisation" => by_category.entry(c).or_default().push(file),
            _ => {}
        }
    }
    let mut objects = BTreeMap::new();
    let mut read = 0;
    for (category, mut files) in by_category {
        files.sort_by_key(|f| stem(f));
        if !conflicts::is_fios(&category) {
            files.reverse();
        }
        // The files are now in priority order: the first definition seen wins.
        for file in files {
            let Ok(bytes) = std::fs::read(game_dir.join(&file)) else { continue };
            read += 1;
            let text = String::from_utf8_lossy(&bytes);
            let src = text.trim_start_matches('\u{feff}');
            for (key, text) in source::index(&category, src) {
                objects.entry(id(&category, &key)).or_insert_with(|| Saved { file: file.clone(), text });
            }
        }
    }
    (Snapshot { version: version.to_owned(), objects }, read)
}

pub fn dir() -> PathBuf {
    crate::platform::cache_dir().join("base-game")
}

fn path(dir: &Path, version: &str) -> PathBuf {
    let safe: String = version.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
    dir.join(format!("{safe}.json.gz"))
}

fn read(path: &Path) -> Option<Snapshot> {
    let mut json = Vec::new();
    GzDecoder::new(std::fs::File::open(path).ok()?).read_to_end(&mut json).ok()?;
    serde_json::from_slice(&json).ok()
}

/// The saved snapshot of `version`; `None` if there isn't one or it can't
/// be read.
pub fn load(dir: &Path, version: &str) -> Option<Snapshot> {
    read(&path(dir, version))
}

fn saved_files(dir: &Path) -> Vec<(PathBuf, std::time::SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json.gz"))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect()
}

/// Deletes all but the most recently saved snapshots.
fn prune(dir: &Path) {
    let mut saved = saved_files(dir);
    saved.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    for (old, _) in saved.into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(old);
    }
}

/// The snapshot of `version`, taken and saved first if there isn't one.
pub fn ensure(dir: &Path, game_dir: &Path, version: &str) -> io::Result<Snapshot> {
    if let Some(found) = load(dir, version) {
        return Ok(found);
    }
    let taken = take(game_dir, version);
    taken.save(dir)?;
    prune(dir);
    Ok(taken)
}

/// The most recently saved snapshot of a different version.
pub fn previous(dir: &Path, version: &str) -> Option<Snapshot> {
    let mut saved = saved_files(dir);
    saved.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    let own = path(dir, version);
    saved.into_iter().filter(|(p, _)| *p != own).find_map(|(p, _)| read(&p)).filter(|s| s.version != version)
}

/// An object an enabled mod replaces whose base-game version changed
/// between two game versions, with the mod's copy still the one used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behind {
    pub category: String,
    pub key: String,
    /// The scanned position of the mod whose copy the game uses.
    pub mod_position: usize,
    /// The mod's file, relative to its root.
    pub file: String,
    /// The game's version before and after; `now` is `None` if the
    /// update removed it.
    pub was: Saved,
    pub now: Option<Saved>,
}

/// Whether two object texts say the same, ignoring whitespace and comments.
fn same(a: &str, b: &str) -> bool {
    match (script::parse(a), script::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.trim() == b.trim(),
    }
}

#[cfg(test)]
pub fn behind(previous: &Snapshot, current: &Snapshot, defined: &BTreeMap<(String, String), Vec<Definition>>, roots: &[Option<PathBuf>]) -> Vec<Behind> {
    behind_counting(previous, current, defined, roots).0
}

/// As [`behind`], also giving how many objects were skipped because the
/// mod's file, or the object in it, couldn't be read.
pub fn behind_counting(previous: &Snapshot, current: &Snapshot, defined: &BTreeMap<(String, String), Vec<Definition>>, roots: &[Option<PathBuf>]) -> (Vec<Behind>, usize) {
    let mut found = Vec::new();
    let mut unreadable = 0;
    for ((category, key), definitions) in defined {
        if category == "localisation" {
            continue;
        }
        let Some(was) = previous.get(category, key) else { continue };
        let now = current.get(category, key);
        if now.is_some_and(|n| same(&was.text, &n.text)) {
            continue;
        }
        // Model the game as one more mod, loaded first. A mod file at the
        // same path replaces the game's file completely.
        let game_file = now.unwrap_or(was).file.clone();
        let mut all = Vec::new();
        if !definitions.iter().any(|d| d.file.eq_ignore_ascii_case(&game_file)) {
            all.push(Definition { mod_position: 0, file: game_file });
        }
        all.extend(definitions.iter().map(|d| Definition { mod_position: d.mod_position + 1, file: d.file.clone() }));
        let conflict = ObjectConflict { category: category.clone(), key: key.clone(), definitions: all };
        let Some((winner, _)) = conflict.winner() else { continue };
        let winner = &conflict.definitions[winner];
        if winner.mod_position == 0 {
            continue;
        }
        let position = winner.mod_position - 1;
        let mod_text = roots
            .get(position)
            .and_then(Option::as_ref)
            .and_then(|root| std::fs::read(root.join(&winner.file)).ok())
            .and_then(|bytes| source::extract(category, key, String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}')));
        let Some(mod_text) = mod_text else {
            unreadable += 1;
            continue;
        };
        if now.is_some_and(|n| same(&mod_text, &n.text)) {
            continue;
        }
        found.push(Behind { category: category.clone(), key: key.clone(), mod_position: position, file: winner.file.clone(), was: was.clone(), now: now.cloned() });
    }
    found.sort_by(|a, b| (a.mod_position, &a.category, &a.key).cmp(&(b.mod_position, &b.category, &b.key)));
    (found, unreadable)
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

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("drukhal-base-game-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn snapshot(version: &str, objects: &[(&str, &str, &str, &str)]) -> Snapshot {
        Snapshot { version: version.to_owned(), objects: objects.iter().map(|(c, k, f, t)| (id(c, k), Saved { file: (*f).to_owned(), text: (*t).to_owned() })).collect() }
    }

    /// Definitions as the scan finds them, for one mod at position 0.
    fn defined(entries: &[(&str, &str, &str)]) -> BTreeMap<(String, String), Vec<Definition>> {
        entries.iter().map(|(c, k, f)| ((c.to_string(), k.to_string()), vec![Definition { mod_position: 0, file: f.to_string() }])).collect()
    }

    #[test]
    fn snapshots_script_objects_but_not_localisation_or_merged_folders() {
        let dir = temp("take");
        write(
            &dir,
            &[
                ("common/technology/00_tech.txt", "tech_a = { cost = 1 }\ntech_b = { cost = 2 }\n"),
                ("events/a.txt", "namespace = a\ncountry_event = { id = a.1 }\n"),
                ("common/on_actions/00.txt", "on_game_start = { events = { a.1 } }\n"),
                ("localisation/english/x_l_english.yml", "l_english:\n k: \"v\"\n"),
            ],
        );
        let snap = take(&dir, "v1");
        assert_eq!(snap.len(), 3);
        assert_eq!(snap.get("common/technology", "tech_a"), Some(&Saved { file: "common/technology/00_tech.txt".into(), text: "tech_a = { cost = 1 }".into() }));
        assert_eq!(snap.get("common/technology", "tech_b").unwrap().text, "tech_b = { cost = 2 }");
        assert_eq!(snap.get("events", "a.1").unwrap().file, "events/a.txt");
        assert_eq!(snap.get("events", "a.1").unwrap().text, "country_event = { id = a.1 }");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_through_the_cache_folder() {
        let game = temp("rt-game");
        let cache = temp("rt-cache");
        write(&game, &[("common/technology/00_tech.txt", "tech_a = { cost = 1 }\n")]);
        let snap = take(&game, "v1.2");
        snap.save(&cache).unwrap();
        let loaded = load(&cache, "v1.2").unwrap();
        assert_eq!(loaded.version, "v1.2");
        assert_eq!(loaded.objects, snap.objects);
        let bytes = std::fs::read(path(&cache, "v1.2")).unwrap();
        assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
        assert!(load(&cache, "v9").is_none());
        let _ = std::fs::remove_dir_all(&game);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn keeps_three_snapshots_and_finds_the_previous_one() {
        let game = temp("keep-game");
        let cache = temp("keep-cache");
        write(&game, &[("common/technology/00_tech.txt", "tech_a = { cost = 1 }\n")]);
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(1000);
        for (i, version) in ["v1", "v2", "v3", "v4"].iter().enumerate() {
            // `ensure` prunes right after saving, so set each time before the next.
            ensure(&cache, &game, version).unwrap();
            let file = std::fs::File::options().write(true).open(path(&cache, version)).unwrap();
            file.set_modified(base + std::time::Duration::from_secs(i as u64 * 10)).unwrap();
        }
        // v4 was saved with the newest mtime only after pruning ran, so prune once more.
        prune(&cache);
        assert_eq!(saved_files(&cache).len(), 3);
        assert!(load(&cache, "v1").is_none());
        assert_eq!(previous(&cache, "v4").unwrap().version, "v3");
        let _ = std::fs::remove_dir_all(&game);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn lists_objects_a_mod_replaces_that_the_update_changed() {
        let dir = temp("behind1");
        write(&dir, &[("m/common/technology/zz_mod.txt", "tech_a = { cost = 1 }\n")]);
        let was = snapshot("v1", &[("common/technology", "tech_a", "common/technology/00_tech.txt", "tech_a = { cost = 1 }")]);
        let now = snapshot("v2", &[("common/technology", "tech_a", "common/technology/00_tech.txt", "tech_a = { cost = 2 }")]);
        let found = behind(&was, &now, &defined(&[("common/technology", "tech_a", "common/technology/zz_mod.txt")]), &[Some(dir.join("m"))]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "tech_a");
        assert_eq!(found[0].mod_position, 0);
        assert_eq!(found[0].file, "common/technology/zz_mod.txt");
        assert_eq!(found[0].now.as_ref().unwrap().text, "tech_a = { cost = 2 }");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_objects_the_update_left_alone() {
        let dir = temp("behind2");
        write(&dir, &[("m/common/technology/zz_mod.txt", "tech_a = { cost = 1 }\n")]);
        let was = snapshot("v1", &[("common/technology", "tech_a", "common/technology/00_tech.txt", "tech_a = { cost = 1 }")]);
        let now = snapshot("v2", &[("common/technology", "tech_a", "common/technology/00_tech.txt", "# note\ntech_a = {\n  cost = 1 # same\n}")]);
        assert!(behind(&was, &now, &defined(&[("common/technology", "tech_a", "common/technology/zz_mod.txt")]), &[Some(dir.join("m"))]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_a_mod_that_already_matches_the_new_game() {
        let dir = temp("behind3");
        write(&dir, &[("m/common/technology/zz_mod.txt", "tech_a = { cost = 2 }\n")]);
        let was = snapshot("v1", &[("common/technology", "tech_a", "common/technology/00_tech.txt", "tech_a = { cost = 1 }")]);
        let now = snapshot("v2", &[("common/technology", "tech_a", "common/technology/00_tech.txt", "tech_a = { cost = 2 }")]);
        assert!(behind(&was, &now, &defined(&[("common/technology", "tech_a", "common/technology/zz_mod.txt")]), &[Some(dir.join("m"))]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_objects_the_update_removed() {
        let dir = temp("behind4");
        write(&dir, &[("m/common/technology/zz_mod.txt", "tech_a = { cost = 1 }\n")]);
        let was = snapshot("v1", &[("common/technology", "tech_a", "common/technology/00_tech.txt", "tech_a = { cost = 1 }")]);
        let now = snapshot("v2", &[]);
        let found = behind(&was, &now, &defined(&[("common/technology", "tech_a", "common/technology/zz_mod.txt")]), &[Some(dir.join("m"))]);
        assert_eq!(found.len(), 1);
        assert!(found[0].now.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn respects_which_copy_the_game_uses() {
        let dir = temp("behind5");
        write(&dir, &[("m/common/traits/zz.txt", "trait_a = { cost = 1 }\n"), ("n/common/traits/00_traits.txt", "trait_a = { cost = 1 }\n")]);
        let was = snapshot("v1", &[("common/traits", "trait_a", "common/traits/00_traits.txt", "trait_a = { cost = 1 }")]);
        let now = snapshot("v2", &[("common/traits", "trait_a", "common/traits/00_traits.txt", "trait_a = { cost = 2 }")]);
        // The folder keeps the first definition, and `00_traits` sorts before `zz`.
        let loses = behind(&was, &now, &defined(&[("common/traits", "trait_a", "common/traits/zz.txt")]), &[Some(dir.join("m"))]);
        assert!(loses.is_empty());
        // A mod file at the game's own path replaces it.
        let wins = behind(&was, &now, &defined(&[("common/traits", "trait_a", "common/traits/00_traits.txt")]), &[Some(dir.join("n"))]);
        assert_eq!(wins.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads the real game: how long a snapshot takes and how big it is.
    /// `DRUKHAL_GAME=<game folder> cargo test --release base_game_snapshot_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn base_game_snapshot_cost() {
        let Some(game) = std::env::var_os("DRUKHAL_GAME") else {
            println!("DRUKHAL_GAME is not set");
            return;
        };
        let started = std::time::Instant::now();
        let (snap, files) = take_counting(Path::new(&game), "measure");
        let seconds = started.elapsed().as_secs_f64();
        let cache = temp("cost");
        snap.save(&cache).unwrap();
        let gz = std::fs::metadata(path(&cache, "measure")).unwrap().len();
        println!("SNAPSHOT objects={} files_read={files} seconds={seconds:.1} gz_bytes={gz}", snap.len());
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// What the check adds to every scan once an older snapshot exists:
    /// loading both snapshots and comparing them against all Workshop mods.
    /// `DRUKHAL_GAME=<game folder> DRUKHAL_WORKSHOP=<workshop folder> cargo test --release base_game_check_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn base_game_check_cost() {
        let (Some(game), Some(workshop)) = (std::env::var_os("DRUKHAL_GAME"), std::env::var_os("DRUKHAL_WORKSHOP")) else {
            println!("DRUKHAL_GAME or DRUKHAL_WORKSHOP is not set");
            return;
        };
        let now = take(Path::new(&game), "vnow");
        let mut folders: Vec<PathBuf> = std::fs::read_dir(&workshop).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        folders.sort();
        let inputs: Vec<conflicts::ModInput> = folders.iter().map(|f| conflicts::ModInput { root: Some(f.clone()), replace_path: Vec::new() }).collect();
        let roots: Vec<Option<PathBuf>> = folders.iter().cloned().map(Some).collect();
        let report = conflicts::analyze(&inputs, None);

        // The older copy differs from the current one only by a comment, like
        // an update that left most objects alone.
        let mut old = Snapshot { version: "vold".into(), objects: BTreeMap::new() };
        for (key, saved) in &now.objects {
            old.objects.insert(key.clone(), Saved { file: saved.file.clone(), text: format!("{}\n# old", saved.text) });
        }
        let cache = temp("check-cost");
        now.save(&cache).unwrap();
        old.save(&cache).unwrap();
        let file = std::fs::File::options().write(true).open(path(&cache, "vold")).unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();

        let best = |mut run: Box<dyn FnMut()>| {
            (0..3)
                .map(|_| {
                    let started = std::time::Instant::now();
                    run();
                    started.elapsed().as_secs_f64() * 1000.0
                })
                .fold(f64::MAX, f64::min)
        };
        let load_ms = best(Box::new(|| drop(std::hint::black_box(load(&cache, "vnow")))));
        let previous_ms = best(Box::new(|| drop(std::hint::black_box(previous(&cache, "vnow")))));
        let behind_ms = best(Box::new(|| drop(std::hint::black_box(behind_counting(&old, &now, &report.defined, &roots)))));
        let overlapping = report.defined.keys().filter(|(c, k)| old.get(c, k).is_some()).count();
        println!("COST load_ms={load_ms:.1} previous_ms={previous_ms:.1} behind_ms={behind_ms:.1} overlapping_keys={overlapping}");
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// Pretends the real game was updated: an older copy of it differs from
    /// the current one in 5 objects that Workshop mods also define, and only
    /// by comments in every other one. Reads the game and the mods only.
    /// `DRUKHAL_GAME=<game folder> DRUKHAL_WORKSHOP=<workshop folder> cargo test --release simulated_update_on_the_real_game -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn simulated_update_on_the_real_game() {
        let (Some(game), Some(workshop)) = (std::env::var_os("DRUKHAL_GAME"), std::env::var_os("DRUKHAL_WORKSHOP")) else {
            println!("DRUKHAL_GAME or DRUKHAL_WORKSHOP is not set");
            return;
        };
        let now = take(Path::new(&game), "vnow");
        let mut folders: Vec<PathBuf> = std::fs::read_dir(&workshop).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        folders.sort();
        let inputs: Vec<conflicts::ModInput> = folders.iter().map(|f| conflicts::ModInput { root: Some(f.clone()), replace_path: Vec::new() }).collect();
        let roots: Vec<Option<PathBuf>> = folders.iter().cloned().map(Some).collect();
        let report = conflicts::analyze(&inputs, None);

        // The text a mod's copy has, if the object can be read from it.
        let mod_text = |category: &str, key: &str, d: &Definition| {
            let bytes = std::fs::read(roots[d.mod_position].as_ref()?.join(&d.file)).ok()?;
            source::extract(category, key, String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}'))
        };
        // Five objects whose first mod definition differs from the new game's,
        // so there is something to find.
        let mut changed = Vec::new();
        for ((category, key), definitions) in &report.defined {
            if changed.len() == 5 {
                break;
            }
            let Some(game_copy) = now.get(category, key) else { continue };
            if definitions.iter().any(|d| mod_text(category, key, d).is_some_and(|t| !same(&t, &game_copy.text))) {
                changed.push((category.clone(), key.clone()));
            }
        }

        let mut old = Snapshot { version: "vold".into(), objects: BTreeMap::new() };
        for (category, key) in report.defined.keys() {
            let Some(game_copy) = now.get(category, key) else { continue };
            let mut text = match game_copy.text.rfind('}') {
                Some(end) => format!("{} # old\n{}", &game_copy.text[..end], &game_copy.text[end..]),
                None => format!("{}\n# old", game_copy.text),
            };
            if changed.contains(&(category.clone(), key.clone())) {
                // A changed value: an extra assignment right after the first brace.
                text = match text.find('{') {
                    Some(open) => format!("{}\n\told_game_only = yes\n{}", &text[..=open], &text[open + 1..]),
                    None => format!("{text}\nold_game_only = yes"),
                };
            }
            old.objects.insert(id(category, key), Saved { file: game_copy.file.clone(), text });
        }

        let found = behind(&old, &now, &report.defined, &roots);
        println!("SIMULATED behind={}", found.len());
        for b in found.iter().take(5) {
            println!("  example: {} {} (mod #{} {})", b.category, b.key, b.mod_position, b.file);
        }
        for (category, key) in &changed {
            let listed = found.iter().any(|b| &b.category == category && &b.key == key);
            let definitions = &report.defined[&(category.clone(), key.clone())];
            let game_file = now.get(category, key).unwrap().file.clone();
            let why = if listed {
                "listed: a mod's copy is the one used and differs from the new game".to_owned()
            } else {
                let mut all = Vec::new();
                if !definitions.iter().any(|d| d.file.eq_ignore_ascii_case(&game_file)) {
                    all.push(Definition { mod_position: 0, file: game_file.clone() });
                }
                all.extend(definitions.iter().map(|d| Definition { mod_position: d.mod_position + 1, file: d.file.clone() }));
                let winner = ObjectConflict { category: category.clone(), key: key.clone(), definitions: all }.winner().map(|(i, _)| i);
                match winner {
                    Some(0) if !definitions.iter().any(|d| d.file.eq_ignore_ascii_case(&game_file)) => format!("not listed: the game's own file {game_file} wins over the mods' copies"),
                    _ => "not listed: the mod copy that is used already matches the new game (or couldn't be read)".to_owned(),
                }
            };
            println!("  changed {category} {key}: {why}");
        }
    }
}
