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
use std::io::{self, Read};
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

/// The merged mod's folder and descriptor name from a collection's slug,
/// e.g. `drukhal_merged_my_game` for "My Game".
fn folder(collection: &str) -> String {
    let slug: String = collection.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let slug = slug.trim_matches('_');
    format!("drukhal_merged_{}", if slug.is_empty() { "collection" } else { slug })
}

/// A short hash of the collection's name, to tell names that make the same
/// slug apart ("A B" and "a-b" both make `a_b`).
fn name_hash(collection: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(collection.trim().as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// The folder to merge `collection` into: its usual one unless another
/// collection's merge already holds that name, then one with a hash of the
/// name. Never picks another collection's folder.
fn folder_for(collection: &str, dir: &Path) -> String {
    let base = folder(collection);
    if !other_merges(dir, &base, collection) {
        return base;
    }
    let hash = name_hash(collection);
    let mut candidate = format!("{base}_{hash}");
    let mut n = 2;
    while other_merges(dir, &candidate, collection) {
        candidate = format!("{base}_{hash}_{n}");
        n += 1;
    }
    candidate
}

/// Whether the folder or zip `name` in `dir` is a merge of a collection
/// other than `collection`, which must not be replaced.
fn other_merges(dir: &Path, name: &str, collection: &str) -> bool {
    let marker = dir.join(name).join(MARKER);
    if marker.is_file() && !std::fs::read_to_string(&marker).is_ok_and(|s| s.trim() == collection.trim()) {
        return true;
    }
    merged_zip_name(&dir.join(format!("{name}.zip"))).is_some_and(|s| s.trim() != collection.trim())
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

/// Whether the merged mod is written as a folder or as one zip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Folder,
    Zip,
}

/// Writes `mods` (in load order) as one mod for `collection`, named `name`,
/// into `out_dir` (usually `<user_dir>/mod`), replacing an earlier merge of
/// the same collection in either form and in either place. Its descriptor
/// goes in `<user_dir>/mod`. Blocks; call off the main thread.
pub fn write(user_dir: &Path, out_dir: &Path, collection: &str, name: &str, supported_version: &str, mods: &[Input], output: Output) -> io::Result<Merged> {
    let mod_dir = out_dir.to_path_buf();
    let folder = folder_for(collection, &mod_dir);
    let target = mod_dir.join(&folder);
    let zip = mod_dir.join(format!("{folder}.zip"));
    if target.exists() && !target.join(MARKER).exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists and isn't a merged collection", target.display())));
    }
    if zip.exists() && merged_zip_name(&zip).is_none() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists and isn't a merged collection", zip.display())));
    }
    let (files, replace_paths) = plan(mods)?;
    let needed: u64 = files.values().filter_map(|(_, source)| std::fs::metadata(source).ok()).map(|m| m.len()).sum();
    if let Some(free) = free_space(&mod_dir)
        && free < needed + (64 << 20)
    {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            format!("the merge needs about {} but only {} is free", size(needed), size(free)),
        ));
    }

    let name = name.replace('"', "'");
    let mut inner = format!("name=\"{name}\"\nversion=\"1\"\ntags={{\n\t\"Merged\"\n}}\nsupported_version=\"{supported_version}\"\n");
    for r in &replace_paths {
        inner.push_str(&format!("replace_path=\"{r}\"\n"));
    }
    // Built next to the target and swapped in at the end, so a failed merge
    // leaves the previous one intact.
    let location = match output {
        Output::Folder => {
            write_folder(&mod_dir, &folder, collection, &files, &inner)?;
            format!("path=\"{}\"", crate::platform::descriptor_path(&target))
        }
        Output::Zip => {
            write_zip(&mod_dir, &folder, collection, &files, &inner)?;
            format!("archive=\"{}\"", crate::platform::descriptor_path(&zip))
        }
    };
    // The other form of an earlier merge would be left behind.
    match output {
        Output::Folder => remove_if_present(&zip)?,
        Output::Zip => {
            if target.exists() {
                std::fs::remove_dir_all(&target)?;
            }
        }
    }
    // An earlier merge of the same collection may be in another folder: a
    // name that used to map here, or one written before another collection
    // took this name. Remove it and its descriptor, so the mod list doesn't
    // keep a broken copy.
    remove_earlier_merges(user_dir, collection, &target, &zip);
    let descriptor = user_dir.join(format!("mod/{folder}.mod"));
    std::fs::write(&descriptor, format!("{inner}{location}\n"))?;
    Ok(Merged { id: format!("mod/{folder}.mod"), files: files.len() })
}

