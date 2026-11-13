//! When Steam last updated each enabled Workshop mod, as of the last time
//! the game was launched from here, so the next time can say which mods
//! changed in between: the question that matters in the middle of a
//! campaign.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSession {
    /// When the game was launched, in Unix seconds.
    pub at: u64,
    /// Steam's update time of each enabled Workshop mod then, by Workshop id.
    pub updated: BTreeMap<u64, u64>,
}

pub fn path() -> PathBuf {
    gtk::glib::user_data_dir().join("DrukhalManager/last-session.json")
}

impl LastSession {
    pub fn load(path: &Path) -> Option<Self> {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    /// The session about to start with `enabled` (Workshop ids), given
    /// Steam's update `times`.
    pub fn starting(at: u64, enabled: &[u64], times: &HashMap<u64, u64>) -> Self {
        Self { at, updated: enabled.iter().filter_map(|id| Some((*id, *times.get(id)?))).collect() }
    }

    /// Of `enabled`, the mods Steam updated since this session, with when.
    /// Mods that weren't enabled then weren't played, so they don't count.
    pub fn updated_since(&self, enabled: &[u64], times: &HashMap<u64, u64>) -> Vec<(u64, u64)> {
        enabled.iter().filter_map(|id| Some((*id, *times.get(id)?))).filter(|(id, now)| self.updated.get(id).is_some_and(|then| now > then)).collect()
    }

    /// Treats the mods' current versions as seen, without a launch.
    pub fn mark_seen(&mut self, ids: &[u64], times: &HashMap<u64, u64>) {
        for id in ids {
            if let Some(&t) = times.get(id) {
                self.updated.insert(*id, t);
            }
        }
    }
}

/// Of the `installed` versions (Steam's update time of each local copy),
/// the ones the Workshop has a newer version of (`published`), with when
/// that came out, oldest first.
pub fn waiting(installed: &HashMap<u64, u64>, published: &HashMap<u64, u64>) -> Vec<(u64, u64)> {
    let mut waiting: Vec<(u64, u64)> = published.iter().filter(|(id, now)| installed.get(id).is_some_and(|have| *now > have)).map(|(id, now)| (*id, *now)).collect();
    waiting.sort_by_key(|&(id, at)| (at, id));
    waiting
}

/// Where a Workshop item's change notes are.
pub fn change_notes_url(id: u64) -> String {
    format!("https://steamcommunity.com/sharedfiles/filedetails/changelog/{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_updates_not_downloaded() {
        let installed = HashMap::from([(1, 100), (2, 200), (3, 300)]);
        let published = HashMap::from([(1, 100), (2, 250), (3, 260), (4, 900)]);
        assert_eq!(waiting(&installed, &published), [(2, 250)], "only newer, and only installed mods");
    }

    #[test]
    fn finds_mods_updated_since() {
        let times = HashMap::from([(1, 100), (2, 200), (3, 300)]);
        let mut last = LastSession::starting(1000, &[1, 2, 9], &times);
        assert_eq!(last.updated, BTreeMap::from([(1, 100), (2, 200)]), "mods Steam doesn't know are left out");

        let later = HashMap::from([(1, 150), (2, 200), (3, 900)]);
        assert_eq!(last.updated_since(&[1, 2, 3], &later), [(1, 150)], "3 wasn't enabled last time");
        last.mark_seen(&[1], &later);
        assert!(last.updated_since(&[1, 2, 3], &later).is_empty());

        let path = std::env::temp_dir().join(format!("drukhal-session-{}/last.json", std::process::id()));
        last.save(&path).unwrap();
        assert_eq!(LastSession::load(&path), Some(last));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
