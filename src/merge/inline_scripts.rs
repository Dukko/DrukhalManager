//! Finding and pairing `inline_script` references when comparing objects, so
//! a mod replacing a vanilla inline script with its own can be diffed side by
//! side and merged into the patch alongside the object.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::patch::{Print, VANILLA};
use super::source::Source;
use crate::paradox::script::{self, Entry, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineScriptFile {
    /// Script name as called, e.g. `MECR/factions/imperialist_460`.
    pub name: String,
    /// Relative path, e.g. `common/inline_scripts/MECR/factions/imperialist_460.txt`.
    pub path: String,
    /// Mod name or `"Vanilla"` where the file came from.
    pub source_label: String,
    pub text: String,
}

/// A mod folder scripts can be read from, with the descriptor id used for
/// fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRoot {
    pub id: String,
    pub label: String,
    pub root: PathBuf,
}

/// One logical inline script several object versions call under different
/// names, ready to merge into `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineGroup {
    /// The name the base game calls, e.g. `pop_faction_types/demands/imperialist`.
    pub vanilla_name: String,
    /// The name the merged object should call, e.g. `MECR/factions/imperialist_460`.
    pub target: String,
    pub vanilla: Option<Source>,
    pub sources: Vec<Source>,
    /// Fingerprints of every version, for the patch and the AI cache.
    pub prints: Vec<Print>,
    /// Every name this group is called by, each mapping to `target`.
    pub names: Vec<(String, String)>,
}

impl InlineGroup {
    /// Whether the versions actually differ, so the script needs an AI merge.
    pub fn needs_merge(&self) -> bool {
        let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut seen = HashSet::new();
        if let Some(v) = &self.vanilla {
            seen.insert(normalize(&v.text));
        }
        for s in &self.sources {
            seen.insert(normalize(&s.text));
        }
        seen.len() > 1
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineScriptComparison {
    pub left: InlineScriptFile,
    pub right: InlineScriptFile,
}

/// Normalizes an inline script name (`\` -> `/`, trims quotes, leading `/`,
/// and optional `.txt` suffix).
fn normalize_name(raw: &str) -> String {
    let s = raw.trim().trim_matches('"').replace('\\', "/");
    let s = s.trim_start_matches('/');
    let s = s.strip_suffix(".txt").or_else(|| s.strip_suffix(".TXT")).unwrap_or(s);
    s.to_owned()
}

/// Extracts `inline_script` names referenced anywhere in `text`, in order
/// of appearance (deduplicated case-insensitively).
pub fn extract_calls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |raw: &str| {
        let name = normalize_name(raw);
        if !name.is_empty() && seen.insert(name.to_ascii_lowercase()) {
            out.push(name);
        }
    };

    if let Ok(entries) = script::parse(text) {
        collect_from_entries(&entries, &mut push);
    } else {
        // Fallback for incomplete/edited script fragments.
        let mut in_inline_block = false;
        let mut depth = 0i32;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim();
                if k.eq_ignore_ascii_case("inline_script") {
                    if v.starts_with('{') {
                        in_inline_block = true;
                        depth = 1;
                    } else {
                        push(v);
                    }
                    continue;
                }
                if in_inline_block && k.eq_ignore_ascii_case("script") {
                    push(v);
                }
            }
            if in_inline_block {
                depth += line.bytes().filter(|&b| b == b'{').count() as i32;
                depth -= line.bytes().filter(|&b| b == b'}').count() as i32;
                if depth <= 0 {
                    in_inline_block = false;
                    depth = 0;
                }
            }
        }
    }
    out
}

fn collect_from_entries(entries: &[Entry], push: &mut impl FnMut(&str)) {
    for entry in entries {
        match entry {
            Entry::Assign { key, value, .. } => {
                if key.eq_ignore_ascii_case("inline_script") {
                    match value {
                        Value::Scalar(s) => push(s),
                        Value::Block(inner) => {
                            for sub in inner {
                                if let Entry::Assign { key: k, value: Value::Scalar(s), .. } = sub
                                    && k.eq_ignore_ascii_case("script")
                                {
                                    push(s);
                                }
                            }
                            collect_from_entries(inner, push);
                        }
                    }
                } else if let Value::Block(inner) = value {
                    collect_from_entries(inner, push);
                }
            }
            Entry::Item(Value::Block(inner)) => collect_from_entries(inner, push),
            Entry::Item(Value::Scalar(_)) => {}
        }
    }
}

