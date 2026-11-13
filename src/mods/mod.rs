//! Mod discovery and parsing of Paradox `.mod` descriptor files.

pub mod archive;
pub mod checks;
pub mod collections;
pub mod crash_hunt;
pub mod extra;
pub mod filter;
pub mod freeze;
mod descriptor;
pub mod repair;
pub mod game_update;
pub mod patches;
pub mod requirements;
pub mod saves;
pub mod smart_sort;
pub mod sync;
pub mod load_order;

pub use descriptor::Descriptor;
use load_order::DlcLoad;

use crate::paradox::steam::{self, ItemState};

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Workshop,
    Local,
}

#[derive(Debug, Clone)]
pub struct Mod {
    /// Descriptor path relative to the user dir, e.g. `mod/ugc_123.mod`,
    /// which is how `dlc_load.json` refers to it.
    pub id: String,
    pub descriptor: Descriptor,
    /// Where the mod's files actually are, if they could be found.
    pub content: Option<PathBuf>,
    pub source: Source,
    /// Position in the load order, if enabled.
    pub load_position: Option<usize>,
    pub error: Option<String>,
    /// What Steam says about a Workshop mod; `None` for local mods, or
    /// when Steam's manifest couldn't be read.
    pub steam: Option<ItemState>,
    /// The Workshop items its Workshop page says it requires, once fetched
    /// (see `requirements`).
    pub requires: Vec<Required>,
}

/// A Workshop item a mod requires.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Required {
    pub steam_id: u64,
    pub name: String,
}

impl Mod {
    /// When the mod's files last changed, which for a Workshop mod is when
    /// Steam last updated it.
    pub fn updated(&self) -> Option<std::time::SystemTime> {
        let content = self.content.as_ref()?;
        [content.clone(), content.join("descriptor.mod")].iter().filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).max()
    }
}

/// Reads every descriptor in `<user_dir>/mod`, resolves where its files live,
/// and annotates it with its position in `dlc_load.json`. Enabled mods come
/// first in load order, then the rest by name.
pub fn discover(user_dir: &Path, workshop_dirs: &[PathBuf]) -> io::Result<Vec<Mod>> {
    let load = DlcLoad::read(user_dir)?;
    let positions: HashMap<&str, usize> = load.enabled_mods.iter().enumerate().map(|(i, m)| (m.as_str(), i)).collect();

    let states = steam::item_states(workshop_dirs);
    let mut mods = Vec::new();
    for entry in std::fs::read_dir(user_dir.join("mod"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("mod")) || !path.is_file() {
            continue;
        }
        let id = format!("mod/{}", path.file_name().unwrap().to_string_lossy());
        let (descriptor, error) = match std::fs::read(&path) {
            Ok(bytes) => match Descriptor::parse(&String::from_utf8_lossy(&bytes)) {
                Ok(d) => (d, None),
                Err(e) => (Descriptor::default(), Some(format!("Invalid descriptor: {e}"))),
            },
            Err(e) => (Descriptor::default(), Some(format!("Unreadable descriptor: {e}"))),
        };
        let content = resolve_content(&descriptor, user_dir, workshop_dirs);
        let steam = descriptor.remote_file_id.zip(states.as_ref()).map(|(id, states)| states.get(&id).copied().unwrap_or(ItemState::Unsubscribed));
        let error = error.or_else(|| {
            content.is_none().then(|| match steam {
                Some(ItemState::Unsubscribed) => "Unsubscribed in Steam, so its files are gone".to_owned(),
                Some(ItemState::Downloading) => "Steam hasn't downloaded it yet".to_owned(),
                _ => "Mod files not found".to_owned(),
            })
        });
        let source = if descriptor.remote_file_id.is_some() { Source::Workshop } else { Source::Local };
        let load_position = positions.get(id.as_str()).copied();
        mods.push(Mod { id, descriptor, content, source, load_position, error, steam, requires: Vec::new() });
    }

    // Keep enabled entries whose descriptor is gone, so saving doesn't
    // silently drop them and the user can see why they won't load.
    let known: std::collections::HashSet<String> = mods.iter().map(|m| m.id.clone()).collect();
    for (position, id) in load.enabled_mods.iter().enumerate() {
        if !known.contains(id) {
            let descriptor = Descriptor { name: id.clone(), ..Descriptor::default() };
            let error = Some("Descriptor file not found".to_owned());
            mods.push(Mod { id: id.clone(), descriptor, content: None, source: Source::Local, load_position: Some(position), error, steam: None, requires: Vec::new() });
        }
    }

    mods.sort_by(|a, b| match (a.load_position, b.load_position) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.descriptor.name.to_lowercase().cmp(&b.descriptor.name.to_lowercase()),
    });
    Ok(mods)
}