/// Removes every merge of `collection` in `user_dir` except `target` and
/// `zip`, with its descriptor.
fn remove_earlier_merges(user_dir: &Path, collection: &str, target: &Path, zip: &Path) {
    let Ok(entries) = std::fs::read_dir(user_dir.join("mod")) else { return };
    for entry in entries.flatten() {
        let descriptor = entry.path();
        if descriptor.extension().is_none_or(|e| !e.eq_ignore_ascii_case("mod")) || !descriptor.is_file() {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&descriptor) else { continue };
        let Ok(d) = crate::mods::Descriptor::parse(&src) else { continue };
        let Some(path) = d.path.as_deref().map(PathBuf::from) else { continue };
        if path == target || path == zip {
            continue;
        }
        let of_collection = if path.is_dir() {
            std::fs::read_to_string(path.join(MARKER)).is_ok_and(|s| s.trim() == collection.trim())
        } else if path.is_file() {
            merged_zip_name(&path).is_some_and(|s| s.trim() == collection.trim())
        } else {
            false
        };
        if !of_collection {
            continue;
        }
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
        let _ = std::fs::remove_file(&descriptor);
    }
}

fn write_folder(mod_dir: &Path, folder: &str, collection: &str, files: &Files, descriptor: &str) -> io::Result<()> {
    let target = mod_dir.join(folder);
    let building = mod_dir.join(format!("{folder}.drukhal-tmp"));
    if building.exists() {
        std::fs::remove_dir_all(&building)?;
    }
    let result = (|| {
        std::fs::create_dir_all(&building)?;
        std::fs::write(building.join(MARKER), collection)?;
        for (path, source) in files.values() {
            let dest = building.join(path);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::copy(source, &dest)?;
        }
        std::fs::write(building.join("descriptor.mod"), descriptor)?;
        // Rename the old merge aside first, then put the new one in place,
        // so a failure never leaves neither. The old folder is deleted once
        // the new one is safely in position.
        let old = mod_dir.join(format!("{folder}.drukhal-old"));
        let _ = std::fs::remove_dir_all(&old);
        let had_old = target.exists();
        if had_old {
            crate::platform::rename(&target, &old)?;
        }
        match crate::platform::rename(&building, &target) {
            Ok(()) => {
                if had_old {
                    let _ = std::fs::remove_dir_all(&old);
                }
                Ok(())
            }
            Err(e) => {
                if had_old {
                    let _ = crate::platform::rename(&old, &target);
                }
                Err(e)
            }
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&building);
    }
    result
}

fn write_zip(mod_dir: &Path, folder: &str, collection: &str, files: &Files, descriptor: &str) -> io::Result<()> {
    use zip::write::SimpleFileOptions;
    let target = mod_dir.join(format!("{folder}.zip"));
    let building = mod_dir.join(format!("{folder}.zip.drukhal-tmp"));
    let result = (|| {
        std::fs::create_dir_all(mod_dir)?;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&building)?);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(false);
        zip.start_file(MARKER, options).map_err(io::Error::other)?;
        io::Write::write_all(&mut zip, collection.as_bytes())?;
        for (path, source) in files.values() {
            zip.start_file(path.as_str(), options.large_file(std::fs::metadata(source)?.len() >= u32::MAX as u64)).map_err(io::Error::other)?;
            io::copy(&mut std::fs::File::open(source)?, &mut zip)?;
        }
        zip.start_file("descriptor.mod", options).map_err(io::Error::other)?;
        io::Write::write_all(&mut zip, descriptor.as_bytes())?;
        zip.finish().map_err(io::Error::other)?.sync_all()?;
        crate::platform::rename(&building, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&building);
    }
    result
}

/// The collection a zip at `path` is a merge of, if it is one.
fn merged_zip_name(path: &Path) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut marker = zip.by_name(MARKER).ok()?;
    let mut name = String::new();
    marker.read_to_string(&mut name).ok()?;
    Some(name)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Free bytes on the filesystem holding `dir`, if it can be told.
fn free_space(dir: &Path) -> Option<u64> {
    fs4::available_space(dir).ok()
}

