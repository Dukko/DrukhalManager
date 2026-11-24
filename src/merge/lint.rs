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

use crate::paradox::script::{self, Entry};

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

    let entries = script::parse(&out).ok()?;
    damaged = damaged
        .into_iter()
        .map(|key| {
            entries
                .iter()
                .find_map(|e| match e {
                    Entry::Assign { key: k, .. } if *k == key => crate::merge::conflicts::object_key_for_entry(category, e),
                    _ => None,
                })
                .unwrap_or(key)
        })
        .collect();
    Some(Repair { text: out, fixes, damaged })
}

/// The byte range of the top-level object `key` (an event id for events).
pub fn object_span(category: &str, key: &str, text: &str) -> Option<std::ops::Range<usize>> {
    script::parse_with_spans(text).ok()?.into_iter().find_map(|(entry, range)| {
        (crate::merge::conflicts::object_key_for_entry(category, &entry).as_deref() == Some(key)).then_some(range)
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    String(String),
    Op(String),
    Open,
    Close,
    Comma,
}

fn tokenize_line(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut s = String::from('"');
            while let Some(sc) = chars.next() {
                s.push(sc);
                if sc == '\\' {
                    if let Some(esc) = chars.next() {
                        s.push(esc);
                    }
                } else if sc == '"' {
                    break;
                }
            }
            tokens.push(Token::String(s));
            continue;
        }
        if c == '{' {
            chars.next();
            tokens.push(Token::Open);
            continue;
        }
        if c == '}' {
            chars.next();
            tokens.push(Token::Close);
            continue;
        }
        if c == ',' {
            chars.next();
            tokens.push(Token::Comma);
            continue;
        }
        if c == ';' {
            // Stray semicolon outside string/comment: skip it!
            chars.next();
            continue;
        }
        // Operators: =, <, >, <=, >=, !=, ?=, ==, <>
        if matches!(c, '=' | '<' | '>' | '!' | '?') {
            chars.next();
            let next = chars.peek().copied();
            match (c, next) {
                ('=', Some('=')) => {
                    chars.next();
                    tokens.push(Token::Op("=".to_owned()));
                }
                ('<', Some('>')) => {
                    chars.next();
                    tokens.push(Token::Op("!=".to_owned()));
                }
                ('<', Some('=')) => {
                    chars.next();
                    tokens.push(Token::Op("<=".to_owned()));
                }
                ('>', Some('=')) => {
                    chars.next();
                    tokens.push(Token::Op(">=".to_owned()));
                }
                ('!', Some('=')) => {
                    chars.next();
                    tokens.push(Token::Op("!=".to_owned()));
                }
                ('?', Some('=')) => {
                    chars.next();
                    tokens.push(Token::Op("?=".to_owned()));
                }
                ('=', _) => tokens.push(Token::Op("=".to_owned())),
                ('<', _) => tokens.push(Token::Op("<".to_owned())),
                ('>', _) => tokens.push(Token::Op(">".to_owned())),
                ('!', _) => tokens.push(Token::Word("!".to_owned())),
                ('?', _) => tokens.push(Token::Word("?".to_owned())),
                _ => {}
            }
            continue;
        }
        // Word: anything else up to whitespace, operators, quotes, braces, commas, semicolons
        let mut word = String::new();
        while let Some(&wc) = chars.peek() {
            if wc.is_whitespace() || matches!(wc, '"' | '{' | '}' | ',' | ';' | '=' | '<' | '>') {
                break;
            }
            if (wc == '!' || wc == '?') && peek_is_eq(&chars) {
                break;
            }
            word.push(wc);
            chars.next();
        }
        if !word.is_empty() {
            tokens.push(Token::Word(word));
        }
    }
    tokens
}

fn peek_is_eq(chars: &std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut it = chars.clone();
    it.next();
    it.peek() == Some(&'=')
}

fn format_tokens(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Token> = None;

    for token in tokens {
        if let Some(p) = prev {
            let need_space = match (p, token) {
                (_, Token::Comma) => false,
                (Token::Comma, _) => true,
                (_, Token::Op(_)) => true,
                (Token::Op(_), _) => true,
                (_, Token::Open) => true,
                (Token::Open, _) => true,
                (_, Token::Close) => true,
                (Token::Close, _) => true,
                (Token::Word(_), Token::Word(_)) => true,
                (Token::Word(_), Token::String(_)) => true,
                (Token::String(_), Token::Word(_)) => true,
                (Token::String(_), Token::String(_)) => true,
            };

            if need_space && !out.ends_with(' ') {
                out.push(' ');
            }
        }

        match token {
            Token::Word(w) => out.push_str(w),
            Token::String(s) => out.push_str(s),
            Token::Op(op) => out.push_str(op),
            Token::Open => out.push('{'),
            Token::Close => out.push('}'),
            Token::Comma => out.push(','),
        }

        prev = Some(token);
    }

    out
}

