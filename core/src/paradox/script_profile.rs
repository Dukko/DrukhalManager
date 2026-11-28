//! The game's script profiler: `script_profiler` in the console starts it,
//! and running it again writes `logs/script_profiling_summary.log`, one
//! line per scripted item with its total time and how often it ran:
//!
//! ```text
//! event.ancrel.9999.immediate.effect                           12.521ms    152
//! ```
//!
//! Names are `<type>.<key>.<field>`, where the key can itself contain dots
//! (event ids do), or `CGameRules::<Rule>` for the game rules.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::merge::conflicts::{ObjectConflict, Report};

/// The width the game pads names to, cutting longer ones.
const NAME_WIDTH: usize = 100;

/// One line of the summary.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub ms: f64,
    pub hits: u64,
}

impl Entry {
    /// The item's type, e.g. `event` or `CGameRules`.
    pub fn kind(&self) -> &str {
        match self.name.split_once("::") {
            Some((kind, _)) => kind,
            None => self.name.split('.').next().unwrap_or(&self.name),
        }
    }

    /// What follows the type, e.g. `ancrel.9999.immediate.effect`.
    pub fn rest(&self) -> &str {
        match self.name.split_once("::") {
            Some((_, rest)) => rest,
            None => self.name.split_once('.').map_or("", |(_, rest)| rest),
        }
    }
}

pub fn summary_path(user_dir: &Path) -> PathBuf {
    user_dir.join("logs").join("script_profiling_summary.log")
}

/// The summary's entries, slowest first as the game writes them. Lines
/// that aren't entries, like the header, are skipped.
pub fn parse_summary(text: &str) -> Vec<Entry> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Entry> {
    // `%-100.100s %13.3fms %6u`: the name fills the first 100 columns and
    // the numbers are the last two fields.
    let mut fields = line.get(NAME_WIDTH..)?.split_whitespace();
    let ms = fields.next()?.strip_suffix("ms")?.parse().ok()?;
    let hits = fields.next()?.parse().ok()?;
    let name = line[..NAME_WIDTH].trim();
    (!name.is_empty() && !name.starts_with('[')).then(|| Entry { name: name.to_owned(), ms, hits })
}

/// The longest leading run of `rest`'s dot-separated parts that `known`
/// accepts, e.g. `ancrel.9999` for `ancrel.9999.immediate.effect`.
pub fn key_of(rest: &str, known: impl Fn(&str) -> bool) -> Option<&str> {
    let ends: Vec<usize> = rest.match_indices('.').map(|(i, _)| i).chain([rest.len()]).collect();
    ends.into_iter().rev().map(|end| &rest[..end]).find(|key| known(key))
}

/// `ShouldHideResourcesForPlanet` as `should_hide_resources_for_planet`.
pub fn snake(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 8);
    for (i, c) in camel.char_indices() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// One profiled item and the folder of the object it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub entry: Entry,
    pub category: String,
}

/// The time credited to one mod, by scanned position.
#[derive(Debug, Clone, PartialEq)]
pub struct ModTime {
    pub position: usize,
    pub ms: f64,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub total_ms: f64,
    /// Time in items no enabled mod defines: the base game's own.
    pub unmatched_ms: f64,
    pub mods: Vec<ModTime>,
}

