//! Structured validation of merged Paradox scripts (both single objects and
//! whole files): syntax, undeclared scripted-effect blocks, duplicate
//! single-valued keys, per-source addition coverage, and optional script-doc
//! name checks.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::script_check::{self, Context as ScriptContext, Known};
use super::source::{self, Source};
use crate::paradox::script::{self, Entry, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    Syntax,
    Structure,
    ScriptedEffectBlock,
    DuplicateScalarKey,
    DroppedAddition,
    UnknownScriptName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub kind: ProblemKind,
    pub message: String,
    /// Source mod label if this problem is about a dropped addition from that mod.
    pub source_label: Option<String>,
}

impl Problem {
    fn error(kind: ProblemKind, message: String) -> Self {
        Self {
            severity: Severity::Error,
            kind,
            message,
            source_label: None,
        }
    }

    fn warning(kind: ProblemKind, message: String) -> Self {
        Self {
            severity: Severity::Warning,
            kind,
            message,
            source_label: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    pub normalized: String,
    pub problems: Vec<Problem>,
}

impl Validation {
    pub fn is_ok(&self) -> bool {
        !self.problems.iter().any(|p| p.severity == Severity::Error)
    }

    pub fn errors(&self) -> impl Iterator<Item = &Problem> {
        self.problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
    }

    pub fn error_summary(&self) -> Option<String> {
        let errs: Vec<&str> = self.errors().map(|p| p.message.as_str()).collect();
        if errs.is_empty() {
            None
        } else {
            Some(errs.join("; "))
        }
    }
}

/// Validates a merged single object against syntax, key uniqueness, and
/// per-source addition coverage.
pub fn validate_object(
    category: &str,
    key: &str,
    text: &str,
    vanilla: Option<&Source>,
    sources: &[Source],
    script_ctx: Option<(&ScriptContext, &Known)>,
) -> Validation {
    let mut problems = Vec::new();
    if category == "localisation" {
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let name = key.split_once(':').map_or(key, |(_, k)| k);
        let normalized = match lines[..] {
            [line] if line.split_once(':').is_some_and(|(k, _)| k == name) => line.to_owned(),
            _ => {
                problems.push(Problem::error(
                    ProblemKind::Structure,
                    tf!(
                        "A localisation merge must be a single line starting with `{name}:`",
                        name = name
                    ),
                ));
                text.trim().to_owned()
            }
        };
        return Validation {
            normalized,
            problems,
        };
    }

    let entries = match script::parse(text) {
        Ok(e) => e,
        Err(e) => {
            problems.push(Problem::error(
                ProblemKind::Syntax,
                tf!("The merged script has a syntax error at {error}", error = e),
            ));
            return Validation {
                normalized: text.trim().to_owned(),
                problems,
            };
        }
    };

    let objects = entries
        .iter()
        .filter(|entry| match entry {
            Entry::Assign {
                key: entry_key, ..
            } => !entry_key.starts_with('@') || entry_key.as_str() == key,
            _ => false,
        })
        .count();
    if objects != 1 {
        problems.push(Problem::error(
            ProblemKind::Structure,
            tf!(
                "The merged script must define `{key}` and nothing else",
                key = key
            ),
        ));
    } else if source::extract(category, key, text).is_none() {
        problems.push(Problem::error(
            ProblemKind::Structure,
            tf!("The merged script must define `{key}`", key = key),
        ));
    }

    let params = script_check::cached_scripted_params();
    let is_definitions = script_check::is_scripted_definitions_category(category);
    for v in script_check::find_undeclared_scripted_effect_blocks(text, &params, is_definitions) {
        problems.push(Problem::error(
            ProblemKind::ScriptedEffectBlock,
            tf!(
                "Invalid child block `{param} = {{ ... }}` inside scripted effect/trigger `{effect}` near line {line}",
                param = &v.param,
                effect = &v.effect,
                line = v.line
            ),
        ));
    }

    check_duplicate_single_valued_keys(category, &entries, &mut problems);
    if !sources.is_empty() && objects == 1 {
        check_source_coverage(&entries, vanilla, sources, &mut problems);
    }
    if let Some((ctx, known)) = script_ctx {
        check_unknown_script_names(ctx, known, category, text, vanilla, sources, &mut problems);
    }

    Validation {
        normalized: text.trim().to_owned(),
        problems,
    }
}

/// Validates a merged whole file against syntax, truncation, brace balance,
/// scripted-effect blocks, duplicate scalar keys, and per-source coverage.
pub fn validate_file(
    path: &str,
    text: &str,
    vanilla: Option<&Source>,
    sources: &[Source],
    script_ctx: Option<(&ScriptContext, &Known)>,
) -> Validation {
    let mut problems = Vec::new();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        problems.push(Problem::error(
            ProblemKind::Structure,
            t!("The merged script is empty"),
        ));
        return Validation {
            normalized: String::new(),
            problems,
        };
    }
    for placeholder in [
        "... rest of",
        "... remaining",
        "... existing",
        "rest of file",
        "remainder of file",
    ] {
        if trimmed.to_lowercase().contains(placeholder) {
            problems.push(Problem::error(
                ProblemKind::Structure,
                format!("The merged script contains a truncation placeholder ('{placeholder}') instead of the full file"),
            ));
        }
    }
    let category = super::file_merge::category(path);
    if category == "localisation" {
        for problem in localisation_problems(text) {
            problems.push(Problem::error(ProblemKind::Structure, problem));
        }
        return Validation {
            normalized: trimmed.to_owned(),
            problems,
        };
    }

    let mut depth: i32 = 0;
    let mut in_quote = false;
    let mut in_comment = false;
    for ch in text.chars() {
        match ch {
            '\n' | '\r' => in_comment = false,
            '#' if !in_quote => in_comment = true,
            '"' if !in_comment => in_quote = !in_quote,
            '{' if !in_quote && !in_comment => depth += 1,
            '}' if !in_quote && !in_comment => {
                depth -= 1;
                if depth < 0 {
                    problems.push(Problem::error(
                        ProblemKind::Syntax,
                        t!("The merged script has an unexpected closing brace `}` before an opening brace"),
                    ));
                    return Validation {
                        normalized: trimmed.to_owned(),
                        problems,
                    };
                }
            }
            _ => {}
        }
    }
    if depth > 0 {
        problems.push(Problem::error(
            ProblemKind::Syntax,
            tf!(
                "The merged script is missing {depth} closing brace `}}`",
                depth = depth
            ),
        ));
        return Validation {
            normalized: trimmed.to_owned(),
            problems,
        };
    }

    if category != "localisation" {
        match script::parse(text) {
            Ok(entries) => {
                let params = script_check::cached_scripted_params();
                let is_definitions = script_check::is_scripted_definitions_category(&category);
                for v in script_check::find_undeclared_scripted_effect_blocks(
                    text,
                    &params,
                    is_definitions,
                ) {
                    problems.push(Problem::error(
                        ProblemKind::ScriptedEffectBlock,
                        tf!(
                            "Invalid child block `{param} = {{ ... }}` inside scripted effect/trigger `{effect}` near line {line}",
                            param = &v.param,
                            effect = &v.effect,
                            line = v.line
                        ),
                    ));
                }
                check_duplicate_single_valued_keys(&category, &entries, &mut problems);
                if !sources.is_empty() {
                    check_source_coverage(&entries, vanilla, sources, &mut problems);
                }
                if let Some((ctx, known)) = script_ctx {
                    check_unknown_script_names(
                        ctx,
                        known,
                        &category,
                        text,
                        vanilla,
                        sources,
                        &mut problems,
                    );
                }
            }
            Err(e) => {
                problems.push(Problem::error(
                    ProblemKind::Syntax,
                    format!("The merged script has a syntax error at {e}"),
                ));
            }
        }
    }

    Validation {
        normalized: trimmed.to_owned(),
        problems,
    }
}

/// A localisation line without its `#` comment; a `#` inside quotes is text.
fn localisation_without_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Problems in a whole localisation file: language headers and
/// `key:version "text"` lines, with closed quotes.
pub(crate) fn localisation_problems(text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut in_language = false;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_start_matches('\u{feff}');
        let trimmed = localisation_without_comment(line).trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let number = index + 1;
        if !line.starts_with([' ', '\t']) && trimmed.ends_with(':') {
            let header = trimmed.trim_end_matches(':');
            if crate::merge::conflicts::is_language_header(header) {
                in_language = true;
            } else {
                problems.push(format!("Line {number}: `{header}` is not a language header"));
            }
            continue;
        }
        if !in_language {
            problems.push(format!("Line {number}: entry before any language header"));
            continue;
        }
        let Some((key, rest)) = trimmed.split_once(':') else {
            problems.push(format!("Line {number}: missing `key:version \"text\"`"));
            continue;
        };
        if key.is_empty() || key.contains([' ', '"']) {
            problems.push(format!("Line {number}: invalid localisation key `{key}`"));
            continue;
        }
        let rest = rest.trim_start();
        let (version, quoted) = if rest.starts_with('"') {
            ("", rest)
        } else {
            match rest.split_once(char::is_whitespace) {
                Some((version, quoted)) => (version, quoted.trim_start()),
                None => ("", rest),
            }
        };
        if !version.is_empty() && !version.bytes().all(|b| b.is_ascii_digit()) {
            problems.push(format!("Line {number}: `{version}` is not a version number"));
            continue;
        }
        if !quoted.starts_with('"') || !quoted.ends_with('"') || quoted.len() < 2 {
            problems.push(format!("Line {number}: text must be quoted"));
            continue;
        }
        let inner = &quoted[1..quoted.len() - 1];
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => drop(chars.next()),
                '"' => {
                    problems.push(format!("Line {number}: unescaped `\"` in text"));
                    break;
                }
                _ => {}
            }
        }
    }
    if !in_language && problems.is_empty() {
        problems.push("The localisation file has no `l_<language>:` header".into());
    }
    problems
}

