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
use std::sync::{Arc, RwLock};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use super::conflicts::{self, ObjectConflict, Report};
use super::{file_merge, source};
use crate::paradox::script::{self, Entry, Node};
use crate::paradox::script_docs::Docs;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ctx {
    Effect,
    Trigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// Unknown, and every use looks like a call of another mod's scripted
    /// trigger or effect that no installed mod defines; or an event from a
    /// namespace nothing installed has.
    Elsewhere,
    /// Fires an event that neither the game nor an enabled mod defines,
    /// though its namespace exists, so it does nothing.
    NoEvent,
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
    /// The documented and scripted effect and trigger names vanilla's own
    /// files use (see [`collect_used`]).
    #[serde(default)]
    used: HashSet<String>,
    /// The events the game defines, as [`event_id`] gives them.
    #[serde(default)]
    events: HashSet<String>,
    /// The game's inline scripts that define an event, by script path,
    /// with the parameters that hold its id.
    #[serde(default)]
    event_templates: HashMap<String, Vec<String>>,
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
    /// The object is a scripted effect or trigger that nothing in the game
    /// or the enabled mods calls, so the finding has no effect in game.
    pub unused: bool,
    /// The use is shaped like a call of a scripted effect or trigger: `yes`,
    /// `no`, or a block of UPPERCASE parameters.
    pub call: bool,
    /// For [`Problem::NoEvent`], the events its namespace does define, as a
    /// hint to which was meant.
    pub siblings: Vec<String>,
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
    node: &'a Node,
}

/// Whether a use is shaped like a call of a scripted effect or trigger.
fn is_call(node: &Node) -> bool {
    let parameter = |k: &str| k.chars().any(|c| c.is_ascii_alphabetic()) && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    match (&node.value, &node.kids) {
        (Some(value), None) => value == "yes" || value == "no",
        (None, Some(kids)) => !kids.is_empty() && kids.iter().all(|k| k.key.as_deref().is_some_and(parameter)),
        _ => false,
    }
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
            f(&Visit { anchor, ctx, key, line: node.line, node });
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
    let Ok(nodes) = script::parse_lines(text) else { return };
    walk_parsed(anchors, known, rel, &nodes, f);
}

