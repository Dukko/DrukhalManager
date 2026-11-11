//! Finding the mod that crashes the game by halving the suspects: each
//! test launches with the mods cleared so far plus half the suspects, and
//! whether it crashed says which half to keep. One culprit takes about
//! log2(n) launches.

use std::collections::{BTreeSet, HashMap};

/// Where a hunt stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Launch with these mods, in load order, and say whether it crashed.
    Test(Vec<usize>),
    /// The game crashes with this mod and the mods proven safe, and
    /// doesn't without it. It may crash only together with one of those.
    Found(usize),
    /// No single mod does: it takes several together. The suspects left.
    Combination(Vec<usize>),
    /// Nothing crashed, even with every mod as at the start.
    NoCrash,
}

#[derive(Debug, Clone)]
pub struct Hunt {
    /// The load order the hunt started from.
    order: Vec<usize>,
    /// What each mod needs, among `order`.
    needs: HashMap<usize, Vec<usize>>,
    /// Mods that may be the culprit, suspects first.
    suspects: Vec<usize>,
    /// Mods that were enabled in a test that didn't crash.
    cleared: BTreeSet<usize>,
    /// The test being run.
    testing: Vec<usize>,
    /// Whether a test has crashed yet; the first test is everything.
    seen_crash: bool,
    /// The suspects when the last test crashed, for combinations.
    last_crash: Vec<usize>,
    pub launches: usize,
}

impl Hunt {
    /// Starts from the load order `order`, testing `first` mods (from the
    /// error log) before the others. `needs` lists each mod's dependencies.
    pub fn new(order: Vec<usize>, needs: HashMap<usize, Vec<usize>>, first: &[usize]) -> Self {
        let mut suspects = order.clone();
        // Stable: suspects from the log first, the rest in load order.
        suspects.sort_by_key(|i| !first.contains(i));
        Self { testing: order.clone(), order, needs, last_crash: suspects.clone(), suspects, cleared: BTreeSet::new(), seen_crash: false, launches: 0 }
    }

    pub fn step(&self) -> Step {
        Step::Test(self.testing.clone())
    }

    /// About how many launches are left.
    pub fn estimate(&self) -> usize {
        let n = self.suspects.len().max(1);
        (usize::BITS - (n - 1).leading_zeros()) as usize + usize::from(!self.seen_crash)
    }

    pub fn suspects(&self) -> usize {
        self.suspects.len()
    }

    /// With the mods each of `set` needs, recursively, in load order.
    fn with_needs(&self, set: impl IntoIterator<Item = usize>) -> Vec<usize> {
        let mut on: BTreeSet<usize> = BTreeSet::new();
        let mut todo: Vec<usize> = set.into_iter().collect();
        while let Some(i) = todo.pop() {
            if on.insert(i) {
                todo.extend(self.needs.get(&i).into_iter().flatten().copied());
            }
        }
        self.order.iter().copied().filter(|i| on.contains(i)).collect()
    }

    /// Records the last test's result and says what's next.
    pub fn answer(&mut self, crashed: bool) -> Step {
        self.launches += 1;
        let tested: BTreeSet<usize> = self.testing.iter().copied().collect();
        if crashed {
            self.seen_crash = true;
            self.suspects.retain(|i| tested.contains(i));
            self.last_crash = self.suspects.clone();
        } else {
            if !self.seen_crash {
                return Step::NoCrash;
            }
            self.cleared.extend(&tested);
            self.suspects.retain(|i| !tested.contains(i));
        }
        match self.suspects.as_slice() {
            // Every suspect passed without the others: it takes several.
            [] => Step::Combination(self.last_crash.clone()),
            // It crashed with this one the only suspect left on.
            [one] if crashed => Step::Found(*one),
            [one] => {
                self.testing = self.with_needs(self.cleared.iter().copied().chain([*one]));
                Step::Test(self.testing.clone())
            }
            suspects => {
                let (a, b) = suspects.split_at(suspects.len() / 2);
                // A test must leave a suspect out to narrow anything down,
                // which dependencies can prevent for one half.
                let leaves_out = |t: &Vec<usize>| suspects.iter().any(|s| !t.contains(s));
                let test = [a, b].into_iter().map(|half| self.with_needs(self.cleared.iter().copied().chain(half.iter().copied()))).find(leaves_out);
                match test {
                    Some(test) => {
                        self.testing = test;
                        Step::Test(self.testing.clone())
                    }
                    // They need each other: they can't be told apart.
                    None => Step::Combination(self.suspects.clone()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a hunt where launching with `crashes` (all of them) crashes.
    fn run(order: Vec<usize>, needs: HashMap<usize, Vec<usize>>, first: &[usize], crashes: &[usize]) -> (Step, usize) {
        let mut hunt = Hunt::new(order, needs, first);
        let mut step = hunt.step();
        while let Step::Test(on) = &step {
            let crashed = crashes.iter().all(|c| on.contains(c));
            step = hunt.answer(crashed);
            assert!(hunt.launches < 50, "runs away");
        }
        (step, hunt.launches)
    }

    #[test]
    fn finds_one_culprit_in_about_log2_launches() {
        for culprit in 0..100 {
            let (step, launches) = run((0..100).collect(), HashMap::new(), &[], &[culprit]);
            assert_eq!(step, Step::Found(culprit));
            assert!(launches <= 9, "{culprit}: {launches} launches");
        }
    }

    #[test]
    fn tests_log_suspects_first() {
        let (step, launches) = run((0..64).collect(), HashMap::new(), &[40], &[40]);
        assert_eq!(step, Step::Found(40));
        assert!(launches <= 8);
    }

    #[test]
    fn keeps_dependencies_with_their_mods() {
        // 7 needs 0; every test with 7 on has 0 on too.
        let needs = HashMap::from([(7, vec![0])]);
        let mut hunt = Hunt::new((0..8).collect(), needs, &[7]);
        let mut step = hunt.answer(true);
        while let Step::Test(on) = &step {
            assert!(hunt.launches < 20, "runs away");
            if on.contains(&7) {
                assert!(on.contains(&0));
            }
            step = hunt.answer(on.contains(&7));
        }
        assert_eq!(step, Step::Found(7));
        // And when the dependency is the culprit.
        let (step, _) = run((0..8).collect(), HashMap::from([(7, vec![0])]), &[7], &[0]);
        assert_eq!(step, Step::Found(0));
        // Mods that need each other can't be told apart.
        let (step, _) = run(vec![0, 1], HashMap::from([(0, vec![1]), (1, vec![0])]), &[], &[0]);
        assert_eq!(step, Step::Combination(vec![0, 1]));
    }

    #[test]
    fn reports_combinations_and_no_crash() {
        // Cleared mods stay on, so the second of a pair is found.
        let (step, _) = run((0..8).collect(), HashMap::new(), &[], &[1, 6]);
        assert_eq!(step, Step::Found(6));
        let (step, launches) = run((0..8).collect(), HashMap::new(), &[], &[99]);
        assert_eq!((step, launches), (Step::NoCrash, 1));
    }
}
