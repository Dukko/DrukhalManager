//! Registering Steam Workshop mods with the game, which the Paradox Launcher
//! otherwise does when it starts. Steam only downloads a mod's files; the
//! game finds mods through descriptors in `mod/` of the user folder, which
//! the launcher writes as `ugc_<id>.mod`: the mod's own `descriptor.mod`
//! plus where its files are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{archive, Descriptor};
use crate::paradox::steam;

#[derive(Debug, Default, PartialEq)]
pub struct Synced {
    /// Names of the mods given a descriptor.
    pub added: Vec<String>,
    /// Names of the mods whose descriptor was brought up to date.
    pub updated: Vec<String>,
    /// Items that couldn't be registered, and why.
    pub errors: Vec<String>,
}

/// Writes descriptors for installed Workshop items that have none, and
/// refreshes the launcher's descriptors whose mod changed its name, version
/// or other details. Local mods and descriptors that aren't the launcher's
/// are left alone, and nothing is removed.
pub fn sync(user_dir: &Path, workshop_dirs: &[PathBuf]) -> Synced {
    let mut synced = Synced::default();
    let mod_dir = user_dir.join("mod");
    if !user_dir.is_dir() {
        return synced;
    }

    // Workshop ids that already have a descriptor, under any file name.
    let mut known: HashMap<u64, PathBuf> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(&mod_dir) {
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mod"))
                && let Ok(bytes) = std::fs::read(&path)
                && let Ok(d) = Descriptor::parse(&String::from_utf8_lossy(&bytes))
                && let Some(id) = d.remote_file_id
            {
                // The launcher's own file wins over a copy with the same id.
                if is_launchers(&path, id) || !known.contains_key(&id) {
                    known.insert(id, path);
                }
            }
        }
    }

    for content_dir in workshop_dirs {
        let ids = steam::installed_items(content_dir).unwrap_or_else(|| folder_ids(content_dir));
        for id in ids {
            let item = content_dir.join(id.to_string());
            let Ok(raw) = std::fs::read(item.join("descriptor.mod")) else { continue };
            let raw = String::from_utf8_lossy(&raw).trim_start_matches('\u{feff}').to_owned();
            let Ok(fresh) = Descriptor::parse(&raw) else {
                synced.errors.push(format!("Workshop item {id} has an unreadable descriptor"));
                continue;
            };
            let name = if fresh.name.is_empty() { id.to_string() } else { fresh.name.clone() };
            match known.get(&id) {
                None => {
                    let path = mod_dir.join(format!("ugc_{id}.mod"));
                    match write(&path, &descriptor_text(&raw, &location(&item), id)) {
                        Ok(()) => synced.added.push(name),
                        Err(e) => synced.errors.push(format!("Could not register {name}: {e}")),
                    }
                }
                Some(path) if is_launchers(path, id) => {
                    let Ok(current) = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()) else { continue };
                    let Ok(old) = Descriptor::parse(&current) else { continue };
                    if same_details(&old, &fresh) {
                        continue;
                    }
                    // Keep where it points if that still works.
                    let keep = old.path.as_deref().filter(|p| Path::new(p).exists()).map(|p| if old.is_archive { Location::Archive(p.into()) } else { Location::Folder(p.into()) });
                    match write(path, &descriptor_text(&raw, &keep.unwrap_or_else(|| location(&item)), id)) {
                        Ok(()) => synced.updated.push(name),
                        Err(e) => synced.errors.push(format!("Could not update {name}: {e}")),
                    }
                }
                Some(_) => {}
            }
        }
    }
    synced
}

/// Whether `path` is the descriptor the launcher writes for item `id`.
fn is_launchers(path: &Path, id: u64) -> bool {
    path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(format!("ugc_{id}.mod").as_str()))
}

/// Item folders, for a library without a readable manifest.
fn folder_ids(content_dir: &Path) -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(content_dir) else { return Vec::new() };
    let mut ids: Vec<u64> = entries.flatten().filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
    ids.sort_unstable();
    ids
}

enum Location {
    Folder(PathBuf),
    Archive(PathBuf),
}

/// Where the game should read the item from: its folder, or for a zipped
/// mod (a zip and no folders of content) the zip.
fn location(item: &Path) -> Location {
    let has_folders = std::fs::read_dir(item).is_ok_and(|entries| entries.flatten().any(|e| e.path().is_dir()));
    match archive::find_zip(item) {
        Some(zip) if !has_folders => Location::Archive(zip),
        _ => Location::Folder(item.to_path_buf()),
    }
}

/// The mod's descriptor, with its location and Workshop id put last, as
/// the launcher writes it.
fn descriptor_text(raw: &str, location: &Location, id: u64) -> String {
    let mut out: String = raw
        .lines()
        .filter(|line| !["path", "archive", "remote_file_id"].iter().any(|key| assigns(line, key)))
        .map(|line| format!("{line}\n"))
        .collect();
    let (key, path) = match location {
        Location::Folder(p) => ("path", p),
        Location::Archive(p) => ("archive", p),
    };
    out.push_str(&format!("{key}=\"{}\"\nremote_file_id=\"{id}\"\n", crate::platform::descriptor_path(path)));
    out
}

