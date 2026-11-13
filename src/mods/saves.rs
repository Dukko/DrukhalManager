//! Which mods each campaign was played with, so a save isn't continued
//! with a different load order by mistake: disabling a mod doesn't undo
//! what it already did to the galaxy.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A campaign's last session played from DrukhalManager.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Played {
    pub empire: String,
    /// The in-game date of the last save.
    pub date: String,
    /// The active collection's name.
    pub collection: String,
    /// The load order, as descriptor ids.
    pub mods: Vec<String>,
    /// Names by descriptor id, to show mods that are gone since.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    /// When the session ended, in Unix seconds.
    pub at: u64,
}

/// Campaigns by save folder, e.g. `save games/omnian_1`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Saves {
    pub campaigns: BTreeMap<String, Played>,
}

pub fn path() -> PathBuf {
    crate::platform::private_data_dir().join("saves.json")
}

impl Saves {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        crate::platform::rename(tmp, path)
    }

    pub fn record(&mut self, folder: &str, played: Played) {
        self.campaigns.insert(folder.to_owned(), played);
    }

    /// The campaigns, most recently played first.
    pub fn recent(&self) -> Vec<(&String, &Played)> {
        let mut list: Vec<_> = self.campaigns.iter().collect();
        list.sort_by_key(|(_, p)| std::cmp::Reverse(p.at));
        list
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// How a load order differs from the one a save was played with.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Difference {
    /// Enabled now, but not then.
    pub added: Vec<String>,
    /// Enabled then, but not now.
    pub removed: Vec<String>,
    /// The same mods, loading in another order.
    pub reordered: bool,
}

impl Difference {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && !self.reordered
    }
}

pub fn compare(then: &[String], now: &[String]) -> Difference {
    let (a, b): (HashSet<&String>, HashSet<&String>) = (then.iter().collect(), now.iter().collect());
    let added: Vec<String> = now.iter().filter(|m| !a.contains(m)).cloned().collect();
    let removed: Vec<String> = then.iter().filter(|m| !b.contains(m)).cloned().collect();
    let common = |list: &[String], other: &HashSet<&String>| list.iter().filter(|m| other.contains(m)).cloned().collect::<Vec<_>>();
    let reordered = common(then, &b) != common(now, &a);
    Difference { added, removed, reordered }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn compares_load_orders() {
        assert!(compare(&ids(&["a", "b"]), &ids(&["a", "b"])).is_empty());
        assert_eq!(compare(&ids(&["a", "b", "c"]), &ids(&["b", "a", "d"])), Difference { added: ids(&["d"]), removed: ids(&["c"]), reordered: true });
        assert_eq!(compare(&ids(&["a", "b"]), &ids(&["a", "x", "b"])), Difference { added: ids(&["x"]), removed: vec![], reordered: false });
    }

    #[test]
    fn records_and_lists_recent_first() {
        let played = |empire: &str, at: u64| Played { empire: empire.into(), date: "2300.01.01".into(), collection: "Main".into(), mods: ids(&["a"]), names: BTreeMap::new(), at };
        let mut saves = Saves::default();
        saves.record("save games/old_1", played("Old", 1));
        saves.record("save games/new_2", played("New", 2));
        assert_eq!(saves.recent()[0].1.empire, "New");
        let path = std::env::temp_dir().join(format!("drukhal-saves-{}/saves.json", std::process::id()));
        saves.save(&path).unwrap();
        assert_eq!(Saves::load(&path).campaigns.len(), 2);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
