//! An extra mod folder, like Irony Mod Manager's Custom Mod Directory:
//! mods kept elsewhere (on another drive, in a git checkout) are listed and
//! loaded like the rest. The game only finds mods through descriptors in
//! `<user_dir>/mod`, so one is written for each mod folder there, and
//! removed again when the mod or the setting goes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Descriptors written here start with this, so only they are ever
/// rewritten or removed.
pub const PREFIX: &str = "drukhal_extra_";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Linked {
    pub added: usize,
    pub removed: usize,
    pub errors: Vec<String>,
}

/// Writes a descriptor for every mod folder in `extra` that no descriptor
/// points at yet, and removes ones written earlier for mods that are gone.
/// With `extra` unset, removes them all.
pub fn link(user_dir: &Path, extra: Option<&Path>) -> Linked {
    let mut linked = Linked::default();
    let mod_dir = user_dir.join("mod");
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    // Folders other descriptors point at, and the ones written here.
    let mut pointed: HashSet<PathBuf> = HashSet::new();
    let mut ours: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&mod_dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_ascii_lowercase().ends_with(".mod") {
            continue;
        }
        if name.starts_with(PREFIX) {
            ours.push(path);
        } else if let Ok(d) = std::fs::read_to_string(&path).map_err(|_| ()).and_then(|s| super::Descriptor::parse(&s).map_err(|_| ()))
            && let Some(p) = d.path.filter(|p| !p.is_empty())
        {
            let p = Path::new(&p);
            pointed.insert(canonical(&if p.is_absolute() { p.to_owned() } else { user_dir.join(p) }));
        }
    }

    let mut wanted: HashSet<PathBuf> = HashSet::new();
    if let Some(extra) = extra {
        match std::fs::read_dir(extra) {
            Err(e) => linked.errors.push(unreadable(extra, &e)),
            Ok(entries) => {
                let mut folders: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("descriptor.mod").is_file()).collect();
                folders.sort();
                for folder in folders {
                    let name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    if name.starts_with('.') || name.ends_with(".drukhal-tmp") || pointed.contains(&canonical(&folder)) {
                        continue;
                    }
                    let target = mod_dir.join(format!("{PREFIX}{}.mod", slug(&name)));
                    if !wanted.insert(target.clone()) {
                        continue; // two folders with the same slug: the first wins
                    }
                    match write_descriptor(&folder, &target) {
                        Ok(true) => linked.added += 1,
                        Ok(false) => {}
                        Err(e) => linked.errors.push(format!("Could not list the mod in {}: {e}", folder.display())),
                    }
                }
            }
        }
    }
    for path in ours {
        if !wanted.contains(&path) {
            match std::fs::remove_file(&path) {
                Ok(()) => linked.removed += 1,
                Err(e) => linked.errors.push(format!("Could not remove {}: {e}", path.display())),
            }
        }
    }
    linked
}

/// Writes the descriptor for the mod in `folder`, from its own one with the
/// location replaced. Returns whether it's new.
fn write_descriptor(folder: &Path, target: &Path) -> std::io::Result<bool> {
    let inner = std::fs::read_to_string(folder.join("descriptor.mod"))?;
    let mut text: String = inner
        .lines()
        .filter(|line| {
            let key = line.trim_start().split(['=', ' ', '\t']).next().unwrap_or_default();
            !key.eq_ignore_ascii_case("path") && !key.eq_ignore_ascii_case("archive")
        })
        .map(|line| format!("{line}\n"))
        .collect();
    text.push_str(&format!("path=\"{}\"\n", folder.display()));
    let existing = std::fs::read_to_string(target).ok();
    if existing.as_deref() != Some(text.as_str()) {
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::write(target, &text)?;
    }
    Ok(existing.is_none())
}

fn slug(name: &str) -> String {
    let slug: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let slug = slug.trim_matches('_');
    if slug.is_empty() { "mod".to_owned() } else { slug.to_owned() }
}

fn unreadable(dir: &Path, e: &std::io::Error) -> String {
    if crate::sandbox::is_flatpak() && !dir.starts_with(gtk::glib::home_dir()) {
        format!(
            "The extra mod folder {} is outside your home folder, so DrukhalManager can't see it from its sandbox. Run this once, then restart: flatpak override --user --filesystem={} io.github.dukko.DrukhalManager",
            dir.display(),
            dir.display()
        )
    } else {
        format!("Could not read the extra mod folder {}: {e}", dir.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_unlists_mods() {
        let root = std::env::temp_dir().join(format!("drukhal-extra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, extra) = (root.join("user"), root.join("extra"));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        for (folder, descriptor) in [("My Mod", "name=\"Mine\"\npath=\"/old/place\"\ntags={\n\t\"Gameplay\"\n}\n"), ("other", "name=\"Other\"\n"), ("linked", "name=\"Linked\"\n")] {
            std::fs::create_dir_all(extra.join(folder)).unwrap();
            std::fs::write(extra.join(folder).join("descriptor.mod"), descriptor).unwrap();
        }
        std::fs::create_dir_all(extra.join("not a mod")).unwrap();
        // Already has a descriptor, e.g. a merged collection written there.
        std::fs::write(user.join("mod/linked.mod"), format!("name=\"Linked\"\npath=\"{}\"\n", extra.join("linked").display())).unwrap();

        assert_eq!(link(&user, Some(&extra)), Linked { added: 2, removed: 0, errors: vec![] });
        let mods = crate::mods::discover(&user, &[]).unwrap();
        let mine = mods.iter().find(|m| m.descriptor.name == "Mine").unwrap();
        assert_eq!(mine.id, "mod/drukhal_extra_my_mod.mod");
        assert_eq!(mine.content.as_deref(), Some(extra.join("My Mod").as_path()));
        assert_eq!(mine.descriptor.tags, ["Gameplay"]);
        assert_eq!(mods.len(), 3);
        assert_eq!(link(&user, Some(&extra)), Linked::default(), "nothing changes the second time");

        std::fs::remove_dir_all(extra.join("other")).unwrap();
        assert_eq!(link(&user, Some(&extra)).removed, 1);
        assert_eq!(link(&user, None), Linked { added: 0, removed: 1, errors: vec![] });
        assert!(user.join("mod/linked.mod").exists(), "never removes others");
        assert_eq!(link(&user, Some(&root.join("missing"))).errors.len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
