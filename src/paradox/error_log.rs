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
    pub kind: Kind,
}

/// What an error amounts to, from its text alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Anything not known to be harmless: shown first and counted.
    Problem,
    /// Saved empire designs that use content from mods that aren't enabled.
    SavedDesign,
    /// A missing icon or texture: something shows blank.
    Cosmetic,
    /// The same object or variable defined twice; the game keeps one.
    Duplicate,
    /// Modifiers defined but never used.
    Unused,
}

impl Kind {
    pub fn classify(message: &str) -> Kind {
        let has = |needles: &[&str]| needles.iter().any(|n| message.contains(n));
        if has(&["empire_design.cpp", "user_empire_designs"]) {
            Kind::SavedDesign
        } else if has(&["is never used from script"]) {
            Kind::Unused
        } else if has(&["already exists, using the one at", "is already taken."]) {
            Kind::Duplicate
        } else if has(&["Missing modifier icon", "missing texture file", "Couldn't find texture file", "Error initialising texture"]) {
            Kind::Cosmetic
        } else {
            Kind::Problem
        }
    }

    /// A short title for the dialog, e.g. "Unused modifiers".
    pub fn title(self) -> String {
        match self {
            Kind::Problem => t!("Problems"),
            Kind::SavedDesign => t!("Saved empire designs"),
            Kind::Cosmetic => t!("Missing icons"),
            Kind::Duplicate => t!("Defined twice"),
            Kind::Unused => t!("Unused modifiers"),
        }
    }

