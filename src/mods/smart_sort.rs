//! Smart sorting: puts each enabled mod in a section of the load order by
//! what it is, following the Stellaris community's load order guides:
//!
//! 1. unofficial patches (names starting with `~`) at the very top;
//! 2. gameplay mods, from graphics and flavor through shipsets, galaxy
//!    generation and mechanics to large overhauls (their order matters
//!    little, but grouping keeps the list readable);
//! 3. patches between two or more of those mods, below what they patch;
//! 4. UI mods such as UI Overhaul Dynamic, which must replace the interface
//!    files gameplay mods ship, then patches for them;
//! 5. universal patches (names starting with `!`) at the bottom.
//!
//! A sub-mod, which needs exactly one other enabled mod, goes directly
//! below it. Within a section mods are sorted by name, and every mod still
//! comes after the enabled mods it depends on.
//!
//! What a mod is comes from its name, its declared dependencies and the
//! folders it ships; tags are too often wrong to go by. The user can put
//! any mod in another section by hand.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use super::Mod;
use super::checks;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Unofficial,
    Graphics,
    Flavor,
    Shipsets,
    Galaxy,
    Gameplay,
    Overhaul,
    Patches,
    Ui,
    UiPatches,
    Universal,
}

impl Category {
    pub const ALL: [Category; 11] = [
        Category::Unofficial,
        Category::Graphics,
        Category::Flavor,
        Category::Shipsets,
        Category::Galaxy,
        Category::Gameplay,
        Category::Overhaul,
        Category::Patches,
        Category::Ui,
        Category::UiPatches,
        Category::Universal,
    ];

    /// How it's saved in the settings.
    pub fn key(self) -> &'static str {
        match self {
            Category::Unofficial => "unofficial",
            Category::Graphics => "graphics",
            Category::Flavor => "flavor",
            Category::Shipsets => "shipsets",
            Category::Galaxy => "galaxy",
            Category::Gameplay => "gameplay",
            Category::Overhaul => "overhaul",
            Category::Patches => "patches",
            Category::Ui => "ui",
            Category::UiPatches => "ui-patches",
            Category::Universal => "universal",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }

    pub fn title(self) -> &'static str {
        match self {
            Category::Unofficial => "Unofficial Patches",
            Category::Graphics => "Graphics & Sound",
            Category::Flavor => "Flavor & Cosmetics",
            Category::Shipsets => "Shipsets",
            Category::Galaxy => "Galaxy Generation",
            Category::Gameplay => "Gameplay",
            Category::Overhaul => "Large Overhauls",
            Category::Patches => "Compatibility Patches",
            Category::Ui => "UI Mods",
            Category::UiPatches => "UI Patches",
            Category::Universal => "Universal Patches",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Category::Unofficial => "Fixes to the base game, loaded first so every other mod builds on them.",
            Category::Graphics => "Models, effects, backgrounds and sounds.",
            Category::Flavor => "Portraits, name lists, flags, pre-scripted empires and text.",
            Category::Shipsets => "Ship models and their graphical cultures.",
            Category::Galaxy => "Galaxy shapes, systems, planets and fallen empires.",
            Category::Gameplay => "Mods that change the rules: buildings, events, civics, technologies and more.",
            Category::Overhaul => "Very large mods that change much of the game.",
            Category::Patches => "Patches between two or more mods, below the mods they patch.",
            Category::Ui => "Interface mods, below gameplay mods so their interface files win.",
            Category::UiPatches => "Add-ons and compatibility patches for the UI mods.",
            Category::Universal => "Universal and global patches, loaded last to override everything.",
        }
    }
}

/// The folders a mod ships, to tell what kind of mod it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// Top-level folders, lower case.
    pub top: BTreeSet<String>,
    /// Folders inside `common/`.
    pub common: BTreeSet<String>,
    /// Folders inside `gfx/`.
    pub gfx: BTreeSet<String>,
}

