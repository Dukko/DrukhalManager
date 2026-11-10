//! Locating Steam libraries and Stellaris workshop content on Linux.
//!
//! The lookup order (native Steam, then Flatpak, then every library listed in
//! `libraryfolders.vdf`) follows Irony Mod Manager's `SteamDirectory`
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::collections::{BTreeSet, HashMap};
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

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

/// Splits a Valve KeyValues document (`.vdf`, `.acf`) into tokens.
fn vdf_tokens(src: &str) -> Vec<Token> {
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
                tokens.push(Token::Str(s));
            }
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
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
}

/// Values of every `"key" "value"` pair named `key` in a Valve KeyValues
/// document, at any depth. Enough for `libraryfolders.vdf`.
fn vdf_values(src: &str, key: &str) -> Vec<String> {
    vdf_tokens(src)
        .windows(2)
        .filter_map(|w| match (&w[0], &w[1]) {
            (Token::Str(k), Token::Str(v)) if k.eq_ignore_ascii_case(key) => Some(v.clone()),
            _ => None,
        })
        .collect()
}

/// The Workshop items Steam has installed for an app, from the manifest next
/// to its content folder (`steamapps/workshop/appworkshop_<app>.acf`).
/// `content_dir` is `steamapps/workshop/content/<app>`. `None` when there is
/// no readable manifest.
pub fn installed_items(content_dir: &Path) -> Option<Vec<u64>> {
    let app = content_dir.file_name()?.to_str()?;
    let acf = content_dir.parent()?.parent()?.join(format!("appworkshop_{app}.acf"));
    Some(installed_in(&std::fs::read_to_string(acf).ok()?))
}

fn installed_in(src: &str) -> Vec<u64> {
    items_in(&vdf_tokens(src), "WorkshopItemsInstalled").into_iter().map(|(id, _)| id).collect()
}

/// The items in the manifest's block `section`, with each one's
/// `"key" "value"` pairs, in file order.
fn items_in(tokens: &[Token], section: &str) -> Vec<(u64, HashMap<String, String>)> {
    let Some(start) = tokens.windows(2).position(|w| matches!(&w[0], Token::Str(k) if k == section) && w[1] == Token::Open) else {
        return Vec::new();
    };
    let mut items: Vec<(u64, HashMap<String, String>)> = Vec::new();
    let mut depth = 0;
    let mut i = start + 1;
    while let Some(token) = tokens.get(i) {
        match token {
            Token::Open => depth += 1,
            Token::Close => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            // Item ids are the keys of the blocks directly inside.
            Token::Str(key) if depth == 1 && tokens.get(i + 1) == Some(&Token::Open) => {
                if let Ok(id) = key.parse() {
                    items.push((id, HashMap::new()));
                }
            }
            Token::Str(key) if depth == 2 => {
                if let (Some(Token::Str(value)), Some((_, fields))) = (tokens.get(i + 1), items.last_mut()) {
                    fields.insert(key.clone(), value.clone());
                    i += 1;
                }
            }
            Token::Str(_) => {}
        }
        i += 1;
    }
    items
}

/// What Steam says about a Workshop item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Installed,
    /// Installed, but Steam has a newer version it hasn't finished
    /// downloading.
    UpdatePending,
    /// Subscribed, but not downloaded yet.
    Downloading,
    /// Not in Steam's manifest: unsubscribed, or removed from the
    /// Workshop.
    Unsubscribed,
}

/// The state of every Workshop item Steam knows about for the app whose
/// content folders are `content_dirs`. `None` when no manifest could be
/// read, so nothing can be said about any item.
pub fn item_states(content_dirs: &[PathBuf]) -> Option<HashMap<u64, ItemState>> {
    let mut states = HashMap::new();
    let mut read_any = false;
    for dir in content_dirs {
        let (Some(app), Some(workshop)) = (dir.file_name().and_then(|a| a.to_str()), dir.parent().and_then(Path::parent)) else { continue };
        let Ok(src) = std::fs::read_to_string(workshop.join(format!("appworkshop_{app}.acf"))) else { continue };
        read_any = true;
        for (id, state) in states_in(&src) {
            // An item in two libraries counts as installed if either has it.
            let entry = states.entry(id).or_insert(state);
            if state == ItemState::Installed {
                *entry = state;
            }
        }
    }
    read_any.then_some(states)
}

