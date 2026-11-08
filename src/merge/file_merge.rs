//! Merging several mods' copies of the same file.
//!
//! The game loads only the last copy of a file path, so everything the other
//! copies add or change is lost. Instead of merging whole files (which can
//! be very large), each copy is split into its top-level objects and those
//! are combined one by one:
//!
//! - an object every copy agrees on is kept;
//! - an object only some copies have is kept, which recovers what the
//!   overridden copies would have lost;
//! - when only one distinct change from the base game exists, it wins;
//! - anything else is a real conflict, resolved per object (by AI or by the
//!   last copy's version).

use std::collections::HashMap;

use super::source::Source;
use crate::paradox::script::{self, Entries, Entry, Value};

/// Whether `path` is a file this module can split into objects.
pub fn mergeable(path: &str) -> bool {
    let lower = path.to_lowercase();
    let Some((dir, _)) = lower.rsplit_once('/') else { return false };
    (lower.ends_with(".txt") && (dir.starts_with("common/") || dir == "events")) || ((lower.starts_with("localisation/") || lower.starts_with("localization/")) && lower.ends_with(".yml"))
}

/// The object category of a mergeable file's path, as used for object
/// conflicts (and for prompting).
pub fn category(path: &str) -> String {
    let lower = path.to_lowercase();
    if lower.starts_with("localisation/") || lower.starts_with("localization/") {
        return "localisation".into();
    }
    path.rsplit_once('/').map_or(String::new(), |(dir, _)| dir.to_owned())
}

/// One mod's copy of the file.
#[derive(Debug, Clone)]
pub struct Version {
    pub label: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Every copy that has it agrees.
    Same,
    /// Missing from the copy that wins today; kept from these copies.
    Recovered(Vec<String>),
    /// Copies disagree, but only one of them differs from the base game.
    OneChange(String),
    /// Copies make different changes. `sources` holds each distinct version.
    Conflict { sources: Vec<Source>, vanilla: Option<Source> },
}

#[derive(Debug, Clone)]
pub struct Item {
    pub key: String,
    /// The text that goes in the merged file. For conflicts this starts as
    /// the winning copy's version until it's merged.
    pub text: String,
    pub status: Status,
    /// Set once a conflict has been merged.
    pub resolved: bool,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub category: String,
    /// Leading lines that aren't objects: `namespace`, `@variables`, or the
    /// localisation language header.
    pub preamble: Vec<String>,
    pub items: Vec<Item>,
}

impl Plan {
    pub fn conflicts(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.items.iter().enumerate().filter(|(_, i)| matches!(i.status, Status::Conflict { .. }))
    }

    pub fn render(&self) -> String {
        if self.category == "localisation" {
            let mut out = String::from("\u{feff}");
            out.push_str(&self.preamble.join("\n"));
            for item in &self.items {
                out.push_str(&format!("\n {}", item.text));
            }
            out.push('\n');
            return out;
        }
        let mut out = String::from("# Merged by DrukhalManager.\n");
        for line in &self.preamble {
            out.push_str(line);
            out.push('\n');
        }
        for item in &self.items {
            out.push('\n');
            out.push_str(&item.text);
            out.push('\n');
        }
        out
    }
}

/// A file's objects in order: key, original text, and parsed form (for
/// comparing versions regardless of formatting).
struct Split {
    preamble: Vec<(String, String)>,
    objects: Vec<(String, String, Option<Entry>)>,
}

fn split(category: &str, src: &str) -> Result<Split, String> {
    if category == "localisation" {
        let mut preamble = Vec::new();
        let mut objects: Vec<(String, String, Option<Entry>)> = Vec::new();
        let mut language = String::new();
        for line in src.lines() {
            let line = line.trim_start_matches('\u{feff}');
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if !line.starts_with([' ', '\t']) && trimmed.ends_with(':') {
                language = trimmed.trim_end_matches(':').to_owned();
                preamble.push((language.clone(), trimmed.to_owned()));
                continue;
            }
            let Some((key, _)) = trimmed.split_once(':') else { continue };
            objects.push((format!("{language}:{key}"), trimmed.to_owned(), None));
        }
        if preamble.len() > 1 {
            return Err("This file holds several languages, which isn't supported yet".into());
        }
        return check_unique(Split { preamble, objects });
    }

    let entries = script::parse_with_spans(src).map_err(|e| format!("could not be read ({e})"))?;
    let mut preamble = Vec::new();
    let mut objects = Vec::new();
    for (entry, range) in entries {
        let text = src[range.clone()].to_owned();
        match &entry {
            Entry::Assign { key, .. } if key.eq_ignore_ascii_case("namespace") || key.starts_with('@') => preamble.push((key.clone(), text)),
            Entry::Assign { key, value, .. } => {
                let key = if category == "events" { value.as_block().and_then(|b| b.scalar("id")).map_or(key.clone(), str::to_owned) } else { key.clone() };
                objects.push((key, text, Some(entry)));
            }
            Entry::Item(_) => return Err("has bare values at the top level, which can't be merged object by object".into()),
        }
    }
    check_unique(Split { preamble, objects })
}

