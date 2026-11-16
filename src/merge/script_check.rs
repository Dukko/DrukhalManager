//! Finding effects, triggers and scopes in mods that the game doesn't have.
//!
//! The game skips script it can't make sense of without saying so, so a mod
//! that still calls something removed in an update quietly loses the feature
//! it belongs to. The game's own scripting reference lists every effect and
//! trigger it has; this module checks the words in effect and trigger
//! positions of each mod file against it.
//!
//! Where those positions are is learned from the base game's files, which
//! have no findings by construction: blocks like `immediate` hold effects,
//! `potential` holds triggers, and each holds some plain settings too.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use super::conflicts::{self, ObjectConflict, Report};
use super::{file_merge, source};
use crate::paradox::script::{self, Entry, Node};
use crate::paradox::script_docs::Docs;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ctx {
    Effect,
    Trigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The game has no such effect or trigger.
    Unknown,
    /// It's a trigger used as an effect, or the reverse.
    WrongKind,
    /// It would be known if it were spelled in lowercase.
    Capitalisation,
    /// Unknown here, but vanilla uses the name as a plain setting in some
    /// other block, so it may be fine.
    Unsure,
}

/// What vanilla teaches: which blocks hold effects or triggers, and which
/// plain settings appear in them. Saved per game version.
#[derive(Serialize, Deserialize)]
pub struct Context {
    pub version: String,
    anchors: HashMap<String, Ctx>,
    /// Keys seen in vanilla, by "anchor\tE" or "anchor\tT".
    allowed: HashMap<String, HashSet<String>>,
    /// Every key vanilla uses anywhere in `common/` and `events/`, in any
    /// block, except the names of top-level objects, plus all of `allowed`.
    #[serde(default)]
    seen: HashSet<String>,
}

impl Context {
    fn indexed(mut self) -> Self {
        let allowed: Vec<String> = self.allowed.values().flatten().cloned().collect();
        self.seen.extend(allowed);
        self
    }
}

/// The names the game and the enabled mods define.
pub struct Known {
    pub effects: HashSet<String>,
    pub triggers: HashSet<String>,
    pub scopes: HashSet<String>,
    pub modifiers: HashSet<String>,
    /// The documented effects that start `every_`, `random_` or `ordered_`.
    pub effect_iters: HashSet<String>,
    /// The documented triggers that start `any_` or `count_`.
    pub trigger_iters: HashSet<String>,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub mod_position: usize,
    pub file: String,
    pub line: usize,
    pub name: String,
    pub ctx: Ctx,
    pub problem: Problem,
    /// The object it's in, if found: (category, key)
    pub object: Option<(String, String)>,
}

const LOGIC: [&str; 5] = ["and", "or", "not", "nor", "nand"];
const FLOW: [&str; 4] = ["if", "else_if", "else", "while"];
/// Settings that mean something in any block.
const SETTINGS: [&str; 3] = ["limit", "inline_script", "count"];
/// Fewest children an anchor needs for its kind to be learned.
const MIN_CHILDREN: usize = 8;

fn is_logic(key: &str) -> bool {
    LOGIC.contains(&key.to_lowercase().as_str())
}

fn is_flow(key: &str) -> bool {
    FLOW.contains(&key.to_lowercase().as_str())
}

/// Whether a key is plain text, not a template or a variable.
fn plain(key: &str) -> bool {
    !key.contains('$') && !key.contains('[') && !key.starts_with('@')
}

fn is_scope(known: &Known, key: &str) -> bool {
    if key.contains(['.', ':', '[']) || key.starts_with(['$', '@']) {
        return true;
    }
    let lower = key.to_lowercase();
    let lower = lower.strip_suffix('?').unwrap_or(&lower);
    if known.scopes.contains(lower) || matches!(lower, "root" | "this" | "owner") {
        return true;
    }
    if (1..=4).any(|n| lower == "from".repeat(n) || lower == "prev".repeat(n)) {
        return true;
    }
    let number = key.strip_suffix('%').unwrap_or(key);
    number.chars().any(|c| c.is_ascii_digit()) && number.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
}

fn is_iterator(known: &Known, key: &str) -> bool {
    known.effect_iters.contains(key) || known.trigger_iters.contains(key)
}

fn letter(ctx: Ctx) -> char {
    match ctx {
        Ctx::Effect => 'E',
        Ctx::Trigger => 'T',
    }
}

fn allowed_key(anchor: &str, ctx: Ctx) -> String {
    format!("{anchor}\t{}", letter(ctx))
}

