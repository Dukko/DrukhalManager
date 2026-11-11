//! Collections: named load orders the user switches between, stored in the
//! app's data dir. Like Irony Mod Manager's collections
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager), they can be
//! exported to a file to share and imported from one, with mods matched by
//! Steam id so the file works on other machines.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Mod;

pub const DEFAULT: &str = "Default";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub name: String,
    /// Descriptor ids (`mod/ugc_1.mod`) in load order, including mods that
    /// aren't installed, which keep their place until they're back.
    pub mods: Vec<String>,
    /// The mods' names, so ones that aren't installed can still be shown.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
}

/// A mod in a collection that isn't installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    pub id: String,
    pub name: String,
    /// The installed mod it loads after, if any.
    pub after: Option<String>,
    pub steam_id: Option<u64>,
}

impl Collection {
    pub fn new(name: &str, mods: Vec<String>) -> Self {
        Self { name: name.to_owned(), mods, names: BTreeMap::new() }
    }

    /// Makes `enabled`, the load order being saved, the collection's mods.
    /// Mods that aren't installed stay after the mod they followed, and
    /// every mod's name is remembered.
    pub fn update(&mut self, enabled: Vec<String>, installed: &[Mod]) {
        let known: HashSet<&str> = installed.iter().map(|m| m.id.as_str()).collect();
        let mut mods = enabled;
        for (at, id) in self.mods.iter().enumerate() {
            if known.contains(id.as_str()) || mods.contains(id) {
                continue;
            }
            // After the nearest earlier mod that's still in the list.
            let slot = self.mods[..at].iter().rev().find_map(|before| mods.iter().position(|m| m == before)).map_or(0, |p| p + 1);
            mods.insert(slot, id.clone());
        }
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        for id in &mods {
            let name = installed.iter().find(|m| &m.id == id).map(|m| m.descriptor.name.trim().to_owned()).filter(|n| !n.is_empty()).or_else(|| self.names.get(id).cloned());
            if let Some(name) = name {
                names.insert(id.clone(), name);
            }
        }
        self.mods = mods;
        self.names = names;
    }

    /// The collection's mods that aren't installed, in load order.
    pub fn placeholders(&self, installed: &[Mod]) -> Vec<Placeholder> {
        let mut after = None;
        let mut found = Vec::new();
        for id in &self.mods {
            if let Some(m) = installed.iter().find(|m| &m.id == id) {
                after = Some(if m.descriptor.name.trim().is_empty() { id.clone() } else { m.descriptor.name.trim().to_owned() });
                continue;
            }
            let steam_id = steam_id_of(id);
            let name = self.names.get(id).cloned().unwrap_or_else(|| steam_id.map_or_else(|| id.clone(), |sid| format!("Workshop mod {sid}")));
            found.push(Placeholder { id: id.clone(), name, after: after.clone(), steam_id });
        }
        found
    }

    /// The installed mods, in order, as the load order to apply.
    pub fn installed(&self, installed: &[Mod]) -> Vec<String> {
        let known: HashSet<&str> = installed.iter().map(|m| m.id.as_str()).collect();
        self.mods.iter().filter(|id| known.contains(id.as_str())).cloned().collect()
    }
}