/// Whether a top-level line sets `key`.
fn assigns(line: &str, key: &str) -> bool {
    !line.starts_with([' ', '\t']) && line.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// Whether what the user sees and the game reads, apart from the location,
/// is unchanged.
fn same_details(a: &Descriptor, b: &Descriptor) -> bool {
    (&a.name, &a.version, &a.supported_version, &a.tags, &a.picture, &a.dependencies, &a.replace_path, &a.user_dir)
        == (&b.name, &b.version, &b.supported_version, &b.tags, &b.picture, &b.dependencies, &b.replace_path, &b.user_dir)
}

/// Replaces `path` in one step, so the game never reads half a file.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("mod.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("drukhal-sync-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn item(content: &Path, id: u64, descriptor: &str) -> PathBuf {
        let dir = content.join(id.to_string());
        std::fs::create_dir_all(dir.join("common")).unwrap();
        std::fs::write(dir.join("descriptor.mod"), descriptor).unwrap();
        dir
    }

    #[test]
    fn registers_new_items_and_refreshes_changed_ones() {
        let root = temp("items");
        let user = root.join("user");
        let workshop = root.join("steamapps/workshop");
        let content = workshop.join("content/281990");
        std::fs::create_dir_all(user.join("mod")).unwrap();
        // 1: new. 2: registered and changed. 3: registered, unchanged.
        // 4: a local copy with the same id. 5: downloaded but not installed.
        let new = item(&content, 1, "\u{feff}name=\"New\"\ntags={\n\t\"Gameplay\"\n}\nversion=\"1.0\"\nremote_file_id=\"1\"\n");
        let changed = item(&content, 2, "name=\"Changed\"\nversion=\"2.0\"\n");
        item(&content, 3, "name=\"Same\"\nversion=\"1.0\"\n");
        item(&content, 4, "name=\"Copied\"\nversion=\"9\"\n");
        item(&content, 5, "name=\"Leftover\"\n");
        std::fs::write(workshop.join("appworkshop_281990.acf"), "\"AppWorkshop\"\n{\n\"WorkshopItemsInstalled\"\n{\n\"1\" { }\n\"2\" { }\n\"3\" { }\n\"4\" { }\n}\n}\n").unwrap();
        let old_path = user.join("old_location");
        std::fs::create_dir_all(&old_path).unwrap();
        std::fs::write(user.join("mod/ugc_2.mod"), format!("name=\"Changed\"\nversion=\"1.0\"\npath=\"{}\"\nremote_file_id=\"2\"\n", old_path.display())).unwrap();
        let same = format!("name=\"Same\"\nversion=\"1.0\"\npath=\"{}\"\nremote_file_id=\"3\"\n", content.join("3").display());
        std::fs::write(user.join("mod/ugc_3.mod"), &same).unwrap();
        std::fs::write(user.join("mod/my_copy.mod"), "name=\"My Copy\"\npath=\"mod/my_copy\"\nremote_file_id=\"4\"\n").unwrap();

        let synced = sync(&user, std::slice::from_ref(&content));
        assert_eq!(synced, Synced { added: vec!["New".into()], updated: vec!["Changed".into()], errors: vec![] });

        let written = std::fs::read_to_string(user.join("mod/ugc_1.mod")).unwrap();
        assert_eq!(written, format!("name=\"New\"\ntags={{\n\t\"Gameplay\"\n}}\nversion=\"1.0\"\npath=\"{}\"\nremote_file_id=\"1\"\n", crate::platform::descriptor_path(&new)));
        let d = Descriptor::parse(&written).unwrap();
        assert_eq!((d.remote_file_id, d.tags), (Some(1), vec!["Gameplay".to_owned()]));
        let updated = Descriptor::parse(&std::fs::read_to_string(user.join("mod/ugc_2.mod")).unwrap()).unwrap();
        assert_eq!((updated.version.as_str(), updated.path), ("2.0", Some(old_path.display().to_string())), "keeps a working path");
        assert_eq!(std::fs::read_to_string(user.join("mod/ugc_3.mod")).unwrap(), same, "unchanged files aren't rewritten");
        assert!(!user.join("mod/ugc_4.mod").exists(), "a copy with the same id counts as registered");
        assert!(!user.join("mod/ugc_5.mod").exists(), "only items Steam lists as installed");
        assert!(!changed.join("descriptor.mod.tmp").exists());

        // Nothing left to do the second time.
        assert_eq!(sync(&user, &[content]), Synced::default());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn points_zipped_mods_at_their_archive() {
        let root = temp("zip");
        let user = root.join("user");
        let content = root.join("content/281990");
        let dir = content.join("7");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(user.join("mod")).unwrap();
        std::fs::write(dir.join("descriptor.mod"), "name=\"Zipped\"\narchive=\"old.zip\"\n").unwrap();
        archive::write_zip(&dir.join("mod.zip"), &[("common/x.txt", "x = {}")]);
        sync(&user, &[content]);
        let d = Descriptor::parse(&std::fs::read_to_string(user.join("mod/ugc_7.mod")).unwrap()).unwrap();
        assert!(d.is_archive);
        assert_eq!(d.path, Some(dir.join("mod.zip").display().to_string()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
