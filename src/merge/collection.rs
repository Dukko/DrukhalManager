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
    let target = mod_dir.join(folder(collection));
    let zip = mod_dir.join(format!("{}.zip", folder(collection)));
    if target.exists() && !target.join(MARKER).exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists and isn't a merged collection", target.display())));
    }
    if zip.exists() && !is_merged_zip(&zip) {
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
            write_folder(&mod_dir, collection, &files, &inner)?;
            format!("path=\"{}\"", crate::platform::descriptor_path(&target))
        }
        Output::Zip => {
            write_zip(&mod_dir, collection, &files, &inner)?;
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
    // As would an earlier merge written to another folder.
    let descriptor = user_dir.join(id(collection));
    if let Some(old) = std::fs::read_to_string(&descriptor).ok().and_then(|s| crate::mods::Descriptor::parse(&s).ok()).and_then(|d| d.path).map(PathBuf::from)
        && old != target
        && old != zip
    {
        if old.join(MARKER).is_file() {
            let _ = std::fs::remove_dir_all(&old);
        } else if old.is_file() && is_merged_zip(&old) {
            let _ = std::fs::remove_file(&old);
        }
    }
    std::fs::write(&descriptor, format!("{inner}{location}\n"))?;
    Ok(Merged { id: id(collection), files: files.len() })
}

fn write_folder(mod_dir: &Path, collection: &str, files: &Files, descriptor: &str) -> io::Result<()> {
    let target = mod_dir.join(folder(collection));
    let building = mod_dir.join(format!("{}.drukhal-tmp", folder(collection)));
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
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        std::fs::rename(&building, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&building);
    }
    result
}

fn write_zip(mod_dir: &Path, collection: &str, files: &Files, descriptor: &str) -> io::Result<()> {
    use zip::write::SimpleFileOptions;
    let target = mod_dir.join(format!("{}.zip", folder(collection)));
    let building = mod_dir.join(format!("{}.zip.drukhal-tmp", folder(collection)));
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
        std::fs::rename(&building, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&building);
    }
    result
}

/// Whether the zip at `path` is a merged collection's.
fn is_merged_zip(path: &Path) -> bool {
    std::fs::File::open(path).ok().and_then(|f| zip::ZipArchive::new(f).ok()).is_some_and(|mut z| z.by_name(MARKER).is_ok())
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
