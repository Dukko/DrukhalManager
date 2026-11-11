//! Freezing a collection: its Workshop mods are copied into local mods and
//! the collection switches to the copies, so Steam updating a mod in the
//! middle of a campaign can't change it. Unfreezing switches back and
//! deletes the copies.
//!
//! Copies keep their mod's name, so other mods' dependencies on it still
//! hold, but not its `remote_file_id`, so neither Steam nor the launcher
//! takes them for the Workshop item. They live in `mod/drukhal_frozen/`,
//! one folder per collection, and nothing outside it is ever deleted.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Descriptor;

/// The folder in the user's `mod` folder holding every frozen copy.
pub const FOLDER: &str = "drukhal_frozen";
/// Frozen copies' descriptor ids start with this.
const ID_PREFIX: &str = "mod/drukhal_frozen_";

/// What a frozen copy was copied from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frozen {
    /// The Workshop mod's descriptor id (`mod/ugc_1.mod`).
    pub workshop: String,
    pub steam_id: u64,
    /// When Steam last updated it, as of freezing; `None` if unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<u64>,
}

/// A Workshop mod to freeze.
pub struct Source<'a> {
    pub id: &'a str,
    pub steam_id: u64,
    pub descriptor: &'a Descriptor,
    /// Its folder, or its zip for `archive` mods.
    pub content: &'a Path,
}

/// Whether `id` is a frozen copy.
pub fn is_frozen(id: &str) -> bool {
    id.starts_with(ID_PREFIX)
}

/// The folder a collection's copies go in, by a name free of other
/// collections' (`taken`).
pub fn folder_name(collection: &str, taken: impl Fn(&str) -> bool) -> String {
    let slug: String = collection.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let slug = slug.split('_').filter(|w| !w.is_empty()).collect::<Vec<_>>().join("_");
    let slug: String = if slug.is_empty() { "collection".into() } else { slug.chars().take(40).collect() };
    (1..).map(|n| if n == 1 { slug.clone() } else { format!("{slug}_{n}") }).find(|f| !taken(f)).unwrap()
}

/// How many bytes copying `contents` takes.
pub fn size(contents: &[&Path]) -> u64 {
    fn walk(path: &Path) -> u64 {
        match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => std::fs::read_dir(path).map_or(0, |entries| entries.filter_map(Result::ok).map(|e| walk(&e.path())).sum()),
            Ok(m) => m.len(),
            Err(_) => 0,
        }
    }
    contents.iter().map(|p| walk(p)).sum()
}