impl Profile {
    /// Lists the folders under a mod's files at `dir`.
    pub fn read(dir: &Path) -> Self {
        let list = |dir: &Path| -> BTreeSet<String> {
            std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                        .map(|e| e.file_name().to_string_lossy().to_lowercase())
                        .filter(|n| !n.starts_with('.'))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self { top: list(dir), common: list(&dir.join("common")), gfx: list(&dir.join("gfx")) }
    }

    fn has(&self, folder: &str) -> bool {
        self.top.contains(folder)
    }
}

/// `common/` folders that only change looks and names.
const COSMETIC: &[&str] = &[
    "graphical_culture",
    "name_lists",
    "random_names",
    "portrait_sets",
    "portrait_categories",
    "species_classes",
    "species_names",
    "prescripted_flags",
    "start_screen_messages",
    "diplo_phrases",
    "frontend_backgrounds",
    "solar_system_initializers",
];
/// `common/` folders that shape the galaxy.
const GALAXY: &[&str] = &["solar_system_initializers", "planet_classes", "star_classes", "planet_modifiers", "asteroid_belts", "fallen_empires", "galaxy_shapes"];
/// More `common/` folders than this make a large overhaul.
const OVERHAUL_FOLDERS: usize = 60;

/// Where a mod goes and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Index into the mods.
    pub index: usize,
    pub category: Category,
    /// The mod it sits directly below, for sub-mods.
    pub parent: Option<usize>,
    pub reason: String,
    /// Whether the user chose the category.
    pub chosen: bool,
}