/// Where a file's entries start: the anchor and the context of the children
/// of its top-level blocks. `None` for files that aren't checked.
fn start(rel: &str) -> Option<(String, Option<Ctx>)> {
    let lower = rel.to_lowercase();
    if !lower.ends_with(".txt") {
        return None;
    }
    if lower.starts_with("events/") {
        return Some(("<events>".to_owned(), None));
    }
    let rest = lower.strip_prefix("common/")?;
    if rest.starts_with("inline_scripts/") {
        return None;
    }
    let folder = rest.split_once('/').map_or("", |(folder, _)| folder);
    let ctx = match folder {
        "scripted_effects" => Some(Ctx::Effect),
        "scripted_triggers" => Some(Ctx::Trigger),
        _ => None,
    };
    Some((format!("<common/{folder}>").replace("/>", ">"), ctx))
}

struct Visit<'a> {
    anchor: &'a str,
    ctx: Option<Ctx>,
    key: &'a str,
    line: usize,
}

/// The anchor and context of the children of `key`'s block.
fn child<'a>(anchors: &HashMap<String, Ctx>, known: &Known, anchor: &'a str, ctx: Option<Ctx>, key: &'a str) -> (&'a str, Option<Ctx>) {
    // The blocks of a `switch`'s cases hold what the switch is used for.
    if anchor == "switch" {
        return ("switch_case", ctx);
    }
    if key == "limit" || is_logic(key) {
        return (anchor, Some(Ctx::Trigger));
    }
    if is_flow(key) || is_scope(known, key) {
        return (anchor, ctx);
    }
    if ctx == Some(Ctx::Effect) && key == "hidden_effect" {
        return (anchor, Some(Ctx::Effect));
    }
    // The options of a random_list hold weight settings such as `modifier`,
    // the same wherever the list is, even inside a parameter block.
    if key == "random_list" {
        return (key, Some(Ctx::Effect));
    }
    if known.effect_iters.contains(key) {
        return (anchor, Some(Ctx::Effect));
    }
    if known.trigger_iters.contains(key) {
        return (anchor, Some(Ctx::Trigger));
    }
    if ctx == Some(Ctx::Trigger) && matches!(key, "calc_true_if" | "hidden_trigger" | "custom_tooltip") {
        return (anchor, Some(Ctx::Trigger));
    }
    // Unlike other effects that are also triggers, a switch's cases aren't parameters.
    if key == "switch" && ctx.is_some() {
        return (key, ctx);
    }
    if ctx.is_some() && (known.effects.contains(key) || known.triggers.contains(key)) {
        // Its block holds parameters, which aren't checked.
        return (key, None);
    }
    (key, anchors.get(key).copied())
}

fn walk_nodes<'a>(anchors: &HashMap<String, Ctx>, known: &Known, nodes: &'a [Node], anchor: &'a str, ctx: Option<Ctx>, f: &mut dyn FnMut(&Visit<'_>)) {
    for node in nodes {
        let Some(key) = node.key.as_deref() else { continue };
        // The keys of a switch are case labels, which can be any value, and
        // its `trigger`, which is a parameter.
        if anchor != "switch" {
            f(&Visit { anchor, ctx, key, line: node.line });
        }
        // The arguments of an inline script belong to that script.
        if key == "inline_script" {
            continue;
        }
        if let Some(kids) = &node.kids {
            let (anchor, ctx) = child(anchors, known, anchor, ctx, key);
            walk_nodes(anchors, known, kids, anchor, ctx, f);
        }
    }
}

/// Visits every keyed node under the top-level blocks of a file.
fn walk_file(anchors: &HashMap<String, Ctx>, known: &Known, rel: &str, text: &str, f: &mut dyn FnMut(&Visit<'_>)) {
    let Some((anchor, ctx)) = start(rel) else { return };
    let Ok(nodes) = script::parse_lines(text) else { return };
    for node in &nodes {
        let (Some(key), Some(kids)) = (node.key.as_deref(), &node.kids) else { continue };
        if key.starts_with('@') {
            continue;
        }
        walk_nodes(anchors, known, kids, &anchor, ctx, f);
    }
}

/// The script files under `common/` and `events/` of `root`, relative,
/// sorted.
fn script_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for folder in ["common", "events"] {
        let dir = root.join(folder);
        if dir.is_dir() {
            let _ = conflicts::walk(root, &dir, &mut files);
        }
    }
    files.retain(|f| start(f).is_some());
    files.sort();
    files
}

fn read_text(path: &Path) -> Option<String> {
    Some(String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned())
}

