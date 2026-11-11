//! Merging a whole load order into one local mod, like Irony Mod Manager's
//! "Merge (Basic)" (`ModMergeService.MergeCollectionByFilesAsync`; MIT,
//! © 2020 Mario, https://github.com/bcssov/IronyModManager).
//!
//! Every mod's files are copied in load order, so a file several mods ship
//! ends up as the copy that loads last, as in the game. Files under a
//! `replace_path` are dropped from mods that load before one declaring it,
//! and the merged mod declares every `replace_path` so the game still
//! ignores the base game's files there. Unlike Irony, which copies only a
//! fixed list of game folders, every top-level folder except hidden ones
//! is copied, so `flags` and `prescripted_countries` aren't lost.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use super::conflicts::{is_under, walk};

/// Marks a folder as a merged collection, so re-merging may replace it
/// and nothing else is ever deleted.
const MARKER: &str = ".drukhal-merged-collection";

/// One mod of the load order, as merged.
pub struct Input {
    /// Where its files are (a zipped mod's unpacked copy).
    pub root: PathBuf,
    pub replace_path: Vec<String>,
}

/// The merged mod: its descriptor id and how many files it holds.
#[derive(Debug, PartialEq, Eq)]
pub struct Merged {
    pub id: String,
    pub files: usize,
}

/// The merged mod's name when no other is chosen.
pub const DEFAULT_TEMPLATE: &str = "{Name} (Merged)";

/// The mod's name for a collection, from a template in which `{Name}`
/// stands for the collection's name. A template without `{Name}` would
/// give every collection's merge the same name, so the default is used.
pub fn mod_name(collection: &str, template: Option<&str>) -> String {
    let template = template.map(str::trim).filter(|t| is_template(t)).unwrap_or(DEFAULT_TEMPLATE);
    template.replace("{Name}", collection.trim())
}

/// Whether `template` can name merged mods.
pub fn is_template(template: &str) -> bool {
    template.contains("{Name}")
}

/// The merged mod's folder and descriptor name, e.g.
/// `drukhal_merged_my_game` for "My Game".
fn folder(collection: &str) -> String {
    let slug: String = collection.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let slug = slug.trim_matches('_');
    format!("drukhal_merged_{}", if slug.is_empty() { "collection" } else { slug })
}

/// The descriptor id the merged mod gets.
pub fn id(collection: &str) -> String {
    format!("mod/{}.mod", folder(collection))
}

/// Lowercased path -> (path as spelled by the winning mod, source file).
type Files = BTreeMap<String, (String, PathBuf)>;

/// Which copy of each file ends up in the merged mod, by relative path,
/// and the union of the mods' `replace_path`s.
fn plan(mods: &[Input]) -> io::Result<(Files, Vec<String>)> {
    let mut files = Files::new();
    let mut replace_paths: Vec<String> = Vec::new();
    for m in mods {
        let mut found = Vec::new();
        walk(&m.root, &m.root, &mut found)?;
        // A later `replace_path` makes the game ignore earlier mods' files.
        files.retain(|path, _| !m.replace_path.iter().any(|r| is_under(path, r)));
        for path in found {
            files.insert(path.to_lowercase(), (path.clone(), m.root.join(&path)));
        }
        for r in &m.replace_path {
            if !replace_paths.iter().any(|p| p.eq_ignore_ascii_case(r)) {
                replace_paths.push(r.clone());
            }
        }
    }
    Ok((files, replace_paths))
}

