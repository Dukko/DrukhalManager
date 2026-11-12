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

/// Caches of another version, or written by another version of the app
/// (whose parsing may differ), are started again.
const VERSION: u32 = 1;
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

/// The objects a file defines, as of its size and modification time.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileKeys {
    size: u64,
    modified: u128,
    /// The folder it was read as, which decides how it's parsed.
    category: String,
    /// Its object keys, or why it couldn't be parsed.
    keys: Result<Vec<String>, String>,
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
        gtk::glib::user_cache_dir().join("DrukhalManager/conflict-scan.json")
    }

    /// The cache at `path`; empty if there is none or it can't be read.
    pub fn load(path: &Path) -> Self {
        let cache: Self = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if cache.version == VERSION && cache.app == APP { cache } else { Self { version: VERSION, app: APP.to_owned(), changed: true, ..Self::default() } }
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
        std::fs::rename(tmp, path)?;
        self.changed = false;
        Ok(())
    }

    /// The object keys of the file at `path`, read as `category`: from the
    /// cache while the file is unchanged, else read and given to `parse`.
    /// A file that can't be read isn't kept, so it's tried again next time.
    pub fn keys(&mut self, path: &Path, category: &str, parse: impl FnOnce(&str) -> Result<Vec<String>, String>) -> Result<Vec<String>, String> {
        let read = || std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| e.to_string());
        let Some((size, modified)) = stamp(path) else { return parse(&read()?) };
        if let Some(f) = self.files.get(path)
            && (f.size, f.modified) == (size, modified)
            && f.category == category
        {
            return f.keys.clone();
        }
        let keys = parse(&read()?);
        self.files.insert(path.to_owned(), FileKeys { size, modified, category: category.to_owned(), keys: keys.clone() });
        self.changed = true;
        keys
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

    #[test]
    fn rereads_only_changed_files() {
        let root = std::env::temp_dir().join(format!("drukhal-scan-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "x = {}").unwrap();
        let mut cache = ScanCache::default();
        let reads = std::cell::Cell::new(0);
        let read = |_: &str| {
            reads.set(reads.get() + 1);
            Ok(vec!["x".to_owned()])
        };
        assert_eq!(cache.keys(&file, "common/a", read), Ok(vec!["x".to_owned()]));
        assert_eq!(cache.keys(&file, "common/a", read), Ok(vec!["x".to_owned()]));
        assert_eq!(reads.get(), 1, "unchanged, so read once");
        cache.keys(&file, "localisation", read).unwrap();
        assert_eq!(reads.get(), 2, "read as another folder");

        // Saved and loaded back, it still knows the file.
        let path = root.join("cache.json");
        cache.save(&path).unwrap();
        let mut cache = ScanCache::load(&path);
        cache.keys(&file, "localisation", read).unwrap();
        assert_eq!(reads.get(), 2);

        let print = |cache: &ScanCache, label: &str| cache.conflict_print("common/a", "x", "4.0.1", &[(label, "common/a/a.txt", &file)]);
        let before = print(&cache, "A").unwrap();
        assert_ne!(Some(&before), print(&cache, "B").as_ref(), "a mod's name is in the reason");
        cache.set_verdict(before.clone(), Some("Every mod ships the same version".into()));
        assert_eq!(cache.verdict(&before), Some(Some("Every mod ships the same version".into())));

        // A changed file is read again, and its conflicts get a new print.
        std::fs::write(&file, "x = { y = 1 }").unwrap();
        cache.keys(&file, "localisation", read).unwrap();
        assert_eq!(reads.get(), 3);
        assert_ne!(print(&cache, "A").unwrap(), before);
        assert_eq!(cache.conflict_print("c", "x", "4.0.1", &[("A", "f", &root.join("never-read.txt"))]), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