/// The top-level names the files in `folder` of `root` define.
fn scripted_names(root: &Path, folder: &str, out: &mut HashSet<String>) {
    let mut files = Vec::new();
    let dir = root.join("common").join(folder);
    if dir.is_dir() {
        let _ = conflicts::walk(root, &dir, &mut files);
    }
    for file in files.iter().filter(|f| f.to_lowercase().ends_with(".txt")) {
        let Some(text) = read_text(&root.join(file)) else { continue };
        let Ok(entries) = script::parse(&text) else { continue };
        for entry in entries {
            if let Entry::Assign { key, .. } = entry
                && !key.starts_with('@')
            {
                out.insert(key);
            }
        }
    }
}

/// The names the docs list plus the scripted effects and triggers of
/// `roots` (the game first, then the enabled mods).
pub fn known(docs: &Docs, roots: &[&Path]) -> Known {
    let mut effects: HashSet<String> = docs.effects.keys().cloned().collect();
    let mut triggers: HashSet<String> = docs.triggers.keys().cloned().collect();
    let effect_iters = effects.iter().filter(|n| ["every_", "random_", "ordered_"].iter().any(|p| n.starts_with(p))).cloned().collect();
    let trigger_iters = triggers.iter().filter(|n| ["any_", "count_"].iter().any(|p| n.starts_with(p))).cloned().collect();
    for root in roots {
        scripted_names(root, "scripted_effects", &mut effects);
        scripted_names(root, "scripted_triggers", &mut triggers);
    }
    Known { effects, triggers, scopes: docs.scopes.clone(), modifiers: docs.modifiers.clone(), effect_iters, trigger_iters }
}

/// How the children of an anchor split, among those visited with no context.
#[derive(Default)]
struct Tally {
    effect: usize,
    trigger: usize,
    both: usize,
    other: usize,
}

fn learn_anchors(tallies: &HashMap<String, Tally>) -> HashMap<String, Ctx> {
    let mut anchors = HashMap::new();
    for (anchor, t) in tallies {
        let total = t.effect + t.trigger + t.both + t.other;
        if total < MIN_CHILDREN {
            continue;
        }
        if (t.effect + t.both) * 100 >= 85 * total && t.effect >= t.trigger {
            anchors.insert(anchor.clone(), Ctx::Effect);
        } else if (t.trigger + t.both) * 100 >= 85 * total && t.trigger > t.effect {
            anchors.insert(anchor.clone(), Ctx::Trigger);
        }
    }
    anchors
}

fn collect_keys(nodes: &[Node], out: &mut HashSet<String>) {
    for node in nodes {
        if let Some(key) = &node.key
            && plain(key)
        {
            out.insert(key.clone());
        }
        if let Some(kids) = &node.kids {
            collect_keys(kids, out);
        }
    }
}

/// Every key the game's script files use, in any block, except the names of
/// top-level objects.
fn vanilla_keys(game_dir: &Path) -> HashSet<String> {
    let mut files = Vec::new();
    for folder in ["common", "events"] {
        let dir = game_dir.join(folder);
        if dir.is_dir() {
            let _ = conflicts::walk(game_dir, &dir, &mut files);
        }
    }
    let mut keys = HashSet::new();
    for file in files.iter().filter(|f| f.to_lowercase().ends_with(".txt")) {
        let Some(text) = read_text(&game_dir.join(file)) else { continue };
        let Ok(nodes) = script::parse_lines(&text) else { continue };
        for node in &nodes {
            if let Some(kids) = &node.kids {
                collect_keys(kids, &mut keys);
            }
        }
    }
    keys
}