    /// One sentence on why it's (probably) harmless; empty for Problem.
    pub fn explanation(self) -> String {
        match self {
            Kind::Problem => String::new(),
            Kind::SavedDesign => t!("Some of your saved empires use civics, origins or planets from mods that aren't enabled. They only matter if you play those empires."),
            Kind::Cosmetic => t!("Something shows a blank icon or picture. It doesn't affect gameplay."),
            Kind::Duplicate => t!("A mod defines something that already exists, and the game keeps one of them. The Conflicts page shows which."),
            Kind::Unused => t!("Mods define resource modifiers nothing uses. The base game does this too."),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The mod's name, or a description for errors not tied to a mod.
    pub label: String,
    pub mod_id: Option<String>,
    pub is_patch: bool,
    /// Errors that name no file, which sort after those that do.
    pub no_file: bool,
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
    pub fn mod_errors(&self) -> usize {
        self.groups.iter().filter(|g| g.mod_id.is_some()).map(Group::total).sum()
    }

    /// Errors that aren't known to be harmless.
    pub fn problems(&self) -> usize {
        self.groups.iter().flat_map(|g| &g.entries).filter(|e| e.kind == Kind::Problem).map(|e| e.count).sum()
    }
}

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

/// The log's lines with continuation lines (those not starting with `[`)
/// appended to the line before.
fn joined(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in log.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        match out.last_mut() {
            Some(prev) if !line.starts_with('[') => {
                prev.push(' ');
                prev.push_str(line.trim());
            }
            _ => out.push(line.to_owned()),
        }
    }
    out
}

/// How many lines of `log` are real problems, after joining multi-line errors.
pub fn count_problems(log: &str) -> usize {
    joined(log).iter().filter(|l| Kind::classify(message(l)) == Kind::Problem).count()
}

/// The line an error points at in `file`, if it says: `file.txt:18`,
/// `line: 47` or `near line: 7`. The `file:N` form wins when present,
/// because later `line:` numbers in the same message can belong to another
/// file.
pub fn find_line(message: &str, file: &str) -> Option<usize> {
    let number = |s: &str| -> Option<usize> {
        let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let mut from = 0;
    while let Some(at) = message[from..].find(file) {
        let after = from + at + file.len();
        if let Some(n) = message[after..].strip_prefix(':').and_then(number) {
            return Some(n);
        }
        from = after;
    }
    let at = message.find("line:")?;
    number(message[at + "line:".len()..].trim_start())
}

/// Lines `line - radius ..= line + radius` of `text` (1-based, clamped),
/// each prefixed with its number and a tab; the first `2 * radius + 1`
/// lines when `line` is `None`. At most `max_bytes`, cut at a line end.
pub fn excerpt(text: &str, line: Option<usize>, radius: usize, max_bytes: usize) -> String {
    let total = text.lines().count();
    let (first, last) = match line {
        Some(l) => {
            let l = l.clamp(1, total.max(1));
            (l.saturating_sub(radius).max(1), l + radius)
        }
        None => (1, 2 * radius + 1),
    };
    let mut out = String::new();
    for (i, l) in text.lines().enumerate().skip(first - 1).take(last + 1 - first) {
        let row = format!("{}\t{}\n", i + 1, l);
        if out.len() + row.len() > max_bytes {
            break;
        }
        out.push_str(&row);
    }
    out
}

/// A group being built: the mod (or none) it is credited to, with its entries.
struct Building {
    target: Option<usize>,
    label: String,
    no_file: bool,
    entries: Vec<Entry>,
    /// Message pattern to its index in `entries`.
    seen: HashMap<String, usize>,
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

    let base_game = t!("Base game or missing files");
    let no_file = t!("Not tied to a file");
    let mut groups: Vec<Building> = Vec::new();
    let mut lines = 0;
    let all = joined(log);
    for line in all.iter().map(String::as_str) {
        lines += 1;
        let msg = message(line);
        let file = find_path(msg);
        // "Could not find files for mod: …/281990/<id>" names a workshop mod.
        let by_steam = msg.rsplit_once("/281990/").and_then(|(_, id)| id.trim().parse::<u64>().ok()).and_then(|id| mods.iter().position(|m| m.steam_id == Some(id)));
        let (target, label, no_file_errors) = match (by_steam, &file) {
            (Some(i), _) => (Some(i), String::new(), false),
            (None, Some(f)) => match owner(f) {
                Some(i) => (Some(i), String::new(), false),
                None => (None, base_game.clone(), false),
            },
            (None, None) => (None, no_file.clone(), true),
        };
        let g = match groups.iter().position(|b| b.target == target && b.label == label) {
            Some(g) => g,
            None => {
                groups.push(Building { target, label, no_file: no_file_errors, entries: Vec::new(), seen: HashMap::new() });
                groups.len() - 1
            }
        };
        let Building { entries, seen, .. } = &mut groups[g];
        match seen.get(&pattern(msg)) {
            Some(&e) => entries[e].count += 1,
            None => {
                seen.insert(pattern(msg), entries.len());
                entries.push(Entry { message: msg.to_owned(), count: 1, kind: Kind::classify(msg), file });
            }
        }
    }

    let mut out: Vec<Group> = groups
        .into_iter()
        .map(|Building { target, label, no_file, entries, .. }| match target {
            Some(i) => Group { label: mods[i].name.clone(), mod_id: Some(mods[i].id.clone()), is_patch: mods[i].is_patch, no_file, entries },
            None => Group { label, mod_id: None, is_patch: false, no_file, entries },
        })
        .collect();
    out.sort_by_key(|g| (!g.is_patch, g.mod_id.is_none(), g.no_file, std::cmp::Reverse(g.total())));
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
        assert_eq!(summary, [("Patch", 1), ("B", 3), ("A", 1), ("Base game or missing files", 1), ("Not tied to a file", 1)]);
        assert_eq!(r.groups[1].entries[0].count, 2, "repeats are counted, not listed");
        let numbered = "[05:00:00][g.cpp:1]: Failed to read key in file: common/x/shared.txt line: 1\n[05:00:01][g.cpp:1]: Failed to read key in file: common/x/shared.txt line: 72\n";
        assert_eq!(analyze(numbered, &mods).groups[0].entries.len(), 1, "errors differing only in numbers are one entry");
        assert_eq!(r.mod_errors(), 5);
        std::fs::remove_dir_all(root).unwrap();
    }

    const UNEXPECTED: &str = "[15:28:00][persistent.cpp:41]: Error: \"Unexpected token: ai_update_type, near line: 7\n\
\" in file: \"common/technology/ui_tech_ascension_theory_repeatable.txt\" near line: 7\n";
    const SCOPE: &str = "[15:28:13][effect.cpp:919]: Wrong scope for effect 'set_planetary_ascension_tier' at  common/scripted_effects/uiod_psionic_auras_scripted_effects.txt:18 @ scripted effect desire_aura_max_intensity_effect at file: common/patrons/psionic_auras/00_auras.txt:583(inline_script) common/inline_scripts/shroud/psionic_aura_definition.txt line: 46\n\
Current Scope: galactic_object\n\
Supported Scopes: planet ship colony\n";
    const COMPILING: &str = "[15:28:13][metascript.cpp:166]: Compiling source for  failed for missing args: limit, has_owner, is_owned_by, has_planetary_ascension_tier, has_ground_combat, is_occupied_flag\n";
    const INVALID: &str = "[15:28:14][effect.cpp:100]: Script Error: Invalid scripted effect foo at file: common/scripted_effects/x.txt line: 3\n";
    const HARMLESS: &[(&str, Kind)] = &[
        ("[15:27:50][texturehandler.cpp:66]: Texture Handler encountered missing texture file: gfx/interface/icons/traits/trait_pc_wet_cave_preference.dds", Kind::Cosmetic),
        ("[15:27:59][reader.cpp:209]: Variable name max_tradition_trees is already taken.  file: common/scripted_variables/07_scripted_variables_machine_age.txt line: 47", Kind::Duplicate),
        ("[15:28:07][empire_design.cpp:516]: Invalid government civic KC_WeaponManufacturers for empire Nomadic Alvanian, bad savegame?", Kind::SavedDesign),
        ("[15:28:07][parser_util.h:48]: Failed to read key reference 'pc_pd_crag' from database 'common/planet_classes' at ' file: user_empire_designs_v3.4.txt line: 424'", Kind::SavedDesign),
        ("Economic Category: ships. 'triggered_cost_modifier' #3 generates 'add' modifiers that is never used from script", Kind::Unused),
        ("Object with key: starbases already exists, using the one at  file: common/economic_categories/~~~modifiers_fix_categories.txt line: 1", Kind::Duplicate),
        ("Missing modifier icon \"gfx/interface/icons/modifiers/mod_unity_logistics_add.dds\"", Kind::Cosmetic),
    ];

    #[test]
    fn joins_continuation_lines() {
        let log = format!("{UNEXPECTED}{SCOPE}{COMPILING}");
        let lines = joined(&log);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("common/technology/ui_tech_ascension_theory_repeatable.txt"));
        assert_eq!(find_path(message(&lines[0])).as_deref(), Some("common/technology/ui_tech_ascension_theory_repeatable.txt"));
        assert!(lines[1].ends_with("Supported Scopes: planet ship colony"));
        assert_eq!(analyze(&log, &[]).lines, 3);
    }

