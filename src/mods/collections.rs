//! Collections: named load orders the user switches between, stored in the
//! app's data dir. Like Irony Mod Manager's collections
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager), they can be
//! exported to a file to share and imported from one, with mods matched by
//! Steam id so the file works on other machines.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Mod;

pub const DEFAULT: &str = "Default";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub name: String,
    /// Descriptor ids (`mod/ugc_1.mod`) in load order.
    pub mods: Vec<String>,
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
        Self { active: DEFAULT.into(), list: vec![Collection { name: DEFAULT.into(), mods }] }
    }

    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(src) => {
                let mut c: Self = serde_json::from_str(&src).map_err(io::Error::other)?;
                if c.list.is_empty() {
                    c.list.push(Collection { name: DEFAULT.into(), mods: Vec::new() });
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

    pub fn set_mods(&mut self, name: &str, mods: Vec<String>) {
        if let Some(c) = self.list.iter_mut().find(|c| c.name == name) {
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
        let name = self.unique_name(name);
        self.list.push(Collection { name: name.clone(), mods });
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

pub fn export(name: &str, ids: &[String], mods: &[Mod]) -> Shared {
    let shared = ids
        .iter()
        .map(|id| {
            let m = mods.iter().find(|m| m.id == *id);
            SharedMod { name: m.map_or_else(|| id.clone(), |m| m.descriptor.name.clone()), steam_id: m.and_then(|m| m.descriptor.remote_file_id), id: id.clone() }
        })
        .collect();
    Shared { format: 1, name: name.to_owned(), mods: shared }
}

/// Matches a shared collection's mods to installed ones: by Steam id first
/// (descriptor ids differ between machines), then descriptor id, then name.
/// Returns the matched ids in order and the names that weren't found.
pub fn resolve(shared: &Shared, mods: &[Mod]) -> (Vec<String>, Vec<String>) {
    let (mut found, mut missing) = (Vec::new(), Vec::new());
    for s in &shared.mods {
        match find(s, mods) {
            Some(m) if !found.contains(&m.id) => found.push(m.id.clone()),
            Some(_) => {}
            None => missing.push(s.name.clone()),
        }
    }
    (found, missing)
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

/// Reads a load order from pasted text: a collection file's JSON, or the
/// list `to_text` writes. Lines that aren't numbered are skipped, so a
/// list quoted with chat around it still reads.
pub fn from_text(text: &str) -> Option<Shared> {
    if let Ok(shared) = serde_json::from_str::<Shared>(text.trim()) {
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

#[cfg(test)]
mod tests {
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
        let shared = export("Mine", &["mod/ugc_1.mod".into(), "mod/local.mod".into(), "mod/gone.mod".into()], &here);
        let json = serde_json::to_string(&shared).unwrap();
        // A friend has the workshop mod under another descriptor name, the
        // local mod under another id, and lacks the third.
        let there = [m("mod/ugc_1_renamed.mod", "One (old name)", Some(1)), m("mod/my_local.mod", "local thing", None)];
        let (found, missing) = resolve(&serde_json::from_str(&json).unwrap(), &there);
        assert_eq!(found, ["mod/ugc_1_renamed.mod", "mod/my_local.mod"]);
        assert_eq!(missing, ["mod/gone.mod"]);
    }
}