/// Resolves a relative path inside `root` case-insensitively.
fn resolve_case_insensitive(root: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for &part in parts {
        let exact = current.join(part);
        if exact.exists() {
            current = exact;
            continue;
        }
        let entries = std::fs::read_dir(&current).ok()?;
        let found = entries.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        current = found.path();
    }
    current.is_file().then_some(current)
}

/// Reads `common/inline_scripts/<script_name>.txt` from the first matching
/// root in `roots` (in priority order).
pub fn read_inline_script(roots: &[(&str, &Path)], script_name: &str) -> Option<InlineScriptFile> {
    let name = normalize_name(script_name);
    let _ = file_path(&name)?;
    let rel_file = format!("common/inline_scripts/{name}.txt");
    let mut parts = vec!["common", "inline_scripts"];
    let file_part = format!("{name}.txt");
    let sub_parts: Vec<&str> = file_part.split('/').filter(|s| !s.is_empty()).collect();
    parts.extend(sub_parts);

    for &(label, root) in roots {
        if let Some(path) = resolve_case_insensitive(root, &parts)
            && let Ok(bytes) = std::fs::read(&path)
        {
            let text = String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_owned();
            let path = path
                .strip_prefix(root)
                .map(crate::platform::game_path)
                .unwrap_or(rel_file);
            return Some(InlineScriptFile { name, path, source_label: label.to_owned(), text });
        }
    }
    None
}

fn name_tokens(name: &str) -> HashSet<String> {
    name.split(['/', '_', '-'])
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 3 && !s.bytes().all(|b| b.is_ascii_digit()))
        .collect()
}

fn pair_score(left: &str, right: &str, li: usize, ri: usize) -> usize {
    let lt = name_tokens(left);
    let rt = name_tokens(right);
    let shared = lt.intersection(&rt).count();
    let pos_bonus = if li == ri { 1 } else { 0 };
    shared * 10 + pos_bonus
}

/// Finds inline script pairs to compare between `left_text` and `right_text`:
/// 1. Replaced scripts (where `left` calls a mod script and `right` calls a
///    different script, paired by name similarity / position).
/// 2. Same-named scripts whose file contents differ between `left_roots` and
///    `right_roots` (e.g. an in-place override of a vanilla inline script).
pub fn find_comparisons(left_text: &str, left_roots: &[(&str, &Path)], right_text: &str, right_roots: &[(&str, &Path)]) -> Vec<InlineScriptComparison> {
    let left_calls = extract_calls(left_text);
    let right_calls = extract_calls(right_text);
    let right_set: HashSet<String> = right_calls.iter().map(|s| s.to_ascii_lowercase()).collect();

    let mut comparisons = Vec::new();

    // 1. Same script name on both sides, but different file contents on disk.
    for name in &left_calls {
        if right_set.contains(&name.to_ascii_lowercase())
            && let (Some(l), Some(r)) = (read_inline_script(left_roots, name), read_inline_script(right_roots, name))
            && l.text.trim() != r.text.trim()
        {
            comparisons.push(InlineScriptComparison { left: l, right: r });
        }
    }

    // 2. Replaced scripts.
    for (l_name, r_name) in replaced_call_pairs(left_text, right_text) {
        if let (Some(l), Some(r)) = (
            read_inline_script(left_roots, &l_name),
            read_inline_script(right_roots, &r_name),
        ) {
            comparisons.push(InlineScriptComparison { left: l, right: r });
        }
    }

    comparisons
}