/// Finds a mod's files. Descriptors often hold stale absolute paths (e.g.
/// after moving a Steam library), so workshop mods fall back to looking up
/// `remote_file_id` in the known workshop directories.
fn resolve_content(d: &Descriptor, user_dir: &Path, workshop_dirs: &[PathBuf]) -> Option<PathBuf> {
    if let Some(path) = d.path.as_deref().filter(|p| !p.is_empty()) {
        let path = Path::new(path);
        let path = if path.is_absolute() { path.to_path_buf() } else { user_dir.join(path) };
        if path.exists() {
            return Some(path);
        }
    }
    let id = d.remote_file_id?.to_string();
    let dir = workshop_dirs.iter().map(|dir| dir.join(&id)).find(|p| p.is_dir())?;
    // A zipped Workshop mod is the zip inside its item folder.
    if d.is_archive { archive::find_zip(&dir) } else { Some(dir) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_steam_is_doing_with_workshop_mods() {
        let root = std::env::temp_dir().join(format!("drukhal-steam-mods-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, workshop) = (root.join("user"), root.join("steamapps/workshop"));
        let content = workshop.join("content/281990");
        std::fs::create_dir_all(user.join("mod")).unwrap();
        std::fs::create_dir_all(content.join("1/common")).unwrap();
        for (id, name) in [(1, "Updating"), (2, "Gone"), (3, "Coming")] {
            std::fs::write(user.join(format!("mod/ugc_{id}.mod")), format!("name=\"{name}\"\npath=\"{}\"\nremote_file_id=\"{id}\"\n", content.join(id.to_string()).display())).unwrap();
        }
        std::fs::write(user.join("mod/local.mod"), "name=\"Local\"\npath=\"mod/local\"\n").unwrap();
        let acf = r#""AppWorkshop" { "WorkshopItemsInstalled" { "1" { "manifest" "a" } }
            "WorkshopItemDetails" { "1" { "manifest" "a" "latest_manifest" "b" } "3" { "manifest" "0" "latest_manifest" "c" } } }"#;
        std::fs::write(workshop.join("appworkshop_281990.acf"), acf).unwrap();

        let mods = discover(&user, std::slice::from_ref(&content)).unwrap();
        let get = |name: &str| mods.iter().find(|m| m.descriptor.name == name).unwrap();
        assert_eq!((get("Updating").steam, get("Updating").error.as_deref()), (Some(ItemState::UpdatePending), None));
        assert_eq!((get("Gone").steam, get("Gone").error.as_deref()), (Some(ItemState::Unsubscribed), Some("Unsubscribed in Steam, so its files are gone")));
        assert_eq!(get("Coming").error.as_deref(), Some("Steam hasn't downloaded it yet"));
        assert_eq!(get("Local").steam, None);

        std::fs::remove_file(workshop.join("appworkshop_281990.acf")).unwrap();
        let mods = discover(&user, &[content]).unwrap();
        assert!(mods.iter().all(|m| m.steam.is_none()), "without a manifest nothing is claimed");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_and_orders_mods() {
        let root = std::env::temp_dir().join(format!("drukhal-test-{}", std::process::id()));
        let user = root.join("user");
        let workshop = root.join("workshop");
        std::fs::create_dir_all(user.join("mod/local")).unwrap();
        std::fs::create_dir_all(workshop.join("42")).unwrap();

        std::fs::write(user.join("mod/local.mod"), "name=\"Zed Local\" path=\"mod/local\"").unwrap();
        std::fs::write(user.join("mod/ugc_42.mod"), "name=\"Moved\" path=\"/gone/42\" remote_file_id=\"42\"").unwrap();
        std::fs::write(user.join("mod/broken.mod"), "name=\"Alpha Broken\" path=\"/nowhere\"").unwrap();
        std::fs::write(user.join("dlc_load.json"), r#"{"enabled_mods":["mod/ugc_42.mod","mod/deleted.mod"],"disabled_dlcs":[]}"#).unwrap();

        let mods = discover(&user, &[workshop.clone()]).unwrap();
        let names: Vec<_> = mods.iter().map(|m| m.descriptor.name.as_str()).collect();
        assert_eq!(names, ["Moved", "mod/deleted.mod", "Alpha Broken", "Zed Local"]);
        assert_eq!(mods[0].content.as_deref(), Some(workshop.join("42").as_path()));
        assert_eq!(mods[0].load_position, Some(0));
        assert_eq!(mods[1].error.as_deref(), Some("Descriptor file not found"));
        assert_eq!(mods[2].error.as_deref(), Some("Mod files not found"));
        assert_eq!(mods[3].source, Source::Local);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_zipped_mods_and_scans_them_for_conflicts() {
        use crate::merge::conflicts::{self, ModInput};
        let root = std::env::temp_dir().join(format!("drukhal-zip-test-{}", std::process::id()));
        let (user, workshop) = (root.join("user"), root.join("workshop"));
        std::fs::create_dir_all(user.join("mod/folder/common/buildings")).unwrap();
        std::fs::create_dir_all(workshop.join("77")).unwrap();
        std::fs::write(user.join("mod/folder/common/buildings/a.txt"), "building_x = {}").unwrap();
        archive::write_zip(&user.join("mod/local.zip"), &[("common/buildings/b.txt", "building_x = {}\nbuilding_y = {}")]);
        archive::write_zip(&workshop.join("77/item.zip"), &[("common/buildings/b.txt", "building_y = {}"), ("common/buildings/c.txt", "building_x = {}")]);

        std::fs::write(user.join("mod/folder.mod"), "name=\"Folder\" path=\"mod/folder\"").unwrap();
        std::fs::write(user.join("mod/local.mod"), "name=\"Local Zip\" archive=\"mod/local.zip\"").unwrap();
        // Stale archive path: found through the Workshop item folder.
        std::fs::write(user.join("mod/ugc_77.mod"), "name=\"Workshop Zip\" archive=\"/gone/item.zip\" remote_file_id=\"77\"").unwrap();
        std::fs::write(user.join("dlc_load.json"), r#"{"enabled_mods":["mod/folder.mod","mod/local.mod","mod/ugc_77.mod"],"disabled_dlcs":[]}"#).unwrap();

        let mods = discover(&user, std::slice::from_ref(&workshop)).unwrap();
        assert_eq!(mods[1].content.as_deref(), Some(user.join("mod/local.zip").as_path()));
        assert_eq!(mods[2].content.as_deref(), Some(workshop.join("77/item.zip").as_path()));

        let cache = root.join("cache");
        let inputs: Vec<ModInput> = mods
            .iter()
            .map(|m| ModInput { root: Some(archive::files_in(m.content.as_ref().unwrap(), &cache).unwrap()), replace_path: vec![] })
            .collect();
        let report = conflicts::analyze(&inputs, None);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        // Both zips ship common/buildings/b.txt; the Workshop one wins.
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].mods, [1, 2]);
        // building_x is defined by the folder mod and inside the Workshop
        // zip (the local zip's copy is in the b.txt that doesn't load).
        assert_eq!(report.objects.len(), 1);
        assert_eq!(report.objects[0].key, "building_x");
        let defined_by: Vec<_> = report.objects[0].definitions.iter().map(|d| (d.mod_position, d.file.as_str())).collect();
        assert_eq!(defined_by, [(0, "common/buildings/a.txt"), (2, "common/buildings/c.txt")]);

        std::fs::remove_dir_all(root).unwrap();
    }
}
