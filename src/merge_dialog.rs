//! Reading every version of one conflicting object, and checking a merged
//! version before it goes into the patch. Used by the Conflicts page and the
//! folder and fix dialogs.

use std::path::PathBuf;

use crate::merge::patch::{Print, VANILLA};
use crate::merge::source::{self, Source};
use crate::paradox::script;

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

/// Checks the edited script before it goes into the patch: it must parse,
/// and it must still define the object being merged. Returns the text to
/// save; for localisation, comments and blank lines are dropped.
pub fn validate(category: &str, key: &str, text: &str) -> Result<String, String> {
    if category == "localisation" {
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        let name = key.split_once(':').map_or(key, |(_, k)| k);
        return match lines[..] {
            [line] if line.split_once(':').is_some_and(|(k, _)| k == name) => Ok(line.to_owned()),
            _ => Err(format!("A localisation merge must be a single line starting with `{name}:`")),
        };
    }
    script::parse(text).map_err(|e| format!("The merged script has a syntax error at {e}"))?;
    if source::extract(category, key, text).is_none() {
        return Err(format!("The merged script must define `{key}`"));
    }
    Ok(text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_merged_scripts() {
        assert!(validate("common/buildings", "building_lab", "@c = 1\nbuilding_lab = { cost = @c }").is_ok());
        assert!(validate("common/buildings", "building_lab", "building_lab = {").unwrap_err().contains("syntax error"));
        assert!(validate("common/buildings", "building_lab", "building_other = {}").unwrap_err().contains("must define"));
        assert!(validate("events", "ns.1", "country_event = { id = ns.1 }").is_ok());
        assert_eq!(validate("localisation", "l_english:lab", "# merged: kept B\n lab:0 \"Lab\" \n").unwrap(), "lab:0 \"Lab\"");
        assert!(validate("localisation", "l_english:lab", "lab:0 \"A\"\nlab:0 \"B\"").is_err());
        assert!(validate("localisation", "l_english:lab", "other:0 \"x\"").is_err());
    }
}