/// The enabled mods in smart order, each with where it went and why. The
/// mod `pinned` picks (the merged patch), if enabled, stays last.
/// `profiles` holds each enabled mod's folders, `overrides` the categories
/// the user chose, by mod id.
pub fn plan(mods: &[Mod], enabled: &[usize], profiles: &HashMap<usize, Profile>, overrides: &HashMap<String, Category>, pinned: &dyn Fn(&str) -> bool) -> Vec<Placement> {
    let by_name: HashMap<String, usize> = enabled.iter().map(|&i| (mods[i].descriptor.name.trim().to_lowercase(), i)).collect();
    let deps_of = |i: usize| -> Vec<usize> {
        let mut deps: Vec<usize> = Vec::new();
        for d in &mods[i].descriptor.dependencies {
            if let Some(&d) = by_name.get(&d.trim().to_lowercase())
                && d != i
                && !deps.contains(&d)
            {
                deps.push(d);
            }
        }
        deps
    };
    let empty = Profile::default();
    let base: HashMap<usize, (Category, String)> = enabled.iter().map(|&i| (i, base_category(&mods[i], profiles.get(&i).unwrap_or(&empty)))).collect();

    let mut placed: Vec<Placement> = Vec::new();
    for &i in enabled {
        if pinned(&mods[i].id) {
            continue;
        }
        if let Some(&category) = overrides.get(&mods[i].id) {
            placed.push(Placement { index: i, category, parent: None, reason: "Chosen by you".into(), chosen: true });
            continue;
        }
        let (category, reason) = base[&i].clone();
        let deps = deps_of(i);
        let name = |d: usize| format!("“{}”", mods[d].descriptor.name.trim());
        let placement = match deps.as_slice() {
            [parent] => Placement { index: i, category, parent: Some(*parent), reason: format!("Needs {}, so it goes directly below it", name(*parent)), chosen: false },
            [] => {
                let lower = mods[i].descriptor.name.to_lowercase();
                let patch_name = ["patch", "compat", "bridge"].iter().any(|w| lower.contains(w)) || plus_joined(&lower);
                match category {
                    Category::Ui if mentions_uiod(&lower) && !is_uiod(&lower) => Placement { index: i, category: Category::UiPatches, parent: None, reason: "An add-on or patch for UI Overhaul Dynamic".into(), chosen: false },
                    Category::Ui if patch_name => Placement { index: i, category: Category::UiPatches, parent: None, reason: "A patch for an interface mod".into(), chosen: false },
                    Category::Unofficial | Category::Universal | Category::Ui => Placement { index: i, category, parent: None, reason, chosen: false },
                    _ if patch_name && lower.contains("patch") => Placement { index: i, category: Category::Patches, parent: None, reason: "Its name says it's a patch".into(), chosen: false },
                    _ => Placement { index: i, category, parent: None, reason, chosen: false },
                }
            }
            many => {
                let names = many.iter().map(|&d| name(d)).collect::<Vec<_>>().join(", ");
                let ui = category == Category::Ui || many.iter().any(|d| base[d].0 == Category::Ui);
                let category = match category {
                    Category::Universal | Category::Unofficial => category,
                    _ if ui => Category::UiPatches,
                    _ => Category::Patches,
                };
                Placement { index: i, category, parent: None, reason: format!("Patches {names}"), chosen: false }
            }
        };
        placed.push(placement);
    }
    // A sub-mod sits with its parent, so it shares its section, through
    // chains of sub-mods too.
    for _ in 0..placed.len() {
        let section: HashMap<usize, Category> = placed.iter().map(|p| (p.index, p.category)).collect();
        let mut changed = false;
        for p in &mut placed {
            if let Some(&category) = p.parent.and_then(|parent| section.get(&parent))
                && p.category != category
            {
                p.category = category;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Sections in order, sorted by name; each sub-mod right below its parent.
    let key = |i: usize| (mods[i].descriptor.name.trim().to_lowercase(), mods[i].id.clone());
    let mut tops: Vec<&Placement> = placed.iter().filter(|p| p.parent.is_none()).collect();
    tops.sort_by_cached_key(|p| (p.category, key(p.index)));
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for p in placed.iter().filter(|p| p.parent.is_some()) {
        children.entry(p.parent.unwrap()).or_default().push(p.index);
    }
    for list in children.values_mut() {
        list.sort_by_cached_key(|&i| key(i));
    }
    fn emit(i: usize, children: &HashMap<usize, Vec<usize>>, seen: &mut HashSet<usize>, order: &mut Vec<usize>) {
        if !seen.insert(i) {
            return;
        }
        order.push(i);
        for &c in children.get(&i).into_iter().flatten() {
            emit(c, children, seen, order);
        }
    }
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    for p in &tops {
        emit(p.index, &children, &mut seen, &mut order);
    }
    // Sub-mods whose parents need them in turn: a dependency cycle.
    let mut rest: Vec<usize> = placed.iter().map(|p| p.index).filter(|i| !seen.contains(i)).collect();
    rest.sort_by_cached_key(|&i| key(i));
    order.extend(rest);
    let mut order = checks::sort_by_dependencies(mods, &order);
    if let Some(&p) = enabled.iter().find(|&&i| pinned(&mods[i].id)) {
        order.push(p);
    }

    let mut by_index: HashMap<usize, Placement> = placed.into_iter().map(|p| (p.index, p)).collect();
    order
        .into_iter()
        .filter_map(|i| {
            by_index.remove(&i).or_else(|| pinned(&mods[i].id).then(|| Placement { index: i, category: Category::Universal, parent: None, reason: "The merged patch always loads last".into(), chosen: false }))
        })
        .collect()
}

/// Mods next to each other in the load order that share a category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Where it starts in the load order, and how many mods it holds.
    pub start: usize,
    pub len: usize,
    pub category: Category,
    /// Smart Sort would put these mods elsewhere: they were moved by hand.
    pub manual: bool,
}

/// The runs of `order` (mod indices, as loaded), by the category each mod
/// has in `placed`, the Smart Sort plan for the same mods. A run is manual
/// when none of its mods follow the plan's order: those kept are the
/// longest part of the load order that does, so one mod dragged away from
/// its section is the one marked, not the section it left. The order
/// within a section doesn't count.
pub fn runs(order: &[usize], placed: &[Placement]) -> Vec<Run> {
    // Each mod's section in the plan, counted along it, so a section that
    // dependencies split in two is two places.
    let mut place: HashMap<usize, (usize, Category)> = HashMap::new();
    let mut section = 0;
    for (at, p) in placed.iter().enumerate() {
        if at > 0 && placed[at - 1].category != p.category {
            section += 1;
        }
        place.insert(p.index, (section, p.category));
    }
    let keyed: Vec<(usize, Category)> = order.iter().filter_map(|i| place.get(i).copied()).collect();
    if keyed.len() != order.len() {
        return Vec::new();
    }
    let kept = longest_non_decreasing(&keyed.iter().map(|k| k.0).collect::<Vec<_>>());

    let mut runs: Vec<Run> = Vec::new();
    for (at, &(_, category)) in keyed.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if run.category == category => {
                run.len += 1;
                run.manual &= !kept[at];
            }
            _ => runs.push(Run { start: at, len: 1, category, manual: !kept[at] }),
        }
    }
    runs
}

