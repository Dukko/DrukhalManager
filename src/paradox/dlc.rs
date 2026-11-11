//! The game's DLCs, as listed in its `dlc/` folder. Turning one off is
//! done in `dlc_load.json`'s `disabled_dlcs`, by descriptor path.

use std::path::{Path, PathBuf};

use super::script::{self, Entries};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dlc {
    /// The descriptor's path relative to the game folder, e.g.
    /// `dlc/dlc002_arachnoid/dlc002.dlc`, which is how `disabled_dlcs`
    /// refers to it.
    pub id: String,
    pub name: String,
    pub thumbnail: Option<PathBuf>,
    /// Content packs (portraits, music…) change nothing other players see.
    pub category: String,
}

/// Every DLC in `<game_dir>/dlc`, in release order. Unreadable descriptors
/// are named after their folder.
pub fn installed(game_dir: &Path) -> Vec<Dlc> {
    let Ok(entries) = std::fs::read_dir(game_dir.join("dlc")) else { return Vec::new() };
    let mut dlcs: Vec<Dlc> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|dir| dir.is_dir())
        .filter_map(|dir| {
            let descriptor = std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).find(|p| p.extension().is_some_and(|e| e == "dlc"))?;
            let folder = dir.file_name()?.to_string_lossy().into_owned();
            let file = descriptor.file_name()?.to_string_lossy().into_owned();
            let entries = std::fs::read_to_string(&descriptor).ok().and_then(|src| script::parse(&src).ok()).unwrap_or_default();
            let entries = entries.as_slice();
            let name = entries.scalar("name").map(str::to_owned).unwrap_or_else(|| folder.clone());
            let thumbnail = entries.scalar("thumbnail").map(|t| dir.join(t)).filter(|t| t.is_file());
            let category = entries.scalar("category").unwrap_or_default().to_owned();
            Some(Dlc { id: format!("dlc/{folder}/{file}"), name, thumbnail, category })
        })
        .collect();
    // Folders are numbered by release: dlc002, dlc004…
    dlcs.sort_by(|a, b| a.id.cmp(&b.id));
    dlcs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_dlcs() {
        let game = std::env::temp_dir().join(format!("drukhal-dlc-{}", std::process::id()));
        let dir = game.join("dlc/dlc012_leviathans");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dlc012.dlc"), "name = \"Leviathans Story Pack\"\ncategory=\"dlc\"\nthumbnail = \"thumbnail.png\"\n").unwrap();
        std::fs::write(dir.join("thumbnail.png"), b"").unwrap();
        let other = game.join("dlc/dlc002_arachnoid");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("dlc002.dlc"), "not { valid").unwrap();
        let dlcs = installed(&game);
        assert_eq!(dlcs.len(), 2);
        assert_eq!((dlcs[0].id.as_str(), dlcs[0].name.as_str()), ("dlc/dlc002_arachnoid/dlc002.dlc", "dlc002_arachnoid"), "in release order; named after the folder when unreadable");
        assert_eq!(dlcs[1], Dlc { id: "dlc/dlc012_leviathans/dlc012.dlc".into(), name: "Leviathans Story Pack".into(), thumbnail: Some(dir.join("thumbnail.png")), category: "dlc".into() });
        std::fs::remove_dir_all(&game).unwrap();
        assert!(installed(&game).is_empty());
    }
}
