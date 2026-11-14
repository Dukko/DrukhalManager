//! Repairing script files with unbalanced braces.
//!
//! The game's parser keeps going after a missing or extra `}`, but every
//! object after the mistake ends up nested inside the broken one, so they
//! silently stop existing. The structural repair here relies on the
//! formatting convention every Paradox file follows: top-level objects start
//! at column 0. Before such a line, any blocks still open are closed; `}`
//! that would close nothing are dropped. The objects whose braces changed
//! are reported as damaged, since only their author's intent (or a careful
//! reader) can say where inside them the brace really belonged.

use crate::paradox::script::{self, Entries, Entry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair {
    pub text: String,
    /// What was changed, for the user.
    pub fixes: Vec<String>,
    /// Keys of top-level objects whose braces were changed.
    pub damaged: Vec<String>,
}

/// Repairs `src` if it doesn't parse because of brace mistakes. `None` if it
/// already parses, or the problem is something this can't fix (such as an
/// unterminated string).
pub fn repair(category: &str, src: &str) -> Option<Repair> {
    if script::parse(src).is_ok() {
        return None;
    }
    let mut out = String::with_capacity(src.len() + 16);
    let mut fixes = Vec::new();
    let mut damaged: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    // The top-level object currently open, by its first line.
    let mut current: Option<(usize, String)> = None;

    let mark = |damaged: &mut Vec<String>, current: &Option<(usize, String)>| {
        if let Some((_, key)) = current
            && !damaged.contains(key)
        {
            damaged.push(key.clone());
        }
    };

    for (n, line) in src.split_inclusive('\n').enumerate() {
        let number = n + 1;
        if !in_string && depth > 0 && starts_top_level_object(line) {
            fixes.push(tnf!("Closed {count} block left open before line {line}", "Closed {count} blocks left open before line {line}", count = depth, line = number));
            mark(&mut damaged, &current);
            out.push_str(&"}".repeat(depth));
            out.push('\n');
            depth = 0;
        }
        if !in_string && depth == 0 && starts_top_level_object(line) {
            current = Some((number, line.split(['=', ' ', '\t']).next().unwrap_or_default().to_owned()));
        }

        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if in_string {
                match c {
                    '\\' => {
                        out.push(c);
                        if let Some(next) = chars.next() {
                            out.push(next);
                        }
                        continue;
                    }
                    '"' => in_string = false,
                    _ => {}
                }
                out.push(c);
                continue;
            }
            match c {
                '#' => {
                    out.push(c);
                    out.extend(chars.by_ref());
                    break;
                }
                '"' => in_string = true,
                '{' => depth += 1,
                '}' if depth == 0 => {
                    fixes.push(tf!("Removed a '}}' that closed nothing on line {line}", line = number));
                    mark(&mut damaged, &current);
                    continue;
                }
                '}' => depth -= 1,
                _ => {}
            }
            out.push(c);
        }
    }
    if in_string {
        return None;
    }
    if depth > 0 {
        fixes.push(tnf!("Closed {count} block left open at the end of the file", "Closed {count} blocks left open at the end of the file", count = depth));
        mark(&mut damaged, &current);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&"}".repeat(depth));
        out.push('\n');
    }
    script::parse(&out).ok()?;

    // Events are identified by their id, as everywhere else.
    if category == "events" {
        let entries = script::parse(&out).ok()?;
        damaged = damaged
            .into_iter()
            .map(|key| {
                entries
                    .iter()
                    .filter_map(|e| match e {
                        Entry::Assign { key: k, value, .. } if *k == key => value.as_block()?.scalar("id").map(str::to_owned),
                        _ => None,
                    })
                    .next()
                    .unwrap_or(key)
            })
            .collect();
    }
    Some(Repair { text: out, fixes, damaged })
}

/// The byte range of the top-level object `key` (an event id for events).
pub fn object_span(category: &str, key: &str, text: &str) -> Option<std::ops::Range<usize>> {
    script::parse_with_spans(text).ok()?.into_iter().find_map(|(entry, range)| match &entry {
        Entry::Assign { key: k, value, .. } if category == "events" => (value.as_block().and_then(|b| b.scalar("id")) == Some(key) && k != "namespace").then_some(range),
        Entry::Assign { key: k, .. } => (k == key).then_some(range),
        Entry::Item(_) => None,
    })
}

/// `text` with the object `key` replaced by `replacement`.
pub fn replace_object(category: &str, key: &str, text: &str, replacement: &str) -> Option<String> {
    let range = object_span(category, key, text)?;
    Some(format!("{}{}{}", &text[..range.start], replacement.trim(), &text[range.end..]))
}

/// `key = {` at column 0, the convention for top-level objects.
fn starts_top_level_object(line: &str) -> bool {
    let Some(first) = line.chars().next() else { return false };
    if !(first.is_alphanumeric() || first == '_' || first == '@') {
        return false;
    }
    let Some((key, rest)) = line.split_once('=') else { return false };
    !key.trim().contains(char::is_whitespace) && rest.trim_start().starts_with('{')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_blocks_before_the_next_object() {
        // Shaped like Better Origins' b_origins_buildings.txt.
        let src = "a = {\n\tpotential = {\n\t\towner = { OR = { x = yes }\n\t}\n}\n\nb = {\n\tcost = 1\n}\n\nc = {\n}\n";
        let r = repair("common/buildings", src).unwrap();
        assert_eq!(r.fixes, ["Closed 1 block left open before line 7"]);
        assert_eq!(r.damaged, ["a"]);
        let entries = script::parse(&r.text).unwrap();
        let keys: Vec<&str> = entries.iter().filter_map(|e| if let Entry::Assign { key, .. } = e { Some(key.as_str()) } else { None }).collect();
        assert_eq!(keys, ["a", "b", "c"], "later objects are top level again");
    }

    #[test]
    fn drops_stray_closers_and_closes_at_end() {
        let src = "a = {\n}\n}\nb = { # comment with } brace\n\tname = \"has } brace\"\n\tc = {\n";
        let r = repair("common/x", src).unwrap();
        assert_eq!(r.fixes, ["Removed a '}' that closed nothing on line 3", "Closed 2 blocks left open at the end of the file"]);
        assert_eq!(r.damaged, ["a", "b"]);
        assert!(r.text.contains("# comment with } brace") && r.text.contains("\"has } brace\""));
    }

    #[test]
    fn reports_event_ids_and_skips_valid_files() {
        let r = repair("events", "namespace = ns\ncountry_event = {\n\tid = ns.1\n\ttrigger = {\n").unwrap();
        assert_eq!(r.damaged, ["ns.1"]);
        assert_eq!(repair("common/x", "a = { }"), None);
        let fixed = replace_object("events", "ns.1", &r.text, "country_event = {\n\tid = ns.1\n\ttrigger = { }\n}").unwrap();
        assert_eq!(fixed, "namespace = ns\ncountry_event = {\n\tid = ns.1\n\ttrigger = { }\n}\n");
        assert_eq!(repair("common/x", "a = { name = \"unterminated }"), None);
    }
}
