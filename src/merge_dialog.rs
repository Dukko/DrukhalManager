//! Reading every version of one conflicting object, and checking a merged
//! version before it goes into the patch. Used by the Conflicts page and the
//! folder and fix dialogs.

use std::path::PathBuf;

use crate::merge::patch::{Print, VANILLA};
use crate::merge::source::{self, Source};
use crate::merge::validate::validate_object_since;

pub(crate) struct Loaded {
    pub vanilla: Option<Source>,
    pub sources: Vec<Source>,
    pub missing: Vec<String>,
    /// Fingerprints of the versions found, to notice later changes.
    pub prints: Vec<Print>,
}

impl Loaded {
    /// Whether there is anything to merge: two mods, or a mod and the game.
    pub fn mergeable(&self) -> bool {
        self.sources.len() >= 2 || (self.vanilla.is_some() && !self.sources.is_empty())
    }
}

pub(crate) fn load(category: &str, key: &str, game_dir: Option<&std::path::Path>, origins: Vec<(String, String, PathBuf, String)>) -> Loaded {
    load_with(category, key, game_dir.and_then(|dir| source::vanilla(dir, category, key)), origins)
}

/// Reads each mod's version of the object, given the game's version.
/// `origins` are (mod id, name, folder, file), in load order.
pub(crate) fn load_with(category: &str, key: &str, vanilla: Option<Source>, origins: Vec<(String, String, PathBuf, String)>) -> Loaded {
    let mut prints: Vec<Print> = vanilla.iter().map(|v| Print::new(VANILLA, "Vanilla", &v.file, &v.text)).collect();
    let mut sources = Vec::new();
    let mut missing = Vec::new();
    for (mod_id, label, root, file) in origins {
        let text = std::fs::read(root.join(&file)).ok().and_then(|bytes| source::extract(category, key, &String::from_utf8_lossy(&bytes)));
        match text {
            Some(text) => {
                prints.push(Print::new(&mod_id, &label, &file, &text));
                sources.push(Source { label, file, text });
            }
            None => missing.push(label),
        }
    }
    Loaded { vanilla, sources, missing, prints }
}

