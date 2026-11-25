//! Finding and pairing `inline_script` references when comparing objects,
//! so a mod replacing a vanilla inline script with its own can be diffed
//! side by side.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
    if name.is_empty() {
        return None;
    }
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
            return Some(InlineScriptFile { name, path: rel_file, source_label: label.to_owned(), text });
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
    let left_set: HashSet<String> = left_calls.iter().map(|s| s.to_ascii_lowercase()).collect();
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

    // 2. Replaced scripts: scripts present only on left vs only on right.
    let left_only: Vec<&str> = left_calls.iter().filter(|s| !right_set.contains(&s.to_ascii_lowercase())).map(String::as_str).collect();
    let right_only: Vec<&str> = right_calls.iter().filter(|s| !left_set.contains(&s.to_ascii_lowercase())).map(String::as_str).collect();

    if !left_only.is_empty() && !right_only.is_empty() {
        let mut used_right = vec![false; right_only.len()];
        let mut pairs: Vec<(usize, usize, usize)> = Vec::new();
        for (li, l) in left_only.iter().enumerate() {
            for (ri, r) in right_only.iter().enumerate() {
                pairs.push((pair_score(l, r, li, ri), li, ri));
            }
        }
        // Highest similarity first, breaking ties by left-then-right order.
        pairs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
        let mut matched: Vec<(usize, &str, &str)> = Vec::new();
        let mut used_left = vec![false; left_only.len()];
        for (_, li, ri) in pairs {
            if !used_left[li] && !used_right[ri] {
                used_left[li] = true;
                used_right[ri] = true;
                matched.push((li, left_only[li], right_only[ri]));
            }
        }
        matched.sort_by_key(|&(li, _, _)| li);
        for (_, l_name, r_name) in matched {
            if let (Some(l), Some(r)) = (read_inline_script(left_roots, l_name), read_inline_script(right_roots, r_name)) {
                comparisons.push(InlineScriptComparison { left: l, right: r });
            }
        }
    }

    comparisons
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
}
