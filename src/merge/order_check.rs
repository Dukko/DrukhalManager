//! Which mods load-order overrides throw away: a mod most of whose files
//! another mod ships too, in different versions, has to load below that mod
//! for its own versions to be used. Found from the conflict scan alone.
//!
//! Only whole-file overrides count. Byte-identical copies are ignored,
//! because sister mods often bundle the same shared files and their order
//! makes no difference.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::conflicts::Report;

/// The fewest replaced files that make a relation.
const MIN_FILES: usize = 3;

/// The share of a mod's files, in percent, that has to be replaced.
const SHARE_PERCENT: usize = 40;

/// `patch` ships few files and `target` ships far more, and most of
/// `patch`'s files are also in `target` with different contents. Positions
/// are scanned positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub patch: usize,
    pub target: usize,
    /// Files both ship whose contents differ.
    pub replaced: usize,
    /// Files `patch` ships.
    pub of: usize,
}

impl Relation {
    /// Whether `patch` loads above `target`, so `target`'s copies win.
    pub fn misplaced(&self) -> bool {
        self.patch < self.target
    }
}

/// Finds the mods most of whose files a much bigger mod also ships in a
/// different version. `roots` are the scanned mods' folders and `depends_on`
/// what each declares it builds on, both by scanned position.
pub fn relations(report: &Report, roots: &[Option<PathBuf>], depends_on: &[BTreeSet<usize>]) -> Vec<Relation> {
    // The conflicts each pair of mods has, with `a < b`.
    let mut shared: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
    for (index, conflict) in report.files.iter().enumerate() {
        for (i, &a) in conflict.mods.iter().enumerate() {
            for &b in &conflict.mods[i + 1..] {
                shared.entry((a.min(b), a.max(b))).or_default().push(index);
            }
        }
    }

    let shipped = |m: usize| report.shipped.get(m).copied().unwrap_or(0);
    let mut found = Vec::new();
    for (&(a, b), conflicts) in &shared {
        let n = conflicts.len();
        // How many shared files differ, found once and only if needed.
        let mut differs: Option<usize> = None;
        for (x, y) in [(a, b), (b, a)] {
            let (sx, sy) = (shipped(x), shipped(y));
            if sx == 0 || sy == 0 || n < MIN_FILES || n * 100 < sx * SHARE_PERCENT || sy <= 2 * sx {
                continue;
            }
            // `y` is declared to build on `x`, so it has to load below it.
            if depends_on.get(y).is_some_and(|d| d.contains(&x)) {
                continue;
            }
            let replaced = *differs.get_or_insert_with(|| {
                let (Some(Some(ra)), Some(Some(rb))) = (roots.get(a), roots.get(b)) else { return 0 };
                conflicts.iter().filter(|&&c| differ(ra, rb, &report.files[c].path)).count()
            });
            if replaced >= MIN_FILES && replaced * 100 >= sx * SHARE_PERCENT {
                found.push(Relation { patch: x, target: y, replaced, of: sx });
            }
        }
    }
    found.sort_by_key(|r| (r.patch, r.target));
    found
}

/// Whether the two mods' copies of `path` exist and differ. Unreadable files
/// don't count as different.
fn differ(a: &Path, b: &Path, path: &str) -> bool {
    match (locate(a, path), locate(b, path)) {
        (Some(a), Some(b)) => same_bytes(&a, &b) == Some(false),
        _ => false,
    }
}

/// The file at `path` in `root`, found even if the mod spells some of its
/// folders or the file with other letter case. Never leaves `root`.
fn locate(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    if relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let direct = root.join(relative);
    if direct.exists() {
        return Some(direct);
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let wanted = component.as_os_str().to_string_lossy().to_lowercase();
        let entry = std::fs::read_dir(&current).ok()?.flatten().find(|e| e.file_name().to_string_lossy().to_lowercase() == wanted)?;
        current = entry.path();
    }
    Some(current)
}

/// Whether two files hold the same bytes; `None` if either can't be read.
fn same_bytes(a: &Path, b: &Path) -> Option<bool> {
    if std::fs::metadata(a).ok()?.len() != std::fs::metadata(b).ok()?.len() {
        return Some(false);
    }
    let (mut fa, mut fb) = (std::fs::File::open(a).ok()?, std::fs::File::open(b).ok()?);
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Some(false);
        }
        if n == 0 {
            return Some(true);
        }
    }
}

