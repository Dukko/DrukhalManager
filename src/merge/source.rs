//! Cutting a single object's definition out of a mod's (or the game's) files,
//! with its original comments and formatting.

use std::collections::HashMap;
use std::path::Path;

use crate::paradox::script::{self, Entries, Entry};

/// One version of an object, as written in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The mod's name, or "Vanilla".
    pub label: String,
    pub file: String,
    pub text: String,
}

/// Finds the definition of `key` in `src`. For script objects this includes
/// any file-local `@variables` the definition uses, since it can't be moved
/// to another file without them.
pub fn extract(category: &str, key: &str, src: &str) -> Option<String> {
    if category == "localisation" {
        return localisation_line(key, src);
    }
    if !src.contains(key.rsplit(':').next().unwrap_or(key)) {
        return None;
    }
    let entries = script::parse_with_spans(src).ok()?;
    let found = entries.iter().find(|(entry, _)| match entry {
        Entry::Assign { key: k, value, .. } if category == "events" => k != "namespace" && value.as_block().and_then(|b| b.scalar("id")) == Some(key),
        Entry::Assign { key: k, .. } => k == key,
        Entry::Item(_) => false,
    })?;
    let text = &src[found.1.clone()];

    let mut parts: Vec<&str> = entries
        .iter()
        .filter_map(|(entry, range)| match entry {
            Entry::Assign { key: var, .. } if var.starts_with('@') && uses_variable(text, var) => Some(&src[range.clone()]),
            _ => None,
        })
        .collect();
    parts.push(text);
    Some(parts.join("\n"))
}

/// Every object `src` defines, as [`extract`] would cut each one out, by
/// key. Parses the file once, for looking up many objects in it.
pub fn index(category: &str, src: &str) -> HashMap<String, String> {
    let mut found = HashMap::new();
    if category == "localisation" {
        let mut current = "";
        for line in src.lines() {
            let line = line.trim_start_matches('\u{feff}');
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            if !line.starts_with([' ', '\t']) && trimmed.ends_with(':') {
                current = trimmed.trim_end_matches(':');
                continue;
            }
            if let Some((key, _)) = trimmed.split_once(':') {
                found.entry(format!("{current}:{key}")).or_insert_with(|| trimmed.to_owned());
            }
        }
        return found;
    }
    let Ok(entries) = script::parse_with_spans(src) else { return found };
    let variables: Vec<(&str, &str)> = entries
        .iter()
        .filter_map(|(entry, range)| match entry {
            Entry::Assign { key, .. } if key.starts_with('@') => Some((key.as_str(), &src[range.clone()])),
            _ => None,
        })
        .collect();
    for (entry, range) in &entries {
        let Entry::Assign { key, value, .. } = entry else { continue };
        let name = if category == "events" {
            if key == "namespace" {
                continue;
            }
            match value.as_block().and_then(|b| b.scalar("id")) {
                Some(id) => id.to_owned(),
                None => continue,
            }
        } else if key.starts_with('@') {
            continue;
        } else {
            key.clone()
        };
        if found.contains_key(&name) {
            continue;
        }
        let text = &src[range.clone()];
        let mut parts: Vec<&str> = variables.iter().filter(|(var, _)| uses_variable(text, var)).map(|(_, definition)| *definition).collect();
        parts.push(text);
        found.insert(name, parts.join("\n"));
    }
    found
}

/// Calls `f` with each wanted `language:key` entry in a localisation file
/// and its line, without allocating for the rest.
fn localisation_lines(src: &str, wanted: &std::collections::HashSet<&str>, mut f: impl FnMut(String, &str)) {
    let mut current = "";
    let mut buffer = String::new();
    for line in src.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t']) && trimmed.ends_with(':') {
            current = trimmed.trim_end_matches(':');
            continue;
        }
        if let Some((key, _)) = trimmed.split_once(':') {
            buffer.clear();
            buffer.push_str(current);
            buffer.push(':');
            buffer.push_str(key);
            if wanted.contains(buffer.as_str()) {
                f(buffer.clone(), trimmed);
            }
        }
    }
}