/// Credits each entry to the mod whose definition of its object the game
/// uses, from the conflict scan. Mods and their items are slowest first.
pub fn attribute(entries: &[Entry], report: &Report) -> Profile {
    let mut by_key: HashMap<&str, Vec<&(String, String)>> = HashMap::new();
    for id in report.defined.keys().filter(|id| !id.0.starts_with("localisation")) {
        by_key.entry(id.1.as_str()).or_default().push(id);
    }
    let categories: BTreeSet<&str> = by_key.values().flatten().map(|id| id.0.as_str()).collect();
    let mut homes: HashMap<&str, bool> = HashMap::new();
    let mut mods: HashMap<usize, ModTime> = HashMap::new();
    let (mut total_ms, mut unmatched_ms) = (0.0, 0.0);
    for entry in entries {
        total_ms += entry.ms;
        let rest = if entry.kind() == "CGameRules" { snake(entry.rest()) } else { entry.rest().to_owned() };
        // A kind with a folder of its own must not match a key shorter than
        // the whole name in another folder: `event.base.2.x` isn't a trigger `base`.
        let kind = entry.kind();
        if kind.is_empty() {
            unmatched_ms += entry.ms;
            continue;
        }
        let has_home = *homes.entry(kind).or_insert_with(|| has_folder(&categories, kind));
        let accepts = |key: &str| by_key.get(key).is_some_and(|ids| key.len() == rest.len() || !has_home || ids.iter().any(|id| prefers(&id.0, kind)));
        let Some(key) = key_of(&rest, accepts) else {
            unmatched_ms += entry.ms;
            continue;
        };
        let ids = &by_key[key];
        let id = ids.iter().find(|id| prefers(&id.0, kind)).unwrap_or(&ids[0]);
        let definitions = report.defined[*id].clone();
        let conflict = ObjectConflict { category: id.0.clone(), key: id.1.clone(), definitions };
        let winner = conflict.winner().map_or(conflict.definitions.len() - 1, |(i, _)| i);
        let position = conflict.definitions[winner].mod_position;
        let credited = mods.entry(position).or_insert_with(|| ModTime { position, ms: 0.0, items: Vec::new() });
        credited.ms += entry.ms;
        credited.items.push(Item { entry: entry.clone(), category: id.0.clone() });
    }
    let mut mods: Vec<ModTime> = mods.into_values().collect();
    mods.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    for m in &mut mods {
        m.items.sort_by(|a, b| b.entry.ms.total_cmp(&a.entry.ms));
    }
    Profile { total_ms, unmatched_ms, mods }
}

/// Whether `category`'s last folder is named for the entry's type, like
/// `common/system_types` for `system_type`.
fn prefers(category: &str, kind: &str) -> bool {
    let folder = category.rsplit('/').next().unwrap_or(category);
    let kind = if kind == "CGameRules" { "game_rules" } else { kind };
    !kind.is_empty()
        && (folder.strip_prefix(kind).is_some_and(|rest| rest.is_empty() || rest == "s")
            || kind.strip_suffix('y').and_then(|stem| folder.strip_prefix(stem)) == Some("ies"))
}