/// As [`walk_file`], for a file already parsed.
fn walk_parsed(anchors: &HashMap<String, Ctx>, known: &Known, rel: &str, nodes: &[Node], f: &mut dyn FnMut(&Visit<'_>)) {
    let Some((anchor, ctx)) = start(rel) else { return };
    for node in nodes {
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
    let mut files = all_files(root);
    files.retain(|f| start(f).is_some());
    files
}

/// Every `.txt` file under `common/` and `events/` of `root`, relative,
/// sorted, including the inline script fragments.
fn all_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for folder in ["common", "events"] {
        let dir = root.join(folder);
        if dir.is_dir() {
            let _ = conflicts::walk(root, &dir, &mut files);
        }
    }
    files.retain(|f| f.to_lowercase().ends_with(".txt"));
    files.sort();
    files
}

fn is_scripted_file(rel: &str) -> bool {
    let lower = rel.to_lowercase();
    lower.starts_with("common/scripted_effects/") || lower.starts_with("common/scripted_triggers/")
}

fn add_used(known: &Known, nodes: &[Node], own: Option<&str>, out: &mut HashSet<String>) {
    for node in nodes {
        for word in [node.key.as_deref(), node.value.as_deref()].into_iter().flatten() {
            if Some(word) != own && (known.effects.contains(word) || known.triggers.contains(word)) && !out.contains(word) {
                out.insert(word.to_owned());
            }
        }
        if let Some(kids) = &node.kids {
            add_used(known, kids, own, out);
        }
    }
}

/// Adds the effect and trigger names a file uses to `out`: every key and
/// every scalar value, which covers `name = yes`, `name = { PARAM = x }`
/// and `trigger = name`. The definitions of scripted effects and triggers
/// don't count, and neither does an object's own name inside its body.
fn collect_used(known: &Known, rel: &str, nodes: &[Node], out: &mut HashSet<String>) {
    if !is_scripted_file(rel) {
        add_used(known, nodes, None, out);
        return;
    }
    for node in nodes {
        if let Some(kids) = &node.kids {
            add_used(known, kids, node.key.as_deref(), out);
        }
    }
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

/// Scripted effect and trigger parameters by name: maps lowercased effect/trigger name -> set of lowercased parameter names ($PARAM$).
pub type ScriptedParams = HashMap<String, HashSet<String>>;

static CACHED_PARAMS: RwLock<Option<Arc<ScriptedParams>>> = RwLock::new(None);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndeclaredBlock {
    pub effect: String,
    pub param: String,
    pub line: usize,
}

fn default_params() -> ScriptedParams {
    let mut map = HashMap::new();
    let mut add_district = HashSet::new();
    add_district.insert("amount".to_owned());
    add_district.insert("district".to_owned());
    map.insert("add_district_and_planet_size_if_needed_effect_amount".to_owned(), add_district);
    map
}

/// Returns the cached scripted effect / trigger parameters, or default vanilla parameters if unpopulated.
pub fn cached_scripted_params() -> Arc<ScriptedParams> {
    if let Ok(lock) = CACHED_PARAMS.read()
        && let Some(params) = lock.as_ref()
    {
        return params.clone();
    }
    Arc::new(default_params())
}

/// Overwrites the cached scripted parameters (e.g. for testing).
#[allow(dead_code)]
pub fn set_cached_scripted_params(params: ScriptedParams) {
    if let Ok(mut lock) = CACHED_PARAMS.write() {
        *lock = Some(Arc::new(params));
    }
}

/// Extracts all `$PARAM$` parameter tokens from a definition body.
pub fn extract_dollar_params(text: &str, out: &mut HashSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            if let Some(rel_end) = bytes[start..].iter().position(|&b| b == b'$') {
                let end = start + rel_end;
                let token = &text[start..end];
                if !token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    out.insert(token.to_ascii_lowercase());
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// Reads the top-level scripted effects or triggers in `folder` of `root` and collects their declared `$PARAM$` parameters.
pub fn collect_scripted_params(root: &Path, folder: &str, out: &mut ScriptedParams) {
    let mut files = Vec::new();
    let dir = root.join("common").join(folder);
    if dir.is_dir() {
        let _ = conflicts::walk(root, &dir, &mut files);
    }
    for file in files.iter().filter(|f| f.to_lowercase().ends_with(".txt")) {
        let Some(text) = read_text(&root.join(file)) else { continue };
        let Ok(spans) = script::parse_with_spans(&text) else { continue };
        for (entry, range) in spans {
            if let Entry::Assign { key, value, .. } = entry
                && !key.starts_with('@')
                && value.as_block().is_some()
            {
                let mut params = HashSet::new();
                extract_dollar_params(&text[range], &mut params);
                out.entry(key.to_ascii_lowercase()).or_default().extend(params);
            }
        }
    }
}

/// Loads and caches all declared `$PARAM$` parameters of scripted effects and triggers from `roots`.
pub fn load_scripted_params(roots: &[&Path]) -> Arc<ScriptedParams> {
    let mut params = default_params();
    for root in roots {
        collect_scripted_params(root, "scripted_effects", &mut params);
        collect_scripted_params(root, "scripted_triggers", &mut params);
    }
    let arc = Arc::new(params);
    if let Ok(mut lock) = CACHED_PARAMS.write() {
        *lock = Some(arc.clone());
    }
    arc
}

/// Whether a category represents top-level definitions of scripted effects or triggers.
pub fn is_scripted_definitions_category(category: &str) -> bool {
    let lower = category.to_ascii_lowercase();
    lower.starts_with("common/scripted_effects")
        || lower.starts_with("common/scripted_triggers")
        || lower == "scripted_effects"
        || lower == "scripted_triggers"
}

/// Finds any undeclared child blocks `X = { ... }` passed inside calls to scripted effects or triggers.
/// When `is_definitions` is true (e.g. for files or objects in `common/scripted_effects` and
/// `common/scripted_triggers`), top-level nodes are treated as the definitions themselves: their
/// child blocks are effect/trigger statements and control structures (such as `if` or `switch`),
/// not call arguments.
pub fn find_undeclared_scripted_effect_blocks(
    text: &str,
    params: &ScriptedParams,
    is_definitions: bool,
) -> Vec<UndeclaredBlock> {
    if params.is_empty() {
        return Vec::new();
    }
    let Ok(nodes) = script::parse_lines(text) else { return Vec::new() };
    let mut violations = Vec::new();
    check_nodes(&nodes, params, is_definitions, &mut violations);
    violations
}

fn check_nodes(
    nodes: &[Node],
    params: &ScriptedParams,
    is_definitions: bool,
    out: &mut Vec<UndeclaredBlock>,
) {
    for node in nodes {
        // Only check for undeclared parameter blocks if this node is a CALL,
        // not a top-level scripted effect / trigger definition.
        if !is_definitions
            && let Some(key) = &node.key
        {
            let key_lower = key.to_ascii_lowercase();
            if let Some(declared) = params.get(&key_lower)
                && let Some(kids) = &node.kids
            {
                for kid in kids {
                    if let Some(kid_key) = &kid.key
                        && kid.kids.is_some()
                    {
                        let kid_lower = kid_key.to_ascii_lowercase();
                        if !declared.contains(&kid_lower) {
                            out.push(UndeclaredBlock {
                                effect: key.clone(),
                                param: kid_key.clone(),
                                line: kid.line,
                            });
                        }
                    }
                }
            }
        }
        // Descendant nodes are inside a body, so any nested scripted effect is a call.
        if let Some(kids) = &node.kids {
            check_nodes(kids, params, false, out);
        }
    }
}

/// Strips any undeclared child blocks `X = { ... }` inside scripted effect or trigger calls,
/// preserving formatting, indentation, and comments of all other lines.
#[cfg(test)]
pub fn strip_undeclared_scripted_effect_blocks(
    text: &str,
    params: &ScriptedParams,
    is_definitions: bool,
) -> String {
    let violations = find_undeclared_scripted_effect_blocks(text, params, is_definitions);
    if violations.is_empty() {
        return text.to_owned();
    }
    let mut violations = violations;
    // Sort descending by line number so stripping earlier blocks doesn't invalidate subsequent line offsets.
    violations.sort_by_key(|a| std::cmp::Reverse(a.line));

    let mut result = text.to_owned();
    for v in violations {
        result = strip_block_at_line(&result, v.line, &v.param);
    }
    result
}

#[cfg(test)]
fn strip_block_at_line(text: &str, target_line: usize, key: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if target_line < 1 || lines.is_empty() {
        return text.to_owned();
    }

    let mut line_idx = target_line.saturating_sub(1).min(lines.len() - 1);
    let key_lower = key.to_ascii_lowercase();
    let mut found_line = None;
    for offset in [0i32, -1, 1, -2, 2] {
        let check = line_idx as i32 + offset;
        if check >= 0 && (check as usize) < lines.len() {
            let l = check as usize;
            if lines[l].to_ascii_lowercase().contains(&key_lower) {
                found_line = Some(l);
                break;
            }
        }
    }
    if let Some(l) = found_line {
        line_idx = l;
    }

    let line_start_offset: usize = lines[..line_idx].iter().map(|l| l.len()).sum();
    let current_line_text = lines[line_idx];

    let Some(key_pos) = current_line_text.to_ascii_lowercase().find(&key_lower) else {
        return text.to_owned();
    };

    let block_start = if current_line_text[..key_pos].trim().is_empty() {
        line_start_offset
    } else {
        line_start_offset + key_pos
    };

    let search_from = line_start_offset + key_pos;
    let Some(open_brace_rel) = text[search_from..].find('{') else {
        return text.to_owned();
    };
    let open_brace_idx = search_from + open_brace_rel;

    let mut depth = 0;
    let mut in_quote = false;
    let mut in_comment = false;
    let mut i = open_brace_idx;
    let bytes = text.as_bytes();

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b'\r' {
            in_comment = false;
        } else if b == b'#' && !in_quote {
            in_comment = true;
        } else if b == b'"' && !in_comment {
            in_quote = !in_quote;
        } else if b == b'{' && !in_quote && !in_comment {
            depth += 1;
        } else if b == b'}' && !in_quote && !in_comment {
            depth -= 1;
            if depth == 0 {
                let mut block_end = i + 1;
                if block_end < bytes.len() && bytes[block_end] == b'\r' {
                    block_end += 1;
                }
                if block_end < bytes.len() && bytes[block_end] == b'\n' {
                    block_end += 1;
                }
                let mut stripped = String::with_capacity(text.len() - (block_end - block_start));
                stripped.push_str(&text[..block_start]);
                stripped.push_str(&text[block_end..]);
                return stripped;
            }
        }
        i += 1;
    }

    text.to_owned()
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
fn vanilla_keys(game_dir: &Path, known: &Known) -> (HashSet<String>, HashSet<String>) {
    let mut keys = HashSet::new();
    let mut used = HashSet::new();
    for file in &all_files(game_dir) {
        let Some(text) = read_text(&game_dir.join(file)) else { continue };
        let Ok(nodes) = script::parse_lines(&text) else { continue };
        collect_used(known, file, &nodes, &mut used);
        for node in &nodes {
            if let Some(kids) = &node.kids {
                collect_keys(kids, &mut keys);
            }
        }
    }
    (keys, used)
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
    let (seen, used) = vanilla_keys(game_dir, &known);
    let (events, event_templates) = vanilla_events(game_dir);
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
        return Context { version: version.to_owned(), anchors, allowed, seen, used, events, event_templates }.indexed();
    }
    let (_, allowed) = pass(&anchors, true);
    Context { version: version.to_owned(), anchors, allowed, seen, used, events, event_templates }.indexed()
}

/// Part of the saved context's name. Bump whenever learn() or the Context
/// layout changes, so a context learned by older rules isn't used.
const CONTEXT_RULES: u32 = 3;

fn context_path(cache_dir: &Path, version: &str) -> PathBuf {
    let safe: String = version.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
    cache_dir.join(format!("script-context-r{CONTEXT_RULES}-{safe}.json.gz"))
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

/// Deletes the saved contexts of other versions and rules from `dir`.
fn remove_other_contexts(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path() != keep && name.starts_with("script-context-") && name.ends_with(".json.gz") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The saved context of `version`, or one learned and saved now.
pub fn ensure_context(cache_dir: &Path, game_dir: &Path, docs: &Docs, version: &str) -> Context {
    let path = context_path(cache_dir, version);
    if let Some(found) = read_context(&path).filter(|c| c.version == version) {
        return found;
    }
    let learned = learn(game_dir, docs, version);
    // Learning again next time is the only cost of a failed save.
    if save_context(&path, &learned).is_ok() {
        remove_other_contexts(cache_dir, &path);
    }
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
    let Ok(nodes) = script::parse_lines(text) else { return Vec::new() };
    check_parsed(context, known, rel_path, &nodes)
}

/// The names `after` uses that the game certainly doesn't have, and that
/// `before` didn't already use wrongly the same way: what an edit made worse.
/// Uncertain findings (`Unsure`, `Elsewhere`) don't count.
pub fn introduced_names(context: &Context, known: &Known, rel_path: &str, before: &str, after: &str) -> Vec<String> {
    let certain = |p: &Problem| matches!(p, Problem::Unknown | Problem::WrongKind | Problem::Capitalisation);
    let had: HashSet<(String, Ctx, Problem)> = check_text(context, known, rel_path, before).into_iter().filter(|(_, _, _, p)| certain(p)).map(|(_, name, ctx, p)| (name, ctx, p)).collect();
    let mut added: Vec<String> = check_text(context, known, rel_path, after).into_iter().filter(|(_, name, ctx, p)| certain(p) && !had.contains(&(name.clone(), *ctx, *p))).map(|(_, name, _, _)| name).collect();
    added.sort();
    added.dedup();
    added
}

/// Names worth looking up in the game's documentation for an object's error:
/// the identifiers in `error` (leaving out `file`'s path) and the names in
/// `flagged`, sorted and without repeats.
pub fn reference_names(error: &str, file: &str, flagged: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = error
        .replace(file, " ")
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.contains('_') && w.len() > 3 && w.chars().any(|c| c.is_ascii_alphabetic()))
        .map(str::to_owned)
        .chain(flagged)
        .collect();
    names.sort();
    names.dedup();
    names
}

fn check_parsed(context: &Context, known: &Known, rel_path: &str, nodes: &[Node]) -> Vec<(usize, String, Ctx, Problem)> {
    check_shaped(context, known, rel_path, nodes).into_iter().map(|(line, name, ctx, problem, _)| (line, name, ctx, problem)).collect()
}

/// As [`check_parsed`], also saying whether each use is shaped like a call.
fn check_shaped(context: &Context, known: &Known, rel_path: &str, nodes: &[Node]) -> Vec<(usize, String, Ctx, Problem, bool)> {
    let mut found = Vec::new();
    walk_parsed(&context.anchors, known, rel_path, nodes, &mut |v| {
        if let Some(kind) = v.ctx
            && let Some(problem) = judge(context, known, v.anchor, v.key, kind)
        {
            found.push((v.line, v.key.to_owned(), kind, problem, is_call(v.node)));
        }
    });
    found
}

/// Checks every enabled mod's files. A file another mod loading later ships
/// at the same path is skipped, and so are objects another mod's definition
/// replaces, since the game never reads those.
/// An event id as written, the way the game compares them: lowercase, and
/// the number without leading zeros. `None` for parameters, variables and
/// anything without a namespace.
fn event_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.contains(['$', '@']) {
        return None;
    }
    let (namespace, number) = raw.rsplit_once('.')?;
    if namespace.is_empty() || number.is_empty() {
        return None;
    }
    let number = match number.parse::<u64>() {
        Ok(n) => n.to_string(),
        Err(_) => number.to_owned(),
    };
    Some(format!("{}.{}", namespace.to_lowercase(), number.to_lowercase()))
}

fn namespace(id: &str) -> &str {
    id.rsplit_once('.').map_or(id, |(n, _)| n)
}

fn is_events_file(rel: &str) -> bool {
    rel.to_lowercase().starts_with("events/")
}

/// An inline script's path as `script = …` names it, from its file under
/// `common/inline_scripts/`.
fn inline_script_name(rel: &str) -> Option<String> {
    let lower = rel.replace('\\', "/").to_lowercase();
    let rest = lower.strip_prefix("common/inline_scripts/")?;
    Some(rest.strip_suffix(".txt").unwrap_or(rest).to_owned())
}

/// The parameters an inline script uses as an event's `id`, as in
/// `id = $EVENT_ID$`.
fn id_parameters(nodes: &[Node], out: &mut Vec<String>) {
    for node in nodes {
        if node.key.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("id"))
            && let Some(param) = node.value.as_deref().and_then(|v| v.strip_prefix('$')?.strip_suffix('$'))
            && !out.iter().any(|p| p.eq_ignore_ascii_case(param))
        {
            out.push(param.to_owned());
        }
        if let Some(kids) = &node.kids {
            id_parameters(kids, out);
        }
    }
}

/// An inline script used at the top of an events file: its path and the
/// parameters it's given, keys lowercased.
type InlineUse = (String, HashMap<String, String>);

/// The events an events file defines: each top-level block's `id`. Top-level
/// inline scripts may define events too; they're returned for resolving
/// once every template is known.
fn event_definitions(rel: &str, nodes: &[Node], ids: &mut HashSet<String>, inline: &mut Vec<InlineUse>) {
    if !is_events_file(rel) {
        return;
    }
    for node in nodes {
        let (Some(key), Some(kids)) = (node.key.as_deref(), &node.kids) else { continue };
        if key.eq_ignore_ascii_case("inline_script") {
            let mut script = None;
            let mut params = HashMap::new();
            for kid in kids {
                let (Some(k), Some(v)) = (kid.key.as_deref(), kid.value.as_deref()) else { continue };
                if k.eq_ignore_ascii_case("script") {
                    script = Some(v.replace('\\', "/").to_lowercase());
                } else {
                    params.insert(k.to_lowercase(), v.to_owned());
                }
            }
            if let Some(script) = script {
                inline.push((script, params));
            }
            continue;
        }
        let id = kids.iter().find(|k| k.key.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("id"))).and_then(|k| k.value.as_deref());
        if let Some(id) = id.and_then(event_id) {
            ids.insert(id);
        }
    }
}

/// The ids the inline scripts in `inline` define, given each template's id
/// parameters.
fn resolve_inline(inline: &[InlineUse], templates: &HashMap<String, Vec<String>>, ids: &mut HashSet<String>) {
    for (script, params) in inline {
        for param in templates.get(script).into_iter().flatten() {
            if let Some(id) = params.get(&param.to_lowercase()).and_then(|v| event_id(v)) {
                ids.insert(id);
            }
        }
    }
}

/// The events a file fires, with their lines: `*_event = { id = … }`
/// blocks, and the `events` and `random_events` lists of on_actions. The
/// top-level blocks of an events file define events rather than fire them.
fn event_references(rel: &str, nodes: &[Node], out: &mut Vec<(usize, String)>) {
    fn walk(nodes: &[Node], top: bool, out: &mut Vec<(usize, String)>) {
        for node in nodes {
            let Some(kids) = &node.kids else { continue };
            let key = node.key.as_deref().unwrap_or("").to_lowercase();
            if key == "events" {
                out.extend(kids.iter().filter(|k| k.key.is_none()).filter_map(|k| Some((k.line, event_id(k.value.as_deref()?)?))));
            } else if key == "random_events" {
                out.extend(kids.iter().filter_map(|k| Some((k.line, event_id(k.value.as_deref()?)?))));
            } else if key.ends_with("event") && !top {
                let id = kids.iter().find(|k| k.key.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("id")));
                if let Some((line, id)) = id.and_then(|k| Some((k.line, event_id(k.value.as_deref()?)?))) {
                    out.push((line, id));
                }
            }
            walk(kids, false, out);
        }
    }
    walk(nodes, is_events_file(rel), out);
}