/// Reads the base game's files and learns where its effects and triggers
/// are. Only game files are used, never mods.
pub fn learn(game_dir: &Path, docs: &Docs, version: &str) -> Context {
    let known = known(docs, &[game_dir]);
    let files = script_files(game_dir);
    let pass = |anchors: &HashMap<String, Ctx>, collect_allowed: bool| {
        let mut tallies: HashMap<String, Tally> = HashMap::new();
        let mut allowed: HashMap<String, HashSet<String>> = HashMap::new();
        for file in &files {
            let Some(text) = read_text(&game_dir.join(file)) else { continue };
            walk_file(anchors, &known, file, &text, &mut |v| {
                if !plain(v.key) || is_scope(&known, v.key) || is_logic(v.key) || is_flow(v.key) {
                    return;
                }
                match v.ctx {
                    None => {
                        let (e, t) = (known.effects.contains(v.key), known.triggers.contains(v.key));
                        let tally = tallies.entry(v.anchor.to_owned()).or_default();
                        match (e, t) {
                            (true, false) => tally.effect += 1,
                            (false, true) => tally.trigger += 1,
                            (true, true) => tally.both += 1,
                            (false, false) => tally.other += 1,
                        }
                    }
                    Some(ctx) if collect_allowed => {
                        allowed.entry(allowed_key(v.anchor, ctx)).or_default().insert(v.key.to_owned());
                    }
                    Some(_) => {}
                }
            });
        }
        (tallies, allowed)
    };
    let seen = vanilla_keys(game_dir);
    // The traversal depends on the anchors, so they're learned in two passes.
    let (first, _) = pass(&HashMap::new(), false);
    let first = learn_anchors(&first);
    // Blocks already known to hold effects or triggers no longer count their
    // children as undecided, so the second pass only adds anchors.
    let (second, allowed) = pass(&first, true);
    let mut anchors = first.clone();
    for (anchor, ctx) in learn_anchors(&second) {
        anchors.entry(anchor).or_insert(ctx);
    }
    if anchors == first {
        return Context { version: version.to_owned(), anchors, allowed, seen }.indexed();
    }
    let (_, allowed) = pass(&anchors, true);
    Context { version: version.to_owned(), anchors, allowed, seen }.indexed()
}

fn context_path(cache_dir: &Path, version: &str) -> PathBuf {
    let safe: String = version.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
    cache_dir.join(format!("script-context-{safe}.json.gz"))
}

fn read_context(path: &Path) -> Option<Context> {
    let mut json = Vec::new();
    GzDecoder::new(std::fs::File::open(path).ok()?).read_to_end(&mut json).ok()?;
    serde_json::from_slice::<Context>(&json).ok().map(Context::indexed)
}

fn save_context(path: &Path, context: &Context) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut encoder = GzEncoder::new(std::fs::File::create(&tmp)?, Compression::default());
    encoder.write_all(&serde_json::to_vec(context).map_err(std::io::Error::other)?)?;
    encoder.finish()?;
    crate::platform::rename(tmp, path)
}

/// The saved context of `version`, or one learned and saved now.
pub fn ensure_context(cache_dir: &Path, game_dir: &Path, docs: &Docs, version: &str) -> Context {
    let path = context_path(cache_dir, version);
    if let Some(found) = read_context(&path).filter(|c| c.version == version) {
        return found;
    }
    let learned = learn(game_dir, docs, version);
    // Learning again next time is the only cost of a failed save.
    let _ = save_context(&path, &learned);
    learned
}

fn judge(context: &Context, known: &Known, anchor: &str, key: &str, kind: Ctx) -> Option<Problem> {
    if !plain(key) || is_scope(known, key) || is_logic(key) || is_flow(key) {
        return None;
    }
    let (right, other) = match kind {
        Ctx::Effect => (&known.effects, &known.triggers),
        Ctx::Trigger => (&known.triggers, &known.effects),
    };
    if is_iterator(known, key)
        || right.contains(key)
        || context.allowed.get(&allowed_key(anchor, kind)).is_some_and(|keys| keys.contains(key))
        || known.modifiers.contains(key)
        || SETTINGS.contains(&key)
    {
        return None;
    }
    if other.contains(key) {
        return Some(Problem::WrongKind);
    }
    if right.contains(&key.to_lowercase()) {
        return Some(Problem::Capitalisation);
    }
    if context.seen.contains(key) {
        return Some(Problem::Unsure);
    }
    Some(Problem::Unknown)
}

/// The problems in one file, as (line, name, kind of position, problem).
/// `rel_path` is relative to the mod's folder. A file that doesn't parse
/// gives nothing.
pub fn check_text(context: &Context, known: &Known, rel_path: &str, text: &str) -> Vec<(usize, String, Ctx, Problem)> {
    let mut found = Vec::new();
    walk_file(&context.anchors, known, rel_path, text, &mut |v| {
        if let Some(kind) = v.ctx
            && let Some(problem) = judge(context, known, v.anchor, v.key, kind)
        {
            found.push((v.line, v.key.to_owned(), kind, problem));
        }
    });
    found
}

