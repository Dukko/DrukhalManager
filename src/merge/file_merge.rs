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
//!
//! Objects are named by their key, or for a type keyword repeated within a
//! file (`ambient_object = { name = x }`) by that keyword plus the object's
//! `name`, `key` or `id`. In `.gui` and `.gfx` files the objects sit inside
//! top-level blocks (`guiTypes = { ... }`, `spriteTypes = { ... }`), which
//! are rebuilt around them.

use std::collections::HashSet;

use super::source::Source;
use crate::paradox::script::{self, Entries, Entry, Value};

/// Whether `path` is a file this module can split into objects.
pub fn mergeable(path: &str) -> bool {
    let lower = path.to_lowercase();
    let Some((dir, _)) = lower.rsplit_once('/') else { return false };
    if super::conflicts::FRAGMENT_FOLDERS.iter().any(|f| lower.starts_with(&format!("{f}/"))) {
        return false;
    }
    (lower.ends_with(".txt") && (dir.starts_with("common/") || dir == "events")) || ((lower.starts_with("localisation/") || lower.starts_with("localization/")) && lower.ends_with(".yml")) || wrapped(&lower)
}

/// `.gui` and `.gfx` files, whose objects sit inside top-level blocks.
fn wrapped(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".gui") || lower.ends_with(".gfx")
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
    /// For `.gui`/`.gfx` files, the top-level block it sits in, e.g.
    /// `spriteTypes`.
    pub group: Option<String>,
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
    /// Type keywords repeated within the file, as `(group, keyword)` in
    /// lower case: objects using them are named by their `name`/`key`/`id`.
    repeated: Repeated,
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
        // Wrappers in order of first use, spelled as first seen.
        let mut groups: Vec<&Option<String>> = Vec::new();
        for item in &self.items {
            if !groups.iter().any(|g| lower(g) == lower(&item.group)) {
                groups.push(&item.group);
            }
        }
        for group in groups {
            let items = self.items.iter().filter(|i| lower(&i.group) == lower(group));
            match group {
                None => {
                    for item in items {
                        out.push('\n');
                        out.push_str(&item.text);
                        out.push('\n');
                    }
                }
                Some(name) => {
                    // Objects keep their own lines' indentation; only the
                    // first line was cut from mid-line.
                    out.push_str(&format!("\n{name} = {{\n"));
                    for (i, item) in items.enumerate() {
                        if i > 0 {
                            out.push('\n');
                        }
                        out.push('\t');
                        out.push_str(&item.text);
                        out.push('\n');
                    }
                    out.push_str("}\n");
                }
            }
        }
        out
    }

    /// Checks an AI's merge of item `index`: it must define that object and
    /// nothing else. Returns the text to use.
    pub fn validate_item(&self, index: usize, text: &str) -> Result<String, String> {
        let item = &self.items[index];
        if self.category == "localisation" {
            let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
            let name = item.key.split_once(':').map_or(item.key.as_str(), |(_, k)| k);
            return match lines[..] {
                [line] if line.split_once(':').is_some_and(|(k, _)| k == name) => Ok(line.to_owned()),
                _ => Err(format!("A localisation merge must be a single line starting with `{name}:`")),
            };
        }
        let entries = script::parse(text).map_err(|e| format!("The merged script has a syntax error at {e}"))?;
        let mut defined = Vec::new();
        for entry in &entries {
            match entry {
                Entry::Assign { key, .. } if key.starts_with('@') => {}
                _ => defined.push(object_key(&self.category, &item.group, entry, &self.repeated)?),
            }
        }
        match &defined[..] {
            [key] if *key == item.key => Ok(text.trim().to_owned()),
            _ => Err(format!("The merged script must define `{}` and nothing else", item.key)),
        }
    }
}

type Repeated = HashSet<(Option<String>, String)>;

/// One object in a file: where it sits, its name, its original text, and
/// its parsed form (for comparing versions regardless of formatting).
#[derive(Debug, Clone)]
struct Object {
    group: Option<String>,
    key: String,
    text: String,
    entry: Option<Entry>,
}

impl Object {
    fn is(&self, group: &Option<String>, key: &str) -> bool {
        lower(&self.group) == lower(group) && self.key == key
    }
}

fn lower(group: &Option<String>) -> Option<String> {
    group.as_ref().map(|g| g.to_lowercase())
}

/// A file's objects in order, plus its preamble lines by key.
struct Split {
    preamble: Vec<(String, String)>,
    objects: Vec<Object>,
}

/// A parsed copy's lists of objects (the top level, or each wrapper's
/// contents), each entry with its original text.
type Lists = Vec<(Option<String>, Vec<(Entry, String)>)>;