/// Calls present on only one side, paired by name similarity and position.
/// These are scripts one side replaced with its own copy.
pub fn replaced_call_pairs(left_text: &str, right_text: &str) -> Vec<(String, String)> {
    let left_calls = extract_calls(left_text);
    let right_calls = extract_calls(right_text);
    let left_set: HashSet<String> = left_calls.iter().map(|s| s.to_ascii_lowercase()).collect();
    let right_set: HashSet<String> = right_calls.iter().map(|s| s.to_ascii_lowercase()).collect();
    let left_only: Vec<&str> = left_calls
        .iter()
        .filter(|s| !right_set.contains(&s.to_ascii_lowercase()))
        .map(String::as_str)
        .collect();
    let right_only: Vec<&str> = right_calls
        .iter()
        .filter(|s| !left_set.contains(&s.to_ascii_lowercase()))
        .map(String::as_str)
        .collect();
    if left_only.is_empty() || right_only.is_empty() {
        return Vec::new();
    }
    let mut pairs: Vec<(usize, usize, usize)> = Vec::new();
    for (li, l) in left_only.iter().enumerate() {
        for (ri, r) in right_only.iter().enumerate() {
            pairs.push((pair_score(l, r, li, ri), li, ri));
        }
    }
    pairs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
    let mut used_left = vec![false; left_only.len()];
    let mut used_right = vec![false; right_only.len()];
    let mut matched = Vec::new();
    for (_, li, ri) in pairs {
        if !used_left[li] && !used_right[ri] {
            used_left[li] = true;
            used_right[ri] = true;
            matched.push((li, left_only[li].to_owned(), right_only[ri].to_owned()));
        }
    }
    matched.sort_by_key(|(li, _, _)| *li);
    matched
        .into_iter()
        .map(|(_, left, right)| (left, right))
        .collect()
}

/// The relative file a script name is stored att name is stored at, refusing names that could
/// leave `common/inline_scripts`.
pub fn file_path(name: &str) -> Option<String> {
    let name = normalize_name(name);
    if name.is_empty()
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(format!("common/inline_scripts/{name}.txt"))
}

/// One logical script a mod replaced in an object, with every version of it.
struct GroupDraft {
    vanilla_name: String,
    target: String,
    replacement_names: Vec<String>,
}

/// Finds inline scripts that a mod calls under a new name in place of a base
/// game script, and every version of each such script. `versions` are the
/// mods' object versions in load order; `roots` are all mod folders in load
/// order.
pub fn replacement_groups(
    vanilla_object: Option<&str>,
    versions: &[(String, String)],
    roots: &[ScriptRoot],
    game_dir: Option<&Path>,
) -> Vec<InlineGroup> {
    let Some(vanilla_object) = vanilla_object else {
        return Vec::new();
    };
    let Some(game_dir) = game_dir else {
        return Vec::new();
    };
    let game_roots = [("Vanilla", game_dir)];

    // A vanilla call paired with the mod call that replaced it, by base name.
    let mut drafts: BTreeMap<String, GroupDraft> = BTreeMap::new();
    for (label, text) in versions {
        let Some(root) = roots.iter().find(|r| &r.label == label) else {
            continue;
        };
        let right_roots = [
            (root.label.as_str(), root.root.as_path()),
            ("Vanilla", game_dir),
        ];
        for pair in find_comparisons(vanilla_object, &game_roots, text, &right_roots) {
            if pair.left.name.eq_ignore_ascii_case(&pair.right.name) {
                continue;
            }
            let key = pair.left.name.to_ascii_lowercase();
            let draft = drafts.entry(key).or_insert_with(|| GroupDraft {
                vanilla_name: pair.left.name.clone(),
                target: pair.right.name.clone(),
                replacement_names: Vec::new(),
            });
            // The highest load-order replacement names the merged script.
            draft.target = pair.right.name.clone();
            if !draft
                .replacement_names
                .iter()
                .any(|n| n.eq_ignore_ascii_case(&pair.right.name))
            {
                draft.replacement_names.push(pair.right.name.clone());
            }
        }
    }

    let mut groups = Vec::new();
    for draft in drafts.into_values() {
        let vanilla_file = read_inline_script(&game_roots, &draft.vanilla_name);
        let vanilla = vanilla_file.as_ref().map(|v| Source {
            label: "Vanilla".into(),
            file: v.path.clone(),
            text: v.text.clone(),
        });
        let mut names = vec![draft.vanilla_name.clone()];
        for name in &draft.replacement_names {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                names.push(name.clone());
            }
        }

        let mut sources: Vec<Source> = Vec::new();
        let mut prints: Vec<Print> = Vec::new();
        if let Some(v) = &vanilla_file {
            prints.push(Print::new(VANILLA, "Vanilla", &v.path, &v.text));
        }
        for root in roots {
            for name in &names {
                let Some(found) =
                    read_inline_script(&[(root.label.as_str(), root.root.as_path())], name)
                else {
                    continue;
                };
                let duplicate = sources
                    .iter()
                    .any(|s| s.label == root.label && s.file == found.path && s.text == found.text);
                if duplicate {
                    continue;
                }
                sources.push(Source {
                    label: root.label.clone(),
                    file: found.path.clone(),
                    text: found.text.clone(),
                });
                prints.push(Print::new(&root.id, &root.label, &found.path, &found.text));
            }
        }

        if sources.is_empty() && vanilla.is_none() {
            continue;
        }
        let group = InlineGroup {
            names: names
                .into_iter()
                .map(|name| (name, draft.target.clone()))
                .collect(),
            vanilla_name: draft.vanilla_name,
            target: draft.target,
            vanilla,
            sources,
            prints,
        };
        if group.needs_merge() {
            groups.push(group);
        }
    }
    groups
}