/// The events `text` fires, as the check compares them.
pub fn fired_events(rel: &str, text: &str) -> Vec<String> {
    let Ok(nodes) = script::parse_lines(text) else { return Vec::new() };
    let mut out = Vec::new();
    event_references(rel, &nodes, &mut out);
    out.into_iter().map(|(_, id)| id).collect()
}

/// The events the base game defines, and its inline scripts that define
/// events with the parameters holding their ids.
fn vanilla_events(game_dir: &Path) -> (HashSet<String>, HashMap<String, Vec<String>>) {
    let mut ids = HashSet::new();
    let mut inline = Vec::new();
    let mut templates = HashMap::new();
    for file in &all_files(game_dir) {
        let Some(text) = read_text(&game_dir.join(file)) else { continue };
        let Ok(nodes) = script::parse_lines(&text) else { continue };
        event_definitions(file, &nodes, &mut ids, &mut inline);
        if let Some(name) = inline_script_name(file) {
            let mut params = Vec::new();
            id_parameters(&nodes, &mut params);
            if !params.is_empty() {
                templates.insert(name, params);
            }
        }
    }
    resolve_inline(&inline, &templates, &mut ids);
    (ids, templates)
}

/// The objects found at lines of one file, as (category, key).
type Objects = HashMap<usize, Option<(String, String)>>;

