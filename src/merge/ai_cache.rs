//! Bounded on-disk cache for AI merge results, keyed by provider, model,
//! system-prompt hash, deterministic request hash, and source fingerprints.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::patch::Print;

const MAX_ENTRIES: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMerge {
    pub script: String,
    pub notes: String,
}

pub fn dir() -> PathBuf {
    crate::platform::cache_dir().join("ai-merges")
}

pub fn exists() -> bool {
    dir().is_dir()
}

pub fn clear() -> std::io::Result<()> {
    let d = dir();
    if d.is_dir() {
        std::fs::remove_dir_all(d)?;
    }
    Ok(())
}

pub fn sha256_hex(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Computes a stable cache key from provider, model, system prompt hash,
/// deterministic request hash, and every source's [`Print`] fingerprint.
pub fn cache_key(
    provider_id: &str,
    model: &str,
    system: &str,
    canonical_request: &str,
    prints: &[Print],
) -> String {
    let mut h = Sha256::new();
    for part in [
        provider_id,
        model,
        &sha256_hex(system),
        &sha256_hex(canonical_request),
    ] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.update((prints.len() as u64).to_le_bytes());
    for p in prints {
        for field in [&p.mod_id, &p.file, &p.hash] {
            h.update((field.len() as u64).to_le_bytes());
            h.update(field.as_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn get_in(dir: &Path, key: &str) -> Option<CachedMerge> {
    let path = dir.join(format!("{key}.json"));
    let data = std::fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

pub fn put_in(dir: &Path, key: &str, value: &CachedMerge) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join(format!("{key}.json"));
    let tmp = dir.join(format!("{key}.tmp"));
    if let Ok(bytes) = serde_json::to_vec(value)
        && std::fs::write(&tmp, bytes).is_ok()
    {
        let _ = crate::platform::rename(&tmp, &path);
    }
    prune(dir);
}

fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect();
    if files.len() <= MAX_ENTRIES {
        return;
    }
    files.sort_by_key(|(_, t)| *t);
    let excess = files.len() - MAX_ENTRIES;
    for (old, _) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_invalidates_on_fingerprint_change() {
        let dir = std::env::temp_dir().join(format!("drukhal-ai-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p1 = vec![Print::new("mod/a.mod", "A", "f.txt", "a = 1")];
        let p2 = vec![Print::new("mod/a.mod", "A", "f.txt", "a = 2")];
        let k1 = cache_key("anthropic", "claude-opus-5-5", "sys", "req", &p1);
        let k2 = cache_key("anthropic", "claude-opus-5-5", "sys", "req", &p2);
        assert_ne!(k1, k2);
        put_in(
            &dir,
            &k1,
            &CachedMerge {
                script: "x = { a = 1 }".into(),
                notes: "ok".into(),
            },
        );
        assert_eq!(get_in(&dir, &k1).unwrap().script, "x = { a = 1 }");
        assert!(get_in(&dir, &k2).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
