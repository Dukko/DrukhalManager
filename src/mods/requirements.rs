//! Workshop "Required items": what a mod's Workshop page says it needs,
//! which many mods list only there and not as descriptor `dependencies`.
//! Fetched from Steam now and then and kept in a cache, so checks work
//! offline and startup doesn't wait for the network.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Mod, Required};

/// How long a fetched list is trusted before it's fetched again.
const FRESH: Duration = Duration::from_secs(3 * 24 * 3600);

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    /// By Workshop id: when it was fetched (Unix seconds) and what it requires.
    items: HashMap<u64, (u64, Vec<Required>)>,
}

impl Cache {
    pub fn path() -> PathBuf {
        gtk::glib::user_cache_dir().join("DrukhalManager/workshop-requirements.json")
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    /// The Workshop mods among `mods` whose requirements are unknown or old.
    pub fn stale(&self, mods: &[Mod]) -> Vec<u64> {
        let now = now();
        let mut ids: Vec<u64> = mods
            .iter()
            .filter_map(|m| m.descriptor.remote_file_id)
            .filter(|id| self.items.get(id).is_none_or(|(at, _)| now.saturating_sub(*at) > FRESH.as_secs()))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Records what Steam said, fetched just now.
    pub fn update(&mut self, fetched: HashMap<u64, Vec<(u64, String)>>) {
        let now = now();
        for (id, required) in fetched {
            let required = required.into_iter().filter(|(r, _)| *r != id).map(|(steam_id, name)| Required { steam_id, name }).collect();
            self.items.insert(id, (now, required));
        }
    }

    /// Fills in each Workshop mod's `requires` from the cache.
    pub fn apply(&self, mods: &mut [Mod]) {
        for m in mods {
            m.requires = m.descriptor.remote_file_id.and_then(|id| self.items.get(&id)).map(|(_, r)| r.clone()).unwrap_or_default();
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::{Descriptor, Source};

    #[test]
    fn caches_and_applies() {
        let m = |id: u64| Mod { id: format!("mod/ugc_{id}.mod"), descriptor: Descriptor { remote_file_id: Some(id), ..Default::default() }, content: None, source: Source::Workshop, load_position: None, error: None, steam: None, requires: Vec::new() };
        let mut mods = vec![m(1), m(2)];
        let mut cache = Cache::default();
        assert_eq!(cache.stale(&mods), [1, 2]);
        cache.update([(1, vec![(2, "Two".to_owned()), (1, "Itself".to_owned())]), (2, vec![])].into());
        assert!(cache.stale(&mods).is_empty());
        cache.apply(&mut mods);
        assert_eq!(mods[0].requires, [Required { steam_id: 2, name: "Two".into() }], "never requires itself");
        assert!(mods[1].requires.is_empty());

        let path = std::env::temp_dir().join(format!("drukhal-requirements-{}/cache.json", std::process::id()));
        cache.save(&path).unwrap();
        assert_eq!(Cache::load(&path).items.len(), 2);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