/// Checks the edited script before it goes into the patch: it must pass the
/// checks an AI merge gets, and still define the object being merged. It need
/// not keep what each mod added; that is the user's call. Returns the text to
/// save; for localisation, comments and blank lines are dropped.
pub fn validate(category: &str, key: &str, text: &str) -> Result<String, String> {
    let validation = validate_object_since(category, key, text, None, &[], &[], None);
    match validation.error_summary() {
        Some(problems) => Err(problems),
        None => Ok(validation.normalized),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_save_rejects_duplicate_single_valued_key() {
        let err = validate("common/buildings", "building_lab", "building_lab = { cost = { a = 1 } cost = { a = 2 } }").unwrap_err();
        assert!(err.contains("`cost`"), "{err}");
    }

    #[test]
    fn manual_save_may_drop_a_mods_addition() {
        assert!(validate("common/buildings", "building_lab", "building_lab = { cost = { a = 1 } }").is_ok());
    }

    #[test]
    fn validates_merged_scripts() {
        assert!(validate("common/buildings", "building_lab", "@c = 1\nbuilding_lab = { cost = @c }").is_ok());
        assert!(validate("common/buildings", "building_lab", "building_lab = {").unwrap_err().contains("syntax error"));
        assert!(validate("common/buildings", "building_lab", "building_other = {}").unwrap_err().contains("must define"));
        assert!(validate("common/buildings", "building_lab", "building_lab = {}\nbuilding_extra = {}").unwrap_err().contains("nothing else"));
        assert!(validate("events", "ns.1", "country_event = { id = ns.1 }\ncountry_event = { id = ns.2 }").is_err());
        assert!(validate("events", "ns.1", "country_event = { id = ns.1 }").is_ok());
        assert_eq!(validate("localisation", "l_english:lab", "# merged: kept B\n lab:0 \"Lab\" \n").unwrap(), "lab:0 \"Lab\"");
        assert!(validate("localisation", "l_english:lab", "lab:0 \"A\"\nlab:0 \"B\"").is_err());
        assert!(validate("localisation", "l_english:lab", "other:0 \"x\"").is_err());

        // Scripted effects definitions can contain control blocks like `if` and `switch`
        let scripted_def = "mzilli_star_crisis_heat_planets = { if = { limit = { is_terraforming = yes } cancel_terraformation = yes } }";
        assert!(validate("common/scripted_effects", "mzilli_star_crisis_heat_planets", scripted_def).is_ok());

        // A scripted variable is its own object, and may bring other
        // `@variable` lines it needs.
        assert!(validate("common/scripted_variables", "@my_cost", "@my_cost = 100").is_ok());
        assert!(validate("common/scripted_variables", "@my_cost", "@helper = 2\n@my_cost = 100").is_ok());
        assert!(validate("common/scripted_variables", "@my_cost", "@other = 100").is_err());
    }

    #[test]
    fn manual_save_allows_conditional_and_identical_repeats() {
        let guarded_costs = "building_gaia = { resources = { category = planet_buildings cost = { trigger = { has_trait = a } energy = 1 } cost = { trigger = { has_trait = b } energy = 2 } cost = { energy = 5 } } }";
        assert!(validate("common/buildings", "building_gaia", guarded_costs).is_ok());
        let guarded_pictures = "country_event = { id = ns.1 picture = { trigger = { a = b } picture = GFX_a } picture = { trigger = { c = d } picture = GFX_b } picture = GFX_c }";
        assert!(validate("events", "ns.1", guarded_pictures).is_ok());
        assert!(validate("common/technology", "tech_a", "tech_a = { start_tech = yes start_tech = yes }").is_ok());
        assert!(validate("common/buildings", "building_lab", "building_lab = { cost = { a = 1 } cost = { a = 1 } }").is_ok());
    }

    #[test]
    fn manual_save_compares_repeats_under_the_same_guard() {
        let same_guard_other_body = "building_lab = { cost = { trigger = { a = b } energy = 1 } cost = { trigger = { a = b } energy = 2 } }";
        assert!(validate("common/buildings", "building_lab", same_guard_other_body).is_err());
        let same_guard_same_body = "building_lab = { cost = { trigger = { a = b } energy = 1 } cost = { trigger = { a = b } energy = 1 } }";
        assert!(validate("common/buildings", "building_lab", same_guard_same_body).is_ok());
    }

    #[test]
    fn manual_save_rejects_unguarded_conflicting_repeats() {
        assert!(validate("common/technology", "tech_a", "tech_a = { cost = 100 cost = 200 }").is_err());
        assert!(validate("common/buildings", "building_lab", "building_lab = { cost = { a = 1 } cost = { a = 2 } }").is_err());
    }

    fn script_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<_> = read.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                script_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "txt") {
                out.push(path);
            }
        }
    }

    /// Runs both save validators over the base game's own scripts, which are
    /// valid, and prints what they reject.
    /// `DRUKHAL_STELLARIS_GAME=<game folder> cargo test save_validation_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn save_validation_survey() {
        let Some(game) = std::env::var_os("DRUKHAL_STELLARIS_GAME").map(PathBuf::from) else {
            println!("SURVEY skipped: set DRUKHAL_STELLARIS_GAME");
            return;
        };
        let mut files = Vec::new();
        script_files(&game.join("common"), &mut files);
        script_files(&game.join("events"), &mut files);
        let (mut objects, mut object_rejects, mut file_rejects) = (0, 0, 0);
        for path in &files {
            let relative = path.strip_prefix(&game).unwrap().to_string_lossy().replace('\\', "/");
            if relative == "common/HOW_TO_MAKE_NEW_SHIPS.txt" {
                continue;
            }
            let text = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
            let category = crate::merge::file_merge::category(&relative);
            if let Err(e) = crate::merge::file_merge::validate(&category, &text) {
                file_rejects += 1;
                println!("FILE {relative}: {e}");
            }
            for (key, object) in source::index(&category, &text) {
                objects += 1;
                if let Err(e) = validate(&category, &key, &object) {
                    object_rejects += 1;
                    println!("OBJECT {relative} {key}: {e}");
                }
            }
        }
        println!("SURVEY {} files ({file_rejects} rejected), {objects} objects ({object_rejects} rejected)", files.len());
    }
}
