//! The load order state: which mods are enabled and in what order.

use super::display_name;
use crate::merge::patch;
use crate::mods::{checks, Mod};

/// How many load order steps Ctrl+Z can go back.
const MAX_HISTORY: usize = 100;

/// A load order snapshot: enabled mod ids (not indices, so it still
/// applies after mods are added or removed) plus whether it was dirty.
type Snapshot = (Vec<String>, bool);

pub(crate) struct State {
    pub(crate) mods: Vec<Mod>,
    /// Indices into `mods`, in load order.
    pub(crate) enabled: Vec<usize>,
    pub(crate) dirty: bool,
    /// Bumped on every load order change, so conflict reports can tell
    /// whether they still describe it.
    pub(crate) revision: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl State {
    pub(crate) fn new(mods: Vec<Mod>) -> Self {
        let mut enabled: Vec<usize> = (0..mods.len())
            .filter(|&i| mods[i].load_position.is_some())
            .collect();
        enabled.sort_by_key(|&i| mods[i].load_position);
        let mut state = Self {
            mods,
            enabled,
            dirty: false,
            revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        // A saved order with the patch elsewhere gets fixed on the next save.
        state.dirty = state.pin_patch();
        state
    }

    fn snapshot(&self) -> Snapshot {
        (self.enabled_ids(), self.dirty)
    }

    /// Records the current load order for undo and drops the redo stack.
    /// Called before every change.
    fn checkpoint(&mut self) {
        if self.undo.len() >= MAX_HISTORY {
            self.undo.remove(0);
        }
        self.undo.push(self.snapshot());
        self.redo.clear();
    }

    /// Restores `snapshot`, skipping ids that are no longer installed.
    fn apply(&mut self, (ids, dirty): Snapshot) {
        let index: std::collections::HashMap<&str, usize> = self
            .mods
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.as_str(), i))
            .collect();
        self.enabled = ids
            .iter()
            .filter_map(|id| index.get(id.as_str()).copied())
            .collect();
        self.dirty = dirty;
        self.pin_patch();
        self.revision += 1;
    }