fn check_unique(split: Split) -> Result<Split, String> {
    let mut seen = HashMap::new();
    for (key, ..) in &split.objects {
        if seen.insert(key.as_str(), ()).is_some() {
            return Err(format!("defines `{key}` more than once, so it can't be merged object by object yet"));
        }
    }
    Ok(split)
}

/// Compares two versions of an object: parsed when possible, so formatting
/// and comments don't count as changes.
fn same(a: &(String, String, Option<Entry>), b: &(String, String, Option<Entry>)) -> bool {
    match (&a.2, &b.2) {
        (Some(x), Some(y)) => normalized(x) == normalized(y),
        _ => a.1.split_whitespace().eq(b.1.split_whitespace()),
    }
}

/// Keys of nested objects are case-insensitive in the game.
fn normalized(e: &Entry) -> Entry {
    match e {
        Entry::Assign { key, op, value } => Entry::Assign { key: key.to_lowercase(), op: *op, value: normalized_value(value) },
        Entry::Item(v) => Entry::Item(normalized_value(v)),
    }
}

fn normalized_value(v: &Value) -> Value {
    match v {
        Value::Scalar(s) => Value::Scalar(s.clone()),
        Value::Block(b) => Value::Block(b.iter().map(normalized).collect()),
    }
}

/// Plans the merge of `versions` (in load order; the last one wins today).
pub fn plan(path: &str, vanilla: Option<&str>, versions: &[Version]) -> Result<Plan, String> {
    let category = category(path);
    let splits: Vec<Split> = versions.iter().map(|v| split(&category, &v.text).map_err(|e| format!("{}'s copy {e}", v.label))).collect::<Result<_, _>>()?;
    let vanilla = match vanilla {
        Some(src) => split(&category, src).ok(),
        None => None,
    };
    let find = |s: &Split, key: &str| s.objects.iter().find(|o| o.0 == key).cloned();

    // Keys in the winning copy's order, then any the others add.
    let mut keys: Vec<String> = Vec::new();
    for s in splits.iter().rev() {
        for (key, ..) in &s.objects {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
    }

    let winner = splits.len() - 1;
    let mut items = Vec::new();
    for key in keys {
        let present: Vec<(usize, (String, String, Option<Entry>))> = splits.iter().enumerate().filter_map(|(i, s)| find(s, &key).map(|o| (i, o))).collect();
        let base = vanilla.as_ref().and_then(|v| find(v, &key));
        let (last_index, last) = present.last().unwrap().clone();

        // Distinct versions, highest priority first.
        let mut distinct: Vec<(usize, (String, String, Option<Entry>))> = Vec::new();
        for (i, o) in present.iter().rev() {
            if !distinct.iter().any(|(_, d)| same(d, o)) {
                distinct.push((*i, o.clone()));
            }
        }

        let (text, status) = if distinct.len() == 1 {
            let status = if last_index == winner { Status::Same } else { Status::Recovered(present.iter().map(|(i, _)| versions[*i].label.clone()).collect()) };
            (last.1.clone(), status)
        } else {
            let changed: Vec<&(usize, (String, String, Option<Entry>))> = distinct.iter().filter(|(_, o)| base.as_ref().is_none_or(|b| !same(b, o))).collect();
            match (&base, changed.as_slice()) {
                (Some(_), [only]) => (only.1.1.clone(), Status::OneChange(versions[only.0].label.clone())),
                _ => {
                    let sources = distinct.iter().rev().map(|(i, o)| Source { label: versions[*i].label.clone(), file: path.to_owned(), text: o.1.clone() }).collect();
                    let vanilla = base.as_ref().map(|b| Source { label: "Vanilla".into(), file: path.to_owned(), text: b.1.clone() });
                    (last.1.clone(), Status::Conflict { sources, vanilla })
                }
            }
        };
        items.push(Item { key, text, status, resolved: false });
    }

    // Preamble: union by key, the winning copy's line first.
    let mut preamble: Vec<(String, String)> = Vec::new();
    for s in splits.iter().rev() {
        for (key, line) in &s.preamble {
            if !preamble.iter().any(|(k, _)| k == key) {
                preamble.push((key.clone(), line.clone()));
            }
        }
    }
    preamble.sort_by_key(|(k, _)| !k.eq_ignore_ascii_case("namespace"));
    Ok(Plan { category, preamble: preamble.into_iter().map(|(_, l)| l).collect(), items })
}

/// Checks a merged script file parses before it's saved.
pub fn validate(category: &str, text: &str) -> Result<(), String> {
    if category == "localisation" {
        return Ok(());
    }
    script::parse(text).map(|_| ()).map_err(|e| format!("The merged file has a syntax error at {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(label: &str, text: &str) -> Version {
        Version { label: label.into(), text: text.into() }
    }

    #[test]
    fn combines_objects_three_ways() {
        let vanilla = "lab = { cost = 10 }\nfarm = { cost = 5 }\nmine = { cost = 7 }";
        let a = "@x = 1\nlab = { cost = 20 }\nfarm = { cost = 5 }\nmine = { cost = 8 }\nonly_a = { }";
        let b = "lab = {\n\tcost = 10 # same as vanilla, different formatting\n}\nfarm = { cost = 6 }\nmine = { cost = 9 }";
        let plan = plan("common/buildings/00_buildings.txt", Some(vanilla), &[v("A", a), v("B", b)]).unwrap();

        let by_key: HashMap<&str, &Item> = plan.items.iter().map(|i| (i.key.as_str(), i)).collect();
        assert_eq!(plan.items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(), ["lab", "farm", "mine", "only_a"], "winner's order, extras after");
        assert_eq!(by_key["lab"].status, Status::OneChange("A".into()), "B only reformatted vanilla");
        assert_eq!(by_key["lab"].text, "lab = { cost = 20 }");
        assert_eq!(by_key["farm"].status, Status::OneChange("B".into()));
        assert!(matches!(&by_key["mine"].status, Status::Conflict { sources, vanilla: Some(_) } if sources.len() == 2 && sources[0].label == "A"));
        assert_eq!(by_key["mine"].text, "mine = { cost = 9 }", "starts as the winner's version");
        assert_eq!(by_key["only_a"].status, Status::Recovered(vec!["A".into()]));
        assert_eq!(plan.preamble, ["@x = 1"]);
        assert_eq!(plan.conflicts().count(), 1);

        let rendered = plan.render();
        assert!(rendered.starts_with("# Merged by DrukhalManager.\n@x = 1\n\nlab = { cost = 20 }"));
        validate(&plan.category, &rendered).unwrap();
    }

    #[test]
    fn without_vanilla_differences_conflict() {
        let plan = plan("events/x.txt", None, &[v("A", "namespace = x\ncountry_event = { id = x.1 a = yes }"), v("B", "namespace = x\ncountry_event = { id = x.1 b = yes }\ncountry_event = { id = x.2 }")]).unwrap();
        assert_eq!(plan.items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(), ["x.1", "x.2"]);
        assert!(matches!(plan.items[0].status, Status::Conflict { vanilla: None, .. }));
        assert_eq!(plan.items[1].status, Status::Same);
        assert!(plan.render().contains("namespace = x\n\ncountry_event = { id = x.1 b = yes }"));
    }

    #[test]
    fn merges_localisation_by_key() {
        let a = "\u{feff}l_english:\n # comment\n lab:0 \"Lab\"\n farm:0 \"Farm\"\n";
        let b = "l_english:\n lab:0 \"Laboratory\"\n";
        let plan = plan("localisation/english/x_l_english.yml", None, &[v("A", a), v("B", b)]).unwrap();
        assert!(matches!(plan.items[0].status, Status::Conflict { .. }));
        assert_eq!(plan.items[1].status, Status::Recovered(vec!["A".into()]));
        assert_eq!(plan.render(), "\u{feff}l_english:\n lab:0 \"Laboratory\"\n farm:0 \"Farm\"\n");
    }

    #[test]
    fn refuses_list_style_files() {
        let err = plan("common/ambient_objects/x.txt", None, &[v("A", "ambient_object = { a = 1 }\nambient_object = { b = 2 }"), v("B", "")]).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
        assert!(mergeable("common/governments/civics/x.txt") && mergeable("localisation/english/x_l_english.yml") && mergeable("events/x.txt"));
        assert!(!mergeable("interface/x.gui") && !mergeable("gfx/x.dds") && !mergeable("common/x.csv"));
    }
}