/// Fills `buf` as far as the file goes; `None` on a read error.
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> Option<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::conflicts::{self, ModInput};

    type Files = Vec<(String, String)>;

    /// Builds each mod's (path, contents) files in a fresh temp folder, scans
    /// them and returns the relations. `depends` lists (mod, builds on).
    fn run(tag: &str, mods: &[Files], depends: &[(usize, usize)]) -> Vec<Relation> {
        let root = std::env::temp_dir().join(format!("drukhal-order-{tag}-{}", std::process::id()));
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
            inputs.push(ModInput { root: Some(dir), replace_path: vec![] });
        }
        let report = conflicts::analyze(&inputs, None);
        let roots: Vec<_> = inputs.iter().map(|m| m.root.clone()).collect();
        let mut deps = vec![BTreeSet::new(); mods.len()];
        for &(m, on) in depends {
            deps[m].insert(on);
        }
        let found = relations(&report, &roots, &deps);
        std::fs::remove_dir_all(root).unwrap();
        found
    }

    /// `count` files `gfx/{prefix}{n}.dds` holding `text`.
    fn files(prefix: &str, count: usize, text: &str) -> Vec<(String, String)> {
        (0..count).map(|n| (format!("gfx/{prefix}{n}.dds"), text.to_string())).collect()
    }

    /// A small mod whose first `shared` files are also in a big one.
    fn pair(small_count: usize, shared: usize, small_text: &str, big_text: &str) -> (Files, Files) {
        let small = files("s", small_count, small_text);
        let mut big = files("b", 10, big_text);
        for (path, _) in small.iter().take(shared) {
            big.push((path.clone(), big_text.to_string()));
        }
        (small, big)
    }

    #[test]
    fn flags_a_patch_above_its_target() {
        let (small, big) = pair(4, 3, "small", "big");
        let found = run("above", &[small, big], &[]);
        assert_eq!(found, vec![Relation { patch: 0, target: 1, replaced: 3, of: 4 }]);
        assert!(found[0].misplaced());
    }

    #[test]
    fn accepts_a_patch_below_its_target() {
        let (small, big) = pair(4, 3, "small", "big");
        let found = run("below", &[big, small], &[]);
        assert_eq!(found, vec![Relation { patch: 1, target: 0, replaced: 3, of: 4 }]);
        assert!(!found[0].misplaced());
    }

    #[test]
    fn ignores_identical_copies() {
        let (small, big) = pair(4, 3, "same", "same");
        assert!(run("identical", &[small, big], &[]).is_empty());
    }

    #[test]
    fn ignores_when_too_few_differ() {
        let (small, mut big) = pair(4, 3, "small", "big");
        // One of the three shared copies is identical.
        let last = big.len() - 1;
        big[last].1 = "small".into();
        assert!(run("partly", &[small, big], &[]).is_empty());
    }

    #[test]
    fn ignores_a_small_share() {
        let (small, big) = pair(10, 3, "small", "big");
        assert!(run("share", &[small, big], &[]).is_empty());
    }

    #[test]
    fn ignores_two_big_mods() {
        let a = files("s", 10, "a");
        let mut b = files("b", 4, "b");
        b.extend(a.iter().take(6).map(|(p, _)| (p.clone(), "b".to_string())));
        assert!(run("big", &[a, b], &[]).is_empty());
    }

    #[test]
    fn ignores_a_declared_add_on() {
        let (small, big) = pair(4, 3, "small", "big");
        // The big mod depends on the small one, wherever each loads.
        assert!(run("addon", &[big.clone(), small.clone()], &[(0, 1)]).is_empty());
        assert!(run("addon2", &[small, big], &[(1, 0)]).is_empty());
    }

    #[test]
    fn matches_paths_regardless_of_letter_case() {
        let small: Vec<(String, String)> = (0..4).map(|n| (format!("gfx/models/s{n}.dds"), "small".to_string())).collect();
        let mut big = files("b", 10, "big");
        big.extend((0..3).map(|n| (format!("GFX/models/s{n}.dds"), "big".to_string())));
        let found = run("case", &[small, big], &[]);
        assert_eq!(found, vec![Relation { patch: 0, target: 1, replaced: 3, of: 4 }]);
    }

    #[test]
    fn locate_stays_inside_the_mod() {
        let root = std::env::temp_dir().join(format!("drukhal-order-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mod")).unwrap();
        std::fs::write(root.join("outside.txt"), "x").unwrap();
        assert!(locate(&root.join("mod"), "../outside.txt").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Lists the relations in the real load order, read only.
    /// `DRUKHAL_STELLARIS_USER=<user folder> cargo test order_check_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn order_check_survey() {
        use crate::mods::Descriptor;
        use crate::mods::load_order::DlcLoad;

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
        let start = std::time::Instant::now();
        let found = relations(&report, &roots, &depends_on);
        let took = start.elapsed();
        for r in &found {
            println!("{} #{} {} vs #{} {}: {}/{}", if r.misplaced() { "ABOVE" } else { "ok" }, r.patch + 1, names[r.patch], r.target + 1, names[r.target], r.replaced, r.of);
        }
        println!("relations took {} ms", took.as_millis());
    }
}
