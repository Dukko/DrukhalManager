//! Snapshots of the merged patch, taken before changes that touch many
//! merges at once, so a batch that went wrong can be undone in one step.
//!
//! Each snapshot is a folder in the backups folder holding a copy of the
//! patch (its files and merge records) and why it was taken.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::patch::{self, Target};

/// What a snapshot's folder name starts with: the shared patch's kept
/// older versions' name, each collection's patch its folder's.
fn prefix(target: &Target) -> String {
    if target.folder == patch::LEGACY_FOLDER { "merged-patch-".into() } else { format!("{}-", target.folder) }
}
const INFO: &str = "snapshot.json";
const COPY: &str = "patch";
/// How many snapshots are kept; older ones are deleted.
pub const KEPT: usize = 10;
/// Removals this soon after a removal snapshot share it, so removing merges
/// one by one doesn't push the other snapshots out.
const REMOVALS_SHARE: Duration = Duration::from_secs(5 * 60);

pub const BEFORE_REMOVING: &str = "Before removing merges";
pub const BEFORE_RESTORING: &str = "Before restoring a backup";
pub const BEFORE_IMPORTING: &str = "Before importing a shared patch";
pub const BEFORE_DELETING: &str = "Before deleting its collection";
pub const BEFORE_UPDATING_PICKS: &str = "Before updating picked versions";

/// Why a snapshot was taken before Merge All of `category`.
pub fn before_merge_all(category: &str) -> String {
    format!("Before Merge All of {category}")
}

#[derive(Serialize, Deserialize)]
struct Info {
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub dir: PathBuf,
    pub taken: SystemTime,
    pub reason: String,
    /// How many merged objects and merged or fixed files it holds.
    pub merges: usize,
    pub files: usize,
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

/// Copies the patch into a new snapshot in `backup_dir` and deletes the
/// oldest beyond [`KEPT`]. Returns `None` if there is no patch yet, or if
/// `reason` is [`BEFORE_REMOVING`] and the newest snapshot was taken for
/// removals moments ago.
pub fn take(user_dir: &Path, target: &Target, backup_dir: &Path, reason: &str) -> io::Result<Option<PathBuf>> {
    let result = take_unlogged(user_dir, target, backup_dir, reason);
    match &result {
        Ok(Some(_)) => crate::diagnostics::log!("patch", "Backed up {} ({reason})", target.name),
        Ok(None) => {}
        Err(e) => crate::diagnostics::log!("patch", "Failed: back up {} ({reason}): {e}", target.name),
    }
    result
}

fn take_unlogged(user_dir: &Path, target: &Target, backup_dir: &Path, reason: &str) -> io::Result<Option<PathBuf>> {
    let root = target.root(user_dir);
    if !root.is_dir() {
        return Ok(None);
    }
    if reason == BEFORE_REMOVING
        && let Some(newest) = list(backup_dir, target).into_iter().next()
        && newest.reason == BEFORE_REMOVING
        && newest.taken.elapsed().is_ok_and(|age| age < REMOVALS_SHARE)
    {
        return Ok(None);
    }
    // Always after the newest, even when taken in the same millisecond.
    let newest = list(backup_dir, target).first().map_or(0, |s| stamp_of(&s.dir, target).unwrap_or(0) + 1);
    let dir = backup_dir.join(format!("{}{}", prefix(target), now_ms().max(newest)));
    let staging = dir.with_extension("drukhal-tmp");
    let _ = std::fs::remove_dir_all(&staging);
    copy_dir(&root, &staging.join(COPY))?;
    let info = serde_json::to_vec_pretty(&Info { reason: reason.to_owned() }).map_err(io::Error::other)?;
    std::fs::write(staging.join(INFO), info)?;
    // Renamed into place whole, so a half-written snapshot is never listed.
    crate::platform::rename(&staging, &dir)?;
    for old in list(backup_dir, target).iter().skip(KEPT) {
        std::fs::remove_dir_all(&old.dir)?;
    }
    Ok(Some(dir))
}

/// The snapshots of `target` in `backup_dir`, newest first.
pub fn list(backup_dir: &Path, target: &Target) -> Vec<Snapshot> {
    let Ok(entries) = std::fs::read_dir(backup_dir) else { return Vec::new() };
    let mut found: Vec<(u128, Snapshot)> = entries
        .filter_map(|e| {
            let dir = e.ok()?.path();
            let stamp = stamp_of(&dir, target)?;
            let info: Info = serde_json::from_slice(&std::fs::read(dir.join(INFO)).ok()?).ok()?;
            let (merges, files) = patch::summary(&dir.join(COPY)).unwrap_or_default();
            let taken = UNIX_EPOCH + Duration::from_millis(u64::try_from(stamp).ok()?);
            Some((stamp, Snapshot { dir, taken, reason: info.reason, merges, files }))
        })
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    found.into_iter().map(|(_, s)| s).collect()
}

/// Puts `snapshot` back as the patch, first taking a snapshot of the patch
/// as it is, so restoring can be undone too.
pub fn restore(user_dir: &Path, target: &Target, backup_dir: &Path, snapshot: &Snapshot) -> io::Result<()> {
    let source = snapshot.dir.join(COPY);
    if !source.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "The backup is gone"));
    }
    take(user_dir, target, backup_dir, BEFORE_RESTORING)?;
    let result = replace_patch(user_dir, target, &source);
    crate::diagnostics::outcome("patch", || format!("Restored {} from its backup ({})", target.name, snapshot.reason), result)
}