    /// Undoes the last load order change. Returns whether there was one.
    pub(crate) fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.apply(previous);
        true
    }

    /// Redoes an undone change. Returns whether there was one.
    pub(crate) fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.apply(next);
        true
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Keeps the merged patch at the very end of the load order: its merged
    /// files and fixes only win when it loads after every mod. Every change
    /// to the order goes through here. Returns whether it had to move.
    pub(crate) fn pin_patch(&mut self) -> bool {
        let Some(at) = self
            .enabled
            .iter()
            .position(|&i| self.mods[i].id == patch::ID)
        else {
            return false;
        };
        if at + 1 == self.enabled.len() {
            return false;
        }
        let patch = self.enabled.remove(at);
        self.enabled.push(patch);
        true
    }

    pub(crate) fn disabled(&self) -> Vec<usize> {
        let mut disabled: Vec<usize> = (0..self.mods.len())
            .filter(|i| !self.enabled.contains(i))
            .collect();
        disabled.sort_by_cached_key(|&i| self.mods[i].descriptor.name.to_lowercase());
        disabled
    }

    /// Enabling appends to the end of the load order.
    pub(crate) fn set_enabled(&mut self, index: usize, on: bool) {
        match (on, self.enabled.iter().position(|&i| i == index)) {
            (true, None) => {
                self.checkpoint();
                self.enabled.push(index);
            }
            (false, Some(p)) => {
                self.checkpoint();
                self.enabled.remove(p);
            }
            _ => return,
        }
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
    }

    /// Moves the mod at load position `from` in front of the mod currently at
    /// `before` (`before == len` moves it to the end).
    /// Moves the mod at load position `from` so it ends up at position `to`
    /// (both 0-based; `to` is clamped to the list).
    pub(crate) fn move_to(&mut self, from: usize, to: usize) {
        let to = to.min(self.enabled.len().saturating_sub(1));
        self.move_enabled(from, if to > from { to + 1 } else { to });
    }

    pub(crate) fn move_enabled(&mut self, from: usize, mut before: usize) {
        if from >= self.enabled.len() {
            return;
        }
        if before > from {
            before -= 1;
        }
        if before == from {
            return;
        }
        self.checkpoint();
        let item = self.enabled.remove(from);
        self.enabled.insert(before.min(self.enabled.len()), item);
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
    }

    /// Enables every mod (newly enabled ones go to the end, alphabetically)
    /// or disables them all. Returns whether anything changed.
    pub(crate) fn set_all(&mut self, on: bool) -> bool {
        if on {
            let disabled = self.disabled();
            if disabled.is_empty() {
                return false;
            }
            self.checkpoint();
            self.enabled.extend(disabled);
        } else {
            if self.enabled.is_empty() {
                return false;
            }
            self.checkpoint();
            self.enabled.clear();
        }
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
        true
    }

    /// Moves mods after the mods they depend on. Returns whether the
    /// order changed.
    pub(crate) fn sort_by_dependencies(&mut self) -> bool {
        let sorted = checks::sort_by_dependencies(&self.mods, &self.enabled);
        if sorted == self.enabled {
            return false;
        }
        self.checkpoint();
        self.enabled = sorted;
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
        true
    }

    /// Drops mods by id, keeping the load order of the rest. Removing an
    /// enabled mod counts as a change.
    pub(crate) fn remove_mods(&mut self, ids: &[String]) {
        if self.mods.iter().any(|m| ids.contains(&m.id)) {
            self.checkpoint();
        }
        let enabled_ids = self.enabled_ids();
        let before = enabled_ids.len();
        self.mods.retain(|m| !ids.contains(&m.id));
        let index: std::collections::HashMap<&str, usize> = self
            .mods
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.as_str(), i))
            .collect();
        self.enabled = enabled_ids
            .iter()
            .filter_map(|id| index.get(id.as_str()).copied())
            .collect();
        self.pin_patch();
        if self.enabled.len() != before {
            self.dirty = true;
        }
        self.revision += 1;
    }

    /// Replaces the load order with `ids`; returns those not installed.
    pub(crate) fn set_enabled_ids(&mut self, ids: &[String]) -> Vec<String> {
        self.checkpoint();
        let index: std::collections::HashMap<&str, usize> = self
            .mods
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.as_str(), i))
            .collect();
        let mut missing = Vec::new();
        let mut enabled = Vec::new();
        for id in ids {
            match index.get(id.as_str()) {
                Some(&i) if !enabled.contains(&i) => enabled.push(i),
                Some(_) => {}
                None => missing.push(id.clone()),
            }
        }
        self.enabled = enabled;
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
        missing
    }

    /// Swaps in a freshly discovered mod list, keeping the current load
    /// order and unsaved changes. New mods start disabled. Returns the
    /// names of mods that appeared, and of those that went away (or whose
    /// files did).
    pub(crate) fn reload(&mut self, mods: Vec<Mod>) -> (Vec<String>, Vec<String>) {
        let old: std::collections::HashMap<&str, &Mod> =
            self.mods.iter().map(|m| (m.id.as_str(), m)).collect();
        let added = mods
            .iter()
            .filter(|m| !old.contains_key(m.id.as_str()))
            .map(|m| display_name(m).to_owned())
            .collect();
        let new: std::collections::HashMap<&str, &Mod> =
            mods.iter().map(|m| (m.id.as_str(), m)).collect();
        let removed = self
            .mods
            .iter()
            .filter(|m| match new.get(m.id.as_str()) {
                None => true,
                Some(n) => m.content.is_some() && n.content.is_none(),
            })
            .map(|m| display_name(m).to_owned())
            .collect();
        let (ids, dirty) = self.snapshot();
        let before = ids.len();
        self.mods = mods;
        // Not a change the user made, so it isn't recorded for undo; the
        // snapshots are ids, so history still applies.
        self.apply((ids, dirty));
        if self.enabled.len() != before {
            self.dirty = true;
        }
        (added, removed)
    }

    pub(crate) fn enabled_ids(&self) -> Vec<String> {
        self.enabled
            .iter()
            .map(|&i| self.mods[i].id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods;

    fn state(enabled: &[usize]) -> State {
        let mods = (0..4)
            .map(|i| Mod {
                id: format!("mod/{i}.mod"),
                descriptor: mods::Descriptor {
                    name: format!("{i}"),
                    ..Default::default()
                },
                content: None,
                source: mods::Source::Local,
                load_position: enabled.iter().position(|&e| e == i),
                error: None,
                steam: None,
            })
            .collect();
        State::new(mods)
    }

    #[test]
    fn moves_within_load_order() {
        let mut s = state(&[0, 1, 2]);
        s.move_enabled(0, 2);
        assert_eq!(s.enabled, [1, 0, 2]);
        s.move_enabled(2, 0);
        assert_eq!(s.enabled, [2, 1, 0]);
        s.move_enabled(0, 3);
        assert_eq!(s.enabled, [1, 0, 2]);
        assert!(s.dirty);

        let mut s = state(&[0, 1]);
        s.move_enabled(0, 1);
        assert_eq!(s.enabled, [0, 1]);
        assert!(!s.dirty, "dropping a mod onto its own slot is a no-op");
    }

    #[test]
    fn enables_and_disables_all_with_undo() {
        let mut s = state(&[2, 0]);
        assert!(s.set_all(true));
        assert_eq!(
            s.enabled,
            [2, 0, 1, 3],
            "new mods go to the end, keeping the existing order"
        );
        assert!(!s.set_all(true));
        assert!(s.undo());
        assert_eq!(s.enabled, [2, 0]);
        assert!(!s.dirty, "undo restores the saved state");
        assert!(s.redo());
        assert_eq!(s.enabled, [2, 0, 1, 3]);
        assert!(s.undo());
        assert!(s.set_all(false));
        assert!(s.enabled.is_empty() && s.dirty);
        assert!(!s.set_all(false));
        assert!(!s.redo(), "a new change drops the redo stack");
    }

    #[test]
    fn removing_mods_keeps_the_order() {
        let mut s = state(&[3, 1, 0]);
        s.remove_mods(&["mod/1.mod".into()]);
        assert_eq!(s.enabled_ids(), ["mod/3.mod", "mod/0.mod"]);
        assert!(s.dirty);
        let mut s = state(&[0]);
        s.remove_mods(&["mod/2.mod".into()]);
        assert_eq!(s.enabled_ids(), ["mod/0.mod"]);
        assert!(
            !s.dirty,
            "removing a disabled mod doesn't change the load order"
        );
    }

    #[test]
    fn patch_always_loads_last() {
        let mut s = state(&[0, 1]);
        s.mods.push(Mod {
            id: patch::ID.into(),
            descriptor: Default::default(),
            content: None,
            source: mods::Source::Local,
            load_position: None,
            error: None,
            steam: None,
        });
        s.set_enabled(4, true);
        s.set_enabled(2, true);
        assert_eq!(
            s.enabled,
            [0, 1, 2, 4],
            "enabling another mod still puts it before the patch"
        );
        s.move_to(3, 0);
        assert_eq!(s.enabled, [0, 1, 2, 4], "the patch can't be moved up");
        s.move_to(0, 99);
        assert_eq!(
            s.enabled,
            [1, 2, 0, 4],
            "moving to the bottom stops above the patch"
        );
        s.set_enabled_ids(&[patch::ID.into(), "mod/3.mod".into()]);
        assert_eq!(
            s.enabled,
            [3, 4],
            "collections and imports can't put it first"
        );
        assert!(s.undo(), "collection switches are undoable");
        assert_eq!(s.enabled, [1, 2, 0, 4]);
        assert!(s.redo());
        assert_eq!(s.enabled, [3, 4]);
        assert!(s.undo());
        assert!(s.set_all(true));
        assert_eq!(*s.enabled.last().unwrap(), 4);

        let mods = vec![
            Mod {
                id: patch::ID.into(),
                descriptor: Default::default(),
                content: None,
                source: mods::Source::Local,
                load_position: Some(0),
                error: None,
                steam: None,
            },
            Mod {
                id: "mod/a.mod".into(),
                descriptor: Default::default(),
                content: None,
                source: mods::Source::Local,
                load_position: Some(1),
                error: None,
                steam: None,
            },
        ];
        let s = State::new(mods);
        assert_eq!(
            s.enabled,
            [1, 0],
            "a saved order with the patch first is fixed on load"
        );
        assert!(s.dirty);
    }

    #[test]
    fn moves_to_a_position() {
        let mut s = state(&[0, 1, 2, 3]);
        s.move_to(0, 2);
        assert_eq!(
            s.enabled,
            [1, 2, 0, 3],
            "the mod ends up at the typed position"
        );
        s.move_to(3, 0);
        assert_eq!(s.enabled, [3, 1, 2, 0]);
        s.move_to(1, 99);
        assert_eq!(s.enabled, [3, 2, 0, 1], "past the end moves to the bottom");
        let before = s.revision;
        s.move_to(2, 2);
        assert_eq!(s.revision, before, "same position is a no-op");
    }

    #[test]
    fn toggling_appends_and_removes() {
        let mut s = state(&[2, 0]);
        assert_eq!(s.disabled(), [1, 3]);
        s.set_enabled(3, true);
        s.set_enabled(2, false);
        assert_eq!(s.enabled, [0, 3]);
        assert_eq!(s.enabled_ids(), ["mod/0.mod", "mod/3.mod"]);
    }

    #[test]
    fn undo_redo_walks_back_through_changes() {
        let mut s = state(&[0, 1]);
        assert!(!s.can_undo() && !s.can_redo());
        assert!(!s.undo(), "nothing to undo at first");
        assert!(!s.redo(), "nothing to redo at first");
        s.set_enabled(2, true);
        s.move_to(0, 2);
        assert_eq!(s.enabled, [1, 2, 0]);
        assert!(s.can_undo() && !s.can_redo());
        assert!(s.undo());
        assert_eq!(s.enabled, [0, 1, 2]);
        assert!(s.can_undo() && s.can_redo());
        assert!(s.undo());
        assert_eq!(s.enabled, [0, 1]);
        assert!(!s.dirty, "undo restores the dirty flag too");
        assert!(!s.can_undo() && s.can_redo());
        assert!(!s.undo(), "history runs out");
        assert!(s.redo());
        assert_eq!(s.enabled, [0, 1, 2]);
        assert!(s.redo());
        assert_eq!(s.enabled, [1, 2, 0]);
        assert!(!s.redo(), "redo runs out");
    }

    #[test]
    fn no_op_changes_leave_no_history() {
        let mut s = state(&[0, 1]);
        s.set_enabled(0, true);
        assert!(!s.can_undo(), "enabling an enabled mod is a no-op");
        s.move_enabled(0, 1);
        assert!(!s.can_undo(), "dropping onto its own slot is a no-op");
        s.set_enabled(5, false);
        assert!(!s.can_undo(), "disabling a missing index is a no-op");
    }

    #[test]
    fn undo_survives_mods_being_removed() {
        let mut s = state(&[0, 1, 2]);
        s.move_to(0, 2);
        assert_eq!(s.enabled, [1, 2, 0]);
        s.remove_mods(&["mod/1.mod".into()]);
        assert_eq!(s.enabled_ids(), ["mod/2.mod", "mod/0.mod"]);
        assert!(s.undo());
        // mod/1 is gone, so only the surviving order comes back.
        assert_eq!(s.enabled_ids(), ["mod/2.mod", "mod/0.mod"]);
        // The move itself is still on the stack: undo again for [0, 1, 2]
        // minus the removed mod.
        assert!(s.undo());
        assert_eq!(s.enabled_ids(), ["mod/0.mod", "mod/2.mod"]);
    }

    #[test]
    fn history_is_capped() {
        let mut s = state(&[0]);
        for _ in 0..(MAX_HISTORY + 10) {
            let on = !s.enabled.contains(&1);
            s.set_enabled(1, on);
        }
        let mut steps = 0;
        while s.undo() {
            steps += 1;
        }
        assert_eq!(steps, MAX_HISTORY);
    }

    #[test]
    fn reloading_keeps_the_order_and_disables_new_mods() {
        let mut s = state(&[2, 0]);
        for m in &mut s.mods {
            m.content = Some(std::path::PathBuf::from(&m.id));
        }
        s.move_to(0, 1);
        assert!(s.dirty);
        // On disk: mod 1 is gone, mod 3 lost its files, mod 4 is new and
        // already in the saved load order.
        let mut mods: Vec<Mod> = s.mods.iter().filter(|m| m.id != "mod/1.mod").cloned().collect();
        mods.iter_mut().find(|m| m.id == "mod/3.mod").unwrap().content = None;
        let mut new = mods[0].clone();
        new.id = "mod/4.mod".into();
        new.descriptor.name = "4".into();
        new.load_position = Some(0);
        mods.insert(0, new);

        let (added, removed) = s.reload(mods);
        assert_eq!(added, ["4"]);
        assert_eq!(removed, ["1", "3"]);
        assert_eq!(s.enabled_ids(), ["mod/0.mod", "mod/2.mod"], "unsaved order kept");
        assert!(s.dirty, "still unsaved");
        assert!(!s.enabled_ids().contains(&"mod/4.mod".to_owned()), "new mods start disabled");
        assert!(s.undo(), "history survives a reload");
        assert_eq!(s.enabled_ids(), ["mod/2.mod", "mod/0.mod"]);

        // Dropping an enabled mod changes the load order.
        let mut s = state(&[0, 1]);
        let mods = s.mods.iter().filter(|m| m.id != "mod/1.mod").cloned().collect();
        s.reload(mods);
        assert_eq!(s.enabled_ids(), ["mod/0.mod"]);
        assert!(s.dirty);
    }
}
