//! Reading the game's `error.log` and crediting each error to the mod whose
//! file it mentions. The log is rewritten every time the game starts, so it
//! always describes the last session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn path(user_dir: &Path) -> PathBuf {
    user_dir.join("logs/error.log")
}

/// One enabled mod, in load order.
#[derive(Debug, Clone)]
pub struct LogMod {
    pub id: String,
    pub name: String,
    pub root: Option<PathBuf>,
    pub steam_id: Option<u64>,
    pub is_patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The line without its timestamp.
    pub message: String,
    pub count: usize,
    /// The game-relative file it mentions, if any.
    pub file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The mod's name, or a description for errors not tied to a mod.
    pub label: String,
    pub mod_id: Option<String>,
    pub is_patch: bool,
    pub entries: Vec<Entry>,
}

impl Group {
    pub fn total(&self) -> usize {
        self.entries.iter().map(|e| e.count).sum()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Mod groups first (the merged patch at the top), then the rest.
    pub groups: Vec<Group>,
    pub lines: usize,
}

impl Report {
    /// Errors credited to mods (not the base game or unknown sources).
    pub fn from_mods(&self) -> usize {
        self.groups.iter().filter(|g| g.mod_id.is_some()).map(Group::total).sum()
    }
}

const BASE_GAME: &str = "Base game or missing files";
const NO_FILE: &str = "Not tied to a file";

/// Top-level folders of game content, plus `mod/` for descriptors.
const ROOTS: &[&str] = &["common/", "events/", "gfx/", "interface/", "localisation/", "localization/", "map/", "prescripted_countries/", "flags/", "music/", "sound/", "fonts/", "mod/"];
const EXTENSIONS: &[&str] = &[".txt", ".yml", ".gui", ".gfx", ".asset", ".mod", ".dds", ".png", ".tga", ".mesh", ".anim", ".shader", ".csv", ".wav", ".ogg", ".fnt"];

/// The first game-relative path in `line`. Paths may contain spaces and any
/// characters, so they're found by their top-level folder and extension.
pub fn find_path(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    let mut best: Option<(usize, usize)> = None;
    for root in ROOTS {
        let mut from = 0;
        while let Some(found) = lower[from..].find(root) {
            let start = from + found;
            from = start + root.len();
            // Must start a word: not `ui_common/…` or `/home/…/common/…`.
            let before = line[..start].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '/') {
                continue;
            }
            let end = EXTENSIONS
                .iter()
                .filter_map(|ext| {
                    let mut search = start;
                    while let Some(at) = lower[search..].find(ext) {
                        let end = search + at + ext.len();
                        if !lower[end..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            return Some(end);
                        }
                        search = end;
                    }
                    None
                })
                .min();
            if let Some(end) = end
                && best.is_none_or(|(s, _)| start < s)
            {
                best = Some((start, end));
            }
            break;
        }
    }
    best.map(|(s, e)| line[s..e].to_owned())
}