    #[test]
    fn sorts_errors_by_kind() {
        for (line, want) in HARMLESS {
            assert_eq!(Kind::classify(message(line)), *want, "{line}");
        }
        for line in joined(&format!("{UNEXPECTED}{SCOPE}{COMPILING}{INVALID}")) {
            assert_eq!(Kind::classify(message(&line)), Kind::Problem, "{line}");
        }
    }

    #[test]
    fn counts_only_problems() {
        let mut log = format!("{UNEXPECTED}{SCOPE}{COMPILING}{INVALID}");
        for (line, _) in HARMLESS {
            log.push_str(line);
            log.push('\n');
        }
        assert_eq!(count_problems(&log), 4);
        assert_eq!(analyze(&log, &[]).problems(), 4);
    }

    #[test]
    fn finds_the_line_an_error_points_at() {
        let scope = joined(SCOPE).remove(0);
        let file = "common/scripted_effects/uiod_psionic_auras_scripted_effects.txt";
        assert_eq!(find_line(&scope, file), Some(18));
        let var = message(HARMLESS[1].0);
        assert_eq!(find_line(var, "common/scripted_variables/07_scripted_variables_machine_age.txt"), Some(47));
        let tok = joined(UNEXPECTED).remove(0);
        assert_eq!(find_line(&tok, "common/technology/ui_tech_ascension_theory_repeatable.txt"), Some(7));
        assert_eq!(find_line("Something went wrong", "common/x.txt"), None);
    }

    #[test]
    fn excerpts_numbered_lines() {
        let text: String = (1..=100).map(|n| format!("l{n}\n")).collect();
        assert_eq!(excerpt(&text, Some(50), 2, 1000), "48\tl48\n49\tl49\n50\tl50\n51\tl51\n52\tl52\n");
        assert!(excerpt(&text, Some(1), 2, 1000).starts_with("1\tl1\n"));
        assert_eq!(excerpt(&text, Some(1), 2, 1000).lines().count(), 3);
        assert!(excerpt(&text, Some(100), 2, 1000).ends_with("100\tl100\n"));
        assert_eq!(excerpt(&text, None, 2, 1000).lines().count(), 5);
        assert_eq!(excerpt(&text, Some(50), 2, 14), "48\tl48\n49\tl49\n");
    }

    /// Prints how a real `error.log` sorts. Read-only; set
    /// `DRUKHAL_STELLARIS_USER` to the game's user folder.
    #[test]
    #[ignore]
    fn classify_survey() {
        let Ok(dir) = std::env::var("DRUKHAL_STELLARIS_USER") else {
            println!("SURVEY skipped: set DRUKHAL_STELLARIS_USER");
            return;
        };
        let text = String::from_utf8_lossy(&std::fs::read(path(Path::new(&dir))).unwrap()).into_owned();
        let mut counts: HashMap<Kind, usize> = HashMap::new();
        for line in joined(&text) {
            let msg = message(&line);
            let kind = Kind::classify(msg);
            *counts.entry(kind).or_default() += 1;
            if kind == Kind::Problem {
                println!("PROBLEM {}", msg.chars().take(200).collect::<String>());
            }
        }
        let mut counts: Vec<_> = counts.into_iter().collect();
        counts.sort();
        for (kind, n) in counts {
            println!("{kind:?}: {n}");
        }
    }
}