/// The Workshop id in a descriptor id like `mod/ugc_123.mod`, as Steam and
/// the launcher name them.
pub fn steam_id_of(id: &str) -> Option<u64> {
    id.strip_prefix("mod/ugc_")?.strip_suffix(".mod")?.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collections {
    pub active: String,
    pub list: Vec<Collection>,
}

pub fn path() -> PathBuf {
    crate::sandbox::data_dir().join("DrukhalManager/collections.json")
}

impl Collections {
    /// A fresh set with one collection holding the current load order.
    pub fn starting_with(mods: Vec<String>) -> Self {
        Self { active: DEFAULT.into(), list: vec![Collection::new(DEFAULT, mods)] }
    }

    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(src) => {
                let mut c: Self = serde_json::from_str(&src).map_err(io::Error::other)?;
                if c.list.is_empty() {
                    c.list.push(Collection::new(DEFAULT, Vec::new()));
                }
                if c.get(&c.active.clone()).is_none() {
                    c.active = c.list[0].name.clone();
                }
                Ok(Some(c))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    pub fn get(&self, name: &str) -> Option<&Collection> {
        self.list.iter().find(|c| c.name == name)
    }

    pub fn active(&self) -> &Collection {
        self.get(&self.active).unwrap_or(&self.list[0])
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Collection> {
        self.list.iter_mut().find(|c| c.name == name)
    }

    pub fn set_mods(&mut self, name: &str, mods: Vec<String>) {
        if let Some(c) = self.get_mut(name) {
            c.mods = mods;
        }
    }

    /// `base`, or `base 2`, `base 3`… if taken.
    pub fn unique_name(&self, base: &str) -> String {
        let base = base.trim();
        let base = if base.is_empty() { "Collection" } else { base };
        if self.get(base).is_none() {
            return base.to_owned();
        }
        (2..).map(|n| format!("{base} {n}")).find(|n| self.get(n).is_none()).unwrap()
    }

    /// Adds a collection under a unique name and returns that name.
    pub fn add(&mut self, name: &str, mods: Vec<String>) -> String {
        self.add_collection(Collection::new(name, mods))
    }

    /// Adds `c` under a unique name based on its own, and returns that name.
    pub fn add_collection(&mut self, mut c: Collection) -> String {
        c.name = self.unique_name(&c.name);
        let name = c.name.clone();
        self.list.push(c);
        name
    }

    /// Renames `from`; returns the name actually used.
    pub fn rename(&mut self, from: &str, to: &str) -> Option<String> {
        let to = if to.trim() == from { from.to_owned() } else { self.unique_name(to) };
        let c = self.list.iter_mut().find(|c| c.name == from)?;
        c.name = to.clone();
        if self.active == from {
            self.active = to.clone();
        }
        Some(to)
    }

    /// Removes `name` unless it's the last collection.
    pub fn remove(&mut self, name: &str) -> bool {
        if self.list.len() <= 1 {
            return false;
        }
        self.list.retain(|c| c.name != name);
        if self.active == name {
            self.active = self.list[0].name.clone();
        }
        true
    }
}

/// The launcher playset a collection syncs to. The default collection keeps
/// the playset name DrukhalManager used before collections existed.
pub fn playset_name(collection: &str) -> String {
    if collection == DEFAULT { "DrukhalManager".to_owned() } else { format!("DrukhalManager – {collection}") }
}

/// A collection as shared in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shared {
    pub format: u32,
    pub name: String,
    pub mods: Vec<SharedMod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedMod {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_id: Option<u64>,
    /// The descriptor id on the machine it was exported from.
    pub id: String,
}

/// The mods with `ids`, in order, to share; mods that aren't installed are
/// named from `names`.
pub fn export(name: &str, ids: &[String], mods: &[Mod], names: &BTreeMap<String, String>) -> Shared {
    let shared = ids
        .iter()
        .map(|id| {
            let m = mods.iter().find(|m| m.id == *id);
            let name = m.map(|m| m.descriptor.name.clone()).or_else(|| names.get(id).cloned()).unwrap_or_else(|| id.clone());
            SharedMod { name, steam_id: m.and_then(|m| m.descriptor.remote_file_id).or_else(|| steam_id_of(id)), id: id.clone() }
        })
        .collect();
    Shared { format: 1, name: name.to_owned(), mods: shared }
}

/// A shared collection as a collection: mods matched to installed ones (by
/// Steam id first, as descriptor ids differ between machines, then
/// descriptor id, then name) by their ids here, and
/// the rest kept as placeholders under the id Steam would give them.
/// Returns it and the names of the mods that aren't installed; mods with
/// neither a Steam id nor a descriptor id can't be kept.
pub fn import(shared: &Shared, mods: &[Mod]) -> (Collection, Vec<String>) {
    let mut c = Collection::new(&shared.name, Vec::new());
    let mut missing = Vec::new();
    for s in &shared.mods {
        let id = match find(s, mods) {
            Some(m) => m.id.clone(),
            None => {
                missing.push(s.name.clone());
                match (s.steam_id, s.id.is_empty()) {
                    (Some(sid), _) => format!("mod/ugc_{sid}.mod"),
                    (None, false) => s.id.clone(),
                    (None, true) => continue,
                }
            }
        };
        if !c.mods.contains(&id) {
            c.names.insert(id.clone(), s.name.clone());
            c.mods.push(id);
        }
    }
    (c, missing)
}

/// The installed mod a shared one stands for, as `resolve` matches them.
pub fn find<'a>(s: &SharedMod, mods: &'a [Mod]) -> Option<&'a Mod> {
    s.steam_id
        .and_then(|sid| mods.iter().find(|m| m.descriptor.remote_file_id == Some(sid)))
        .or_else(|| mods.iter().find(|m| m.id == s.id))
        .or_else(|| mods.iter().find(|m| m.descriptor.name.eq_ignore_ascii_case(&s.name)))
}

/// The first line of a copied load order, before its name.
const TEXT_HEADER: &str = "Stellaris load order";

/// A shared collection as text to paste in a chat or forum post: one
/// numbered line per mod, with its Workshop page or, for a local mod, its
/// descriptor id. `from_text` reads it back.
pub fn to_text(shared: &Shared) -> String {
    let mut text = format!("{TEXT_HEADER} “{}” ({} mods)\n", shared.name, shared.mods.len());
    for (i, m) in shared.mods.iter().enumerate() {
        let name = m.name.trim();
        match m.steam_id {
            Some(id) => text.push_str(&format!("{}. {name} — {}\n", i + 1, crate::paradox::workshop::page_url(id))),
            None => text.push_str(&format!("{}. {name} ({})\n", i + 1, m.id)),
        }
    }
    text
}

/// Reads a load order from pasted text: a collection file's JSON, a
/// launcher playset's, or the list `to_text` writes. Lines that aren't
/// numbered are skipped, so a list quoted with chat around it still reads.
pub fn from_text(text: &str) -> Option<Shared> {
    if let Ok(shared) = serde_json::from_str::<Shared>(text.trim()) {
        return Some(shared);
    }
    if let Some(shared) = from_launcher_json(text) {
        return Some(shared);
    }
    let mut name = "Pasted".to_owned();
    let mut mods = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix(TEXT_HEADER) {
            if let Some(n) = rest.split('“').nth(1).and_then(|r| r.split('”').next()) {
                name = n.to_owned();
            }
            continue;
        }
        let Some((number, rest)) = line.split_once(". ") else { continue };
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let rest = rest.trim();
        let mod_ = if let Some((title, url)) = rest.rsplit_once(" — ")
            && let Some(id) = url.split("id=").nth(1).and_then(|id| id.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
        {
            SharedMod { name: title.trim().to_owned(), steam_id: Some(id), id: String::new() }
        } else if let Some(open) = rest.rfind(" (mod/")
            && rest.ends_with(".mod)")
        {
            SharedMod { name: rest[..open].trim().to_owned(), steam_id: None, id: rest[open + 2..rest.len() - 1].to_owned() }
        } else {
            SharedMod { name: rest.to_owned(), steam_id: None, id: String::new() }
        };
        mods.push(mod_);
    }
    (!mods.is_empty()).then_some(Shared { format: 1, name, mods })
}

/// A Paradox Launcher playset file, as the launcher's Export Playset writes
/// it (and Irony reads and writes): Workshop mods by Steam id, with their
/// position in the load order.
#[derive(Debug, Serialize, Deserialize)]
struct LauncherPlayset {
    #[serde(default = "stellaris")]
    game: String,
    #[serde(default)]
    name: String,
    mods: Vec<LauncherMod>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LauncherMod {
    #[serde(default)]
    display_name: String,
    #[serde(default = "yes")]
    enabled: bool,
    /// A number since the launcher's 2021.10 release; before it, a string
    /// such as `"4097"`, also read here.
    #[serde(default)]
    position: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "id_string")]
    steam_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "id_string")]
    pdx_id: Option<String>,
}