/// Returns whether `key` is single-valued (must appear at most once per block)
/// in `category`. Block-valued keys count too: duplicate `potential`, `allow`
/// or `cost` blocks would otherwise silently drop one mod's change.
fn is_single_valued_key(category: &str, key: &str) -> bool {
    let lower_cat = category.to_ascii_lowercase();
    let lower_key = key.to_ascii_lowercase();
    let common = [
        "category",
        "icon",
        "picture",
        "show_sound",
        "potential",
        "allow",
        "trigger",
    ];
    if common.contains(&lower_key.as_str()) {
        return true;
    }
    if lower_cat.contains("buildings") {
        return matches!(
            lower_key.as_str(),
            "base_buildtime"
                | "base_cap_amount"
                | "can_build"
                | "can_demolish"
                | "can_be_ruined"
                | "can_be_disabled"
                | "is_capped_by_modifier"
                | "exempt_from_ai_planet_specialization"
                | "position_priority"
                | "cost"
        );
    }
    if lower_cat.contains("pop_jobs") {
        return matches!(
            lower_key.as_str(),
            "condition_string"
                | "building_icon"
                | "clothes_texture_index"
                | "exempt_from_ai_economy_specialization"
                | "is_capped_by_modifier"
                | "count_as_available_for_ai"
        );
    }
    if lower_cat.contains("technology") {
        return matches!(
            lower_key.as_str(),
            "area"
                | "tier"
                | "cost"
                | "is_rare"
                | "is_dangerous"
                | "is_reverse_engineerable"
                | "start_tech"
        );
    }
    if lower_cat == "events" {
        return matches!(
            lower_key.as_str(),
            "id" | "title"
                | "is_triggered_only"
                | "hide_window"
                | "fire_only_once"
                | "mean_time_to_happen"
        );
    }
    false
}

