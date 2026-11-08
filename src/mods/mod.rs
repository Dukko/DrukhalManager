//! Mod discovery and parsing of Paradox `.mod` descriptor files.

mod descriptor;

pub use descriptor::Descriptor;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
}

/// The launcher's `dlc_load.json`.
#[derive(Debug, Default, Deserialize)]
pub struct DlcLoad {
    #[serde(default)]
    pub enabled_mods: Vec<String>,
}

impl DlcLoad {
    pub fn read(user_dir: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(user_dir.join("dlc_load.json")) {
            Ok(src) => serde_json::from_str(&src).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

/// Reads every descriptor in `<user_dir>/mod`, resolves where its files live,
/// and annotates it with its position in `dlc_load.json`. Enabled mods come
/// first in load order, then the rest by name.
pub fn discover(user_dir: &Path, workshop_dirs: &[PathBuf]) -> io::Result<Vec<Mod>> {
    let load = DlcLoad::read(user_dir)?;
    let positions: HashMap<&str, usize> = load.enabled_mods.iter().enumerate().map(|(i, m)| (m.as_str(), i)).collect();

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
        let error = error.or_else(|| content.is_none().then(|| "Mod files not found".to_owned()));
        let source = if descriptor.remote_file_id.is_some() { Source::Workshop } else { Source::Local };
        let load_position = positions.get(id.as_str()).copied();
        mods.push(Mod { id, descriptor, content, source, load_position, error });
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
    workshop_dirs.iter().map(|dir| dir.join(&id)).find(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::write(user.join("dlc_load.json"), r#"{"enabled_mods":["mod/ugc_42.mod"],"disabled_dlcs":[]}"#).unwrap();

        let mods = discover(&user, &[workshop.clone()]).unwrap();
        let names: Vec<_> = mods.iter().map(|m| m.descriptor.name.as_str()).collect();
        assert_eq!(names, ["Moved", "Alpha Broken", "Zed Local"]);
        assert_eq!(mods[0].content.as_deref(), Some(workshop.join("42").as_path()));
        assert_eq!(mods[0].load_position, Some(0));
        assert_eq!(mods[1].error.as_deref(), Some("Mod files not found"));
        assert_eq!(mods[2].source, Source::Local);

        std::fs::remove_dir_all(root).unwrap();
    }
}