/// Splits a line into its code and its `#` comment, ignoring `#` in quotes.
fn split_comment(line: &str) -> (&str, &str) {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return (&line[..i], &line[i..]),
            _ => {}
        }
    }
    (line, "")
}

/// Whether `at` is inside a quoted string in `text`.
fn inside_quotes(text: &str, at: usize) -> bool {
    let mut quoted = false;
    let mut escaped = false;
    for c in text[..at].chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        }
    }
    quoted
}

/// Rewrites `script = <old>` and `inline_script = <old>` values whose name is
/// in `names` to the new name.  Only the value changes; indentation, spacing
/// and comments stay.
pub fn rewrite_calls(text: &str, names: &[(String, String)]) -> String {
    if names.is_empty() {
        return text.to_owned();
    }
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        let (code, comment) = split_comment(body);
        let mut rewritten = String::with_capacity(code.len());
        let mut last = 0;
        // We search for two keywords:
        // - "inline_script" followed by `= <value>` (scalar form)
        // - "script" (not preceded by a word char) followed by `= <value>`
        //   (block-interior `script = …` key)
        // We iterate over "script" matches — an `inline_script` match always
        // contains a `script` substring.  When we see `inline_` right before
        // the match we handle it as the scalar form; otherwise we handle it
        // as the block-interior key form, rejecting word-char predecessors.
        for (at, _) in code.match_indices("script") {
            if inside_quotes(code, at) {
                continue;
            }
            // Determine whether this is `inline_script` (scalar form) or
            // bare `script` (block-interior key form).
            let is_inline = at >= 7
                && &code[at - 7..at] == "inline_"
                && !(at >= 8 && code[..at - 7].chars().next_back().is_some_and(word));
            if !is_inline {
                // Bare `script` — reject if preceded or followed by a word char.
                if at > 0 && code[..at].chars().next_back().is_some_and(word) {
                    continue;
                }
                let after = at + "script".len();
                if code[after..].chars().next().is_some_and(word) {
                    continue;
                }
            }
            let after = at + "script".len();
            let rest = code[after..].trim_start();
            let spaces = code[after..].len() - rest.len();
            let Some(rest) = rest.strip_prefix('=') else {
                continue;
            };
            let value_spaces = rest.len() - rest.trim_start().len();
            let value_start = after + spaces + 1 + value_spaces;
            let value = &code[value_start..];
            if is_inline && value.starts_with('{') {
                // Block form `inline_script = { script = … }` — the inner
                // `script =` match will handle it.
                continue;
            }
            let (value_len, quoted) = if let Some(text) = value.strip_prefix('"') {
                let Some(end) = text.find('"') else { continue };
                (end + 2, true)
            } else {
                let end = value
                    .find(|c: char| c.is_whitespace() || c == '}' || c == ',')
                    .unwrap_or(value.len());
                (end, false)
            };
            if value_len == 0 {
                continue;
            }
            let raw = &value[..value_len];
            let name = normalize_name(raw);
            let Some((_, target)) = names.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name)) else {
                continue;
            };
            // For inline_script, include the whole `inline_script = ` key in
            // the unchanged prefix so we only replace the value.
            let prefix_start = if is_inline { at - 7 } else { last };
            // Avoid emitting text we already emitted (overlapping match).
            if prefix_start < last {
                continue;
            }
            rewritten.push_str(&code[last..value_start]);
            if quoted {
                rewritten.push('"');
                rewritten.push_str(target);
                rewritten.push('"');
            } else {
                rewritten.push_str(target);
            }
            last = value_start + value_len;
        }
        rewritten.push_str(&code[last..]);
        rewritten.push_str(comment);
        rewritten.push_str(newline);
        out.push_str(&rewritten);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("drukhal-inline-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn extracts_both_inline_script_forms() {
        let text = r#"
imperialist = {
    inline_script = "pop_faction_types/demands/shared"
    demands = {
        inline_script = {
            script = MECR/factions/imperialist_460
            VALUE = 1
        }
    }
}
"#;
        assert_eq!(extract_calls(text), ["pop_faction_types/demands/shared", "MECR/factions/imperialist_460"]);
    }

    #[test]
    fn pairs_replaced_inline_scripts_and_reads_files_case_insensitively() {
        let mod_dir = temp("mod");
        let game_dir = temp("game");
        write(&mod_dir, "common/inline_scripts/MECR/Factions/imperialist_460.txt", "demand = { trigger = { has_ethic = ethic_militarist } }");
        write(&mod_dir, "common/inline_scripts/MECR/Factions/imperialist_actions.txt", "actions = { embrace = yes }");
        write(&game_dir, "common/inline_scripts/pop_faction_types/demands/imperialist.txt", "demand = { trigger = { always = yes } }");
        write(&game_dir, "common/inline_scripts/pop_faction_types/actions/imperialist.txt", "actions = { embrace = no }");

        let left_obj = r#"
imperialist = {
    inline_script = { script = MECR/factions/imperialist_460 }
    inline_script = { script = MECR/factions/imperialist_actions }
}
"#;
        let right_obj = r#"
imperialist = {
    inline_script = { script = pop_faction_types/actions/imperialist }
    inline_script = { script = pop_faction_types/demands/imperialist }
}
"#;
        let found = find_comparisons(left_obj, &[("MECR", &mod_dir), ("Vanilla", &game_dir)], right_obj, &[("Vanilla", &game_dir)]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].left.name, "MECR/factions/imperialist_460");
        assert_eq!(found[0].right.name, "pop_faction_types/demands/imperialist");
        assert_eq!(found[1].left.name, "MECR/factions/imperialist_actions");
        assert_eq!(found[1].right.name, "pop_faction_types/actions/imperialist");

        let _ = std::fs::remove_dir_all(&mod_dir);
        let _ = std::fs::remove_dir_all(&game_dir);
    }

    #[test]
    fn detects_same_named_inline_script_modified_in_mod() {
        let mod_dir = temp("same_mod");
        let game_dir = temp("same_game");
        write(&mod_dir, "common/inline_scripts/tech/shared.txt", "cost = 200");
        write(&game_dir, "common/inline_scripts/tech/shared.txt", "cost = 100");

        let obj = "tech_a = { inline_script = tech/shared }";
        let found = find_comparisons(obj, &[("Mod", &mod_dir), ("Vanilla", &game_dir)], obj, &[("Vanilla", &game_dir)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].left.text, "cost = 200");
        assert_eq!(found[0].right.text, "cost = 100");

        let _ = std::fs::remove_dir_all(&mod_dir);
        let _ = std::fs::remove_dir_all(&game_dir);
    }

    #[test]
    fn groups_replaced_vanilla_scripts_and_rewrites_calls() {
        let mod_dir = temp("group_mod");
        let game_dir = temp("group_game");
        write(
            &game_dir,
            "common/inline_scripts/pop_faction_types/demands/imperialist.txt",
            "demand = { trigger = { always = yes } }",
        );
        write(
            &mod_dir,
            "common/inline_scripts/MECR/factions/imperialist_460.txt",
            "demand = { trigger = { has_ethic = ethic_militarist } }",
        );
        let roots = [ScriptRoot {
            id: "mod/mecr.mod".into(),
            label: "MECR".into(),
            root: mod_dir.clone(),
        }];
        let vanilla = "imperialist = { inline_script = { script = pop_faction_types/demands/imperialist } }";
        let versions = [(
            "MECR".to_owned(),
            "imperialist = {\n\tinline_script = {\n\t\tscript = MECR/factions/imperialist_460\n\t}\n}"
                .to_owned(),
        )];
        let groups = replacement_groups(Some(vanilla), &versions, &roots, Some(&game_dir));
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.vanilla_name, "pop_faction_types/demands/imperialist");
        assert_eq!(group.target, "MECR/factions/imperialist_460");
        assert!(group.needs_merge());
        assert!(group.vanilla.is_some());
        assert_eq!(group.sources.len(), 1);
        assert_eq!(group.prints.len(), 2);

        let rewritten = rewrite_calls(vanilla, &group.names);
        assert!(
            rewritten.contains("script = MECR/factions/imperialist_460"),
            "{rewritten}"
        );
        assert_eq!(
            rewrite_calls(
                "x = {\n\tscript = \"pop_faction_types/demands/imperialist\" # keep\n}",
                &group.names
            ),
            "x = {\n\tscript = \"MECR/factions/imperialist_460\" # keep\n}"
        );
        // Text that merely mentions the name isn't a call.
        assert_eq!(
            rewrite_calls(
                "x = { desc = \"script = pop_faction_types/demands/imperialist\" }",
                &group.names
            ),
            "x = { desc = \"script = pop_faction_types/demands/imperialist\" }"
        );

        let _ = std::fs::remove_dir_all(&mod_dir);
        let _ = std::fs::remove_dir_all(&game_dir);
    }

    #[test]
    fn rewrite_calls_scalar_inline_script_unquoted() {
        let names = vec![
            ("pop_faction_types/demands/imperialist".into(), "MECR/factions/imperialist_460".into()),
        ];
        let input = "imperialist = { inline_script = pop_faction_types/demands/imperialist }";
        let result = rewrite_calls(input, &names);
        assert_eq!(result, "imperialist = { inline_script = MECR/factions/imperialist_460 }");
    }

    #[test]
    fn rewrite_calls_scalar_inline_script_quoted() {
        let names = vec![
            ("pop_faction_types/demands/imperialist".into(), "MECR/factions/imperialist_460".into()),
        ];
        let input = r#"imperialist = { inline_script = "pop_faction_types/demands/imperialist" }"#;
        let result = rewrite_calls(input, &names);
        assert_eq!(result, r#"imperialist = { inline_script = "MECR/factions/imperialist_460" }"#);
    }

    #[test]
    fn rewrite_calls_scalar_inside_string_untouched() {
        let names = vec![
            ("pop_faction_types/demands/imperialist".into(), "MECR/factions/imperialist_460".into()),
        ];
        // Value inside a string should not be rewritten.
        let input = r#"desc = "inline_script = pop_faction_types/demands/imperialist""#;
        let result = rewrite_calls(input, &names);
        assert_eq!(result, input);
    }

    #[test]
    fn rewrite_calls_block_inline_script_still_works() {
        let names = vec![
            ("pop_faction_types/demands/imperialist".into(), "MECR/factions/imperialist_460".into()),
        ];
        let input = "inline_script = { script = pop_faction_types/demands/imperialist }";
        let result = rewrite_calls(input, &names);
        assert_eq!(result, "inline_script = { script = MECR/factions/imperialist_460 }");
    }

    #[test]
    fn rewrite_calls_does_not_match_other_inline_script_prefix() {
        let names = vec![
            ("pop_faction_types/demands/imperialist".into(), "MECR/factions/imperialist_460".into()),
        ];
        // `not_inline_script` should not trigger.
        let input = "not_inline_script = pop_faction_types/demands/imperialist";
        let result = rewrite_calls(input, &names);
        assert_eq!(result, input);
    }
}