/// Which items are in a longest non-decreasing subsequence of `keys`.
fn longest_non_decreasing(keys: &[usize]) -> Vec<bool> {
    // `tails[l]` ends the best subsequence of length l + 1 found so far.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; keys.len()];
    for (i, &k) in keys.iter().enumerate() {
        let l = tails.partition_point(|&t| keys[t] <= k);
        previous[i] = l.checked_sub(1).map(|l| tails[l]);
        if l == tails.len() {
            tails.push(i);
        } else {
            tails[l] = i;
        }
    }
    let mut kept = vec![false; keys.len()];
    let mut at = tails.last().copied();
    while let Some(i) = at {
        kept[i] = true;
        at = previous[i];
    }
    kept
}

/// Whether a name joins two mods with `+`, as in `MECR+UIOD`.
pub(crate) fn plus_joined(lower: &str) -> bool {
    lower.split('+').filter(|part| part.chars().any(char::is_alphanumeric)).count() >= 2
}

fn mentions_uiod(lower: &str) -> bool {
    lower.contains("ui overhaul") || lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == "uiod")
}

/// UI Overhaul Dynamic itself, rather than one of its add-ons.
fn is_uiod(lower: &str) -> bool {
    matches!(lower.trim(), "ui overhaul dynamic" | "uiod")
}