/// The object at `line` of a mod's file, if found, or `None` when another
/// mod's copy of that object is the one the game uses. `objects` caches
/// the lookups of one file by line.
fn used_object(report: &Report, position: usize, file: &str, text: &str, line: usize, objects: &mut Objects) -> Option<Option<(String, String)>> {
    let object = objects
        .entry(line)
        .or_insert_with(|| {
            let category = file_merge::category(file);
            source::object_at(&category, text, line).map(|(key, _)| (category, key))
        })
        .clone();
    if let Some((category, key)) = &object
        && let Some(definitions) = report.defined.get(&(category.clone(), key.clone()))
        && definitions.len() > 1
    {
        let conflict = ObjectConflict { category: category.clone(), key: key.clone(), definitions: definitions.clone() };
        if let Some((winner, _)) = conflict.winner()
            && conflict.definitions[winner].mod_position != position
        {
            return None;
        }
    }
    Some(object)
}

pub fn check_mods(context: &Context, known: &Known, roots: &[Option<PathBuf>], report: &Report) -> Vec<Finding> {
    let files: Vec<Vec<String>> = roots.iter().map(|root| root.as_deref().map(all_files).unwrap_or_default()).collect();
    let mut last: HashMap<String, usize> = HashMap::new();
    for (position, files) in files.iter().enumerate() {
        for file in files {
            last.insert(file.to_lowercase(), position);
        }
    }
    let mut found = Vec::new();
    // Each file is parsed once, for the check and for what it calls.
    let mut used = context.used.clone();
    let mut events = context.events.clone();
    let mut inline = Vec::new();
    let mut templates = context.event_templates.clone();
    // The events each file fires: (position, file, line, id). Most exist,
    // so the object each is in is only looked up for those that don't.
    let mut fired: Vec<(usize, String, usize, String)> = Vec::new();
    for (position, (root, files)) in roots.iter().zip(&files).enumerate() {
        let Some(root) = root else { continue };
        for file in files {
            if last.get(&file.to_lowercase()) != Some(&position) {
                continue;
            }
            let Some(text) = read_text(&root.join(file)) else { continue };
            let Ok(nodes) = script::parse_lines(&text) else { continue };
            collect_used(known, file, &nodes, &mut used);
            event_definitions(file, &nodes, &mut events, &mut inline);
            if let Some(name) = inline_script_name(file) {
                let mut params = Vec::new();
                id_parameters(&nodes, &mut params);
                // A mod's copy replaces the game's, even without an event.
                templates.insert(name, params);
            }
            let mut fires = Vec::new();
            event_references(file, &nodes, &mut fires);
            fired.extend(fires.into_iter().map(|(line, id)| (position, file.clone(), line, id)));
            let problems = check_shaped(context, known, file, &nodes);
            if problems.is_empty() {
                continue;
            }
            let mut objects = HashMap::new();
            for (line, name, ctx, problem, call) in problems {
                let Some(object) = used_object(report, position, file, &text, line, &mut objects) else { continue };
                found.push(Finding { mod_position: position, file: file.clone(), line, name, ctx, problem, object, unused: false, call, siblings: Vec::new() });
            }
        }
    }
    // A name only ever used like a call, that nothing defines, probably
    // comes from a mod that isn't installed.
    let mut calls: HashMap<String, bool> = HashMap::new();
    for finding in &found {
        let all = calls.entry(finding.name.clone()).or_insert(true);
        *all &= finding.problem == Problem::Unknown && finding.call;
    }
    for finding in &mut found {
        if finding.problem == Problem::Unknown && calls[&finding.name] {
            finding.problem = Problem::Elsewhere;
        }
    }
    // Events nothing defines. A namespace nothing has belongs to a mod
    // that isn't installed, often on purpose for compatibility.
    resolve_inline(&inline, &templates, &mut events);
    let mut by_namespace: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in &events {
        by_namespace.entry(namespace(id)).or_default().push(id);
    }
    let mut texts: HashMap<(usize, String), (String, Objects)> = HashMap::new();
    for (position, file, line, id) in fired {
        if events.contains(&id) {
            continue;
        }
        let Some(root) = &roots[position] else { continue };
        let (text, objects) = texts.entry((position, file.clone())).or_insert_with(|| (read_text(&root.join(&file)).unwrap_or_default(), HashMap::new()));
        let Some(object) = used_object(report, position, &file, text, line, objects) else { continue };
        let (problem, siblings) = match by_namespace.get(namespace(&id)) {
            Some(ids) => {
                let mut ids: Vec<&str> = ids.clone();
                ids.sort_by_key(|i| (i.rsplit_once('.').and_then(|(_, n)| n.parse::<u64>().ok()), *i));
                (Problem::NoEvent, ids.into_iter().map(str::to_owned).collect())
            }
            None => (Problem::Elsewhere, Vec::new()),
        };
        found.push(Finding { mod_position: position, file, line, name: id, ctx: Ctx::Effect, problem, object, unused: false, call: false, siblings });
    }
    for finding in &mut found {
        finding.unused = finding.object.as_ref().is_some_and(|(category, key)| {
            let scripted = ["common/scripted_effects", "common/scripted_triggers"].iter().any(|folder| category == folder || category.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/')));
            scripted && !used.contains(key)
        });
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
        let map = |names: &[&str]| names.iter().map(|n| ((*n).to_owned(), DocEntry { text: format!("{n} - doc") })).collect();
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
    fn an_edit_may_keep_a_name_but_not_add_one() {
        let (game, context, known) = setup("introduced");
        let event = |inner: &str| format!("country_event = {{ id = m.1 immediate = {{ {inner} }} }}");
        let before = event("every_system_colony = { }");
        // Keeping the name the mod already had is fine.
        assert_eq!(introduced_names(&context, &known, "events/m.txt", &before, &event("every_system_colony = { } set_flag = x")), Vec::<String>::new());
        // Fixing it is fine too.
        assert_eq!(introduced_names(&context, &known, "events/m.txt", &before, &event("set_flag = x")), Vec::<String>::new());
        // A new unknown name, and a trigger used as an effect, are not.
        let worse = event("every_system_colony = { } invented_effect = yes has_flag = x");
        assert_eq!(introduced_names(&context, &known, "events/m.txt", &before, &worse), ["has_flag", "invented_effect"]);
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn reference_names_come_from_the_error_and_the_findings() {
        let names = reference_names("Unexpected token: has_job_x, near line: 4 file: common/scripted_effects/m.txt", "common/scripted_effects/m.txt", ["every_system_colony".to_owned(), "has_job_x".to_owned()]);
        assert_eq!(names, ["every_system_colony", "has_job_x"]);
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
        let found = check_text(&context, &known, "events/m.txt", &event("if = { limit = { is_capital = yes } add_modifier = { modifier = m } }"));
        assert_eq!(found, vec![]);
        let _ = std::fs::remove_dir_all(game);
    }

    #[test]
    fn event_ids_compare_like_the_game() {
        assert_eq!(event_id("Borigins.01").as_deref(), Some("borigins.1"));
        assert_eq!(event_id("\tmy_mod.10 ").as_deref(), Some("my_mod.10"));
        assert_eq!(event_id("$EVENT$"), None);
        assert_eq!(event_id("@ev"), None);
        assert_eq!(event_id("no_namespace"), None);
    }

    #[test]
    fn finds_events_fired_and_defined() {
        let events = "namespace = a\ncarrier_event = { id = a.1 immediate = { country_event = { id = a.2 days = 5 } planet_event = { id = $P$ } } }\ncountry_event = { id = \"a.03\" }\ninline_script = { script = events/maker EVENT_ID = a.7 OTHER = x }";
        let nodes = script::parse_lines(events).unwrap();
        let (mut ids, mut inline) = (HashSet::new(), Vec::new());
        event_definitions("events/a.txt", &nodes, &mut ids, &mut inline);
        let mut sorted: Vec<_> = ids.iter().cloned().collect();
        sorted.sort();
        assert_eq!(sorted, ["a.1", "a.3"]);
        let template = script::parse_lines("country_event = { id = $EVENT_ID$ trigger = { has_flag = $OTHER$ } }").unwrap();
        let mut params = Vec::new();
        id_parameters(&template, &mut params);
        assert_eq!(params, ["EVENT_ID"]);
        resolve_inline(&inline, &HashMap::from([("events/maker".to_owned(), params)]), &mut ids);
        assert!(ids.contains("a.7"));
        assert_eq!(inline_script_name("common/inline_scripts/Events/Maker.txt").as_deref(), Some("events/maker"));

        let mut fired = Vec::new();
        event_references("events/a.txt", &nodes, &mut fired);
        assert_eq!(fired, [(2, "a.2".to_owned())], "the top-level blocks define events");
        let on_actions = "on_monthly_pulse = {\n events = {\n a.1\n A.02\n }\n random_events = { 10 = a.5 0 = 0 }\n}\nmy_project = { on_success = { code = \"country_event = { id = c.1 }\" fleet_event = { id = b.4 } } }";
        let mut fired = Vec::new();
        event_references("common/on_actions/x.txt", &script::parse_lines(on_actions).unwrap(), &mut fired);
        let ids: Vec<&str> = fired.iter().map(|(_, id)| id.as_str()).collect();
        assert_eq!(ids, ["a.1", "a.2", "a.5", "b.4"], "quoted code isn't read");
    }

    #[test]
    fn missing_events_are_findings() {
        let game = game("events");
        write(&game, &[("events/g.txt", "namespace = g\ncountry_event = { id = g.1 }\ncountry_event = { id = g.2 }")]);
        let docs = docs();
        let context = learn(&game, &docs, "test");
        assert!(context.events.contains("g.1"));
        let mod_root = temp("events-mod");
        write(
            &mod_root,
            &[
                ("events/m.txt", "namespace = m\ncountry_event = { id = m.1 immediate = { country_event = { id = g.3 } country_event = { id = g.02 } country_event = { id = m.1 } country_event = { id = other_mod.5 } } }"),
            ],
        );
        let known = known(&docs, &[&game, &mod_root]);
        let found = check_mods(&context, &known, &[Some(mod_root.clone())], &Report::default());
        let events: Vec<(&str, Problem)> = found.iter().filter(|f| matches!(f.problem, Problem::NoEvent | Problem::Elsewhere)).map(|f| (f.name.as_str(), f.problem)).collect();
        assert_eq!(events, [("g.3", Problem::NoEvent), ("other_mod.5", Problem::Elsewhere)]);
        assert_eq!(found.iter().find(|f| f.name == "g.3").unwrap().siblings, ["g.1", "g.2"]);
        assert_eq!(found[0].object, Some(("events".to_owned(), "m.1".to_owned())));
        assert_eq!(fired_events("events/m.txt", "country_event = { id = m.9 immediate = { ship_event = { id = g.1 } } }"), ["g.1"]);
        let _ = std::fs::remove_dir_all(game);
        let _ = std::fs::remove_dir_all(mod_root);
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

    /// The findings in one mod over the fake game.
    fn unused_findings(name: &str, files: &[(&str, &str)]) -> Vec<Finding> {
        let (game, context, _) = setup(name);
        let root = temp(&format!("{name}-mod"));
        write(&root, files);
        let known = known(&docs(), &[&game, &root]);
        let found = check_mods(&context, &known, &[Some(root.clone())], &Report::default());
        let _ = std::fs::remove_dir_all(game);
        let _ = std::fs::remove_dir_all(root);
        found
    }

    /// The findings in one mod over the fake game, with whether each is unused.
    fn unused_in(name: &str, files: &[(&str, &str)]) -> Vec<(String, bool)> {
        let (game, context, _) = setup(name);
        let root = temp(&format!("{name}-mod"));
        write(&root, files);
        let known = known(&docs(), &[&game, &root]);
        let found = check_mods(&context, &known, &[Some(root.clone())], &Report::default());
        let _ = std::fs::remove_dir_all(game);
        let _ = std::fs::remove_dir_all(root);
        found.into_iter().map(|f| (f.object.map(|(_, key)| key).unwrap_or_default(), f.unused)).collect()
    }

    #[test]
    fn findings_in_uncalled_scripted_objects_are_unused() {
        let dead = ("common/scripted_effects/x.txt", "dead_fx = { every_system_colony = { } }");
        assert_eq!(unused_in("unused-dead", &[dead]), [("dead_fx".to_owned(), true)]);

        let call = ("events/e.txt", "country_event = { id = e.1 immediate = { dead_fx = yes } }");
        assert_eq!(unused_in("unused-called", &[dead, call]), [("dead_fx".to_owned(), false)]);

        let trigger = ("common/scripted_triggers/t.txt", "my_trg = { bogus_trigger_word = yes }");
        let switch = ("events/e.txt", "country_event = { id = e.1 immediate = { switch = { trigger = my_trg a = { } } } }");
        assert_eq!(unused_in("unused-switch", &[trigger, switch]), [("my_trg".to_owned(), false)]);
        assert_eq!(unused_in("unused-trigger", &[trigger]), [("my_trg".to_owned(), true)]);

        let own = ("common/scripted_effects/y.txt", "loop_fx = { every_system_colony = { } loop_fx = yes }");
        assert_eq!(unused_in("unused-self", &[own]), [("loop_fx".to_owned(), true)]);

        // Called by another mod's file, or by a fragment, counts too.
        let fragment = ("common/inline_scripts/f.txt", "dead_fx = yes");
        assert_eq!(unused_in("unused-fragment", &[dead, fragment]), [("dead_fx".to_owned(), false)]);
        // Other objects are never unused.
        let event = ("events/o.txt", "country_event = { id = o.1 immediate = { every_system_colony = { } } }");
        assert_eq!(unused_in("unused-event", &[event]), [("o.1".to_owned(), false)]);
    }

    #[test]
    fn scripted_names_vanilla_calls_are_used() {
        let game = game("unused-vanilla");
        write(&game, &[("common/scripted_effects/v.txt", "vanilla_fx = { set_flag = a }"), ("events/v.txt", "country_event = { id = v.1 immediate = { vanilla_fx = yes } }")]);
        let docs = docs();
        let context = learn(&game, &docs, "4.0");
        assert!(context.used.contains("vanilla_fx"));
        let root = temp("unused-vanilla-mod");
        write(&root, &[("common/scripted_effects/v.txt", "vanilla_fx = { every_system_colony = { } }")]);
        let known = known(&docs, &[&game, &root]);
        let found = check_mods(&context, &known, &[Some(root.clone())], &Report::default());
        assert_eq!(found.len(), 1);
        assert!(!found[0].unused);
        let _ = std::fs::remove_dir_all(game);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn call_shaped_names_nothing_defines_are_elsewhere() {
        let call = |a: &str, b: &str| format!("country_event = {{ id = c.1 immediate = {{ {a} }} }}\ncountry_event = {{ id = c.2 immediate = {{ {b} }} }}");
        let problems = |name: &str, text: String| {
            let found = unused_findings(name, &[("events/c.txt", &text)]);
            found.into_iter().map(|f| (f.name, f.problem)).collect::<Vec<_>>()
        };
        let elsewhere = |n: &str| (n.to_owned(), Problem::Elsewhere);
        let unknown = |n: &str| (n.to_owned(), Problem::Unknown);
        assert_eq!(problems("else-yes", call("foo_trg = yes", "foo_trg = no")), [elsewhere("foo_trg"), elsewhere("foo_trg")]);
        assert_eq!(problems("else-mixed", call("foo_trg = yes", "foo_trg = miner")), [unknown("foo_trg"), unknown("foo_trg")]);
        assert_eq!(problems("else-block", call("fx = { PARAM = 1 OTHER_2 = x }", "set_flag = a")), [elsewhere("fx")]);
        assert_eq!(problems("else-iterator", call("every_system_colony = { limit = { } }", "set_flag = a")), [unknown("every_system_colony")]);
        assert_eq!(problems("else-empty", call("fx = { }", "set_flag = a")), [unknown("fx")]);
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

    #[test]
    fn a_context_from_older_rules_is_replaced() {
        let (game, context, _) = setup("rules");
        let root = temp("rules-cache");
        let old = root.join("script-context-4.0.json.gz");
        save_context(&old, &context).unwrap();
        let unrelated = root.join("other.json.gz");
        std::fs::write(&unrelated, "x").unwrap();
        assert!(read_context(&old).is_some());
        // Not loaded: a fresh one is learned and the old file goes.
        let docs = docs();
        ensure_context(&root, &game, &docs, "4.0");
        assert!(context_path(&root, "4.0").is_file());
        assert!(!old.exists());
        assert!(unrelated.exists());
        let _ = std::fs::remove_dir_all(game);
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
        // Every mod in a folder, such as the Workshop's, instead of the enabled ones.
        if let Some(dir) = std::env::var_os("DRUKHAL_MODS_DIR") {
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for root in dirs {
                let name = std::fs::read_to_string(root.join("descriptor.mod")).ok().and_then(|s| Descriptor::parse(&s).ok()).map_or_else(|| root.display().to_string(), |d| d.name);
                mods.push((name, root));
            }
        }
        let enabled = if mods.is_empty() { DlcLoad::read(&user).unwrap().enabled_mods } else { Vec::new() };
        for entry in enabled {
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
        let mut dead = Vec::new();
        for file in all_files(&game) {
            let Some(text) = read_text(&game.join(&file)) else { continue };
            dead.extend(fired_events(&file, &text).into_iter().filter(|id| !context.events.contains(id)).map(|id| format!("{file}: {id}")));
        }
        println!("{} game events, {} game inline scripts define events; the game fires {} it doesn't define: {dead:?}", context.events.len(), context.event_templates.len(), dead.len());

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

        // The same, split by whether anything calls the object they're in.
        let timed = std::time::Instant::now();
        let roots: Vec<Option<PathBuf>> = mods.iter().map(|(_, root)| Some(root.clone())).collect();
        let found = check_mods(&context, &known, &roots, &Report::default());
        println!("check_mods took {} ms", timed.elapsed().as_millis());
        for (position, (name, _)) in mods.iter().enumerate() {
            let here: Vec<&Finding> = found.iter().filter(|f| f.mod_position == position).collect();
            if here.is_empty() {
                continue;
            }
            let unused = here.iter().filter(|f| f.unused).count();
            let unsure = here.iter().filter(|f| !f.unused && f.problem == Problem::Unsure).count();
            let elsewhere = here.iter().filter(|f| !f.unused && f.problem == Problem::Elsewhere).count();
            println!("{:5} live, {unused:5} unused, {unsure:5} unsure, {elsewhere:5} elsewhere  {name}", here.len() - unused - unsure - elsewhere);
        }
        let mut elsewhere: BTreeMap<&str, usize> = BTreeMap::new();
        for finding in found.iter().filter(|f| f.problem == Problem::Elsewhere) {
            *elsewhere.entry(finding.name.as_str()).or_default() += 1;
        }
        for (name, uses) in &elsewhere {
            println!("ELSEWHERE {uses:5} {name}");
        }
        for f in found.iter().filter(|f| f.problem == Problem::NoEvent) {
            let shown: Vec<&str> = f.siblings.iter().take(8).map(String::as_str).collect();
            println!("NO EVENT {}{} {}:{} {} (namespace has {}: {shown:?})", mods[f.mod_position].0, if f.unused { " [unused]" } else { "" }, f.file, f.line, f.name, f.siblings.len());
        }
    }

    #[test]
    fn extracts_dollar_parameters_from_effect_definition() {
        let def = r#"
        add_district_and_planet_size_if_needed_effect_amount = {
            add_district = {
                district = $district$
                amount = $amount$
            }
        }
        "#;
        let mut params = HashSet::new();
        extract_dollar_params(def, &mut params);
        assert!(params.contains("district"));
        assert!(params.contains("amount"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn detects_and_strips_undeclared_scripted_effect_child_blocks() {
        let mut params = HashMap::new();
        let mut declared = HashSet::new();
        declared.insert("amount".to_owned());
        declared.insert("district".to_owned());
        params.insert("add_district_and_planet_size_if_needed_effect_amount".to_owned(), declared.clone());

        let input = "\
# Arkship Decks
if = {
\tlimit = {
\t\tis_planet_class = pc_ark
\t}
\t# merged: combined amount logic using Expanded Mods Base variable with Exotic Worlds fallback check
\tadd_district_and_planet_size_if_needed_effect_amount = {
\t\tamount = ark_basic_districts
\t\tark_basic_districts = {
\t\t\tmin = 1
\t\t}
\t\tdistrict = district_ark_basic
\t}
}
";

        let violations = find_undeclared_scripted_effect_blocks(input, &params, false);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].effect, "add_district_and_planet_size_if_needed_effect_amount");
        assert_eq!(violations[0].param, "ark_basic_districts");

        let stripped = strip_undeclared_scripted_effect_blocks(input, &params, false);
        let expected = "\
# Arkship Decks
if = {
\tlimit = {
\t\tis_planet_class = pc_ark
\t}
\t# merged: combined amount logic using Expanded Mods Base variable with Exotic Worlds fallback check
\tadd_district_and_planet_size_if_needed_effect_amount = {
\t\tamount = ark_basic_districts
\t\tdistrict = district_ark_basic
\t}
}
";
        assert_eq!(stripped, expected);

        // Stripping again is a no-op
        assert_eq!(strip_undeclared_scripted_effect_blocks(&stripped, &params, false), expected);
        assert!(find_undeclared_scripted_effect_blocks(&stripped, &params, false).is_empty());

        // When checking a scripted effect definition itself (is_definitions = true):
        // Top-level if = { ... }, switch = { ... } blocks are effect implementations, not parameter calls.
        let mut def_params = HashMap::new();
        def_params.insert("mzilli_star_crisis_heat_planets".to_owned(), HashSet::new());
        def_params.insert("add_district_and_planet_size_if_needed_effect_amount".to_owned(), declared);

        let def_input = "\
mzilli_star_crisis_heat_planets = {
\tif = {
\t\tlimit = { is_terraforming = yes }
\t\tcancel_terraformation = yes
\t}
\tswitch = {
\t\ttrigger = is_planet_class
\t\tpc_molten = {
\t\t\tset_ring = no
\t\t}
\t}
}
";
        assert!(find_undeclared_scripted_effect_blocks(def_input, &def_params, true).is_empty());
        assert_eq!(strip_undeclared_scripted_effect_blocks(def_input, &def_params, true), def_input);

        // But if the definition calls another scripted effect with an undeclared child block:
        let def_with_bad_nested_call = "\
mzilli_star_crisis_heat_planets = {
\tif = {
\t\tlimit = { is_terraforming = yes }
\t\tadd_district_and_planet_size_if_needed_effect_amount = {
\t\t\tamount = 1
\t\t\tstray_block = { min = 1 }
\t\t}
\t}
}
";
        let nested_violations = find_undeclared_scripted_effect_blocks(def_with_bad_nested_call, &def_params, true);
        assert_eq!(nested_violations.len(), 1);
        assert_eq!(nested_violations[0].effect, "add_district_and_planet_size_if_needed_effect_amount");
        assert_eq!(nested_violations[0].param, "stray_block");
    }
}