/// Makes a copy of the patch folder at `source` the patch, swapping it in
/// whole so a failure leaves the current one as it was. Take a snapshot
/// first.
pub fn replace_patch(user_dir: &Path, target: &Target, source: &Path) -> io::Result<()> {
    let root = target.root(user_dir);
    let staging = root.with_extension("drukhal-restore");
    let old = root.with_extension("drukhal-old");
    let _ = std::fs::remove_dir_all(&staging);
    copy_dir(source, &staging)?;
    let _ = std::fs::remove_dir_all(&old);
    let had_patch = root.is_dir();
    if had_patch {
        crate::platform::rename(&root, &old)?;
    }
    if let Err(e) = crate::platform::rename(&staging, &root) {
        if had_patch {
            let _ = crate::platform::rename(&old, &root);
        }
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&old);
    patch::link(user_dir, target)
}

fn stamp_of(dir: &Path, target: &Target) -> Option<u128> {
    dir.file_name()?.to_str()?.strip_prefix(&prefix(target))?.parse().ok()
}

pub(crate) fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::patch::Patch;

    #[test]
    fn takes_lists_and_restores() {
        let root = std::env::temp_dir().join(format!("drukhal-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, backups) = (root.join("user"), root.join("backups"));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        assert_eq!(take(&user, &patch::test_target(), &backups, "x").unwrap(), None, "no patch yet");

        let mut patch = Patch::open(&user, &patch::test_target()).unwrap();
        patch.save(&user, Some("v4.5.3"), "common/buildings", "lab", "lab = { cost = 1 }", vec![]).unwrap();
        patch.save_file(&user, None, "interface/x.gui", "a", vec![]).unwrap();
        take(&user, &patch::test_target(), &backups, &before_merge_all("common/buildings")).unwrap().unwrap();

        // The batch goes wrong: lab is changed and farm added.
        patch.save(&user, Some("v4.5.3"), "common/buildings", "lab", "lab = { broken", vec![]).unwrap();
        patch.save(&user, Some("v4.5.3"), "common/buildings", "farm", "farm = {}", vec![]).unwrap();
        std::fs::remove_file(user.join(patch::test_target().id())).unwrap();

        let snapshots = list(&backups, &patch::test_target());
        assert_eq!(snapshots.len(), 1);
        assert_eq!((snapshots[0].reason.as_str(), snapshots[0].merges, snapshots[0].files), ("Before Merge All of common/buildings", 1, 1));
        restore(&user, &patch::test_target(), &backups, &snapshots[0]).unwrap();

        let patch = Patch::open(&user, &patch::test_target()).unwrap();
        assert_eq!(patch.get("common/buildings", "lab").unwrap().script, "lab = { cost = 1 }");
        assert!(patch.get("common/buildings", "farm").is_none());
        let farm_file = patch::test_target().root(&user).join("common/buildings/伞伞伞_drukhal_farm.txt");
        assert!(!farm_file.exists(), "files the snapshot didn't have are gone");
        assert!(std::fs::read_to_string(user.join(patch::test_target().id())).unwrap().contains("path=\""), "the launcher's descriptor is back");

        // Restoring is itself undoable.
        let snapshots = list(&backups, &patch::test_target());
        assert_eq!(snapshots.iter().map(|s| (s.reason.as_str(), s.merges)).collect::<Vec<_>>(), [(BEFORE_RESTORING, 2), ("Before Merge All of common/buildings", 1)]);
        restore(&user, &patch::test_target(), &backups, &snapshots[0]).unwrap();
        assert!(Patch::open(&user, &patch::test_target()).unwrap().get("common/buildings", "farm").is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shares_removal_snapshots_and_keeps_the_newest() {
        let root = std::env::temp_dir().join(format!("drukhal-snapshot-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, backups) = (root.join("user"), root.join("backups"));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        Patch::open(&user, &patch::test_target()).unwrap().save(&user, None, "events", "ns.1", "x = {}", vec![]).unwrap();

        assert!(take(&user, &patch::test_target(), &backups, BEFORE_REMOVING).unwrap().is_some());
        assert!(take(&user, &patch::test_target(), &backups, BEFORE_REMOVING).unwrap().is_none(), "removing one more right after");
        assert!(take(&user, &patch::test_target(), &backups, &before_merge_all("events")).unwrap().is_some());
        assert!(take(&user, &patch::test_target(), &backups, BEFORE_REMOVING).unwrap().is_some(), "a Merge All came in between");

        for i in 0..KEPT + 3 {
            take(&user, &patch::test_target(), &backups, &format!("run {i}")).unwrap().unwrap();
        }
        let snapshots = list(&backups, &patch::test_target());
        assert_eq!(snapshots.len(), KEPT);
        assert_eq!(snapshots[0].reason, format!("run {}", KEPT + 2));
        assert_eq!(snapshots[KEPT - 1].reason, "run 3");
        std::fs::remove_dir_all(root).unwrap();
    }
}