/// What a mod is on its own, before its dependencies are considered.
fn base_category(m: &Mod, p: &Profile) -> (Category, String) {
    let name = m.descriptor.name.trim();
    let lower = name.to_lowercase();
    let tagged = |t: &str| m.descriptor.tags.iter().any(|tag| tag.eq_ignore_ascii_case(t));
    if name.starts_with('~') {
        return (Category::Unofficial, "Its name starts with ~".into());
    }
    if lower.contains("unofficial patch") {
        return (Category::Unofficial, "An unofficial patch".into());
    }
    if name.starts_with('!') {
        return (Category::Universal, "Its name starts with !, which goes towards the bottom".into());
    }
    if (lower.contains("universal") || lower.contains("global")) && ["patch", "fix", "compat"].iter().any(|w| lower.contains(w)) || lower.contains("merger of rules") {
        return (Category::Universal, "A universal patch".into());
    }
    if mentions_uiod(&lower) {
        return (Category::Ui, "Part of UI Overhaul Dynamic".into());
    }

    let events = p.has("events");
    let mechanics: Vec<&String> = p.common.iter().filter(|c| !COSMETIC.contains(&c.as_str()) && !GALAXY.contains(&c.as_str())).collect();
    let cosmetic_only = !events && mechanics.is_empty();
    let interface = p.has("interface") || p.has("fonts");
    if p.top.is_empty() {
        return (Category::Gameplay, "Its files couldn't be read".into());
    }
    let ui_gfx = |g: &String| ["interface", "fonts", "keyicons", "replace", "shipview"].contains(&g.as_str()) || g.starts_with("ui_");
    if interface && cosmetic_only && p.common.iter().all(|c| c == "frontend_backgrounds") && p.gfx.iter().all(ui_gfx) {
        return (Category::Ui, "Only changes the interface".into());
    }
    if p.common.len() >= OVERHAUL_FOLDERS || tagged("Total Conversion") {
        return (Category::Overhaul, format!("Changes {} kinds of game objects", p.common.len()));
    }
    if cosmetic_only {
        let galaxy = p.common.iter().any(|c| GALAXY.contains(&c.as_str()) && c != "solar_system_initializers") || p.has("map");
        let flavor = ["name_lists", "portrait_sets", "species_classes", "prescripted_flags", "species_names", "random_names"].iter().any(|c| p.common.contains(*c))
            || p.has("prescripted_countries")
            || p.has("flags")
            || p.gfx.contains("portraits") && !p.gfx.contains("models");
        let ships = lower.contains("ship") || tagged("Spaceships") && (p.gfx.contains("models") || p.common.contains("graphical_culture"));
        if ships {
            return (Category::Shipsets, "Changes ship models".into());
        }
        if galaxy && !flavor {
            return (Category::Galaxy, "Changes how the galaxy is made".into());
        }
        if flavor || p.gfx.contains("portraits") || (p.top.iter().all(|t| t == "localisation" || t == "localisation_synced")) {
            return (Category::Flavor, "Changes portraits, names or text".into());
        }
        return (Category::Graphics, if p.top.iter().all(|t| t == "sound" || t == "music") { "Only changes sounds".into() } else { "Only changes graphics".into() });
    }
    // Species packs ship a few scripts with their portraits.
    if (p.common.contains("portrait_sets") || p.common.contains("species_classes")) && mechanics.len() <= 4 {
        return (Category::Flavor, "Adds species: portraits, names and the like".into());
    }
    let galaxy_only = !events && mechanics.iter().all(|c| ["defines", "scripted_effects", "scripted_triggers"].contains(&c.as_str())) && p.common.iter().any(|c| GALAXY.contains(&c.as_str()));
    if galaxy_only {
        return (Category::Galaxy, "Changes how the galaxy is made".into());
    }
    let sample: Vec<&str> = mechanics.iter().take(3).map(|s| s.as_str()).collect();
    let what = if sample.is_empty() { "events".to_owned() } else { sample.join(", ").replace('_', " ") };
    (Category::Gameplay, format!("Changes game rules ({what}…)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::{Descriptor, Source};

    fn m(name: &str, deps: &[&str]) -> Mod {
        Mod {
            id: format!("mod/{}.mod", name.to_lowercase().replace(' ', "_")),
            descriptor: Descriptor { name: name.into(), dependencies: deps.iter().map(|s| s.to_string()).collect(), ..Default::default() },
            content: None,
            source: Source::Local,
            load_position: None,
            error: None,
            steam: None,
            requires: Vec::new(),
        }
    }

    fn profile(top: &[&str], common: &[&str], gfx: &[&str]) -> Profile {
        let set = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        let mut top: BTreeSet<String> = set(top);
        if !common.is_empty() {
            top.insert("common".into());
        }
        if !gfx.is_empty() {
            top.insert("gfx".into());
        }
        Profile { top, common: set(common), gfx: set(gfx) }
    }

    #[test]
    fn sorts_like_the_guides() {
        let gameplay = profile(&["events", "localisation"], &["buildings", "technology", "decisions"], &["interface"]);
        let many: Vec<String> = (0..70).map(|i| format!("folder_{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let mods = vec![
            (m("UI Overhaul Dynamic", &[]), profile(&["interface", "localisation"], &[], &["interface", "ui_overhaul_qhd"])),
            (m("Planetary Diversity", &[]), gameplay.clone()),
            (m("!!!Universal Resource Patch", &[]), profile(&["interface"], &[], &[])),
            (m("Planetary Diversity - Exotic Worlds", &["Planetary Diversity"]), gameplay.clone()),
            (m("Animated Portraits", &[]), profile(&["localisation", "prescripted_countries"], &["name_lists", "portrait_sets", "species_classes"], &["models", "portraits"])),
            (m("UI Overhaul Dynamic + Planetary Diversity", &["UI Overhaul Dynamic", "Planetary Diversity"]), profile(&["interface"], &[], &["interface"])),
            (m("MCF Bridge: Planetary Diversity", &["Modular Core Framework", "Planetary Diversity"]), profile(&[], &["inline_scripts"], &[])),
            (m("Modular Core Framework", &[]), gameplay.clone()),
            (m("~ Unofficial Fixes", &[]), gameplay.clone()),
            (m("Kleom's Colorful Ships", &[]), profile(&[], &[], &["models"])),
            (m("Gigastructural Engineering", &[]), profile(&["events"], &many, &[])),
            (m("Whiter Stars", &[]), profile(&["map"], &[], &[])),
            (m("UI Overhaul Dynamic - Tiny Options", &["UI Overhaul Dynamic"]), profile(&["interface"], &[], &[])),
            (m("Epic Explosions", &[]), profile(&[], &[], &["particles"])),
            (m("Kek's Origins - PD Patch", &[]), profile(&["events"], &["scripted_effects"], &[])),
            (m("MECR+UIOD", &[]), profile(&["interface"], &[], &[])),
            (m("DrukhalManager Merged Patch", &[]), profile(&["interface"], &[], &[])),
        ];
        let (mods, profiles): (Vec<Mod>, Vec<Profile>) = mods.into_iter().unzip();
        let enabled: Vec<usize> = (0..mods.len()).collect();
        let profiles: HashMap<usize, Profile> = profiles.into_iter().enumerate().collect();
        let pinned = &|id: &str| id == "mod/drukhalmanager_merged_patch.mod";
        let names = |placed: &[Placement]| placed.iter().map(|p| mods[p.index].descriptor.name.as_str()).collect::<Vec<_>>();
        let placed = plan(&mods, &enabled, &profiles, &HashMap::new(), pinned);
        assert_eq!(names(&placed), [
            "~ Unofficial Fixes",
            "Epic Explosions",
            "Animated Portraits",
            "Kleom's Colorful Ships",
            "Whiter Stars",
            "Modular Core Framework",
            "Planetary Diversity",
            "Planetary Diversity - Exotic Worlds",
            "Gigastructural Engineering",
            "Kek's Origins - PD Patch",
            "MCF Bridge: Planetary Diversity",
            "UI Overhaul Dynamic",
            "UI Overhaul Dynamic - Tiny Options",
            "MECR+UIOD",
            "UI Overhaul Dynamic + Planetary Diversity",
            "!!!Universal Resource Patch",
            "DrukhalManager Merged Patch",
        ]);
        let of = |name: &str| placed.iter().find(|p| mods[p.index].descriptor.name == name).unwrap();
        assert_eq!(of("Planetary Diversity - Exotic Worlds").parent, Some(1));
        assert_eq!(of("MCF Bridge: Planetary Diversity").reason, "Patches “Modular Core Framework”, “Planetary Diversity”");

        // A category the user chose wins, and a sub-mod of it follows along.
        let overrides: HashMap<String, Category> = [(mods[1].id.clone(), Category::Galaxy)].into();
        let placed = plan(&mods, &enabled, &profiles, &overrides, pinned);
        let at = |name: &str| names(&placed).iter().position(|n| *n == name).unwrap();
        assert_eq!(at("Planetary Diversity"), at("Kleom's Colorful Ships") + 1, "first in Galaxy Generation");
        assert_eq!(at("Planetary Diversity - Exotic Worlds"), at("Planetary Diversity") + 1);
        assert_eq!(at("Whiter Stars"), at("Planetary Diversity - Exotic Worlds") + 1);
        assert!(placed.iter().find(|p| p.index == 1).unwrap().chosen);
    }

    #[test]
    fn finds_runs_moved_by_hand() {
        use Category::*;
        let plan: Vec<Placement> = [Unofficial, Graphics, Graphics, Graphics, Gameplay, Gameplay, Ui]
            .into_iter()
            .enumerate()
            .map(|(index, category)| Placement { index, category, parent: None, reason: String::new(), chosen: false })
            .collect();
        let run = |start, len, category, manual| Run { start, len, category, manual };

        // As sorted, with the order inside a section changed: nothing manual.
        assert_eq!(runs(&[0, 3, 1, 2, 5, 4, 6], &plan), [
            run(0, 1, Unofficial, false),
            run(1, 3, Graphics, false),
            run(4, 2, Gameplay, false),
            run(6, 1, Ui, false),
        ]);
        // A gameplay mod dragged into the graphics: only it is marked, and
        // the graphics on either side are runs of their own.
        assert_eq!(runs(&[0, 1, 4, 2, 3, 5, 6], &plan), [
            run(0, 1, Unofficial, false),
            run(1, 1, Graphics, false),
            run(2, 1, Gameplay, true),
            run(3, 2, Graphics, false),
            run(5, 1, Gameplay, false),
            run(6, 1, Ui, false),
        ]);
        // A UI mod dragged to the top.
        assert_eq!(runs(&[6, 0, 1, 2, 3, 4, 5], &plan)[0], run(0, 1, Ui, true));
        // Mods the plan doesn't know of: no runs rather than wrong ones.
        assert!(runs(&[0, 9], &plan).is_empty());
    }

    #[test]
    fn reads_folders() {
        let root = std::env::temp_dir().join(format!("drukhal-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["common/buildings", "common/Name_Lists", "gfx/models", "events", ".git"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("descriptor.mod"), "").unwrap();
        let p = Profile::read(&root);
        assert_eq!(p.top.iter().map(String::as_str).collect::<Vec<_>>(), ["common", "events", "gfx"]);
        assert_eq!(p.common.iter().map(String::as_str).collect::<Vec<_>>(), ["buildings", "name_lists"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}


