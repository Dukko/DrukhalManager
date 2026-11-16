//! The game's own scripting reference. Stellaris writes it to
//! `logs/script_documentation` in its user folder each time it starts: one
//! file each for effects, triggers, scope links and modifiers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The game's own scripting reference, written to `logs/script_documentation`
/// each time Stellaris starts.
pub struct Docs {
    pub effects: HashMap<String, Entry>,
    pub triggers: HashMap<String, Entry>,
    /// Lowercase scope link names.
    pub scopes: HashSet<String>,
    pub modifiers: HashSet<String>,
}

/// One documented effect or trigger.
pub struct Entry {
    /// The entry's lines, joined.
    pub text: String,
    /// The scopes it can be used in, lowercase, if the entry lists them.
    pub scopes: Option<Vec<String>>,
}

/// Where the game writes its documentation.
pub fn dir(user_dir: &Path) -> PathBuf {
    user_dir.join("logs").join("script_documentation")
}

fn read(dir: &Path, name: &str) -> Option<String> {
    Some(String::from_utf8_lossy(&std::fs::read(dir.join(name)).ok()?).into_owned())
}

/// Reads the documentation; `None` if `effects.log` or `triggers.log` is
/// missing. The scope and modifier lists are optional.
pub fn load(user_dir: &Path) -> Option<Docs> {
    let dir = dir(user_dir);
    let effects = parse_entries(&read(&dir, "effects.log")?);
    let triggers = parse_entries(&read(&dir, "triggers.log")?);
    let scopes = read(&dir, "scopes.log").map(|t| parse_entries(&t).into_keys().map(|k| k.to_lowercase()).collect()).unwrap_or_default();
    let modifiers = read(&dir, "modifiers.log").map(|t| parse_modifiers(&t)).unwrap_or_default();
    Some(Docs { effects, triggers, scopes, modifiers })
}

/// The name an entry starts with, if `line` is the first line of one:
/// `name - description`.
fn entry_name(line: &str) -> Option<&str> {
    let (name, _) = line.split_once(" - ")?;
    let mut chars = name.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic() || first == '_').then_some(())?;
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':').then_some(name)
}

