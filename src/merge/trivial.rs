//! Conflicts that don't need a model to merge: every mod ships the same
//! version, or only one version differs from the base game, so taking it
//! keeps every mod's changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::conflicts::Report;
use super::patch::fingerprint;
use super::source::{self, Source};

/// A version that can be used as the merge as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Index into the mods' versions of the one to use.
    pub index: usize,
    /// Why, for the player.
    pub reason: String,
}

/// Picks the version to use when no real merge is needed. `sources` are the
/// mods' versions in load order.
pub fn resolve(vanilla: Option<&Source>, sources: &[Source]) -> Option<Resolved> {
    let prints: Vec<String> = sources.iter().map(|s| print(&s.text)).collect();
    let last = prints.len().checked_sub(1)?;
    let base = vanilla.map(|v| print(&v.text));
    if prints.iter().all(|p| *p == prints[0]) {
        let reason = if base.as_ref() == Some(&prints[0]) { "Every mod's version matches the base game" } else { "Every mod ships the same version" };
        return Some(Resolved { index: last, reason: reason.to_owned() });
    }
    let base = base?;
    let changed: Vec<usize> = (0..prints.len()).filter(|&i| prints[i] != base).collect();
    let &newest = changed.last()?;
    if changed.iter().any(|&i| prints[i] != prints[newest]) {
        return None;
    }
    let mut names: Vec<&str> = changed.iter().map(|&i| sources[i].label.as_str()).collect();
    names.dedup();
    let reason = match names[..] {
        [one] => format!("Only {one} changed it from the base game"),
        _ => format!("Only {} changed it from the base game, in the same way", join(&names)),
    };
    Some(Resolved { index: newest, reason })
}

/// The object conflicts in `report` that need no AI, by index, with the
/// reason. `roots` and `names` are by scanned position. Reads each file once
/// and each game folder once; slow parts of a big load order, so run it off
/// the main thread.
pub fn simple_conflicts(report: &Report, roots: &[Option<PathBuf>], names: &[String], game_dir: Option<&Path>) -> HashMap<usize, String> {
    let mut by_category: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, c) in report.objects.iter().enumerate() {
        by_category.entry(c.category.as_str()).or_default().push(i);
    }
    // Each file parsed once: its objects by key.
    let mut files: HashMap<(PathBuf, &str), HashMap<String, String>> = HashMap::new();
    let mut simple = HashMap::new();
    for (category, indices) in by_category {
        let keys: Vec<String> = indices.iter().map(|&i| report.objects[i].key.clone()).collect();
        let vanilla = game_dir.map(|dir| source::vanilla_many(dir, category, &keys)).unwrap_or_default();
        'objects: for i in indices {
            let c = &report.objects[i];
            let mut sources = Vec::new();
            for d in &c.definitions {
                let Some(root) = roots.get(d.mod_position).cloned().flatten() else { continue 'objects };
                let path = root.join(&d.file);
                let objects = files.entry((path.clone(), category)).or_insert_with(|| {
                    std::fs::read(&path).map(|b| source::index(category, &String::from_utf8_lossy(&b))).unwrap_or_default()
                });
                let Some(text) = objects.get(&c.key).cloned() else { continue 'objects };
                sources.push(Source { label: names[d.mod_position].clone(), file: d.file.clone(), text });
            }
            if let Some(resolved) = resolve(vanilla.get(&c.key), &sources) {
                simple.insert(i, resolved.reason);
            }
        }
    }
    simple
}

fn join(names: &[&str]) -> String {
    match names {
        [rest @ .., last] if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.join(""),
    }
}

/// A fingerprint that ignores formatting and comments.
fn print(text: &str) -> String {
    let stripped: String = text.lines().map(strip_comment).collect::<Vec<_>>().join("\n");
    fingerprint(&stripped)
}