fn parse_lists(wrapped: bool, src: &str) -> Result<(Vec<(String, String)>, Lists), String> {
    let entries = script::parse_with_spans(src).map_err(|e| format!("could not be read ({e})"))?;
    let mut preamble = Vec::new();
    let mut top = Vec::new();
    let mut lists: Lists = Vec::new();
    for (entry, range) in entries {
        let text = src[range.clone()].to_owned();
        match &entry {
            Entry::Assign { key, .. } if key.eq_ignore_ascii_case("namespace") || key.starts_with('@') => preamble.push((key.clone(), text)),
            Entry::Assign { key, value, .. } if wrapped => {
                let (Some(_), Some(open)) = (value.as_block(), text.find('{')) else {
                    return Err(format!("has `{key}` at the top level outside a block, which can't be merged object by object"));
                };
                // The block's contents, between its braces.
                let inner = &text[open + 1..text.len() - 1];
                let children = script::parse_with_spans(inner).map_err(|e| format!("could not be read ({e})"))?;
                let mut list = Vec::new();
                for (child, range) in children {
                    if let Entry::Item(_) = child {
                        return Err(format!("has bare values in `{key}`, which can't be merged object by object"));
                    }
                    list.push((child, inner[range].to_owned()));
                }
                // A file may repeat a wrapper; its contents join the first.
                match lists.iter_mut().find(|(g, _)| g.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(key))) {
                    Some((_, existing)) => existing.extend(list),
                    None => lists.push((Some(key.clone()), list)),
                }
            }
            Entry::Assign { .. } => top.push((entry, text)),
            Entry::Item(_) => return Err("has bare values at the top level, which can't be merged object by object".into()),
        }
    }
    if !wrapped {
        lists.push((None, top));
    }
    Ok((preamble, lists))
}

/// Type keywords used more than once in one of `lists`.
fn repeated_in(lists: &Lists, out: &mut Repeated) {
    for (group, entries) in lists {
        let mut seen = HashSet::new();
        for (entry, _) in entries {
            if let Entry::Assign { key, .. } = entry
                && !seen.insert(key.to_lowercase())
            {
                out.insert((lower(group), key.to_lowercase()));
            }
        }
    }
}

/// The name an object is merged under. See the module docs.
fn object_key(category: &str, group: &Option<String>, entry: &Entry, repeated: &Repeated) -> Result<String, String> {
    let Entry::Assign { key, value, .. } = entry else { return Err("has a bare value where an object should be".into()) };
    if category == "events" && group.is_none() {
        return Ok(value.as_block().and_then(|b| b.scalar("id")).map_or(key.clone(), str::to_owned));
    }
    if !repeated.contains(&(lower(group), key.to_lowercase())) {
        return Ok(key.clone());
    }
    let block = value.as_block();
    // Terraforming links are named by the planet classes they join.
    let link = block.and_then(|b| Some(format!("{} → {}", b.scalar("from")?, b.scalar("to")?)));
    let name = block.and_then(|b| ["name", "key", "id"].iter().find_map(|f| b.scalar(f))).map(str::to_owned).or(link);
    // Font overrides reuse a font's name, once per set of languages.
    let languages = block.map(|b| b.scalar_items("languages")).unwrap_or_default();
    match name {
        Some(name) if !languages.is_empty() => Ok(format!("{key} {name} ({})", languages.join(" "))),
        Some(name) => Ok(format!("{key} {name}")),
        None => Err(format!("defines `{key}` more than once without a name, so it can't be merged object by object")),
    }
}

fn split(category: &str, wrapped: bool, src: &str, repeated: &Repeated) -> Result<Split, String> {
    if category == "localisation" {
        let mut preamble = Vec::new();
        let mut objects = Vec::new();
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
            objects.push(Object { group: None, key: format!("{language}:{key}"), text: trimmed.to_owned(), entry: None });
        }
        if preamble.len() > 1 {
            return Err("This file holds several languages, which isn't supported yet".into());
        }
        return check_unique(Split { preamble, objects });
    }

    let (preamble, lists) = parse_lists(wrapped, src)?;
    let mut objects = Vec::new();
    for (group, entries) in lists {
        for (entry, text) in entries {
            let key = object_key(category, &group, &entry, repeated)?;
            objects.push(Object { group: group.clone(), key, text, entry: Some(entry) });
        }
    }
    check_unique(Split { preamble, objects })
}