fn stellaris() -> String {
    "stellaris".into()
}

fn yes() -> bool {
    true
}

/// An id written as a string or a number.
fn id_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) if !s.is_empty() => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Reads a Paradox Launcher playset file, keeping its enabled mods in
/// their positions.
pub fn from_launcher_json(text: &str) -> Option<Shared> {
    let playset: LauncherPlayset = serde_json::from_str(text.trim()).ok()?;
    if !playset.game.eq_ignore_ascii_case("stellaris") {
        return None;
    }
    let mut mods: Vec<(u64, LauncherMod)> = playset
        .mods
        .into_iter()
        .filter(|m| m.enabled)
        .enumerate()
        .map(|(i, m)| {
            let position = match &m.position {
                serde_json::Value::Number(n) => n.as_u64(),
                serde_json::Value::String(s) => s.trim().parse().ok(),
                _ => None,
            };
            (position.unwrap_or(i as u64), m)
        })
        .collect();
    // Stable, so mods without a position keep the file's order.
    mods.sort_by_key(|(position, _)| *position);
    let mods: Vec<SharedMod> = mods
        .into_iter()
        .filter_map(|(_, m)| {
            let steam_id = m.steam_id.as_deref().and_then(|s| s.trim().parse().ok());
            (steam_id.is_some() || !m.display_name.is_empty()).then(|| SharedMod { name: m.display_name, steam_id, id: String::new() })
        })
        .collect();
    let name = if playset.name.trim().is_empty() { "Launcher Playset".to_owned() } else { playset.name.trim().to_owned() };
    Some(Shared { format: 1, name, mods })
}