fn states_in(src: &str) -> HashMap<u64, ItemState> {
    let tokens = vdf_tokens(src);
    let installed: HashMap<u64, HashMap<String, String>> = items_in(&tokens, "WorkshopItemsInstalled").into_iter().collect();
    let mut states: HashMap<u64, ItemState> = installed.keys().map(|&id| (id, ItemState::Installed)).collect();
    for (id, details) in items_in(&tokens, "WorkshopItemDetails") {
        let newer = |current: &str, latest: &str| match (details.get(current), details.get(latest)) {
            (Some(c), Some(l)) => c != l && !l.is_empty() && l != "0",
            _ => false,
        };
        let state = if !installed.contains_key(&id) {
            ItemState::Downloading
        } else if newer("manifest", "latest_manifest") || newer("timeupdated", "latest_timeupdated") {
            ItemState::UpdatePending
        } else {
            ItemState::Installed
        };
        states.insert(id, state);
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_installed_workshop_items() {
        let acf = "\"AppWorkshop\"\n{\n\t\"appid\"\t\"281990\"\n\t\"WorkshopItemsInstalled\"\n\t{\n\t\t\"865040033\"\n\t\t{\n\t\t\t\"size\"\t\"1\"\n\t\t}\n\t\t\"1067631798\"\n\t\t{\n\t\t\t\"size\"\t\"2\"\n\t\t}\n\t}\n\t\"WorkshopItemDetails\"\n\t{\n\t\t\"999\"\n\t\t{\n\t\t}\n\t}\n}\n";
        assert_eq!(installed_in(acf), [865040033, 1067631798]);
        assert_eq!(installed_in("\"AppWorkshop\" { }"), Vec::<u64>::new());
    }

    #[test]
    fn reads_what_steam_says_about_each_item() {
        let acf = r#""AppWorkshop"
{
	"WorkshopItemsInstalled"
	{
		"1" { "size" "1" "timeupdated" "100" "manifest" "a" }
		"2" { "size" "1" "timeupdated" "100" "manifest" "b" }
		"3" { "size" "1" "timeupdated" "100" "manifest" "c" }
	}
	"WorkshopItemDetails"
	{
		"1" { "manifest" "a" "timeupdated" "100" "latest_timeupdated" "100" "latest_manifest" "a" }
		"2" { "manifest" "b" "timeupdated" "100" "latest_timeupdated" "200" "latest_manifest" "b2" }
		"4" { "manifest" "0" "timeupdated" "0" "latest_timeupdated" "300" "latest_manifest" "d" }
	}
}"#;
        let states = states_in(acf);
        assert_eq!(states[&1], ItemState::Installed);
        assert_eq!(states[&2], ItemState::UpdatePending);
        assert_eq!(states[&3], ItemState::Installed, "no details: as installed");
        assert_eq!(states[&4], ItemState::Downloading);
        assert_eq!(states.len(), 4);

        let root = std::env::temp_dir().join(format!("drukhal-steam-states-{}", std::process::id()));
        let content = root.join("steamapps/workshop/content/281990");
        std::fs::create_dir_all(&content).unwrap();
        assert_eq!(item_states(std::slice::from_ref(&content)), None, "no manifest, nothing known");
        std::fs::write(root.join("steamapps/workshop/appworkshop_281990.acf"), acf).unwrap();
        assert_eq!(item_states(&[content]).unwrap()[&2], ItemState::UpdatePending);
        std::fs::remove_dir_all(root).unwrap();
    }

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