fn check_duplicate_single_valued_keys(
    category: &str,
    entries: &[Entry],
    problems: &mut Vec<Problem>,
) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        match entry {
            Entry::Assign { key, value, .. } => {
                if is_single_valued_key(category, key) {
                    let k = key.to_ascii_lowercase();
                    let n = counts.entry(k).or_insert(0);
                    *n += 1;
                    if *n == 2 {
                        problems.push(Problem::error(
                            ProblemKind::DuplicateScalarKey,
                            tf!(
                                "Single-valued key `{key}` appears more than once in the same block",
                                key = key
                            ),
                        ));
                    }
                }
                if let Value::Block(inner) = value {
                    check_duplicate_single_valued_keys(category, inner, problems);
                }
            }
            Entry::Item(Value::Block(inner)) => {
                check_duplicate_single_valued_keys(category, inner, problems)
            }
            Entry::Item(Value::Scalar(_)) => {}
        }
    }
}

/// Flattens an entry tree into path-qualified normalized leaf signatures
/// (`parent/key=value`, `parent/key={}`) for coverage checking. Transparently
/// flattens `AND` / `OR` wrappers so legitimate trigger restructuring doesn't
/// trigger false positives.
pub(crate) fn leaf_signatures(entries: &[Entry], out: &mut Vec<String>) {
    leaves_at("", entries, out);
}