/// Whether some category is the folder for `kind`.
fn has_folder(categories: &BTreeSet<&str>, kind: &str) -> bool {
    categories.iter().any(|c| prefers(c, kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::conflicts::Definition;

    fn line(name: &str, ms: f64, hits: u64) -> String {
        format!("{name:<100.100} {ms:13.3}ms {hits:6}")
    }

    #[test]
    fn parses_summary_lines() {
        let text = [
            "[20:24:19][pdx_profiling.cpp:450]: Node, Total Time, Hits".to_owned(),
            String::new(),
            line("CGameRules::ShouldHideResourcesForPlanet", 67.311, 163332),
            line("event.ancrel.9999.immediate.effect", 12.521, 152),
            line(&format!("situation.{}.allowed", "x".repeat(120)), 0.5, 3),
        ]
        .join("\n");
        let entries = parse_summary(&text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], Entry { name: "CGameRules::ShouldHideResourcesForPlanet".into(), ms: 67.311, hits: 163332 });
        assert_eq!((entries[0].kind(), entries[0].rest()), ("CGameRules", "ShouldHideResourcesForPlanet"));
        assert_eq!((entries[1].kind(), entries[1].rest()), ("event", "ancrel.9999.immediate.effect"));
        assert_eq!(entries[2].name.len(), NAME_WIDTH);
    }

    #[test]
    fn finds_the_longest_known_key() {
        let known = |k: &str| matches!(k, "ancrel" | "ancrel.9999");
        assert_eq!(key_of("ancrel.9999.immediate.effect", known), Some("ancrel.9999"));
        assert_eq!(key_of("ancrel.1.immediate", known), Some("ancrel"));
        assert_eq!(key_of("other.potential", known), None);
        assert_eq!(snake("ShouldHideResourcesForPlanet"), "should_hide_resources_for_planet");
    }

    type Object<'a> = (&'a str, &'a str, &'a [(usize, &'a str)]);

    fn report(objects: &[Object]) -> Report {
        let defined = objects.iter().map(|&(category, key, defs)| ((category.to_owned(), key.to_owned()), defs.iter().map(|&(mod_position, file)| Definition { mod_position, file: file.to_owned() }).collect())).collect();
        Report { defined, ..Default::default() }
    }

    fn entry(name: &str, ms: f64) -> Entry {
        Entry { name: name.to_owned(), ms, hits: 1 }
    }

    #[test]
    fn credits_the_winning_mod_and_totals_the_rest_as_unmatched() {
        let report = report(&[("events", "ancrel.1", &[(0, "events/aa.txt"), (1, "events/zz.txt")]), ("common/game_rules", "should_hide", &[(2, "common/game_rules/a.txt")])]);
        let entries = [entry("event.ancrel.1.immediate", 5.0), entry("CGameRules::ShouldHide", 3.0), entry("event.ancrel.1.option", 1.0), entry("event.base.2.trigger", 7.0)];

        let profile = attribute(&entries, &report);

        assert_eq!((profile.total_ms, profile.unmatched_ms), (16.0, 7.0));
        let credited: Vec<(usize, f64)> = profile.mods.iter().map(|m| (m.position, m.ms)).collect();
        assert_eq!(credited, [(0, 6.0), (2, 3.0)], "the first event definition wins, slowest mod first");
        let names: Vec<&str> = profile.mods[0].items.iter().map(|i| i.entry.name.as_str()).collect();
        assert_eq!(names, ["event.ancrel.1.immediate", "event.ancrel.1.option"]);
    }

    #[test]
    fn prefers_the_category_named_for_the_entry_type() {
        let report = report(&[("common/scripted_triggers", "ship", &[(0, "a.txt")]), ("common/system_types", "ship", &[(1, "b.txt")])]);

        let profile = attribute(&[entry("system_type.ship.potential", 2.0)], &report);

        assert_eq!(profile.mods.len(), 1);
        assert_eq!((profile.mods[0].position, profile.mods[0].items[0].category.as_str()), (1, "common/system_types"));
    }

    #[test]
    fn a_shorter_key_counts_only_in_a_folder_named_for_the_entry_type() {
        let report = report(&[("common/scripted_triggers", "base", &[(0, "a.txt")]), ("events", "base.1", &[(1, "events/b.txt")])]);

        let profile = attribute(&[entry("event.base.2.trigger", 7.0), entry("event.base.1.trigger", 3.0)], &report);

        assert_eq!(profile.unmatched_ms, 7.0, "the scripted trigger named base isn't the event");
        assert_eq!(profile.mods.iter().map(|m| (m.position, m.ms)).collect::<Vec<_>>(), [(1, 3.0)]);
    }

    #[test]
    fn a_kind_without_a_folder_of_its_name_matches_any_folder() {
        let report = report(&[("common/archaeological_site_types", "ancient", &[(3, "a.txt")])]);

        let profile = attribute(&[entry("arc_site.ancient.potential", 4.0)], &report);

        assert_eq!(profile.mods.iter().map(|m| (m.position, m.ms)).collect::<Vec<_>>(), [(3, 4.0)]);
    }

    #[test]
    fn a_kind_ending_in_y_finds_its_ies_folder() {
        let report = report(&[("common/scripted_triggers", "ancrel", &[(0, "a.txt")]), ("common/pop_categories", "ancrel", &[(1, "b.txt")])]);

        let profile = attribute(&[entry("pop_category.ancrel.visible", 4.0)], &report);

        assert_eq!(profile.mods.iter().map(|m| (m.position, m.ms)).collect::<Vec<_>>(), [(1, 4.0)]);
    }

    #[test]
    fn a_folder_that_only_shares_a_prefix_with_the_kind_is_not_its_folder() {
        let report = report(&[("common/event_chains", "base", &[(0, "a.txt")]), ("events", "base.1", &[(1, "events/b.txt")])]);

        let profile = attribute(&[entry("event.base.2.trigger", 7.0)], &report);

        assert!(profile.mods.is_empty());
        assert_eq!(profile.unmatched_ms, 7.0);
    }

    #[test]
    fn a_name_with_no_kind_has_no_folder() {
        let report = report(&[("events", "base", &[(0, "events/a.txt")])]);

        let profile = attribute(&[entry(".base.trigger", 2.0)], &report);

        assert!(profile.mods.is_empty());
        assert_eq!(profile.unmatched_ms, 2.0);
    }

    #[test]
    fn leaves_localisation_out() {
        let report = report(&[("localisation", "ancrel", &[(0, "localisation/a.yml")])]);

        let profile = attribute(&[entry("event.ancrel.immediate", 2.0)], &report);

        assert!(profile.mods.is_empty());
        assert_eq!(profile.unmatched_ms, 2.0);
    }

    /// Reads the real profile and says how much of its time mods' objects
    /// take, using the conflict scan of the enabled mods. Read only:
    /// `DRUKHAL_STELLARIS_USER=<user folder> cargo test script_profile_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn script_profile_survey() {
        use crate::merge::conflicts::{self, ModInput};
        use crate::mods::Descriptor;
        use crate::mods::load_order::DlcLoad;

        let Some(user) = std::env::var_os("DRUKHAL_STELLARIS_USER") else {
            println!("SURVEY skipped: set DRUKHAL_STELLARIS_USER");
            return;
        };
        let user = PathBuf::from(user);
        let entries = parse_summary(&std::fs::read_to_string(summary_path(&user)).unwrap());
        let total: f64 = entries.iter().map(|e| e.ms).sum();
        println!("{} entries, {total:.0} ms", entries.len());

        let mut names = Vec::new();
        let mut inputs = Vec::new();
        for entry in DlcLoad::read(&user).unwrap().enabled_mods {
            let Ok(src) = std::fs::read_to_string(user.join(&entry)) else { continue };
            let Ok(descriptor) = Descriptor::parse(&src) else { continue };
            let Some(path) = descriptor.path.clone() else { continue };
            let root = user.join(path);
            names.push(descriptor.name.clone());
            inputs.push(ModInput { root: Some(root), replace_path: descriptor.replace_path.clone() });
        }
        let report = conflicts::analyze(&inputs, None);
        let profile = attribute(&entries, &report);
        let matched = profile.total_ms - profile.unmatched_ms;
        println!("matched to mods: {matched:.0} ms ({:.0}%)", matched * 100.0 / profile.total_ms);
        println!("\nBY MOD");
        for m in profile.mods.iter().take(25) {
            println!("{:8.1} ms  {}", m.ms, names[m.position]);
        }
        println!("\nSLOWEST MOD ITEMS");
        let mut top: Vec<_> = profile.mods.iter().flat_map(|m| m.items.iter().map(move |i| (i, m.position))).collect();
        top.sort_by(|a, b| b.0.entry.ms.total_cmp(&a.0.entry.ms));
        for (i, position) in top.iter().take(30) {
            println!("{:8.1} ms {:7} hits  {}  [{}]  {}", i.entry.ms, i.entry.hits, i.entry.name, i.category, names[*position]);
        }
        let categories: BTreeSet<&str> = report.defined.keys().map(|id| id.0.as_str()).collect();
        let mut homes: HashMap<&str, bool> = HashMap::new();
        let mut homeless: HashMap<&str, f64> = HashMap::new();
        for e in &entries {
            if !*homes.entry(e.kind()).or_insert_with(|| has_folder(&categories, e.kind())) {
                *homeless.entry(e.kind()).or_default() += e.ms;
            }
        }
        let mut homeless: Vec<_> = homeless.into_iter().collect();
        homeless.sort_by(|a, b| b.1.total_cmp(&a.1));
        println!("\nKINDS WITH NO FOLDER OF THEIR NAME (any folder accepted)");
        for (kind, ms) in homeless.iter().take(15) {
            println!("{ms:8.1} ms  {kind}");
        }
        println!("\nBASE GAME (no mod defines the key): {:.0} ms", profile.unmatched_ms);
    }
}