fn split_line_comment(line: &str) -> (&str, Option<&str>) {
    let mut in_string = false;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if in_string {
            if c == '\\' {
                chars.next();
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '#' {
            return (&line[..i], Some(&line[i..]));
        }
    }
    (line, None)
}

/// Counts the net brace nesting depth in `text` outside strings and comments.
pub fn count_brace_depth(text: &str) -> usize {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            if c == '\\' {
                chars.next();
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '#' => {
                for sc in chars.by_ref() {
                    if sc == '\n' {
                        break;
                    }
                }
            }
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

/// Formats and indents script code starting from `initial_depth`.
pub fn format_script(src: &str, initial_depth: usize) -> String {
    let normalized = src.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len() + 32);
    let mut depth = initial_depth;
    let mut in_string = false;
    let mut blank_run = 0usize;

    for raw_line in normalized.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            if blank_run == 0 && !out.is_empty() {
                out.push('\n');
                blank_run += 1;
            }
            continue;
        }
        blank_run = 0;

        if in_string {
            let mut chars = raw_line.chars().peekable();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '\\' {
                    chars.next();
                } else if c == '"' {
                    in_string = false;
                    closed = true;
                    break;
                }
            }
            out.push_str(raw_line);
            out.push('\n');
            if !closed {
                continue;
            }
            continue;
        }

        let (code, comment) = split_line_comment(raw_line);
        let code_trimmed = code.trim();

        if code_trimmed.is_empty() {
            if let Some(comm) = comment {
                if depth > 0 {
                    out.push_str(&"\t".repeat(depth));
                }
                out.push_str(comm.trim_end());
                out.push('\n');
            }
            continue;
        }

        let tokens = tokenize_line(code_trimmed);
        if tokens.is_empty() {
            if let Some(comm) = comment {
                if depth > 0 {
                    out.push_str(&"\t".repeat(depth));
                }
                out.push_str(comm.trim_end());
                out.push('\n');
            }
            continue;
        }

        if let Some(Token::String(s)) = tokens.last()
            && (!s.ends_with('"') || s.len() == 1)
        {
            in_string = true;
        }

        let leading_closes = tokens.iter().take_while(|t| matches!(t, Token::Close)).count();
        let total_opens = tokens.iter().filter(|t| matches!(t, Token::Open)).count();
        let total_closes = tokens.iter().filter(|t| matches!(t, Token::Close)).count();

        let line_indent = depth.saturating_sub(leading_closes);
        if line_indent > 0 {
            out.push_str(&"\t".repeat(line_indent));
        }

        let formatted_code = format_tokens(&tokens);
        out.push_str(&formatted_code);

        if let Some(comm) = comment {
            out.push_str("  ");
            out.push_str(comm.trim_end());
        }
        out.push('\n');

        depth = depth.saturating_sub(total_closes) + total_opens;
    }

    if src.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }

    out
}

/// Cleans up script code: repairs unbalanced braces if broken, normalizes operator
/// spacing, brace spacing, comments, and tab-indents blocks consistently.
pub fn clean_up(src: &str) -> String {
    let repaired = repair("", src).map(|r| r.text).unwrap_or_else(|| src.to_owned());
    format_script(&repaired, 0)
}

/// Cleans up a script snippet that is indented at `initial_depth`.
pub fn clean_up_snippet(src: &str, initial_depth: usize) -> String {
    format_script(src, initial_depth)
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

    #[test]
    fn clean_up_fixes_spacing_operators_and_indentation() {
        let messy = "\
country_event={
id=test.1
desc = \"hello = world\"
trigger={count<3 trait!=trait_foo is_ai=no}
immediate = {
if={
limit={
is_capital=yes
}
add_modifier={modifier=m}
}
}
}
";
        let cleaned = clean_up(messy);
        let expected = "\
country_event = {
\tid = test.1
\tdesc = \"hello = world\"
\ttrigger = { count < 3 trait != trait_foo is_ai = no }
\timmediate = {
\t\tif = {
\t\t\tlimit = {
\t\t\t\tis_capital = yes
\t\t\t}
\t\t\tadd_modifier = { modifier = m }
\t\t}
\t}
}
";
        assert_eq!(cleaned, expected);
    }

    #[test]
    fn clean_up_preserves_strings_and_comments() {
        let input = "\
# Top level comment
foo = {  # comment next to open
\tbar = \"a = { b } # not a comment\"  # real comment
\tbaz = 1;
}
";
        let cleaned = clean_up(input);
        let expected = "\
# Top level comment
foo = {  # comment next to open
\tbar = \"a = { b } # not a comment\"  # real comment
\tbaz = 1
}
";
        assert_eq!(cleaned, expected);
    }

    #[test]
    fn clean_up_collapses_excess_blank_lines() {
        let input = "a = 1\n\n\n\n\nb = 2\n";
        let cleaned = clean_up(input);
        assert_eq!(cleaned, "a = 1\n\nb = 2\n");
    }

    #[test]
    fn clean_up_snippet_uses_initial_depth() {
        let snippet = "if = {\nlimit = { x = 1 }\n}\n";
        let cleaned = clean_up_snippet(snippet, 2);
        let expected = "\t\tif = {\n\t\t\tlimit = { x = 1 }\n\t\t}\n";
        assert_eq!(cleaned, expected);
    }

    #[test]
    fn count_brace_depth_handles_strings_and_comments() {
        let text = "a = { b = { # { not opened\nc = \"{ string }\"\n";
        assert_eq!(count_brace_depth(text), 2);
        let closed = format!("{text}}}}}}}");
        assert_eq!(count_brace_depth(&closed), 0);
    }
}