fn join_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}/{key}")
    }
}

fn normalized_scalar(value: &str) -> String {
    value.trim().trim_matches('"').to_ascii_lowercase()
}

fn leaves_at(prefix: &str, entries: &[Entry], out: &mut Vec<String>) {
    for entry in entries {
        match entry {
            Entry::Assign { key, value, .. } => {
                let lk = key.to_ascii_lowercase();
                let path = join_path(prefix, &lk);
                match value {
                    Value::Scalar(v) => out.push(format!("{path}={}", normalized_scalar(v))),
                    Value::Block(inner) => {
                        if matches!(lk.as_str(), "and" | "or") {
                            leaves_at(prefix, inner, out);
                        } else {
                            out.push(format!("{path}={{}}"));
                            leaves_at(&path, inner, out);
                        }
                    }
                }
            }
            Entry::Item(Value::Scalar(s)) => out.push(if prefix.is_empty() {
                normalized_scalar(s)
            } else {
                format!("{prefix}={}", normalized_scalar(s))
            }),
            Entry::Item(Value::Block(inner)) => leaves_at(prefix, inner, out),
        }
    }
}

/// The scalar values each path has in `leaves`.
fn values_by_path(leaves: &[String], out: &mut HashMap<String, HashSet<String>>) {
    for leaf in leaves {
        if let Some((path, value)) = leaf.split_once('=')
            && value != "{}"
        {
            out.entry(path.to_owned())
                .or_default()
                .insert(value.to_owned());
        }
    }
}

fn leaves_of(text: &str) -> Option<Vec<String>> {
    let entries = script::parse(text).ok()?;
    let mut leaves = Vec::new();
    leaf_signatures(&entries, &mut leaves);
    Some(leaves)
}

/// Collapses the two ways Stellaris spells the same ethic condition.
/// `has_ethic = ethic_pacifist` together with
/// `has_ethic = ethic_fanatic_pacifist` means the same as
/// `is_pacifist = yes`; the xenophile and xenophobe pairs work the same
/// way, and a fanatic ethic alone matches its `is_fanatic_*` trigger.
/// Without this, a merge that rewrites one form as the other looks like it
/// dropped an addition.
fn normalize_ethics(leaves: &mut Vec<String>) {
    let mut ethics: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    leaves.retain(|leaf| {
        let Some((path, value)) = leaf.split_once('=') else { return true };
        let Some(parent) = path.strip_suffix("/has_ethic") else { return true };
        ethics.entry(parent.to_owned()).or_default().insert(value.to_owned());
        false
    });
    for (parent, values) in ethics {
        let mut rest = values;
        let mut normalized = Vec::new();
        for base in [
            "materialist",
            "spiritualist",
            "militarist",
            "pacifist",
            "xenophile",
            "xenophobe",
            "authoritarian",
            "egalitarian",
        ] {
            let ethic = format!("ethic_{base}");
            let fanatic = format!("ethic_fanatic_{base}");
            if rest.contains(&ethic) && rest.contains(&fanatic) {
                rest.remove(&ethic);
                rest.remove(&fanatic);
                normalized.push(format!("{parent}/is_{base}=yes"));
            } else if rest.remove(&fanatic) {
                normalized.push(format!("{parent}/is_fanatic_{base}=yes"));
            }
        }
        for value in rest {
            normalized.push(format!("{parent}/has_ethic={value}"));
        }
        leaves.extend(normalized);
    }
}