fn uses_variable(text: &str, var: &str) -> bool {
    text.match_indices(var).any(|(i, _)| {
        let after = text[i + var.len()..].chars().next();
        !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// `key` is `l_english:some_key`, as produced by the conflict scan.
fn localisation_line(key: &str, src: &str) -> Option<String> {
    let (language, key) = key.split_once(':')?;
    let mut current = "";
    for line in src.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t']) && trimmed.ends_with(':') {
            current = trimmed.trim_end_matches(':');
            continue;
        }
        if current == language && trimmed.split_once(':').is_some_and(|(k, _)| k == key) {
            return Some(trimmed.to_owned());
        }
    }
    None
}

/// The base game's definition of the object, if it has one.
pub fn vanilla(game_dir: &Path, category: &str, key: &str) -> Option<Source> {
    let (dir, recursive, extension) = match category {
        "localisation" => (game_dir.join("localisation"), true, "yml"),
        _ => (game_dir.join(category), false, "txt"),
    };
    let mut files = Vec::new();
    collect(&dir, recursive, extension, &mut files);
    files.sort();
    files.into_iter().find_map(|path| {
        let src = String::from_utf8_lossy(&std::fs::read(&path).ok()?).into_owned();
        let text = extract(category, key, &src)?;
        let file = path.strip_prefix(game_dir).ok()?.to_string_lossy().into_owned();
        Some(Source { label: "Vanilla".into(), file, text })
    })
}

/// The base game's definitions of several objects in one category, reading
/// each game file once. Missing ones are left out.
pub fn vanilla_many(game_dir: &Path, category: &str, keys: &[String]) -> std::collections::HashMap<String, Source> {
    let (dir, recursive, extension) = match category {
        "localisation" => (game_dir.join("localisation"), true, "yml"),
        _ => (game_dir.join(category), false, "txt"),
    };
    let mut files = Vec::new();
    collect(&dir, recursive, extension, &mut files);
    files.sort();
    if category == "localisation" {
        // Only the languages asked for; the game ships a dozen.
        let languages: std::collections::HashSet<String> = keys.iter().filter_map(|k| Some(format!("_{}.yml", k.split_once(':')?.0.to_lowercase()))).collect();
        files.retain(|f| f.file_name().is_some_and(|n| languages.iter().any(|l| n.to_string_lossy().to_lowercase().ends_with(l.as_str()))));
    }
    let wanted: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
    let mut found = HashMap::new();
    for path in files {
        if found.len() == wanted.len() {
            break;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let src = String::from_utf8_lossy(&bytes);
        let Some(file) = path.strip_prefix(game_dir).ok().map(|f| f.to_string_lossy().into_owned()) else { continue };
        if category == "localisation" {
            localisation_lines(&src, &wanted, |key, line| {
                found.entry(key).or_insert_with(|| Source { label: "Vanilla".into(), file: file.clone(), text: line.to_owned() });
            });
            continue;
        }
        // Parsing is the slow part: skip files that can't define any of them.
        let remaining: Vec<&&str> = wanted.iter().filter(|k| !found.contains_key(**k)).collect();
        if remaining.len() <= 64 && !remaining.iter().any(|k| src.contains(**k)) {
            continue;
        }
        for (key, text) in index(category, &src) {
            // The first file defining it wins, as with `vanilla`.
            if wanted.contains(key.as_str()) && !found.contains_key(&key) {
                found.insert(key, Source { label: "Vanilla".into(), file: file.clone(), text });
            }
        }
    }
    found
}

fn collect(dir: &Path, recursive: bool, extension: &str, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect(&path, recursive, extension, out);
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(extension)) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_objects_with_their_variables() {
        let src = "@cost = 10\n@unused = 1\n@cost_extra = 2\n# Lab\nbuilding_lab = {\n\tcost = @cost # cheap\n}\nbuilding_other = { cost = @unused }";
        assert_eq!(extract("common/buildings", "building_lab", src).unwrap(), "@cost = 10\nbuilding_lab = {\n\tcost = @cost # cheap\n}");
        assert_eq!(extract("common/buildings", "building_missing", src), None);
    }

    #[test]
    fn extracts_events_by_id() {
        let src = "namespace = ns\ncountry_event = { id = ns.1 }\nship_event = {\n\tid = ns.2\n\tis_triggered_only = yes\n}";
        assert_eq!(extract("events", "ns.2", src).unwrap(), "ship_event = {\n\tid = ns.2\n\tis_triggered_only = yes\n}");
    }

    #[test]
    fn indexes_like_extract() {
        let src = "@cost = 10\n@unused = 1\nbuilding_lab = {\n\tcost = @cost\n}\nbuilding_other = { cost = @unused }\nbuilding_lab = { second = yes }";
        let all = index("common/buildings", src);
        for key in ["building_lab", "building_other"] {
            assert_eq!(all.get(key), extract("common/buildings", key, src).as_ref());
        }
        assert_eq!(all.len(), 2);
        let events = "namespace = ns\ncountry_event = { id = ns.1 }\nship_event = {\n\tid = ns.2\n}";
        assert_eq!(index("events", events).get("ns.2"), extract("events", "ns.2", events).as_ref());
        let loc = "\u{feff}l_english:\n building_lab:0 \"Lab\"\nl_german:\n building_lab:0 \"Labor\"\n";
        let all = index("localisation", loc);
        assert_eq!(all.get("l_german:building_lab"), extract("localisation", "l_german:building_lab", loc).as_ref());
        assert_eq!(all.get("l_english:building_lab").map(String::as_str), Some("building_lab:0 \"Lab\""));
    }

    #[test]
    fn extracts_localisation_lines() {
        let src = "\u{feff}l_english:\r\n# Buildings:\r\n building_lab:0 \"Lab\"\r\n building_lab_desc:0 \"A lab\"\nl_german:\n building_lab:0 \"Labor\"\n";
        assert_eq!(extract("localisation", "l_english:building_lab", src).unwrap(), "building_lab:0 \"Lab\"");
        assert_eq!(extract("localisation", "l_german:building_lab", src).unwrap(), "building_lab:0 \"Labor\"");
    }
}
