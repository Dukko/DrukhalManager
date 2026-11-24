//! Cutting a single object's definition out of a mod's (or the game's) files,
//! with its original comments and formatting.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::paradox::script::{self, Entry};

/// One version of an object, as written in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The mod's name, or "Vanilla".
    pub label: String,
    pub file: String,
    pub text: String,
}

/// The substring of `key` that must appear in a file defining it.
pub fn key_token(key: &str) -> &str {
    let check = key.rsplit(':').next().unwrap_or(key);
    check.split_once(" → ").map_or(check, |(from, _)| from)
}

/// Finds the definition of `key` in `src`. For script objects this includes
/// any file-local `@variables` the definition uses, since it can't be moved
/// to another file without them.
pub fn extract(category: &str, key: &str, src: &str) -> Option<String> {
    if category == "localisation" {
        return localisation_line(key, src);
    }
    if !src.contains(key_token(key)) {
        return None;
    }
    let entries = script::parse_with_spans(src).ok()?;
    extract_parsed(category, key, src, &entries)
}

/// As [`extract`], from `src` already parsed into `entries`, so one parse
/// can serve many lookups.
pub fn extract_parsed(category: &str, key: &str, src: &str, entries: &[(Entry, std::ops::Range<usize>)]) -> Option<String> {
    if category == "localisation" {
        return localisation_line(key, src);
    }
    let found = entries.iter().find(|(entry, _)| {
        crate::merge::conflicts::object_key_for_entry(category, entry).as_deref() == Some(key)
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

/// The key and text (as [`extract`] cuts it) of the top-level object in
/// `src` whose definition covers 1-based `line`. Events are keyed by `id`.
/// `None` for localisation, `@variable` lines, `namespace`, lines between
/// objects, or a file that doesn't parse.
pub fn object_at(category: &str, src: &str, line: usize) -> Option<(String, String)> {
    if category == "localisation" || line == 0 {
        return None;
    }
    let entries = script::parse_with_spans(src).ok()?;
    object_at_parsed(category, src, &entries, line)
}

/// As [`object_at`], from `src` already parsed into `entries`.
fn object_at_parsed(category: &str, src: &str, entries: &[(Entry, Range<usize>)], line: usize) -> Option<(String, String)> {
    if category == "localisation" || line == 0 {
        return None;
    }
    let start = if line == 1 { 0 } else { src.match_indices('\n').nth(line - 2)?.0 + 1 };
    let end = src[start..].find('\n').map_or(src.len(), |i| start + i);
    let (entry, _) = entries.iter().find(|(_, range)| range.start <= end && range.end > start)?;
    let key = crate::merge::conflicts::object_key_for_entry(category, entry)?;
    let text = extract_parsed(category, &key, src, entries)?;
    Some((key, text))
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
        let Some(name) = crate::merge::conflicts::object_key_for_entry(category, entry) else { continue };
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

/// The base game's files for `category`, in the order they're searched.
pub fn vanilla_files(game_dir: &Path, category: &str) -> Vec<std::path::PathBuf> {
    let (dir, recursive, extension) = match category {
        "localisation" => (game_dir.join("localisation"), true, "yml"),
        _ => (game_dir.join(category), false, "txt"),
    };
    let mut files = Vec::new();
    collect(&dir, recursive, extension, &mut files);
    files.sort();
    files
}

/// A file's top-level entries with their byte ranges.
type Parsed = Vec<(Entry, Range<usize>)>;

/// Source files read during one pass (a freshness check, a scan step),
/// each read and parsed at most once, since many objects come from the
/// same game and mod files.
#[derive(Default)]
pub struct Files {
    texts: RefCell<HashMap<PathBuf, Option<Rc<String>>>>,
    parsed: RefCell<HashMap<PathBuf, Option<Rc<Parsed>>>>,
}

impl Files {
    /// The file's text, read as UTF-8 with bad bytes replaced; `None` if
    /// it can't be read.
    pub fn text(&self, root: &Path, file: &str) -> Option<Rc<String>> {
        let path = root.join(file);
        if let Some(text) = self.texts.borrow().get(&path) {
            return text.clone();
        }
        let text = std::fs::read(&path).ok().map(|b| Rc::new(String::from_utf8_lossy(&b).into_owned()));
        self.texts.borrow_mut().insert(path, text.clone());
        text
    }

    /// The file's text and its top-level entries, parsed once; `None` if
    /// it can't be read or doesn't parse.
    fn parsed(&self, root: &Path, file: &str) -> Option<(Rc<String>, Rc<Parsed>)> {
        let text = self.text(root, file)?;
        let path = root.join(file);
        let cached = self.parsed.borrow().get(&path).cloned();
        let entries = match cached {
            Some(entries) => entries,
            None => {
                let entries = script::parse_with_spans(&text).ok().map(Rc::new);
                self.parsed.borrow_mut().insert(path, entries.clone());
                entries
            }
        }?;
        Some((text, entries))
    }

    /// The object's definition in the file, as [`extract`] finds it, but
    /// parsing each file only once.
    pub fn object(&self, root: &Path, file: &str, category: &str, key: &str) -> Option<String> {
        if category == "localisation" {
            return extract_parsed(category, key, &self.text(root, file)?, &[]);
        }
        // As `extract` does: files that can't define it aren't parsed.
        if !self.text(root, file)?.contains(key_token(key)) {
            return None;
        }
        let (text, entries) = self.parsed(root, file)?;
        extract_parsed(category, key, &text, &entries)
    }

    /// The object covering `line` in the file, as [`object_at`] finds it,
    /// but parsing each file only once.
    pub fn object_at(&self, root: &Path, file: &str, category: &str, line: usize) -> Option<(String, String)> {
        if category == "localisation" || line == 0 {
            return None;
        }
        let (text, entries) = self.parsed(root, file)?;
        object_at_parsed(category, &text, &entries, line)
    }
}

/// The base game's definition of the object, if it has one.
pub fn vanilla(game_dir: &Path, category: &str, key: &str) -> Option<Source> {
    vanilla_files(game_dir, category).into_iter().find_map(|path| {
        let src = String::from_utf8_lossy(&std::fs::read(&path).ok()?).into_owned();
        let text = extract(category, key, &src)?;
        let file = crate::platform::game_path(path.strip_prefix(game_dir).ok()?);
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
        let Some(file) = path.strip_prefix(game_dir).ok().map(crate::platform::game_path) else { continue };
        if category == "localisation" {
            localisation_lines(&src, &wanted, |key, line| {
                found.entry(key).or_insert_with(|| Source { label: "Vanilla".into(), file: file.clone(), text: line.to_owned() });
            });
            continue;
        }
        // Parsing is the slow part: skip files that can't define any of them.
        let remaining: Vec<&&str> = wanted.iter().filter(|k| !found.contains_key(**k)).collect();
        if remaining.len() <= 64 && !remaining.iter().any(|k| src.contains(key_token(k))) {
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
    fn finds_the_object_a_line_is_in() {
        let src = "@var = 3\nfirst = {\n\ta = 1\n}\n\nsecond = {\n\tb = @var\n}\n";
        let (key, text) = object_at("common/x", src, 7).unwrap();
        assert_eq!(key, "second");
        assert_eq!(text, "@var = 3\nsecond = {\n\tb = @var\n}");
        assert_eq!(object_at("common/x", src, 3).unwrap().0, "first");
        assert_eq!(object_at("common/x", src, 5), None);
        assert_eq!(object_at("common/x", src, 1), None);
        assert_eq!(object_at("common/x", src, 0), None);
        let events = "namespace = ns\ncountry_event = { id = ns.1 }\nship_event = {\n\tid = ns.2\n}\n";
        assert_eq!(object_at("events", events, 4).unwrap().0, "ns.2");
        assert_eq!(object_at("events", events, 1), None);
    }

    #[test]
    fn extracts_localisation_lines() {
        let src = "\u{feff}l_english:\r\n# Buildings:\r\n building_lab:0 \"Lab\"\r\n building_lab_desc:0 \"A lab\"\nl_german:\n building_lab:0 \"Labor\"\n";
        assert_eq!(extract("localisation", "l_english:building_lab", src).unwrap(), "building_lab:0 \"Lab\"");
        assert_eq!(extract("localisation", "l_german:building_lab", src).unwrap(), "building_lab:0 \"Labor\"");
    }

    #[test]
    fn extracts_inner_keyed_objects() {
        let sets = "component_set = { key = \"SET_1\" icon = \"GFX_1\" }\ncomponent_set = { key = \"SET_2\" icon = \"GFX_2\" }\n";
        assert_eq!(extract("common/component_sets", "SET_1", sets).as_deref(), Some("component_set = { key = \"SET_1\" icon = \"GFX_1\" }"));
        assert_eq!(extract("common/component_sets", "SET_2", sets).as_deref(), Some("component_set = { key = \"SET_2\" icon = \"GFX_2\" }"));
        assert_eq!(object_at("common/component_sets", sets, 2).unwrap().0, "SET_2");

        let tmpls = "@pow = 5\nutility_component_template = { key = \"UTIL_1\" power = @pow }\nweapon_component_template = { key = \"WEAP_1\" }\n";
        assert_eq!(extract("common/component_templates", "UTIL_1", tmpls).as_deref(), Some("@pow = 5\nutility_component_template = { key = \"UTIL_1\" power = @pow }"));
        assert_eq!(extract("common/component_templates", "WEAP_1", tmpls).as_deref(), Some("weapon_component_template = { key = \"WEAP_1\" }"));

        let amb = "ambient_object = { name = \"amb_x\" entity = \"ent_x\" }\n";
        assert_eq!(extract("common/ambient_objects", "amb_x", amb).as_deref(), Some("ambient_object = { name = \"amb_x\" entity = \"ent_x\" }"));

        let msg = "message_type = { key = \"msg_x\" sound = \"s\" }\n";
        assert_eq!(extract("common/message_types", "msg_x", msg).as_deref(), Some("message_type = { key = \"msg_x\" sound = \"s\" }"));

        let ship = "ship_design = { name = \"ship_x\" }\n";
        assert_eq!(extract("common/global_ship_designs", "ship_x", ship).as_deref(), Some("ship_design = { name = \"ship_x\" }"));

        let proj = "special_project = { key = \"SYNTHETIC_EVOLUTION_PROJECT\" cost = 100 }\n";
        assert_eq!(extract("common/special_projects", "SYNTHETIC_EVOLUTION_PROJECT", proj).as_deref(), Some("special_project = { key = \"SYNTHETIC_EVOLUTION_PROJECT\" cost = 100 }"));

        let rl = "random_list = {\n\tname = \"rl_habitable_normal\"\n\tplanets = { pc_desert }\n}\n";
        assert_eq!(extract("common/planet_classes", "rl_habitable_normal", rl).as_deref(), Some(rl.trim_end()));

        let tf = "terraform_link = {\n\tfrom = pc_barren\n\tto = pc_desert\n}\n";
        assert_eq!(extract("common/terraform", "pc_barren → pc_desert", tf).as_deref(), Some(tf.trim_end()));

        let files = Files::default();
        let root = std::env::temp_dir().join(format!("dm-source-tf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("common/terraform")).unwrap();
        std::fs::write(root.join("common/terraform/links.txt"), tf).unwrap();
        assert_eq!(
            files.object(&root, "common/terraform/links.txt", "common/terraform", "pc_barren → pc_desert").as_deref(),
            Some(tf.trim_end())
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