/// The same error repeated for numbered objects or lines (`group_1`,
/// `group_2`, `line: 40`…) counts as one: digits are ignored.
fn pattern(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for c in message.chars() {
        if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Strips the `[hh:mm:ss]` timestamp so repeated errors can be counted.
fn message(line: &str) -> &str {
    match line.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
        Some((time, rest)) if time.chars().all(|c| c.is_ascii_digit() || c == ':') => rest,
        _ => line,
    }
}

pub fn analyze(log: &str, mods: &[LogMod]) -> Report {
    // Which mod's copy of a file loads: the last one in load order.
    let mut owner_cache: HashMap<String, Option<usize>> = HashMap::new();
    let mut owner = |file: &str| -> Option<usize> {
        *owner_cache.entry(file.to_owned()).or_insert_with(|| {
            if file.starts_with("mod/") {
                return mods.iter().position(|m| m.id.eq_ignore_ascii_case(file));
            }
            mods.iter().rposition(|m| m.root.as_ref().is_some_and(|r| r.join(file).is_file()))
        })
    };

    let mut groups: Vec<(Option<usize>, &str, Vec<Entry>, HashMap<String, usize>)> = Vec::new();
    let mut lines = 0;
    for line in log.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        lines += 1;
        let msg = message(line);
        let file = find_path(msg);
        // "Could not find files for mod: …/281990/<id>" names a workshop mod.
        let by_steam = msg.rsplit_once("/281990/").and_then(|(_, id)| id.trim().parse::<u64>().ok()).and_then(|id| mods.iter().position(|m| m.steam_id == Some(id)));
        let (target, label) = match (by_steam, &file) {
            (Some(i), _) => (Some(i), ""),
            (None, Some(f)) => match owner(f) {
                Some(i) => (Some(i), ""),
                None => (None, BASE_GAME),
            },
            (None, None) => (None, NO_FILE),
        };
        let g = match groups.iter().position(|(t, l, ..)| *t == target && *l == label) {
            Some(g) => g,
            None => {
                groups.push((target, label, Vec::new(), HashMap::new()));
                groups.len() - 1
            }
        };
        let (_, _, entries, seen) = &mut groups[g];
        match seen.get(&pattern(msg)) {
            Some(&e) => entries[e].count += 1,
            None => {
                seen.insert(pattern(msg), entries.len());
                entries.push(Entry { message: msg.to_owned(), count: 1, file });
            }
        }
    }

    let mut out: Vec<Group> = groups
        .into_iter()
        .map(|(target, label, entries, _)| match target {
            Some(i) => Group { label: mods[i].name.clone(), mod_id: Some(mods[i].id.clone()), is_patch: mods[i].is_patch, entries },
            None => Group { label: label.to_owned(), mod_id: None, is_patch: false, entries },
        })
        .collect();
    out.sort_by_key(|g| (!g.is_patch, g.mod_id.is_none(), g.label == NO_FILE, std::cmp::Reverse(g.total())));
    for g in &mut out {
        g.entries.sort_by_key(|e| std::cmp::Reverse(e.count));
    }
    Report { groups: out, lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_paths_in_every_style() {
        let cases = [
            ("Portrait set 'machines' has invalid custom portrait 'x'. Lines '2-130', file 'common/portrait_sets/伞伞伞_drukhal_machines.txt'", Some("common/portrait_sets/伞伞伞_drukhal_machines.txt")),
            ("Unknown key at ' file: common/portrait_categories/x.txt line: 15'", Some("common/portrait_categories/x.txt")),
            ("Invalid supported_version in  file: mod/enclave modules.mod line: 7", Some("mod/enclave modules.mod")),
            ("Couldn't find texture file: gfx/interface/system/IBS_map_frame_sb.dds.", Some("gfx/interface/system/IBS_map_frame_sb.dds")),
            ("in file: \"common/solar_system_initializers/kosmian_star_systems.txt\" near line: 4", Some("common/solar_system_initializers/kosmian_star_systems.txt")),
            ("Could not find files for mod: /home/u/SteamLibrary/steamapps/workshop/content/281990/1595876588", None),
            ("Failed to find portrait selector holosphere_f_01", None),
        ];
        for (line, want) in cases {
            assert_eq!(find_path(line).as_deref(), want, "{line}");
        }
    }

    #[test]
    fn credits_errors_to_the_mod_whose_copy_loads() {
        let root = std::env::temp_dir().join(format!("drukhal-errorlog-{}", std::process::id()));
        let (a, b, patch) = (root.join("a"), root.join("b"), root.join("patch"));
        for (dir, file) in [(&a, "common/x/shared.txt"), (&b, "common/x/shared.txt"), (&a, "events/a.txt"), (&patch, "common/x/fixed.txt"), (&a, "common/x/fixed.txt")] {
            std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
            std::fs::write(dir.join(file), "").unwrap();
        }
        let m = |id: &str, name: &str, dir: &Path, steam, is_patch| LogMod { id: id.into(), name: name.into(), root: Some(dir.to_path_buf()), steam_id: steam, is_patch };
        let mods = [m("mod/ugc_1.mod", "A", &a, Some(1), false), m("mod/ugc_2.mod", "B", &b, Some(2), false), m("mod/patch.mod", "Patch", &patch, None, true)];
        let log = "[05:42:45][x.cpp:1]: bad thing file: common/x/shared.txt line: 3\n\
                   [05:42:46][x.cpp:1]: bad thing file: common/x/shared.txt line: 3\n\
                   [05:42:46][y.cpp:2]: Unexpected token at file: events/a.txt line: 9\n\
                   [05:42:47][z.cpp:3]: broken file 'common/x/fixed.txt'\n\
                   [05:42:48][dlc.cpp:1]: Could not find files for mod: /lib/workshop/content/281990/2\n\
                   [05:42:49][t.cpp:4]: Couldn't find texture file: gfx/nowhere.dds\n\
                   [05:42:50][q.cpp:5]: Something without a file\n";
        let r = analyze(log, &mods);
        assert_eq!(r.lines, 7);
        let summary: Vec<(&str, usize)> = r.groups.iter().map(|g| (g.label.as_str(), g.total())).collect();
        assert_eq!(summary, [("Patch", 1), ("B", 3), ("A", 1), (BASE_GAME, 1), (NO_FILE, 1)]);
        assert_eq!(r.groups[1].entries[0].count, 2, "repeats are counted, not listed");
        let numbered = "[05:00:00][g.cpp:1]: Failed to read key in file: common/x/shared.txt line: 1\n[05:00:01][g.cpp:1]: Failed to read key in file: common/x/shared.txt line: 72\n";
        assert_eq!(analyze(numbered, &mods).groups[0].entries.len(), 1, "errors differing only in numbers are one entry");
        assert_eq!(r.from_mods(), 5);
        std::fs::remove_dir_all(root).unwrap();
    }
}