/// Writes `mods` (in load order) as one mod named after `collection` into
/// `<user_dir>/mod`, replacing an earlier merge of the same collection.
/// Blocks; call off the main thread.
/// Writes the merged mod for `collection`, named `name`.
pub fn write(user_dir: &Path, collection: &str, name: &str, supported_version: &str, mods: &[Input]) -> io::Result<Merged> {
    let mod_dir = user_dir.join("mod");
    let target = mod_dir.join(folder(collection));
    if target.exists() && !target.join(MARKER).exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists and isn't a merged collection", target.display())));
    }
    let (files, replace_paths) = plan(mods)?;

    // Built next to the target and swapped in at the end, so a failed merge
    // leaves the previous one intact.
    let building = mod_dir.join(format!("{}.drukhal-tmp", folder(collection)));
    if building.exists() {
        std::fs::remove_dir_all(&building)?;
    }
    let result = (|| {
        for (path, source) in files.values() {
            let dest = building.join(path);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::copy(source, &dest)?;
        }
        std::fs::create_dir_all(&building)?;
        std::fs::write(building.join(MARKER), collection)?;
        let name = name.replace('"', "'");
        let mut inner = format!("name=\"{name}\"\nversion=\"1\"\ntags={{\n\t\"Merged\"\n}}\nsupported_version=\"{supported_version}\"\n");
        for r in &replace_paths {
            inner.push_str(&format!("replace_path=\"{r}\"\n"));
        }
        std::fs::write(building.join("descriptor.mod"), &inner)?;
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        std::fs::rename(&building, &target)?;
        std::fs::write(user_dir.join(id(collection)), format!("{inner}path=\"{}\"\n", target.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&building);
    }
    result.map(|()| Merged { id: id(collection), files: files.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_templates() {
        assert_eq!(mod_name(" Main ", None), "Main (Merged)");
        assert_eq!(mod_name("Main", Some("[Merged] {Name}")), "[Merged] Main");
        assert_eq!(mod_name("Main", Some("No placeholder")), "Main (Merged)");
    }

    fn put(root: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    fn input(root: &Path, replace_path: &[&str]) -> Input {
        Input { root: root.to_path_buf(), replace_path: replace_path.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn copies_in_load_order() {
        let root = std::env::temp_dir().join(format!("drukhal-collection-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b, c, user) = (root.join("a"), root.join("b"), root.join("c"), root.join("user"));
        put(&a, &[("descriptor.mod", "name=\"A\""), ("common/buildings/x.txt", "a"), ("common/ethics/e.txt", "a"), ("flags/colors.txt", "a"), (".git/config", "no")]);
        put(&b, &[("Common/Buildings/x.txt", "b"), ("events/b.txt", "b")]);
        put(&c, &[("common/ethics/mine.txt", "c"), ("prescripted_countries/p.txt", "c")]);
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mods = [input(&a, &[]), input(&b, &[]), input(&c, &["common/ethics"])];

        let merged = write(&user, "My Game!", &mod_name("My Game!", None), "v4.5.*", &mods).unwrap();
        assert_eq!(merged, Merged { id: "mod/drukhal_merged_my_game.mod".into(), files: 5 });
        let out = user.join("mod/drukhal_merged_my_game");
        let read = |p: &str| std::fs::read_to_string(out.join(p)).ok();
        assert_eq!(read("Common/Buildings/x.txt").as_deref(), Some("b"), "the last copy wins, spelled as it spells it");
        assert_eq!(read("common/ethics/e.txt"), None, "dropped by a later replace_path");
        assert_eq!(read("common/ethics/mine.txt").as_deref(), Some("c"));
        assert_eq!(read("flags/colors.txt").as_deref(), Some("a"));
        assert_eq!(read("prescripted_countries/p.txt").as_deref(), Some("c"));
        assert_eq!(read(".git/config"), None, "hidden folders aren't content");

        let descriptor = std::fs::read_to_string(user.join(&merged.id)).unwrap();
        let parsed = crate::mods::Descriptor::parse(&descriptor).unwrap();
        assert_eq!(parsed.name, "My Game! (Merged)");
        assert_eq!(parsed.replace_path, ["common/ethics"]);
        assert_eq!(parsed.supported_version, "v4.5.*");
        assert_eq!(parsed.path.as_deref(), Some(out.to_str().unwrap()));

        // Merging again replaces the earlier merge.
        let merged = write(&user, "My Game!", &mod_name("My Game!", None), "v4.5.*", &mods[..1]).unwrap();
        assert_eq!(merged.files, 3);
        assert_eq!(read("events/b.txt"), None);
        assert!(!user.join("mod/drukhal_merged_my_game.drukhal-tmp").exists());

        // A folder that isn't a merge is never replaced.
        std::fs::create_dir_all(user.join("mod/drukhal_merged_other")).unwrap();
        let err = write(&user, "Other", "Other", "*", &mods).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