/// Rewrites the boolean leaves of `NOR = { is_x = yes }` and
/// `NOT = { is_x = yes }` as `is_x = no` at the parent path. Merges often
/// rewrite the block form as the flat one, which is equivalent.
fn normalize_negations(leaves: &mut Vec<String>) {
    // The block marker itself isn't a change; its leaves carry the meaning.
    leaves.retain(|leaf| {
        !matches!(
            leaf.rsplit_once('='),
            Some((path, "{}")) if path == "nor" || path == "not" || path.ends_with("/nor") || path.ends_with("/not")
        )
    });
    for leaf in leaves {
        let Some((path, value)) = leaf.split_once('=') else { continue };
        if value != "yes" && value != "no" {
            continue;
        }
        let Some((block, key)) = path.rsplit_once('/') else { continue };
        if !block.ends_with("/nor") && !block.ends_with("/not") {
            continue;
        }
        if !key.starts_with("is_") {
            continue;
        }
        let Some((parent, _)) = block.rsplit_once('/') else { continue };
        let flipped = if value == "yes" { "no" } else { "yes" };
        *leaf = format!("{parent}/{key}={flipped}");
    }
}

/// Checks that additions each source made relative to `vanilla` survive in `result`.
fn check_source_coverage(
    result_entries: &[Entry],
    vanilla: Option<&Source>,
    sources: &[Source],
    problems: &mut Vec<Problem>,
) {
    let mut result_leaves = Vec::new();
    leaf_signatures(result_entries, &mut result_leaves);
    normalize_ethics(&mut result_leaves);
    normalize_negations(&mut result_leaves);
    let result_set: HashSet<String> = result_leaves.iter().cloned().collect();
    let mut result_values: HashMap<String, HashSet<String>> = HashMap::new();
    values_by_path(&result_leaves, &mut result_values);

    let mut vanilla_leaves = vanilla.and_then(|v| leaves_of(&v.text)).unwrap_or_default();
    normalize_ethics(&mut vanilla_leaves);
    normalize_negations(&mut vanilla_leaves);
    let vanilla_set: HashSet<String> = vanilla_leaves.iter().cloned().collect();
    let mut vanilla_values: HashMap<String, HashSet<String>> = HashMap::new();
    values_by_path(&vanilla_leaves, &mut vanilla_values);

    // Values each source *changed* at a path. A source that keeps the base
    // game's value doesn't take part in a priority decision.
    let mut changed_values: HashMap<String, HashSet<String>> = HashMap::new();
    let mut by_source: Vec<Vec<String>> = Vec::new();
    for s in sources {
        let mut leaves = leaves_of(&s.text).unwrap_or_default();
        normalize_ethics(&mut leaves);
        normalize_negations(&mut leaves);
        for leaf in &leaves {
            if vanilla_set.contains(leaf) {
                continue;
            }
            if let Some((path, value)) = leaf.split_once('=')
                && value != "{}"
            {
                let unchanged = vanilla_values
                    .get(path)
                    .is_some_and(|values| values.contains(value));
                if !unchanged {
                    changed_values
                        .entry(path.to_owned())
                        .or_default()
                        .insert(value.to_owned());
                }
            }
        }
        by_source.push(leaves);
    }

    for (s, leaves) in sources.iter().zip(by_source) {
        let mut dropped = Vec::new();
        let mut seen = HashSet::new();
        for leaf in leaves {
            if !seen.insert(leaf.clone())
                || vanilla_set.contains(&leaf)
                || result_set.contains(&leaf)
            {
                continue;
            }
            if let Some((path, value)) = leaf.split_once('=')
                && value != "{}"
            {
                // A game update sometimes renames a script value. Keeping
                // the current game's `value:...` reference instead of an
                // older mod's one is a priority choice, not a lost change.
                let script_value_pick = value.starts_with("value:")
                    && result_values
                        .get(path)
                        .is_some_and(|kept| kept.iter().any(|v| v.starts_with("value:")));
                if script_value_pick {
                    continue;
                }
                // Several sources changed this path differently and the
                // result kept one of those values: priority chose it, so the
                // losing sources aren't missing anything.
                let priority_pick = changed_values.get(path).is_some_and(|changed| {
                    changed.len() > 1
                        && result_values
                            .get(path)
                            .is_some_and(|kept| kept.iter().any(|v| changed.contains(v)))
                });
                if priority_pick {
                    continue;
                }
            }
            dropped.push(leaf);
        }
        if !dropped.is_empty() {
            let sample: Vec<&str> = dropped.iter().take(4).map(String::as_str).collect();
            let more = if dropped.len() > 4 {
                format!(" (+{} more)", dropped.len() - 4)
            } else {
                String::new()
            };
            problems.push(Problem {
                severity: Severity::Error,
                kind: ProblemKind::DroppedAddition,
                message: tf!(
                    "Missing additions from {name}: {items}{more}",
                    name = &s.label,
                    items = sample.join(", "),
                    more = more
                ),
                source_label: Some(s.label.clone()),
            });
        }
    }
}

