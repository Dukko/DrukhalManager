//! Update All after a game update: which of the changed objects a batch
//! takes, and how its results are ordered for review. No widgets here.

use std::collections::BTreeSet;

use super::base_game::Behind;

/// The objects a batch updates, as indexes into `behind`, in the order the
/// list shows them (by mod, then as listed). Objects the update removed
/// have nothing to update with, and ones the patch already holds are done.
pub fn included(behind: &[Behind], is_updated: impl Fn(&str, &str) -> bool) -> Vec<usize> {
    let mut chosen: Vec<usize> = (0..behind.len()).filter(|&i| behind[i].now.is_some() && !is_updated(&behind[i].category, &behind[i].key)).collect();
    // Stable, so objects of one mod keep their order.
    chosen.sort_by_key(|&i| behind[i].mod_position);
    chosen
}

/// What a batch ended with, each in the order the objects were queued.
#[derive(Debug, PartialEq)]
pub struct Results<T> {
    /// Results to review one at a time.
    pub ready: Vec<(usize, T)>,
    /// Objects whose request or result failed, with why.
    pub failed: Vec<(usize, String)>,
    /// Objects never started because the run was stopped.
    pub not_started: Vec<usize>,
}

/// Sorts the finished `results`, keyed by queue position, out of `total`
/// queued objects. Results arrive in the order the requests finished.
pub fn sort<T>(total: usize, results: Vec<(usize, Result<T, String>)>) -> Results<T> {
    let mut ready = Vec::new();
    let mut failed = Vec::new();
    let mut done = BTreeSet::new();
    for (position, result) in results {
        done.insert(position);
        match result {
            Ok(value) => ready.push((position, value)),
            Err(message) => failed.push((position, message)),
        }
    }
    ready.sort_by_key(|(position, _)| *position);
    failed.sort_by_key(|(position, _)| *position);
    Results { ready, failed, not_started: (0..total).filter(|p| !done.contains(p)).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::base_game::Saved;

    fn behind(key: &str, mod_position: usize, removed: bool) -> Behind {
        let saved = |text: &str| Saved { file: "common/x/a.txt".into(), text: text.into() };
        Behind { category: "common/x".into(), key: key.into(), mod_position, file: "common/x/m.txt".into(), was: saved("old"), now: (!removed).then(|| saved("new")) }
    }

    #[test]
    fn a_batch_skips_updated_and_removed_objects_and_follows_the_list() {
        let all = [behind("a", 2, false), behind("b", 0, false), behind("c", 0, true), behind("d", 1, false), behind("e", 0, false)];
        let updated = |_: &str, key: &str| key == "d";
        // By mod, then as listed; "c" was removed from the game, "d" is done.
        assert_eq!(included(&all, updated), [1, 4, 0]);
        assert_eq!(included(&all, |_, _| true), Vec::<usize>::new());
    }

    #[test]
    fn failures_are_listed_apart_and_results_come_back_in_queue_order() {
        let results = vec![(2, Ok("two")), (0, Err("rate limited".to_owned())), (3, Ok("three")), (1, Err("rejected".to_owned()))];
        let sorted = sort(5, results);
        assert_eq!(sorted.ready, [(2, "two"), (3, "three")]);
        assert_eq!(sorted.failed, [(0, "rate limited".to_owned()), (1, "rejected".to_owned())]);
        assert_eq!(sorted.not_started, [4]);
    }

    #[test]
    fn stopping_early_keeps_the_finished_results() {
        let sorted = sort(4, vec![(1, Ok(())), (0, Ok(()))]);
        assert_eq!(sorted.ready.len(), 2);
        assert!(sorted.failed.is_empty());
        assert_eq!(sorted.not_started, [2, 3]);
    }
}
