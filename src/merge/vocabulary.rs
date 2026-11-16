//! The keys a script uses, to spot ones an AI invented.

use std::collections::BTreeSet;

/// Every key in `text` that's assigned or compared: a bare word directly
/// followed (after spaces) by `=`, `<`, `>`, `<=`, `>=` or `!=`. Skips
/// comments (`#` to the line end), quoted strings, `@variables`, numbers, and
/// words holding `:` or `.` (scope chains such as `root.owner` or
/// `event_target:x`, whose parts the game resolves at runtime).
pub fn keys(text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let delimiter = |c: char| c.is_whitespace() || matches!(c, '{' | '}' | '=' | '<' | '>' | '!' | '#' | '"');
    let mut found = BTreeSet::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            i += 1;
        } else if delimiter(c) {
            i += 1;
        } else {
            let start = i;
            while i < chars.len() && !delimiter(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            let operator = match chars.get(j) {
                Some('=' | '<' | '>') => true,
                Some('!') => chars.get(j + 1) == Some(&'='),
                _ => false,
            };
            let numeric = word.trim_start_matches('-').starts_with(|d: char| d.is_ascii_digit() || d == '.');
            if operator && !word.starts_with('@') && !word.contains([':', '.']) && !numeric {
                found.insert(word);
            }
        }
    }
    found
}

/// The keys of `answer` found in none of `known`, sorted.
pub fn unknown<'a>(answer: &str, known: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut missing = keys(answer);
    for text in known {
        if missing.is_empty() {
            break;
        }
        for key in keys(text) {
            missing.remove(&key);
        }
    }
    missing.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_assigned_and_compared_keys() {
        let text = "limit = {\n\thas_planetary_ascension_tier < 10\n\tvalue >= 2 # trailing = comment\n\t# every_fake = {\n\tname = \"x = y\"\n}\n@cost = 3\nroot.owner = { }\nevent_target:foo = { }\n";
        let expected: BTreeSet<String> = ["has_planetary_ascension_tier", "limit", "value", "name"].into_iter().map(String::from).collect();
        assert_eq!(keys(text), expected);
    }

    #[test]
    fn finds_keys_the_game_lacks() {
        assert_eq!(unknown("every_system_colony = { limit = { } }", ["every_system_planet_colony = { limit = { } }"]), vec!["every_system_colony".to_owned()]);
    }
}