fn check_unknown_script_names(
    ctx: &ScriptContext,
    known: &Known,
    category: &str,
    text: &str,
    vanilla: Option<&Source>,
    sources: &[Source],
    problems: &mut Vec<Problem>,
) {
    let rel_path = format!("{category}/merged.txt");
    let mut allowed: HashSet<String> = HashSet::new();
    for s in sources.iter().chain(vanilla) {
        for (_, name, _, _) in script_check::check_text(ctx, known, &rel_path, &s.text) {
            allowed.insert(name);
        }
    }
    let found = script_check::check_text(ctx, known, &rel_path, text);
    let mut flagged = Vec::new();
    for (_, name, _, problem) in found {
        if matches!(
            problem,
            script_check::Problem::Unknown | script_check::Problem::WrongKind
        ) && !allowed.contains(&name)
            && !flagged.contains(&name)
        {
            flagged.push(name);
        }
    }
    if !flagged.is_empty() {
        problems.push(Problem::warning(
            ProblemKind::UnknownScriptName,
            tf!(
                "Unknown or misplaced script names introduced: {names}",
                names = flagged.join(", ")
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(label: &str, text: &str) -> Source {
        Source {
            label: label.into(),
            file: "common/buildings/t.txt".into(),
            text: text.into(),
        }
    }

    #[test]
    fn reports_dropped_source_additions() {
        let v = src("Vanilla", "building_lab = { cost = { minerals = 100 } }");
        let a = src(
            "Mod A",
            "building_lab = { cost = { minerals = 50 } planet_modifier = { job_researcher_add = 1 } }",
        );
        let b = src(
            "Mod B",
            "building_lab = { cost = { minerals = 100 } triggered_desc = { text = extra } }",
        );
        let bad = "building_lab = { cost = { minerals = 50 } planet_modifier = { job_researcher_add = 1 } }";
        let res = validate_object(
            "common/buildings",
            "building_lab",
            bad,
            Some(&v),
            &[a.clone(), b.clone()],
            None,
        );
        assert!(!res.is_ok());
        assert!(
            res.problems
                .iter()
                .any(|p| p.kind == ProblemKind::DroppedAddition
                    && p.source_label.as_deref() == Some("Mod B"))
        );

        let good = "building_lab = { cost = { minerals = 50 } planet_modifier = { job_researcher_add = 1 } triggered_desc = { text = extra } }";
        let ok = validate_object(
            "common/buildings",
            "building_lab",
            good,
            Some(&v),
            &[a, b],
            None,
        );
        assert!(ok.is_ok(), "{:?}", ok.problems);
    }

    #[test]
    fn reports_duplicate_scalar_keys() {
        let dup = "building_lab = { category = research category = unity }";
        let res = validate_object("common/buildings", "building_lab", dup, None, &[], None);
        assert!(!res.is_ok());
        assert!(
            res.problems
                .iter()
                .any(|p| p.kind == ProblemKind::DuplicateScalarKey)
        );

        // Events can have multiple `desc = { trigger = ... }` blocks, which are not scalar.
        let event_ok = "country_event = { id = ns.1 desc = { text = a } desc = { text = b } }";
        assert!(validate_object("events", "ns.1", event_ok, None, &[], None).is_ok());
    }

    #[test]
    fn reports_duplicate_single_valued_blocks() {
        for (category, key, bad) in [
            (
                "common/buildings",
                "building_lab",
                "building_lab = { potential = { exists = owner } potential = { is_ai = no } }",
            ),
            (
                "common/buildings",
                "building_lab",
                "building_lab = { cost = { minerals = 1 } cost = { energy = 1 } }",
            ),
            (
                "events",
                "ns.1",
                "country_event = { id = ns.1 trigger = { a = yes } trigger = { b = yes } }",
            ),
        ] {
            let res = validate_object(category, key, bad, None, &[], None);
            assert!(
                res.problems
                    .iter()
                    .any(|p| p.kind == ProblemKind::DuplicateScalarKey),
                "{bad}: {:?}",
                res.problems
            );
        }
    }

    #[test]
    fn reports_a_dropped_value_change() {
        let v = src("Vanilla", "building_lab = { cost = { minerals = 100 } }");
        let a = src("Mod A", "building_lab = { cost = { minerals = 50 } }");
        let kept_vanilla = "building_lab = { cost = { minerals = 100 } }";
        let res = validate_object(
            "common/buildings",
            "building_lab",
            kept_vanilla,
            Some(&v),
            std::slice::from_ref(&a),
            None,
        );
        assert!(
            res.problems
                .iter()
                .any(|p| p.kind == ProblemKind::DroppedAddition),
            "{:?}",
            res.problems
        );

        // When both changed it differently, keeping either is a priority call.
        let b = src("Mod B", "building_lab = { cost = { minerals = 70 } }");
        let kept_a = "building_lab = { cost = { minerals = 50 } }";
        let ok = validate_object(
            "common/buildings",
            "building_lab",
            kept_a,
            Some(&v),
            &[a, b],
            None,
        );
        assert!(ok.is_ok(), "{:?}", ok.problems);
    }

    #[test]
    fn validates_scripted_variable_objects() {
        let ok = validate_object(
            "common/scripted_variables",
            "@my_cost",
            "@helper = 1\n@my_cost = 100",
            None,
            &[],
            None,
        );
        assert!(ok.is_ok(), "{:?}", ok.problems);
        let wrong = validate_object(
            "common/scripted_variables",
            "@my_cost",
            "@other = 100",
            None,
            &[],
            None,
        );
        assert!(!wrong.is_ok());
    }

    #[test]
    fn validates_localisation_files() {
        let ok = validate_file(
            "localisation/english/x_l_english.yml",
            "l_english:\n key:0 \"Text\" # a note\n key2:0 \"Two\"\n key3: \"No version\"",
            None,
            &[],
            None,
        );
        assert!(ok.is_ok(), "{:?}", ok.problems);

        for bad in [
            "english:\n key:0 \"Text\"",
            "l_english:\n key:0 \"Text",
            "key:0 \"Text\"",
            "l_english:\n key:zero \"Text\"",
            "l_english:\n key:0 Text",
            "l_english:\n key:0 \"a \"quote\" inside\"",
        ] {
            let res = validate_file(
                "localisation/english/x_l_english.yml",
                bad,
                None,
                &[],
                None,
            );
            assert!(!res.is_ok(), "{bad}: {:?}", res.problems);
        }
    }

    #[test]
    fn allows_legitimate_and_or_restructuring() {
        let v = src(
            "Vanilla",
            "building_lab = { potential = { exists = owner } }",
        );
        let a = src(
            "Mod A",
            "building_lab = { potential = { exists = owner is_ai = no } }",
        );
        let b = src(
            "Mod B",
            "building_lab = { potential = { exists = owner has_building = b1 } }",
        );
        let merged = "building_lab = { potential = { exists = owner AND = { is_ai = no has_building = b1 } } }";
        let res = validate_object(
            "common/buildings",
            "building_lab",
            merged,
            Some(&v),
            &[a, b],
            None,
        );
        assert!(res.is_ok(), "{:?}", res.problems);
    }

    #[test]
    fn treats_ethic_spellings_as_equivalent() {
        let v = src(
            "Vanilla",
            "interference = { option = { valid = { NOR = { is_pacifist = yes is_xenophile = yes is_fanatic_xenophobe = yes } } } }",
        );
        let m = src(
            "Mod",
            "interference = { option = { valid = { NOR = { has_ethic = ethic_pacifist has_ethic = ethic_fanatic_pacifist has_ethic = ethic_xenophile has_ethic = ethic_fanatic_xenophile has_ethic = ethic_fanatic_xenophobe } } } }",
        );
        let merged = "interference = { option = { valid = { NOR = { is_pacifist = yes is_xenophile = yes is_fanatic_xenophobe = yes } } } }";
        let res = validate_object(
            "common/policies",
            "interference",
            merged,
            Some(&v),
            &[m],
            None,
        );
        assert!(res.is_ok(), "{:?}", res.problems);
    }

    #[test]
    fn treats_flat_negations_as_a_nor_block() {
        let v = src(
            "Vanilla",
            "policy = { option = { ai_weight = { modifier = { is_ai = yes } } } }",
        );
        let m = src(
            "Mod",
            "policy = { option = { ai_weight = { modifier = { is_ai = yes NOR = { is_militarist = yes is_xenophobe = yes } } } } }",
        );
        let merged = "policy = { option = { ai_weight = { modifier = { is_ai = yes is_militarist = no is_xenophobe = no } } } }";
        let res = validate_object(
            "common/policies",
            "policy",
            merged,
            Some(&v),
            &[m],
            None,
        );
        assert!(res.is_ok(), "{:?}", res.problems);
    }

    #[test]
    fn treats_a_script_value_choice_as_a_priority_pick() {
        let v = src(
            "Vanilla",
            "agenda = { add_resource = { mult = value:count_assigned_priests } }",
        );
        let m = src(
            "Mod",
            "agenda = { add_resource = { mult = value:country_assigned_priests } }",
        );
        let merged =
            "agenda = { add_resource = { mult = value:count_assigned_priests } }";
        let res = validate_object(
            "common/council_agendas",
            "agenda",
            merged,
            Some(&v),
            &[m],
            None,
        );
        assert!(res.is_ok(), "{:?}", res.problems);
    }

    #[test]
    fn reports_a_dropped_event_option() {
        let v = src("Vanilla", "country_event = { id = x.1 option = { name = a } }");
        let m = src(
            "Mod",
            "country_event = { id = x.1 option = { name = a } option = { name = extra } }",
        );
        let bad = "country_event = { id = x.1 option = { name = a } }";
        let res = validate_object("events", "x.1", bad, Some(&v), &[m], None);
        assert!(
            res.problems
                .iter()
                .any(|p| p.kind == ProblemKind::DroppedAddition),
            "{:?}",
            res.problems
        );
    }
}
