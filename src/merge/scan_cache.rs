//! What conflict scans found before, kept between scans and runs, so a
//! rescan only reads the files that changed since.
//!
//! Two things are kept:
//! - the objects each file defines, by its path, size and modification
//!   time, which spares parsing every file again;
//! - whether each object conflict needs no AI to merge, by a fingerprint of
//!   the files that define it, the mods' names and the game version, which
//!   spares reading the base game's files again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Caches of another version are started again. Bump it whenever how files
/// are parsed into keys changes (`object_keys`, `localisation_keys`,
/// `object_category`, the script parser in `src/paradox/script.rs`).
const VERSION: u32 = 6;
/// Caches written by another version of the app keep their files' keys but
/// lose their verdicts, which depend on the merge logic.
const APP: &str = env!("CARGO_PKG_VERSION");
/// Verdicts not used for this many days are dropped.
const KEEP_DAYS: u32 = 30;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ScanCache {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    app: String,
    #[serde(default)]
    files: HashMap<PathBuf, FileKeys>,
    #[serde(default)]
    simple: HashMap<String, Verdict>,
    #[serde(skip)]
    changed: bool,
}

/// Why a file couldn't be read or parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanError {
    pub message: String,
    /// The line the parser failed at, when the file was read but didn't
    /// parse. Read failures have none, so the UI can tell a broken file from
    /// one it lacks the rights to open.
    pub line: Option<usize>,
}

impl From<std::io::Error> for ScanError {
    fn from(e: std::io::Error) -> Self {
        Self { message: e.to_string(), line: None }
    }
}

/// A file's object keys, or why it couldn't be read or parsed.
pub type Keys = Result<Vec<String>, ScanError>;

/// The objects a file defines, as of its size and modification time.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileKeys {
    size: u64,
    modified: u128,
    /// The folder it was read as, which decides how it's parsed.
    category: String,
    /// Its object keys, or why it couldn't be parsed.
    keys: Result<Vec<String>, ScanError>,
}

/// Why a conflict needs no AI to merge, or `None` when it does.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Verdict {
    reason: Option<String>,
    /// Days since the epoch it was last used.
    used: u32,
}

/// A file's size and modification time.
fn stamp(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((meta.len(), modified))
}

fn today() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| (d.as_secs() / 86_400) as u32)
}

impl ScanCache {
    pub fn path() -> PathBuf {
        crate::platform::cache_dir().join("conflict-scan.json")
    }

    /// The cache at `path`; empty if there is none or it can't be read.
    pub fn load(path: &Path) -> Self {
        let mut cache: Self = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if cache.version != VERSION {
            return Self { version: VERSION, app: APP.to_owned(), changed: true, ..Self::default() };
        }
        if cache.app != APP {
            cache.simple.clear();
            cache.app = APP.to_owned();
            cache.changed = true;
        }
        cache
    }