/// `bytes` for people: "1.5 GB".
fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
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
    fn keeps_merges_of_names_with_the_same_slug_apart() {
        let root = std::env::temp_dir().join(format!("drukhal-collection-slug-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, user) = (root.join("a"), root.join("user"));
        put(&a, &[("descriptor.mod", "name=\"A\""), ("common/buildings/x.txt", "a")]);
        std::fs::create_dir_all(user.join("mod")).unwrap();
        let mods = [input(&a, &[])];
        let folder = |m: &Merged| m.id.strip_prefix("mod/").unwrap().strip_suffix(".mod").unwrap().to_owned();

        let first = write(&user, &user.join("mod"), "A B", "A B", "*", &mods, Output::Folder).unwrap();
        // "a-b" makes the same slug as "A B".
        let second = write(&user, &user.join("mod"), "a-b", "a-b", "*", &mods, Output::Folder).unwrap();
        assert_ne!(first.id, second.id, "the second merge gets a folder of its own");
        let (first_dir, second_dir) = (user.join("mod").join(folder(&first)), user.join("mod").join(folder(&second)));
        assert!(first_dir.join("descriptor.mod").is_file(), "the first merge isn't deleted");
        assert!(second_dir.join("descriptor.mod").is_file());
        assert_eq!(std::fs::read_to_string(second_dir.join(MARKER)).unwrap(), "a-b");

        // Merging "a-b" again replaces its own folder, and leaves the other.
        let again = write(&user, &user.join("mod"), "a-b", "a-b", "*", &mods, Output::Folder).unwrap();
        assert_eq!(again.id, second.id);
        assert!(first_dir.join("descriptor.mod").is_file());
        assert!(!user.join("mod").join(format!("{}.drukhal-old", folder(&second))).exists());
        std::fs::remove_dir_all(&root).unwrap();
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

        let merged = write(&user, &user.join("mod"), "My Game!", &mod_name("My Game!", None), "v4.5.*", &mods, Output::Folder).unwrap();
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
        assert_eq!(parsed.path, Some(crate::platform::descriptor_path(&out)));

        // Merging again replaces the earlier merge.
        let merged = write(&user, &user.join("mod"), "My Game!", &mod_name("My Game!", None), "v4.5.*", &mods[..1], Output::Folder).unwrap();
        assert_eq!(merged.files, 3);
        assert_eq!(read("events/b.txt"), None);
        assert!(!user.join("mod/drukhal_merged_my_game.drukhal-tmp").exists());

        // A folder that isn't a merge is never replaced.
        std::fs::create_dir_all(user.join("mod/drukhal_merged_other")).unwrap();
        let err = write(&user, &user.join("mod"), "Other", "Other", "*", &mods, Output::Folder).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);

        // As a zip, which replaces the folder, and is read like any zipped mod.
        let merged = write(&user, &user.join("mod"), "My Game!", &mod_name("My Game!", None), "v4.5.*", &mods, Output::Zip).unwrap();
        assert_eq!(merged.files, 5);
        assert!(!out.exists(), "the folder form is removed");
        let descriptor = crate::mods::Descriptor::parse(&std::fs::read_to_string(user.join(&merged.id)).unwrap()).unwrap();
        assert!(descriptor.is_archive);
        let zip = user.join("mod/drukhal_merged_my_game.zip");
        assert_eq!(descriptor.path, Some(crate::platform::descriptor_path(&zip)));
        let files = crate::mods::archive::files_in(&zip, &root.join("cache")).unwrap();
        assert_eq!(std::fs::read_to_string(files.join("Common/Buildings/x.txt")).unwrap(), "b");
        assert_eq!(std::fs::read_to_string(files.join("descriptor.mod")).unwrap().lines().next(), Some("name=\"My Game! (Merged)\""));
        // And back to a folder, which removes the zip.
        write(&user, &user.join("mod"), "My Game!", &mod_name("My Game!", None), "v4.5.*", &mods, Output::Folder).unwrap();
        assert!(out.exists() && !zip.exists());

        // A zip that isn't a merge is never replaced.
        // Merging into another folder removes the earlier merge.
        let elsewhere = root.join("elsewhere");
        let merged = write(&user, &elsewhere, "My Game!", &mod_name("My Game!", None), "v4.5.*", &mods, Output::Folder).unwrap();
        assert!(!out.exists() && elsewhere.join("drukhal_merged_my_game/descriptor.mod").exists());
        let descriptor = crate::mods::Descriptor::parse(&std::fs::read_to_string(user.join(&merged.id)).unwrap()).unwrap();
        assert_eq!(descriptor.path, Some(crate::platform::descriptor_path(&elsewhere.join("drukhal_merged_my_game"))));

        std::fs::write(user.join("mod/drukhal_merged_other2.zip"), "not ours").unwrap();
        assert!(write(&user, &user.join("mod"), "Other2", "Other2", "*", &mods, Output::Zip).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