/// The line without its `#` comment; a `#` inside quotes is text.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(label: &str, text: &str) -> Source {
        Source { label: label.into(), file: "f.txt".into(), text: text.into() }
    }

    #[test]
    fn takes_the_only_changed_version() {
        let v = src("Vanilla", "x = { cost = 100 }");
        let a = src("A", "x = {\n\tcost = 100 # same, reformatted\n}");
        let b = src("B", "x = { cost = 50 }");
        let c = src("C", "x = { cost = 100 }");
        assert_eq!(resolve(Some(&v), &[a.clone(), b.clone(), c]), Some(Resolved { index: 1, reason: "Only B changed it from the base game".into() }));
        // Two mods making the same change.
        let b2 = src("B2", "x = { cost = 50 } # cheaper");
        assert_eq!(resolve(Some(&v), &[b.clone(), a.clone(), b2]).unwrap(), Resolved { index: 2, reason: "Only B and B2 changed it from the base game, in the same way".into() });
        // Two different changes need a real merge.
        assert_eq!(resolve(Some(&v), &[b, src("D", "x = { cost = 70 }")]), None);
    }

    #[test]
    fn finds_simple_conflicts_in_a_report() {
        use crate::merge::conflicts::{Definition, ObjectConflict};
        let root = std::env::temp_dir().join(format!("drukhal-simple-{}", std::process::id()));
        let (game, a, b) = (root.join("game"), root.join("a"), root.join("b"));
        for (dir, text) in [(&game, "x = { cost = 1 }\ny = { cost = 1 }"), (&a, "x = { cost = 1 }\ny = { cost = 2 }"), (&b, "x = { cost = 3 }\ny = { cost = 4 }")] {
            std::fs::create_dir_all(dir.join("common/things")).unwrap();
            std::fs::write(dir.join("common/things/t.txt"), text).unwrap();
        }
        let conflict = |key: &str| ObjectConflict { category: "common/things".into(), key: key.into(), definitions: vec![Definition { mod_position: 0, file: "common/things/t.txt".into() }, Definition { mod_position: 1, file: "common/things/t.txt".into() }] };
        let report = Report { files: vec![], objects: vec![conflict("x"), conflict("y")], errors: vec![], skipped: vec![] };
        let simple = simple_conflicts(&report, &[Some(a), Some(b)], &["A".into(), "B".into()], Some(&game));
        assert_eq!(simple.get(&0).map(String::as_str), Some("Only B changed it from the base game"));
        assert!(!simple.contains_key(&1), "both changed y differently");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Times the check on the real enabled mods.
    /// `cargo test time_simple -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn time_simple_conflicts_on_real_mods() {
        let home = gtk::glib::home_dir();
        let user = crate::paradox::steam::stellaris_user_dir();
        let workshop = crate::paradox::steam::workshop_dirs(&home, 281990);
        let mods = crate::mods::discover(&user, &workshop).unwrap();
        let enabled: Vec<_> = mods.iter().filter(|m| m.load_position.is_some() && m.id != crate::merge::patch::ID).collect();
        let mut inputs: Vec<_> = enabled.iter().map(|m| crate::merge::conflicts::ModInput { root: m.content.clone(), replace_path: m.descriptor.replace_path.clone() }).collect();
        // As the scan does: zipped mods are read from an unpacked copy.
        for input in &mut inputs {
            if let Some(root) = &input.root
                && let Ok(files) = crate::mods::archive::files(root)
            {
                input.root = Some(files);
            }
        }
        let names: Vec<String> = enabled.iter().map(|m| m.descriptor.name.clone()).collect();
        let start = std::time::Instant::now();
        let report = crate::merge::conflicts::analyze(&inputs);
        println!("scan: {:?}, {} object conflicts", start.elapsed(), report.objects.len());
        let game = crate::paradox::game::find(&crate::paradox::steam::library_folders(&home)).map(|g| g.dir);
        let roots: Vec<_> = inputs.iter().map(|i| i.root.clone().filter(|r| r.is_dir())).collect();
        let start = std::time::Instant::now();
        let simple = simple_conflicts(&report, &roots, &names, game.as_deref());
        println!("simple check: {:?}, {} of {} simple", start.elapsed(), simple.len(), report.objects.len());
    }

    #[test]
    fn takes_identical_versions() {
        let a = src("A", "x = { y = \"#1\" }");
        let b = src("B", "x = {\n y = \"#1\"\n}");
        assert_eq!(resolve(None, &[a.clone(), b.clone()]).unwrap().reason, "Every mod ships the same version");
        assert_eq!(resolve(Some(&a), &[a.clone(), b]).unwrap().reason, "Every mod's version matches the base game");
        assert_eq!(resolve(None, &[a, src("C", "x = { y = \"#2\" }")]), None, "different without a base to compare");
        assert_eq!(resolve(None, &[]), None);
    }
}