fn check_unique(split: Split) -> Result<Split, String> {
    let mut seen = HashSet::new();
    for o in &split.objects {
        if !seen.insert((lower(&o.group), o.key.as_str())) {
            return Err(format!("defines `{}` more than once, so it can't be merged object by object yet", o.key));
        }
    }
    Ok(split)
}

/// Compares two versions of an object: parsed when possible, so formatting
/// and comments don't count as changes.
fn same(a: &Object, b: &Object) -> bool {
    match (&a.entry, &b.entry) {
        (Some(x), Some(y)) => normalized(x) == normalized(y),
        _ => a.text.split_whitespace().eq(b.text.split_whitespace()),
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
    let wrapped = wrapped(path);

    // Which type keywords repeat, across every copy, so that every copy
    // names an object the same way.
    let mut repeated = Repeated::new();
    if category != "localisation" {
        for v in versions {
            let (_, lists) = parse_lists(wrapped, &v.text).map_err(|e| format!("{}'s copy {e}", v.label))?;
            repeated_in(&lists, &mut repeated);
        }
        if let Some(Ok((_, lists))) = vanilla.map(|src| parse_lists(wrapped, src)) {
            repeated_in(&lists, &mut repeated);
        }
    }

    let splits: Vec<Split> = versions.iter().map(|v| split(&category, wrapped, &v.text, &repeated).map_err(|e| format!("{}'s copy {e}", v.label))).collect::<Result<_, _>>()?;
    let vanilla = vanilla.and_then(|src| split(&category, wrapped, src, &repeated).ok());
    let find = |s: &Split, group: &Option<String>, key: &str| s.objects.iter().find(|o| o.is(group, key)).cloned();

    // Objects in the winning copy's order, then any the others add.
    let mut keys: Vec<(Option<String>, String)> = Vec::new();
    for s in splits.iter().rev() {
        for o in &s.objects {
            if !keys.iter().any(|(g, k)| o.is(g, k)) {
                keys.push((o.group.clone(), o.key.clone()));
            }
        }
    }

    let winner = splits.len() - 1;
    let mut items = Vec::new();
    for (group, key) in keys {
        let present: Vec<(usize, Object)> = splits.iter().enumerate().filter_map(|(i, s)| find(s, &group, &key).map(|o| (i, o))).collect();
        let base = vanilla.as_ref().and_then(|v| find(v, &group, &key));
        let (last_index, last) = present.last().unwrap().clone();

        // Distinct versions, highest priority first.
        let mut distinct: Vec<(usize, Object)> = Vec::new();
        for (i, o) in present.iter().rev() {
            if !distinct.iter().any(|(_, d)| same(d, o)) {
                distinct.push((*i, o.clone()));
            }
        }

        let (text, status) = if distinct.len() == 1 {
            let status = if last_index == winner { Status::Same } else { Status::Recovered(present.iter().map(|(i, _)| versions[*i].label.clone()).collect()) };
            (last.text.clone(), status)
        } else {
            let changed: Vec<&(usize, Object)> = distinct.iter().filter(|(_, o)| base.as_ref().is_none_or(|b| !same(b, o))).collect();
            match (&base, changed.as_slice()) {
                (Some(_), [only]) => (only.1.text.clone(), Status::OneChange(versions[only.0].label.clone())),
                _ => {
                    let sources = distinct.iter().rev().map(|(i, o)| Source { label: versions[*i].label.clone(), file: path.to_owned(), text: o.text.clone() }).collect();
                    let vanilla = base.as_ref().map(|b| Source { label: "Vanilla".into(), file: path.to_owned(), text: b.text.clone() });
                    (last.text.clone(), Status::Conflict { sources, vanilla })
                }
            }
        };
        items.push(Item { group, key, text, status, resolved: false });
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
    Ok(Plan { category, repeated, preamble: preamble.into_iter().map(|(_, l)| l).collect(), items })
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
    use std::collections::HashMap;

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
    fn merges_repeated_type_keywords_by_name() {
        let a = "ambient_object = { name = x scale = 1 }\nambient_object = { name = y scale = 1 }";
        let b = "ambient_object = { name = x scale = 2 }\nambient_object = { name = z }";
        let plan = plan("common/ambient_objects/x.txt", Some("ambient_object = { name = x scale = 1 }"), &[v("A", a), v("B", b)]).unwrap();
        assert_eq!(plan.items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(), ["ambient_object x", "ambient_object z", "ambient_object y"]);
        assert_eq!(plan.items[0].status, Status::OneChange("B".into()));
        assert_eq!(plan.items[2].status, Status::Recovered(vec!["A".into()]));
        validate(&plan.category, &plan.render()).unwrap();

        // A copy with just one still names it the same way.
        let plan = plan_of("common/ambient_objects/x.txt", &[a, "ambient_object = { name = x scale = 3 }"]);
        assert_eq!(plan.items[0].key, "ambient_object x");
        assert!(matches!(plan.items[0].status, Status::Conflict { .. }));
        assert_eq!(plan.validate_item(0, "ambient_object = { name = x scale = 4 }").unwrap(), "ambient_object = { name = x scale = 4 }");
        assert!(plan.validate_item(0, "ambient_object = { name = q }").is_err(), "a different object");
        assert!(plan.validate_item(0, "ambient_object = { name = x }\nambient_object = { name = y }").is_err(), "more than asked");

        let fonts = plan_of("interface/fonts.gfx", &["bitmapfonts = {\n\tbitmapfont_override = { name = f languages = { l_russian } }\n\tbitmapfont_override = { name = f languages = { l_japanese } }\n}", ""]);
        assert_eq!(fonts.items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(), ["bitmapfont_override f (l_russian)", "bitmapfont_override f (l_japanese)"]);

        let links = plan_of("common/terraform/x.txt", &["terraform_link = { from = a to = b }\nterraform_link = { from = a to = c }", ""]);
        assert_eq!(links.items[1].key, "terraform_link a → c");

        let err = plan_err("common/x/x.txt", &["thing = { a = 1 }\nthing = { b = 2 }", ""]);
        assert!(err.contains("without a name"), "{err}");
    }

    #[test]
    fn merges_gui_and_gfx_inside_their_blocks() {
        let a = "spriteTypes = {\n\tspriteType = {\n\t\tname = \"GFX_a\"\n\t\ttexturefile = \"a.dds\"\n\t}\n\tspriteType = { name = \"GFX_b\" texturefile = \"b.dds\" }\n}\n";
        let b = "# B's sprites\nspriteTypes = {\n\tspriteType = { name = \"GFX_a\" texturefile = \"a_new.dds\" }\n}\nobjectTypes = {\n\tpdxmesh = { name = \"m\" }\n}\n";
        let plan = plan("interface/x.gfx", None, &[v("A", a), v("B", b)]).unwrap();
        assert_eq!(plan.items.iter().map(|i| (i.group.as_deref().unwrap(), i.key.as_str())).collect::<Vec<_>>(), [("spriteTypes", "spriteType GFX_a"), ("objectTypes", "pdxmesh"), ("spriteTypes", "spriteType GFX_b")]);
        assert!(matches!(plan.items[0].status, Status::Conflict { .. }));
        assert_eq!(plan.items[2].status, Status::Recovered(vec!["A".into()]));
        let rendered = plan.render();
        assert_eq!(
            rendered,
            "# Merged by DrukhalManager.\n\nspriteTypes = {\n\tspriteType = { name = \"GFX_a\" texturefile = \"a_new.dds\" }\n\n\tspriteType = { name = \"GFX_b\" texturefile = \"b.dds\" }\n}\n\nobjectTypes = {\n\tpdxmesh = { name = \"m\" }\n}\n"
        );
        validate(&plan.category, &rendered).unwrap();

        // Multi-line objects keep their indentation.
        let plan = plan_of("interface/x.gui", &["guiTypes = {\n\tcontainerWindowType = {\n\t\tname = \"w\"\n\t}\n}", "guiTypes = { }"]);
        assert_eq!(plan.render(), "# Merged by DrukhalManager.\n\nguiTypes = {\n\tcontainerWindowType = {\n\t\tname = \"w\"\n\t}\n}\n");

        assert!(plan_err("interface/x.gui", &["guiTypes = { 1 2 }", ""]).contains("bare values"));
        assert!(mergeable("interface/x.gui") && mergeable("gfx/x.gfx") && mergeable("common/governments/civics/x.txt") && mergeable("localisation/english/x_l_english.yml") && mergeable("events/x.txt"));
        assert!(!mergeable("gfx/x.dds") && !mergeable("common/x.csv") && !mergeable("common/inline_scripts/a/b.txt"));
    }

    fn plan_of(path: &str, texts: &[&str]) -> Plan {
        let versions: Vec<Version> = texts.iter().enumerate().map(|(i, t)| v(&format!("M{i}"), t)).collect();
        plan(path, None, &versions).unwrap()
    }

    fn plan_err(path: &str, texts: &[&str]) -> String {
        let versions: Vec<Version> = texts.iter().enumerate().map(|(i, t)| v(&format!("M{i}"), t)).collect();
        plan(path, None, &versions).unwrap_err()
    }
}

