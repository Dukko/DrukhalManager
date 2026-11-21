//! Mods that change nothing in game: every file they ship is replaced by a
//! mod that loads later, so the game never uses any of them. Turning such a
//! mod off changes nothing. Found from the conflict scan alone.
//!
//! Byte-identical copies count as replaced.

use std::collections::BTreeSet;

use super::conflicts::Report;
use super::order_check::Relation;

/// A mod all of whose files later mods replace. Positions are scanned positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unused {
    pub mod_position: usize,
    /// The mods replacing its files, with how many each replaces, most first.
    pub by: Vec<(usize, usize)>,
}

/// The enabled mods whose files are all replaced. `depends_on` is what each
/// mod declares it builds on, by scanned position. Mods another mod depends on
/// are left out, because turning them off would trip that mod's requirement,
/// and so are mods declaring a `replace_path`, which hides base-game files
/// whatever else happens to them, and mods that load above the mod `order` says they belong below:
/// moving them fixes that.
pub fn unused(report: &Report, depends_on: &[BTreeSet<usize>], order: &[Relation]) -> Vec<Unused> {
    let mut found = Vec::new();
    for (position, replacers) in report.replaced_by.iter().enumerate() {
        let shipped = report.shipped.get(position).copied().unwrap_or(0);
        if shipped == 0 || replacers.values().sum::<usize>() != shipped || report.skipped.iter().any(|(s, _)| *s == position) {
            continue;
        }
        if report.has_replace_path.get(position).copied().unwrap_or(false) {
            continue;
        }
        if depends_on.iter().enumerate().any(|(other, on)| other != position && on.contains(&position)) {
            continue;
        }
        if order.iter().any(|r| r.patch == position && r.misplaced()) {
            continue;
        }
        let mut by: Vec<(usize, usize)> = replacers.iter().map(|(&m, &n)| (m, n)).collect();
        by.sort_by_key(|&(m, n)| (std::cmp::Reverse(n), m));
        found.push(Unused { mod_position: position, by });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::conflicts::{self, ModInput};

    type Files = Vec<(&'static str, &'static str)>;

    /// Builds each mod's (path, contents) files in a fresh temp folder, scans
    /// them and returns the unused mods. `replace_paths` lists (mod, folder),
    /// `depends` lists (mod, builds on) and `order` the load-order relations.
    fn run(tag: &str, mods: &[Files], replace_paths: &[(usize, &str)], depends: &[(usize, usize)], order: &[Relation]) -> Vec<Unused> {
        let root = std::env::temp_dir().join(format!("drukhal-unused-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut inputs = Vec::new();
        for (i, files) in mods.iter().enumerate() {
            let dir = root.join(format!("m{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            for (path, contents) in files {
                let full = dir.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(full, contents).unwrap();
            }
            let replace_path = replace_paths.iter().filter(|(m, _)| *m == i).map(|(_, p)| p.to_string()).collect();
            inputs.push(ModInput { root: Some(dir), replace_path });
        }
        let report = conflicts::analyze(&inputs, None);
        let mut deps = vec![BTreeSet::new(); mods.len()];
        for &(m, on) in depends {
            deps[m].insert(on);
        }
        let found = unused(&report, &deps, order);
        std::fs::remove_dir_all(root).unwrap();
        found
    }

    #[test]
    fn flags_a_mod_whose_every_file_is_replaced() {
        let a = vec![("gfx/a.dds", "a"), ("gfx/b.dds", "a")];
        let b = vec![("gfx/a.dds", "b"), ("gfx/b.dds", "b"), ("gfx/c.dds", "b")];
        assert_eq!(run("full", &[a, b], &[], &[], &[]), vec![Unused { mod_position: 0, by: vec![(1, 2)] }]);
    }

    #[test]
    fn counts_each_replacer_of_a_mod_spread_over_several() {
        let a = vec![("gfx/a.dds", "a"), ("gfx/b.dds", "a"), ("gfx/c.dds", "a")];
        let b = vec![("gfx/a.dds", "b")];
        let c = vec![("gfx/b.dds", "c"), ("gfx/c.dds", "c")];
        assert_eq!(run("spread", &[a, b, c], &[], &[], &[]), vec![Unused { mod_position: 0, by: vec![(2, 2), (1, 1)] }]);
    }

    #[test]
    fn keeps_a_mod_with_one_file_nobody_replaces() {
        let a = vec![("gfx/a.dds", "a"), ("gfx/b.dds", "a")];
        let b = vec![("gfx/a.dds", "b")];
        assert!(run("kept", &[a, b], &[], &[], &[]).is_empty());
    }

    #[test]
    fn flags_only_the_mod_that_loads_first() {
        let a = vec![("gfx/a.dds", "a")];
        let b = vec![("gfx/a.dds", "b")];
        let found = run("last", &[a, b], &[], &[], &[]);
        assert_eq!(found.iter().map(|u| u.mod_position).collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn flags_a_mod_whose_folder_a_later_replace_path_covers() {
        let a = vec![("events/a.txt", "a"), ("events/b.txt", "a")];
        let b = vec![("common/x.txt", "b")];
        assert_eq!(run("replace", &[a, b], &[(1, "events")], &[], &[]), vec![Unused { mod_position: 0, by: vec![(1, 2)] }]);
    }

    #[test]
    fn keeps_a_mod_with_a_replace_path_even_if_all_its_files_are_replaced() {
        let a = vec![("events/x.txt", "a")];
        let b = vec![("events/x.txt", "b")];
        assert!(run("rp-kept", &[a, b], &[(0, "events")], &[], &[]).is_empty());
    }

    #[test]
    fn flags_a_mod_dropped_by_a_replace_path_that_stays() {
        let a = vec![("events/a.txt", "a")];
        let b = vec![("events/b.txt", "b")];
        let c = vec![("events/b.txt", "c")];
        assert_eq!(run("rp-chain", &[a, b, c], &[(1, "events")], &[], &[]), vec![Unused { mod_position: 0, by: vec![(1, 1)] }]);
    }

    #[test]
    fn counts_files_a_replace_path_drops_and_the_same_mod_ships_again() {
        let a = vec![("events/a.txt", "a"), ("events/b.txt", "a")];
        let b = vec![("events/a.txt", "b")];
        assert_eq!(run("rp-reship", &[a, b], &[(1, "events")], &[], &[]), vec![Unused { mod_position: 0, by: vec![(1, 2)] }]);
    }

    #[test]
    fn keeps_a_mod_whose_case_variant_paths_nobody_replaces() {
        let a = vec![("gfx/A.dds", "a1"), ("gfx/a.dds", "a2")];
        assert!(run("case-alone", &[a], &[], &[], &[]).is_empty());
    }

    #[test]
    fn counts_case_variant_paths_of_one_mod_each_when_replaced() {
        let a = vec![("gfx/A.dds", "a1"), ("gfx/a.dds", "a2")];
        let b = vec![("gfx/a.dds", "b")];
        assert_eq!(run("case-replaced", &[a, b], &[], &[], &[]), vec![Unused { mod_position: 0, by: vec![(1, 2)] }]);
    }

    #[test]
    fn keeps_a_mod_another_mod_depends_on() {
        let a = vec![("gfx/a.dds", "a")];
        let b = vec![("gfx/a.dds", "b")];
        assert!(run("depended", &[a, b], &[], &[(1, 0)], &[]).is_empty());
    }

    #[test]
    fn leaves_a_misplaced_patch_to_the_load_order_check() {
        let a = vec![("gfx/a.dds", "a")];
        let b = vec![("gfx/a.dds", "b")];
        let misplaced = Relation { patch: 0, target: 1, replaced: 1, of: 1 };
        assert!(run("misplaced", &[a.clone(), b.clone()], &[], &[], &[misplaced]).is_empty());
        let placed = Relation { patch: 1, target: 0, replaced: 1, of: 1 };
        assert_eq!(run("placed", &[a, b], &[], &[], &[placed]).len(), 1);
    }

    #[test]
    fn ignores_a_mod_the_scan_read_no_files_from() {
        let a: Files = vec![];
        let b = vec![("gfx/a.dds", "b")];
        assert!(run("empty", &[a, b], &[], &[], &[]).is_empty());
    }

    #[test]
    fn ignores_a_skipped_mod() {
        let report = Report { shipped: vec![1], replaced_by: vec![[(1, 1)].into()], skipped: vec![(0, "x".into())], ..Report::default() };
        assert!(unused(&report, &[BTreeSet::new()], &[]).is_empty());
    }

    /// Lists the unused mods in the real load order, read only.
    /// `DRUKHAL_STELLARIS_USER=<user folder> cargo test unused_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn unused_survey() {
        use crate::mods::Descriptor;
        use crate::mods::load_order::DlcLoad;
        use std::path::PathBuf;

        let Some(user) = std::env::var_os("DRUKHAL_STELLARIS_USER") else {
            println!("SURVEY skipped: set DRUKHAL_STELLARIS_USER");
            return;
        };
        let user = PathBuf::from(user);
        let mut names = Vec::new();
        let mut dependencies = Vec::new();
        let mut inputs = Vec::new();
        for entry in DlcLoad::read(&user).unwrap().enabled_mods {
            if entry.contains("drukhal_merged_patch") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(user.join(&entry)) else { continue };
            let Ok(descriptor) = Descriptor::parse(&src) else { continue };
            let Some(path) = descriptor.path.clone() else { continue };
            let root = user.join(path);
            if !root.is_dir() {
                continue;
            }
            names.push(descriptor.name.clone());
            dependencies.push(descriptor.dependencies.clone());
            inputs.push(ModInput { root: Some(root), replace_path: descriptor.replace_path.clone() });
        }
        let depends_on = conflicts::depends_on(&names, &dependencies);
        let report = conflicts::analyze(&inputs, None);
        let roots: Vec<_> = inputs.iter().map(|m| m.root.clone()).collect();
        let order = crate::merge::order_check::relations(&report, &roots, &depends_on);
        let start = std::time::Instant::now();
        let found = unused(&report, &depends_on, &order);
        let took = start.elapsed();
        for u in &found {
            let by: Vec<String> = u.by.iter().map(|&(m, n)| format!("#{} {} ({n})", m + 1, names[m])).collect();
            println!("UNUSED #{} {} ({} files): {}", u.mod_position + 1, names[u.mod_position], report.shipped[u.mod_position], by.join(", "));
        }
        println!("scanned {} mods, {} flagged", inputs.len(), found.len());
        println!("unused took {} us", took.as_micros());
    }
}
