//! Locating Steam libraries and Stellaris workshop content on Linux.
//!
//! The lookup order (native Steam, then Flatpak, then every library listed in
//! `libraryfolders.vdf`) follows Irony Mod Manager's `SteamDirectory`
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const STELLARIS_APP_ID: u32 = 281990;

const ROOTS: &[&str] = &[
    ".local/share/Steam",
    ".steam/steam",
    ".steam/root",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    ".var/app/com.valvesoftware.Steam/data/Steam",
];

/// Candidate Steam install roots, most common first.
fn steam_roots(home: &Path) -> Vec<PathBuf> {
    ROOTS.iter().map(|p| home.join(p)).filter(|p| p.is_dir()).collect()
}

/// Steam roots that are links to folders this process can't see, such as
/// a Steam install on another drive when running sandboxed. Returns where
/// they point, resolved through any chain of links.
pub fn unreachable_roots(home: &Path) -> Vec<PathBuf> {
    let mut targets = BTreeSet::new();
    for root in ROOTS.iter().map(|p| home.join(p)) {
        let mut path = root;
        // Follow the links by hand: the final target is what's missing.
        for _ in 0..8 {
            match std::fs::read_link(&path) {
                Ok(target) => path = if target.is_absolute() { target } else { path.parent().unwrap_or(Path::new("/")).join(target) },
                Err(_) => break,
            }
        }
        if !path.exists() && path.is_absolute() && std::fs::symlink_metadata(&path).is_err() && !path.starts_with(home) {
            targets.insert(path);
        }
    }
    targets.into_iter().collect()
}

/// Every Steam library folder known on this machine, deduplicated.
pub fn library_folders(home: &Path) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut libraries = Vec::new();
    let mut add = |path: PathBuf| {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if path.is_dir() && seen.insert(key) {
            libraries.push(path);
        }
    };

    for root in steam_roots(home) {
        add(root.clone());
        for vdf in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            if let Ok(src) = std::fs::read_to_string(root.join(vdf)) {
                for path in vdf_values(&src, "path") {
                    add(PathBuf::from(path));
                }
            }
        }
    }
    libraries
}

/// Workshop content directories for `app_id` across all libraries.
pub fn workshop_dirs(home: &Path, app_id: u32) -> Vec<PathBuf> {
    library_folders(home)
        .into_iter()
        .map(|lib| lib.join("steamapps/workshop/content").join(app_id.to_string()))
        .filter(|p| p.is_dir())
        .collect()
}

/// The Stellaris user directory (`~/.local/share/Paradox Interactive/Stellaris`),
/// looked up on the host when sandboxed.
pub fn stellaris_user_dir() -> PathBuf {
    crate::sandbox::data_dir().join("Paradox Interactive/Stellaris")
}

/// Values of every `"key" "value"` pair named `key` in a Valve KeyValues
/// document, at any depth. Enough for `libraryfolders.vdf`.
fn vdf_values(src: &str, key: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                s.push(escaped);
                            }
                        }
                        _ => s.push(c),
                    }
                }
                tokens.push(Some(s));
            }
            '{' | '}' => tokens.push(None),
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    tokens
        .windows(2)
        .filter_map(|w| match (&w[0], &w[1]) {
            (Some(k), Some(v)) if k.eq_ignore_ascii_case(key) => Some(v.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_links_to_missing_folders() {
        let home = std::env::temp_dir().join(format!("drukhal-steam-links-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".local/share")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/drive/Steam", home.join(".local/share/Steam")).unwrap();
        assert_eq!(unreachable_roots(&home), [PathBuf::from("/nonexistent/drive/Steam")]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn reads_library_paths() {
        let src = r#""libraryfolders"
{
	"0"
	{
		"path"		"/home/u/.local/share/Steam"
		"apps" { "281990" "123" }
	}
	"1" { "path" "/mnt/games/Steam" }
}"#;
        assert_eq!(vdf_values(src, "path"), vec!["/home/u/.local/share/Steam", "/mnt/games/Steam"]);
    }
}