    /// Writes the cache, if it changed, dropping files that are gone and
    /// verdicts long unused.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if !self.changed {
            return Ok(());
        }
        self.files.retain(|p, _| p.exists());
        let oldest = today().saturating_sub(KEEP_DAYS);
        self.simple.retain(|_, v| v.used >= oldest);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.version = VERSION;
        self.app = APP.to_owned();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        crate::platform::rename(tmp, path)?;
        self.changed = false;
        Ok(())
    }

    /// The object keys of each file, given as `(path, category)` for the
    /// folder it's read as: from the cache while the file is unchanged, else
    /// read and given to `parse(category, text)`, several files at once on
    /// separate threads. A file that can't be read isn't kept, so it's tried
    /// again next time. Results are in the order of `files`.
    pub fn keys_many(&mut self, files: &[(PathBuf, String)], parse: impl Fn(&str, &str) -> Keys + Sync) -> Vec<Keys> {
        let stamps: Vec<Option<(u64, u128)>> = files.iter().map(|(path, _)| stamp(path)).collect();
        let mut results: Vec<Option<Keys>> = files
            .iter()
            .zip(&stamps)
            .map(|((path, category), stamp)| {
                let f = self.files.get(path)?;
                (Some((f.size, f.modified)) == *stamp && f.category == *category).then(|| f.keys.clone())
            })
            .collect();
        let missing: Vec<usize> = (0..files.len()).filter(|&i| results[i].is_none()).collect();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(missing.len()).max(1);
        // Read failures come back as `Err` from the read, not the parse, and
        // aren't kept, so they're tried again next time.
        let parsed: Vec<(usize, Result<Keys, ScanError>)> = std::thread::scope(|scope| {
            let parse = &parse;
            let workers: Vec<_> = missing
                .chunks(missing.len().div_ceil(threads).max(1))
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|&i| {
                                let (path, category) = &files[i];
                                let read = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(ScanError::from);
                                (i, read.map(|src| parse(category, &src)))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            // A panicking parser is a bug; pass it on as the serial scan would.
            workers.into_iter().flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
        });
        for (i, outcome) in parsed {
            let keys = match outcome {
                Ok(keys) => {
                    if let Some((size, modified)) = stamps[i] {
                        let (path, category) = &files[i];
                        self.files.insert(path.clone(), FileKeys { size, modified, category: category.clone(), keys: keys.clone() });
                        self.changed = true;
                    }
                    keys
                }
                Err(e) => Err(e),
            };
            results[i] = Some(keys);
        }
        results.into_iter().map(|r| r.expect("every file is looked up or parsed")).collect()
    }

    /// A fingerprint of a conflict for its verdict: its object, the base
    /// game's version, and each definition's mod name, file and that file's
    /// stamp as last read. `None` if a file wasn't read through the cache,
    /// or there's no game version to go by.
    pub fn conflict_print(&self, category: &str, key: &str, game: &str, definitions: &[(&str, &str, &Path)]) -> Option<String> {
        let mut hasher = Sha256::new();
        for part in [category, key, game] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        for (label, file, path) in definitions {
            let f = self.files.get(*path)?;
            hasher.update(format!("{label}\0{file}\0{}\0{}\0", f.size, f.modified).as_bytes());
        }
        Some(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }

    /// The verdict kept for a conflict's fingerprint, if any: `Some(None)`
    /// for one that needs AI.
    pub fn verdict(&mut self, print: &str) -> Option<Option<String>> {
        let today = today();
        let v = self.simple.get_mut(print)?;
        if v.used != today {
            v.used = today;
            self.changed = true;
        }
        Some(v.reason.clone())
    }

    pub fn set_verdict(&mut self, print: String, reason: Option<String>) {
        self.simple.insert(print, Verdict { reason, used: today() });
        self.changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn keys(cache: &mut ScanCache, path: &Path, category: &str, parse: impl Fn(&str) -> Keys + Sync) -> Keys {
        cache.keys_many(&[(path.to_owned(), category.to_owned())], |_, src| parse(src)).remove(0)
    }

    #[test]
    fn rereads_only_changed_files() {
        let root = std::env::temp_dir().join(format!("drukhal-scan-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "x = {}").unwrap();
        let mut cache = ScanCache::default();
        let reads = AtomicUsize::new(0);
        let read = |_: &str| {
            reads.fetch_add(1, Ordering::Relaxed);
            Ok(vec!["x".to_owned()])
        };
        assert_eq!(keys(&mut cache, &file, "common/a", read), Ok(vec!["x".to_owned()]));
        assert_eq!(keys(&mut cache, &file, "common/a", read), Ok(vec!["x".to_owned()]));
        assert_eq!(reads.load(Ordering::Relaxed), 1, "unchanged, so read once");
        keys(&mut cache, &file, "localisation", read).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 2, "read as another folder");

        // Saved and loaded back, it still knows the file.
        let path = root.join("cache.json");
        cache.save(&path).unwrap();
        let mut cache = ScanCache::load(&path);
        keys(&mut cache, &file, "localisation", read).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 2);

        let print = |cache: &ScanCache, label: &str| cache.conflict_print("common/a", "x", "4.0.1", &[(label, "common/a/a.txt", &file)]);
        let before = print(&cache, "A").unwrap();
        assert_ne!(Some(&before), print(&cache, "B").as_ref(), "a mod's name is in the reason");
        cache.set_verdict(before.clone(), Some("Every mod ships the same version".into()));
        assert_eq!(cache.verdict(&before), Some(Some("Every mod ships the same version".into())));

        // A changed file is read again, and its conflicts get a new print.
        std::fs::write(&file, "x = { y = 1 }").unwrap();
        keys(&mut cache, &file, "localisation", read).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 3);
        assert_ne!(print(&cache, "A").unwrap(), before);
        assert_eq!(cache.conflict_print("c", "x", "4.0.1", &[("A", "f", &root.join("never-read.txt"))]), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_many_files_in_order() {
        let root = std::env::temp_dir().join(format!("drukhal-scan-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut files: Vec<(PathBuf, String)> = (0..40)
            .map(|i| {
                let path = root.join(format!("{i}.txt"));
                std::fs::write(&path, format!("k{i}")).unwrap();
                (path, "common/a".to_owned())
            })
            .collect();
        files.insert(7, (root.join("missing.txt"), "common/a".to_owned()));
        let parses = AtomicUsize::new(0);
        let parse = |_: &str, src: &str| {
            parses.fetch_add(1, Ordering::Relaxed);
            Ok(vec![src.to_owned()])
        };
        let mut cache = ScanCache::default();
        let found = cache.keys_many(&files, parse);
        assert_eq!(found.len(), 41);
        assert!(found[7].as_ref().is_err_and(|e| e.line.is_none()), "a read failure");
        let names: Vec<String> = found.iter().filter_map(|k| k.as_ref().ok()).map(|k| k[0].clone()).collect();
        assert_eq!(names, (0..40).map(|i| format!("k{i}")).collect::<Vec<_>>());
        assert_eq!(parses.load(Ordering::Relaxed), 40);
        // Known now, except the file that couldn't be read.
        cache.keys_many(&files, parse);
        assert_eq!(parses.load(Ordering::Relaxed), 40);
        assert_eq!(cache.files.len(), 40);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keeps_file_keys_after_an_app_update() {
        let root = std::env::temp_dir().join(format!("drukhal-scan-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "x = {}").unwrap();
        let mut cache = ScanCache::default();
        keys(&mut cache, &file, "common/a", |_| Ok(vec!["x".to_owned()])).unwrap();
        cache.set_verdict("print".into(), None);
        cache.version = VERSION;
        cache.app = "0.0.0".into();
        let path = root.join("cache.json");
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();

        let mut cache = ScanCache::load(&path);
        assert_eq!(keys(&mut cache, &file, "common/a", |_| panic!("parsed again")), Ok(vec!["x".to_owned()]));
        assert_eq!(cache.verdict("print"), None);
        assert!(cache.changed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn starts_again_after_a_parser_change() {
        let root = std::env::temp_dir().join(format!("drukhal-scan-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "x = {}").unwrap();
        let mut cache = ScanCache::default();
        keys(&mut cache, &file, "common/a", |_| Ok(vec!["x".to_owned()])).unwrap();
        cache.version = VERSION - 1;
        cache.app = APP.to_owned();
        let path = root.join("cache.json");
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();

        let cache = ScanCache::load(&path);
        assert!(cache.files.is_empty() && cache.simple.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