/// Checks every enabled mod's files. A file another mod loading later ships
/// at the same path is skipped, and so are objects another mod's definition
/// replaces, since the game never reads those.
pub fn check_mods(context: &Context, known: &Known, roots: &[Option<PathBuf>], report: &Report) -> Vec<Finding> {
    let files: Vec<Vec<String>> = roots.iter().map(|root| root.as_deref().map(script_files).unwrap_or_default()).collect();
    let mut last: HashMap<String, usize> = HashMap::new();
    for (position, files) in files.iter().enumerate() {
        for file in files {
            last.insert(file.to_lowercase(), position);
        }
    }
    let mut found = Vec::new();
    for (position, (root, files)) in roots.iter().zip(&files).enumerate() {
        let Some(root) = root else { continue };
        for file in files {
            if last.get(&file.to_lowercase()) != Some(&position) {
                continue;
            }
            let Some(text) = read_text(&root.join(file)) else { continue };
            let problems = check_text(context, known, file, &text);
            if problems.is_empty() {
                continue;
            }
            let category = file_merge::category(file);
            let mut objects: HashMap<usize, Option<(String, String)>> = HashMap::new();
            for (line, name, ctx, problem) in problems {
                let object = objects.entry(line).or_insert_with(|| source::object_at(&category, &text, line).map(|(key, _)| (category.clone(), key))).clone();
                if let Some((category, key)) = &object
                    && let Some(definitions) = report.defined.get(&(category.clone(), key.clone()))
                    && definitions.len() > 1
                {
                    let conflict = ObjectConflict { category: category.clone(), key: key.clone(), definitions: definitions.clone() };
                    if let Some((winner, _)) = conflict.winner()
                        && conflict.definitions[winner].mod_position != position
                    {
                        continue;
                    }
                }
                found.push(Finding { mod_position: position, file: file.clone(), line, name, ctx, problem, object });
            }
        }
    }
    found.sort_by(|a, b| (a.mod_position, &a.file, a.line).cmp(&(b.mod_position, &b.file, b.line)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::merge::conflicts::Definition;
    use crate::paradox::script_docs::Entry as DocEntry;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("drukhal-scriptcheck-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, contents) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    fn docs() -> Docs {
        let map = |names: &[&str]| names.iter().map(|n| ((*n).to_owned(), DocEntry { text: format!("{n} - doc"), scopes: None })).collect();
        Docs {
            effects: map(&["every_owned_planet", "random_owned_planet", "add_modifier", "set_flag", "add_resource", "create_message", "country_event", "random_list", "switch", "custom_tooltip"]),
            triggers: map(&["any_owned_planet", "is_capital", "has_technology", "has_flag", "has_job_type", "is_nomadic", "has_trait", "switch"]),
            scopes: ["owner", "capital_scope"].iter().map(|s| (*s).to_owned()).collect(),
            modifiers: ["pop_happiness"].iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// A game whose events hold effects in `immediate` and triggers in
    /// `trigger`, with enough uses for both to be learned.
    fn game(name: &str) -> PathBuf {
        let game = temp(name);
        let immediate = "set_flag = a\nadd_resource = { energy = 1 }\nset_flag = b\nset_flag = c\nset_flag = d\nset_flag = e\nset_flag = f\nset_flag = g\n";
        let trigger = "has_flag = a\nis_capital = yes\nhas_flag = b\nhas_flag = c\nhas_flag = d\nhas_flag = e\nhas_flag = f\nhas_technology = g\n";
        write(
            &game,
            &[
                ("events/a.txt", &format!("namespace = a\ncountry_event = {{ id = a.1 trigger = {{ {trigger} }} immediate = {{ {immediate} every_owned_planet = {{ limit = {{ is_capital = yes }} add_modifier = {{ modifier = m days = 5 }} }} }} }}")),
                ("common/buildings/b.txt", "x = { potential = { has_technology = t } }"),
            ],
        );
        game
    }

    fn setup(name: &str) -> (PathBuf, Context, Known) {
        let game = game(name);
        let docs = docs();
        let context = learn(&game, &docs, "4.0");
        let known = known(&docs, &[&game]);
        (game, context, known)
    }

    fn names(found: &[(usize, String, Ctx, Problem)]) -> Vec<(&str, Ctx, Problem)> {
        found.iter().map(|(_, n, c, p)| (n.as_str(), *c, *p)).collect()
    }

    #[test]
    fn learns_which_blocks_hold_effects_and_triggers() {
        let (game, context, _) = setup("learns");
        assert_eq!(context.anchors.get("immediate"), Some(&Ctx::Effect));
        assert_eq!(context.anchors.get("trigger"), Some(&Ctx::Trigger));
        assert!(!context.anchors.contains_key("add_modifier"));
        assert!(!context.allowed.contains_key("add_modifier\tE"));
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn vanilla_has_no_findings() {
        let (game, context, known) = setup("vanilla");
        for file in script_files(&game) {
            let text = std::fs::read_to_string(game.join(&file)).unwrap();
            assert_eq!(check_text(&context, &known, &file, &text), vec![], "{file}");
        }
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn checks_a_mods_file() {
        let (game, context, known) = setup("mod");
        let event = |immediate: &str| format!("country_event = {{ id = m.1 immediate = {{ {immediate} }} }}");
        let check = |text: &str| check_text(&context, &known, "events/m.txt", text);

        let found = check(&event("every_system_colony = { add_modifier = { modifier = m } }"));
        assert_eq!(names(&found), [("every_system_colony", Ctx::Effect, Problem::Unknown)]);
        assert_eq!(found[0].0, 1);

        let found = check(&event("is_capital = yes"));
        assert_eq!(names(&found), [("is_capital", Ctx::Effect, Problem::WrongKind)]);

        let found = check(&event("every_Owned_planet = { set_flag = x }"));
        assert_eq!(names(&found), [("every_Owned_planet", Ctx::Effect, Problem::Capitalisation)]);

        let found = check(&event("every_owned_planet = { limit = { has_job = miner } set_flag = x }"));
        assert_eq!(names(&found), [("has_job", Ctx::Trigger, Problem::Unknown)]);

        let found = check(&event("add_modifier = { modifier = m something_else = 1 }"));
        assert_eq!(found, vec![]);

        let found = check(&event("$PARAM$ = yes @var = 5 owner = { set_flag = x } 5 = { set_flag = y } add_resource = { energy = 1 } if = { limit = { NOT = { has_flag = a } } set_flag = z } pop_happiness = 1"));
        assert_eq!(found, vec![]);

        // Triggers in a trigger block, effects used there.
        let found = check("country_event = { id = m.2 trigger = { set_flag = a any_owned_planet = { is_nomadic = yes } } }");
        assert_eq!(names(&found), [("set_flag", Ctx::Trigger, Problem::WrongKind)]);

        // A file that doesn't parse is the scan's business.
        assert_eq!(check("country_event = {"), vec![]);
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn random_list_options_have_their_own_settings() {
        let game = game("random-list");
        write(&game, &[("common/scripted_effects/v.txt", "vanilla_effect = { random_list = { 30 = { modifier = { factor = 2 has_flag = a } set_flag = a } 70 = { set_flag = b } } }")]);
        let docs = docs();
        let context = learn(&game, &docs, "4.0");
        let known = known(&docs, &[&game]);
        let text = "country_event = { id = r.1 immediate = { random_list = { 50 = { modifier = { factor = 0 } bogus_effect = yes } 50 = { set_flag = c } } } }";
        let found = check_text(&context, &known, "events/m.txt", text);
        assert_eq!(names(&found), [("bogus_effect", Ctx::Effect, Problem::Unknown)]);
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn switch_cases_are_checked_as_their_context() {
        let game = game("switch");
        write(
            &game,
            &[
                ("common/scripted_effects/v.txt", "vanilla_effect = { random_list = { 30 = { modifier = { factor = 2 } set_flag = a } 70 = { set_flag = b } } }"),
                ("events/v.txt", "country_event = { id = v.1 immediate = { switch = { trigger = has_authority auth_democratic = { add_modifier = { modifier = m } } default = { } } } }"),
            ],
        );
        let docs = docs();
        let context = learn(&game, &docs, "4.0");
        let known = known(&docs, &[&game]);
        let vanilla = std::fs::read_to_string(game.join("events/v.txt")).unwrap();
        assert_eq!(check_text(&context, &known, "events/v.txt", &vanilla), vec![]);

        let event = |inner: &str| format!("country_event = {{ id = m.1 immediate = {{ switch = {{ trigger = has_authority auth_other = {{ {inner} }} }} }} }}");
        let found = check_text(&context, &known, "events/m.txt", &event("every_system_colony = { }"));
        assert_eq!(names(&found), [("every_system_colony", Ctx::Effect, Problem::Unknown)]);
        let found = check_text(&context, &known, "events/m.txt", &event("random_list = { 50 = { modifier = { factor = 0 } } }"));
        assert_eq!(found, vec![]);
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn inline_script_arguments_and_random_lists_anywhere() {
        let game = game("inline");
        write(&game, &[("common/scripted_effects/v.txt", "vanilla_effect = { random_list = { 30 = { modifier = { factor = 2 } set_flag = a } 70 = { set_flag = b } } }")]);
        let docs = docs();
        let context = learn(&game, &docs, "4.0");
        let known = known(&docs, &[&game]);
        let event = |inner: &str| format!("country_event = {{ id = m.1 immediate = {{ {inner} }} }}");
        let check = |inner: &str| check_text(&context, &known, "events/m.txt", &event(inner));
        assert_eq!(check("inline_script = { script = x class = y climate = z }"), vec![]);
        // A random_list in a parameter block, where no effect context reaches it.
        let found = check("custom_tooltip = { random_list = { 1 = { set_flag = a modifier = { factor = 0 } } } }");
        assert_eq!(found, vec![]);
        let found = check("add_modifier = { random_list = { 1 = { modifier = { factor = 0 } } } }");
        assert_eq!(found, vec![]);
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn names_vanilla_uses_elsewhere_are_unsure() {
        let game = game("unsure");
        // `species_class` is only ever a parameter of an effect in vanilla.
        write(&game, &[("common/scripted_effects/c.txt", "y = { create_message = { species_class = x } }"), ("common/buildings/top.txt", "top_object_name = { cost = 1 }")]);
        let docs = docs();
        let context = learn(&game, &docs, "4.0");
        let known = known(&docs, &[&game]);
        assert!(context.seen.contains("species_class") && context.seen.contains("cost"));
        assert!(!context.seen.contains("top_object_name") && !context.allowed.values().any(|keys| keys.contains("species_class")));
        let text = "country_event = { id = m.1 immediate = { species_class = x every_system_colony = { } } }";
        let found = check_text(&context, &known, "events/m.txt", text);
        assert_eq!(names(&found), [("species_class", Ctx::Effect, Problem::Unsure), ("every_system_colony", Ctx::Effect, Problem::Unknown)]);
        // The tier survives the saved copy.
        let cache = temp("unsure-cache");
        ensure_context(&cache, &game, &docs, "4.0");
        let again = ensure_context(&cache, &temp("unsure-nogame"), &docs, "4.0");
        assert!(again.seen.contains("species_class"));
        let _ = std::fs::remove_dir_all(game);
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn scripted_names_count_as_known() {
        let (game, context, _) = setup("scripted");
        let root = temp("scripted-mod");
        write(
            &root,
            &[
                ("common/scripted_triggers/x.txt", "my_trigger = { is_capital = yes }\n@var = 1"),
                ("common/scripted_effects/y.txt", "my_effect = { set_flag = a my_trigger = yes }"),
                ("events/e.txt", "country_event = { id = e.1 trigger = { my_trigger = yes } immediate = { my_effect = yes } }"),
            ],
        );
        let docs = docs();
        let before = known(&docs, &[&game]);
        assert_eq!(check_text(&context, &before, "events/e.txt", &std::fs::read_to_string(root.join("events/e.txt")).unwrap()).len(), 2);
        let after = known(&docs, &[&game, &root]);
        assert!(after.triggers.contains("my_trigger") && after.effects.contains("my_effect") && !after.triggers.contains("@var"));
        assert_eq!(check_text(&context, &after, "events/e.txt", &std::fs::read_to_string(root.join("events/e.txt")).unwrap()), vec![]);
        // The scripted effect body is checked as effects: my_trigger there is a trigger.
        let found = check_text(&context, &after, "common/scripted_effects/y.txt", &std::fs::read_to_string(root.join("common/scripted_effects/y.txt")).unwrap());
        assert_eq!(names(&found), [("my_trigger", Ctx::Effect, Problem::WrongKind)]);
        let _ = std::fs::remove_dir_all(game);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_later_mod_shipping_the_same_path_hides_findings() {
        let (game, context, known) = setup("override");
        let bad = "country_event = { id = o.1 immediate = { every_system_colony = { set_flag = a } } }";
        let (first, second, third) = (temp("override-1"), temp("override-2"), temp("override-3"));
        write(&first, &[("events/e.txt", bad), ("events/f.txt", bad)]);
        write(&second, &[("events/e.txt", "country_event = { id = o.1 immediate = { set_flag = a } }")]);
        write(&third, &[("common/buildings/skip.txt", "x = { potential = { has_technology = t } }")]);
        let roots = [Some(first.clone()), Some(second.clone()), Some(third.clone()), None];
        let found = check_mods(&context, &known, &roots, &Report::default());
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].mod_position, found[0].file.as_str(), found[0].line, found[0].name.as_str()), (0, "events/f.txt", 1, "every_system_colony"));
        assert_eq!(found[0].object, Some(("events".to_owned(), "o.1".to_owned())));
        for dir in [game, first, second, third] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn only_the_winning_definition_counts() {
        let (game, context, known) = setup("winner");
        let bad = "country_event = { id = w.1 immediate = { every_system_colony = { set_flag = a } } }";
        let (first, second) = (temp("winner-1"), temp("winner-2"));
        write(&first, &[("events/a_first.txt", bad)]);
        write(&second, &[("events/b_second.txt", bad)]);
        let mut report = Report::default();
        report.defined.insert(("events".into(), "w.1".into()), vec![Definition { mod_position: 0, file: "events/a_first.txt".into() }, Definition { mod_position: 1, file: "events/b_second.txt".into() }]);
        let roots = [Some(first.clone()), Some(second.clone())];
        let found = check_mods(&context, &known, &roots, &report);
        // Events keep the first definition they read.
        assert_eq!(found.iter().map(|f| f.mod_position).collect::<Vec<_>>(), [0]);
        assert_eq!(check_mods(&context, &known, &roots, &Report::default()).len(), 2);
        for dir in [game, first, second] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn the_context_is_saved_per_version() {
        let (game, context, _) = setup("cache");
        let root = temp("cache-dir");
        let cache = root.join("script-context");
        let docs = docs();
        let first = ensure_context(&cache, &game, &docs, "4.0");
        assert_eq!(first.anchors, context.anchors);
        assert!(context_path(&cache, "4.0").is_file());
        // Served from the file: the game can go.
        std::fs::remove_dir_all(&game).unwrap();
        let again = ensure_context(&cache, &game, &docs, "4.0");
        assert_eq!(again.anchors, context.anchors);
        assert_eq!(again.allowed, context.allowed);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Runs the checker over real files, read only. Skips the override and
    /// winner filters, since there's no scan `Report` here.
    /// `DRUKHAL_STELLARIS_GAME=<game folder> DRUKHAL_STELLARIS_USER=<user folder> cargo test script_check_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn script_check_survey() {
        use crate::mods::Descriptor;
        use crate::mods::load_order::DlcLoad;

        let (Some(game), Some(user)) = (std::env::var_os("DRUKHAL_STELLARIS_GAME"), std::env::var_os("DRUKHAL_STELLARIS_USER")) else {
            println!("SURVEY skipped: set DRUKHAL_STELLARIS_GAME and DRUKHAL_STELLARIS_USER");
            return;
        };
        let (game, user) = (PathBuf::from(game), PathBuf::from(user));
        let Some(docs) = crate::paradox::script_docs::load(&user) else {
            println!("SURVEY skipped: the game hasn't written its script documentation");
            return;
        };
        let mut mods = Vec::new();
        for entry in DlcLoad::read(&user).unwrap().enabled_mods {
            if entry.contains("drukhal_merged_patch") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(user.join(&entry)) else { continue };
            let Ok(descriptor) = Descriptor::parse(&src) else { continue };
            let Some(path) = descriptor.path.clone() else { continue };
            let root = user.join(path);
            if root.is_dir() {
                mods.push((descriptor.name.clone(), root));
            }
        }
        let started = std::time::Instant::now();
        let context = learn(&game, &docs, "survey");
        println!("learned {} anchors in {} ms", context.anchors.len(), started.elapsed().as_millis());
        let mut roots: Vec<&Path> = vec![&game];
        roots.extend(mods.iter().map(|(_, r)| r.as_path()));
        let known = known(&docs, &roots);

        let mut vanilla = 0;
        for file in script_files(&game) {
            let Some(text) = read_text(&game.join(&file)) else { continue };
            let found = check_text(&context, &known, &file, &text);
            for (line, name, _, problem) in &found {
                println!("VANILLA {file}:{line} {name} {problem:?}");
            }
            vanilla += found.len();
        }
        assert_eq!(vanilla, 0, "vanilla must have no findings");

        let mut common: BTreeMap<(String, String), (usize, String)> = BTreeMap::new();
        let mut total = 0;
        for (name, root) in &mods {
            let mut count = 0;
            for file in script_files(root) {
                let Some(text) = read_text(&root.join(&file)) else { continue };
                for (line, word, _, problem) in check_text(&context, &known, &file, &text) {
                    count += 1;
                    let entry = common.entry((word, format!("{problem:?}"))).or_insert((0, format!("{name}: {file}:{line}")));
                    entry.0 += 1;
                }
            }
            if count > 0 {
                println!("{count:5} {name}");
            }
            total += count;
        }
        let mut ranked: Vec<_> = common.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        for ((word, problem), (uses, example)) in ranked.iter().take(40) {
            println!("{uses:5} {word} ({problem}) e.g. {example}");
        }
        println!("{total} findings, {} distinct names, in {} ms", ranked.len(), started.elapsed().as_millis());
    }
}