/// Copies `sources` into `mod/drukhal_frozen/<folder>/` in `user_dir` and
/// writes their descriptors. Returns each copy's id and what it froze.
/// `updated` is when Steam last updated each item. On failure, the copies
/// made so far are removed.
pub fn freeze(user_dir: &Path, folder: &str, sources: &[Source], updated: &HashMap<u64, u64>) -> io::Result<BTreeMap<String, Frozen>> {
    let root = user_dir.join("mod").join(FOLDER).join(folder);
    let mut done = BTreeMap::new();
    let result = (|| {
        for s in sources {
            let id = format!("{ID_PREFIX}{folder}_{}.mod", s.steam_id);
            let dest = root.join(s.steam_id.to_string());
            let _ = std::fs::remove_dir_all(&dest);
            let path = if s.content.is_file() {
                // A zipped mod stays zipped.
                std::fs::create_dir_all(&dest)?;
                let zip = dest.join(s.content.file_name().unwrap_or_default());
                std::fs::copy(s.content, &zip)?;
                (zip, true)
            } else {
                copy_dir(s.content, &dest)?;
                (dest.clone(), false)
            };
            let inner = render(s.descriptor);
            std::fs::write(dest.join("descriptor.mod"), &inner)?;
            let key = if path.1 { "archive" } else { "path" };
            std::fs::write(user_dir.join(&id), format!("{inner}{key}=\"{}\"\n", path.0.display()))?;
            done.insert(id, Frozen { workshop: s.id.to_owned(), steam_id: s.steam_id, updated: updated.get(&s.steam_id).copied() });
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = unfreeze(user_dir, &done);
        return Err(e);
    }
    Ok(done)
}

/// Deletes the frozen copies `frozen` lists, and their descriptors.
pub fn unfreeze(user_dir: &Path, frozen: &BTreeMap<String, Frozen>) -> io::Result<()> {
    let base = user_dir.join("mod").join(FOLDER);
    for id in frozen.keys().filter(|id| is_frozen(id)) {
        let descriptor = user_dir.join(id);
        let copy = std::fs::read_to_string(&descriptor).ok().and_then(|src| Descriptor::parse(&src).ok()).and_then(|d| d.path).map(PathBuf::from);
        if let Some(copy) = copy {
            // The copy's folder: the zip's for an archive.
            let dir = if copy.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) { copy.parent().map(Path::to_path_buf) } else { Some(copy) };
            if let Some(dir) = dir.filter(|d| d.starts_with(&base) && *d != base) {
                match std::fs::remove_dir_all(&dir) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
        }
        match std::fs::remove_file(&descriptor) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    // Empty collection folders go too.
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.filter_map(Result::ok) {
            let _ = std::fs::remove_dir(entry.path());
        }
    }
    Ok(())
}

/// Swaps ids in `mods` by `map`, keeping their places.
pub fn swap(mods: &mut [String], map: &HashMap<&str, &str>) {
    for id in mods.iter_mut() {
        if let Some(to) = map.get(id.as_str()) {
            *id = (*to).to_owned();
        }
    }
}

/// A descriptor for a copy of `d`: everything but where it is and its
/// Workshop id.
fn render(d: &Descriptor) -> String {
    let q = |s: &str| s.replace('"', "\\\"");
    let list = |key: &str, items: &[String]| if items.is_empty() { String::new() } else { format!("{key}={{\n{}}}\n", items.iter().map(|i| format!("\t\"{}\"\n", q(i))).collect::<String>()) };
    let mut out = format!("name=\"{}\"\n", q(&d.name));
    if !d.version.is_empty() {
        out += &format!("version=\"{}\"\n", q(&d.version));
    }
    if !d.supported_version.is_empty() {
        out += &format!("supported_version=\"{}\"\n", q(&d.supported_version));
    }
    if let Some(p) = &d.picture {
        out += &format!("picture=\"{}\"\n", q(p));
    }
    out += &list("tags", &d.tags);
    out += &list("dependencies", &d.dependencies);
    for r in &d.replace_path {
        out += &format!("replace_path=\"{}\"\n", q(r));
    }
    for u in &d.user_dir {
        out += &format!("user_dir=\"{}\"\n", q(u));
    }
    out
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
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

    #[test]
    fn freezes_and_unfreezes() {
        let root = std::env::temp_dir().join(format!("drukhal-freeze-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, workshop) = (root.join("user"), root.join("workshop/111"));
        std::fs::create_dir_all(user.join("mod")).unwrap();
        std::fs::create_dir_all(workshop.join("common/traits")).unwrap();
        std::fs::write(workshop.join("common/traits/t.txt"), "t = {}").unwrap();
        let descriptor = Descriptor::parse("name=\"Big \\\"Mod\\\"\"\nsupported_version=\"v4.5.*\"\ntags={\n\t\"Gameplay\"\n}\ndependencies={\n\t\"Core\"\n}\nremote_file_id=\"111\"\npath=\"x\"").unwrap();
        let sources = [Source { id: "mod/ugc_111.mod", steam_id: 111, descriptor: &descriptor, content: &workshop }];
        let frozen = freeze(&user, "main", &sources, &[(111, 1700)].into()).unwrap();
        let id = "mod/drukhal_frozen_main_111.mod";
        assert!(is_frozen(id) && !is_frozen("mod/ugc_111.mod"));
        assert_eq!(frozen[id], Frozen { workshop: "mod/ugc_111.mod".into(), steam_id: 111, updated: Some(1700) });

        let copy = Descriptor::parse(&std::fs::read_to_string(user.join(id)).unwrap()).unwrap();
        assert_eq!((copy.name.as_str(), copy.remote_file_id, copy.dependencies.as_slice()), (descriptor.name.as_str(), None, ["Core".to_owned()].as_slice()));
        let folder = PathBuf::from(copy.path.unwrap());
        assert_eq!(std::fs::read_to_string(folder.join("common/traits/t.txt")).unwrap(), "t = {}");

        let mut mods = vec!["mod/a.mod".to_owned(), "mod/ugc_111.mod".into()];
        swap(&mut mods, &[("mod/ugc_111.mod", id)].into());
        assert_eq!(mods, ["mod/a.mod", id]);

        unfreeze(&user, &frozen).unwrap();
        assert!(!user.join(id).exists() && !folder.exists());
        assert!(workshop.join("common/traits/t.txt").is_file(), "the Workshop copy is untouched");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_folders_apart() {
        assert_eq!(folder_name("Main Campaign", |_| false), "main_campaign");
        assert_eq!(folder_name("Main Campaign", |f| f == "main_campaign"), "main_campaign_2");
    }
}