fn parse_entries(text: &str) -> HashMap<String, Entry> {
    let mut entries = HashMap::new();
    let mut current: Option<(&str, Vec<&str>)> = None;
    let mut finish = |current: Option<(&str, Vec<&str>)>| {
        let Some((name, mut lines)) = current else { return };
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        let scopes = lines.iter().find_map(|l| l.trim().strip_prefix("Supported Scopes:")).map(|rest| rest.split_whitespace().map(str::to_lowercase).collect());
        entries.entry(name.to_owned()).or_insert(Entry { text: lines.join("\n"), scopes });
    };
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(name) = entry_name(line) {
            finish(current.take());
            current = Some((name, vec![line]));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    finish(current.take());
    entries
}

/// `- pop_happiness, Category: Pops`
fn parse_modifiers(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| {
            let name = line.trim().strip_prefix("- ")?.split(',').next()?.trim();
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

impl Docs {
    /// Up to `per_name` documented effects/triggers whose `_`-separated words
    /// overlap most with `name` (ties by shorter name), excluding `name`.
    pub fn related(&self, name: &str, per_name: usize) -> Vec<(&str, &Entry)> {
        let words = |s: &str| s.to_lowercase().split('_').filter(|w| !w.is_empty()).map(str::to_owned).collect::<HashSet<String>>();
        let wanted = words(name);
        let mut scored: Vec<(usize, &str, &Entry)> = self
            .effects
            .iter()
            .chain(self.triggers.iter())
            .filter(|(candidate, _)| candidate.as_str() != name)
            .filter_map(|(candidate, entry)| {
                let shared = words(candidate).intersection(&wanted).count();
                (shared > 0).then_some((shared, candidate.as_str(), entry))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
        scored.dedup_by(|a, b| a.1 == b.1);
        scored.into_iter().take(per_name).map(|(_, name, entry)| (name, entry)).collect()
    }

    /// The entries for `names` and their related ones, as text, at most `max_bytes`.
    pub fn reference(&self, names: &[&str], max_bytes: usize) -> String {
        let mut out = String::new();
        let mut seen = HashSet::new();
        let mut add = |name: &str, entry: &Entry, out: &mut String| {
            if !seen.insert(name.to_owned()) {
                return;
            }
            let extra = entry.text.len() + 2;
            if out.len() + extra <= max_bytes {
                out.push_str(&entry.text);
                out.push_str("\n\n");
            }
        };
        for name in names {
            if let Some(entry) = self.effects.get(*name).or_else(|| self.triggers.get(*name)) {
                add(name, entry, &mut out);
            }
        }
        for name in names {
            for (related, entry) in self.related(name, 3) {
                add(related, entry, &mut out);
            }
        }
        out.truncate(out.trim_end().len());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EFFECTS: &str = "[12:00:00][F][effects.cpp:1]: Script documentation\n== EFFECTS DOCUMENTATION ==\n\nset_planetary_ascension_tier - Sets the tier.\nset_planetary_ascension_tier = <value>\nSupported Scopes: planet Ship colony\n\nadd_modifier - Adds a modifier\nto the scope.\n\nadd_modifier = { modifier = <name> }\nSupported Scopes: all\n";

    fn docs() -> Docs {
        let entry = |text: &str| Entry { text: text.to_owned(), scopes: None };
        Docs {
            effects: HashMap::new(),
            triggers: [("has_job_type", "has_job_type - a job"), ("has_job_category", "has_job_category - a category"), ("has_ethic", "has_ethic - an ethic")].into_iter().map(|(k, t)| (k.to_owned(), entry(t))).collect(),
            scopes: HashSet::new(),
            modifiers: HashSet::new(),
        }
    }

    #[test]
    fn parses_entries_with_text_and_scopes() {
        let entries = parse_entries(EFFECTS);
        assert_eq!(entries.len(), 2);
        let tier = &entries["set_planetary_ascension_tier"];
        assert_eq!(tier.text, "set_planetary_ascension_tier - Sets the tier.\nset_planetary_ascension_tier = <value>\nSupported Scopes: planet Ship colony");
        assert_eq!(tier.scopes.as_deref(), Some(&["planet".to_owned(), "ship".to_owned(), "colony".to_owned()][..]));
        let modifier = &entries["add_modifier"];
        assert!(modifier.text.contains("to the scope."));
        assert!(modifier.text.contains("add_modifier = { modifier = <name> }"));
    }

    #[test]
    fn parses_modifier_names() {
        let names = parse_modifiers("Modifiers:\n- pop_happiness, Category: Pops\n- ship_speed_mult, Category: Ships\n");
        assert!(names.contains("pop_happiness") && names.contains("ship_speed_mult"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn ranks_related_names_by_shared_words() {
        let docs = docs();
        let related = docs.related("has_job", 3);
        let names: Vec<&str> = related.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "has_job_type");
        assert_eq!(names[1], "has_job_category");
        assert_eq!(names[2], "has_ethic");
        assert_eq!(docs.related("has_job", 2).len(), 2);
    }

    #[test]
    fn reference_stays_under_the_limit() {
        let docs = docs();
        let all = docs.reference(&["has_job", "has_ethic"], 10_000);
        assert!(all.contains("has_job_type - a job"));
        assert!(all.contains("has_ethic - an ethic"));
        let small = docs.reference(&["has_job"], 40);
        assert!(small.len() <= 40, "{}", small.len());
        assert!(!small.is_empty());
    }

    #[test]
    fn loads_nothing_without_the_documentation() {
        let dir = std::env::temp_dir().join(format!("drukhal-docs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load(&dir).is_none());
        let logs = super::dir(&dir);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("effects.log"), EFFECTS).unwrap();
        assert!(load(&dir).is_none());
        std::fs::write(logs.join("triggers.log"), "x\n\nhas_flag - Flag\nSupported Scopes: all\n").unwrap();
        std::fs::write(logs.join("scopes.log"), "Owner - the owner\nOutput Scope: country\n").unwrap();
        let docs = load(&dir).unwrap();
        assert!(docs.effects.contains_key("add_modifier") && docs.triggers.contains_key("has_flag"));
        assert!(docs.scopes.contains("owner"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