/// A shared collection as a Paradox Launcher playset file. The launcher
/// only knows Workshop mods, so local ones are left out; their names are
/// returned.
pub fn to_launcher_json(shared: &Shared) -> (String, Vec<String>) {
    let (workshop, local): (Vec<&SharedMod>, Vec<&SharedMod>) = shared.mods.iter().partition(|m| m.steam_id.is_some());
    let mods = workshop
        .into_iter()
        .enumerate()
        .map(|(position, m)| LauncherMod {
            display_name: m.name.trim().to_owned(),
            enabled: true,
            position: position.into(),
            steam_id: m.steam_id.map(|id| id.to_string()),
            pdx_id: None,
        })
        .collect();
    let playset = LauncherPlayset { game: stellaris(), name: shared.name.clone(), mods };
    (serde_json::to_string_pretty(&playset).expect("plain data"), local.into_iter().map(|m| m.name.clone()).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_and_writes_launcher_playsets() {
        let new = r#"{"game":"stellaris","name":"Main","mods":[
            {"displayName":"B","enabled":true,"position":1,"steamId":"222"},
            {"displayName":"Off","enabled":false,"position":0,"steamId":"333"},
            {"displayName":"A","enabled":true,"position":0,"steamId":"111"}]}"#;
        let shared = from_launcher_json(new).unwrap();
        assert_eq!(shared.name, "Main");
        assert_eq!(shared.mods.iter().map(|m| (m.name.as_str(), m.steam_id)).collect::<Vec<_>>(), [("A", Some(111)), ("B", Some(222))]);

        let old = r#"{"game":"stellaris","name":"Old","mods":[{"displayName":"X","enabled":true,"position":"4098","steamId":444},{"displayName":"Y","position":"4097","steamId":"555"}]}"#;
        let shared = from_launcher_json(old).unwrap();
        assert_eq!(shared.mods.iter().map(|m| m.steam_id).collect::<Vec<_>>(), [Some(555), Some(444)]);
        assert!(from_launcher_json(r#"{"game":"hoi4","mods":[]}"#).is_none());
        assert!(from_launcher_json(r#"{"format":1,"name":"x","mods":[]}"#).is_none_or(|s| s.mods.is_empty()));

        let mut shared = from_launcher_json(new).unwrap();
        shared.mods.insert(1, SharedMod { name: "Local".into(), steam_id: None, id: "mod/local.mod".into() });
        let (json, skipped) = to_launcher_json(&shared);
        assert_eq!(skipped, ["Local"]);
        let back = from_launcher_json(&json).unwrap();
        assert_eq!(back.mods.iter().map(|m| m.steam_id).collect::<Vec<_>>(), [Some(111), Some(222)]);
        assert!(json.contains("\"steamId\": \"111\"") && json.contains("\"position\": 1"));
    }

    #[test]
    fn copies_as_text_and_back() {
        let shared = Shared {
            format: 1,
            name: "Main".into(),
            mods: vec![
                SharedMod { name: "UI Overhaul Dynamic".into(), steam_id: Some(1623423360), id: "mod/ugc_1623423360.mod".into() },
                SharedMod { name: "My Patch (v2)".into(), steam_id: None, id: "mod/my_patch.mod".into() },
            ],
        };
        let text = to_text(&shared);
        assert_eq!(
            text,
            "Stellaris load order “Main” (2 mods)\n1. UI Overhaul Dynamic — https://steamcommunity.com/sharedfiles/filedetails/?id=1623423360\n2. My Patch (v2) (mod/my_patch.mod)\n"
        );
        let back = from_text(&format!("here's mine:\n{text}\nhave fun")).unwrap();
        assert_eq!(back.name, "Main");
        assert_eq!(back.mods[0].steam_id, Some(1623423360));
        assert_eq!((back.mods[1].name.as_str(), back.mods[1].id.as_str()), ("My Patch (v2)", "mod/my_patch.mod"));
        assert_eq!(from_text(&serde_json::to_string(&shared).unwrap()), Some(shared), "JSON exports paste too");
        let plain = from_text("1. Gigastructural Engineering\n2. ACOT").unwrap();
        assert_eq!((plain.name.as_str(), plain.mods[1].name.as_str()), ("Pasted", "ACOT"), "names alone match by name");
        assert_eq!(from_text("hello"), None);
    }

    use super::*;
    use crate::mods::{Descriptor, Source};

    fn m(id: &str, name: &str, steam: Option<u64>) -> Mod {
        Mod { id: id.into(), descriptor: Descriptor { name: name.into(), remote_file_id: steam, ..Default::default() }, content: None, source: Source::Workshop, load_position: None, error: None, steam: None }
    }

    #[test]
    fn manages_collections() {
        let mut c = Collections::starting_with(vec!["mod/a.mod".into()]);
        assert_eq!(c.add("Ironman", vec![]), "Ironman");
        assert_eq!(c.add("Ironman", vec![]), "Ironman 2");
        assert_eq!(c.rename("Ironman 2", "Portraits").as_deref(), Some("Portraits"));
        assert_eq!(c.rename("Portraits", "Default").as_deref(), Some("Default 2"), "names stay unique");
        c.active = "Default 2".into();
        assert!(c.remove("Default 2"));
        assert_eq!(c.active, "Default");
        assert!(c.remove("Ironman") && !c.remove("Default"), "the last collection stays");
        assert_eq!(playset_name("Default"), "DrukhalManager");
        assert_eq!(playset_name("Ironman"), "DrukhalManager – Ironman");

        let path = std::env::temp_dir().join(format!("drukhal-collections-{}/c.json", std::process::id()));
        assert_eq!(Collections::load(&path).unwrap(), None);
        c.save(&path).unwrap();
        assert_eq!(Collections::load(&path).unwrap().unwrap(), c);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn shares_between_machines() {
        let here = [m("mod/ugc_1.mod", "One", Some(1)), m("mod/local.mod", "Local Thing", None)];
        let shared = export("Mine", &["mod/ugc_1.mod".into(), "mod/local.mod".into(), "mod/gone.mod".into()], &here, &BTreeMap::new());
        let json = serde_json::to_string(&shared).unwrap();
        // A friend has the workshop mod under another descriptor name, the
        // local mod under another id, and lacks the third.
        let there = [m("mod/ugc_1_renamed.mod", "One (old name)", Some(1)), m("mod/my_local.mod", "local thing", None)];
        let (c, missing) = import(&serde_json::from_str(&json).unwrap(), &there);
        assert_eq!(c.mods, ["mod/ugc_1_renamed.mod", "mod/my_local.mod", "mod/gone.mod"], "kept as a placeholder");
        assert_eq!(missing, ["mod/gone.mod"]);
    }

    #[test]
    fn keeps_missing_mods_in_place() {
        let mut c = Collection::new("Main", ["mod/a.mod", "mod/ugc_9.mod", "mod/b.mod", "mod/c.mod"].map(String::from).to_vec());
        c.names.insert("mod/ugc_9.mod".into(), "Nine".into());
        let installed = [m("mod/a.mod", "A", None), m("mod/b.mod", "B", None), m("mod/c.mod", "C", None)];
        assert_eq!(c.installed(&installed), ["mod/a.mod", "mod/b.mod", "mod/c.mod"]);
        assert_eq!(
            c.placeholders(&installed),
            [Placeholder { id: "mod/ugc_9.mod".into(), name: "Nine".into(), after: Some("A".into()), steam_id: Some(9) }]
        );
        // A moves below B and C is disabled: Nine stays right after A.
        c.update(["mod/b.mod", "mod/a.mod"].map(String::from).to_vec(), &installed);
        assert_eq!(c.mods, ["mod/b.mod", "mod/a.mod", "mod/ugc_9.mod"]);
        assert_eq!(c.names.get("mod/a.mod").map(String::as_str), Some("A"));
        assert_eq!(c.names.get("mod/ugc_9.mod").map(String::as_str), Some("Nine"), "kept for the missing mod");
        assert!(!c.names.contains_key("mod/c.mod"), "dropped with the mod");
        assert_eq!(c.placeholders(&installed)[0].after.as_deref(), Some("A"));
        // Removed from the front: it stays first.
        let mut c = Collection::new("Main", ["mod/ugc_9.mod", "mod/a.mod"].map(String::from).to_vec());
        c.update(vec!["mod/a.mod".into()], &installed);
        assert_eq!(c.mods, ["mod/ugc_9.mod", "mod/a.mod"]);
        assert_eq!(c.placeholders(&installed)[0].name, "Workshop mod 9");
    }
}
