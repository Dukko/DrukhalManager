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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixKind {
    /// A block left open was closed before the next top-level object started; nesting inside may be wrong.
    ClosedBeforeNextObject,
    /// A stray `}` that closed nothing at depth 0 was removed.
    RemovedStray,
    /// Unclosed blocks were closed at the end of the file.
    ClosedAtEof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair {
    pub text: String,
    /// What was changed, for the user.
    pub fixes: Vec<String>,
    /// Classification of each change in `fixes`.
    pub kinds: Vec<FixKind>,
    /// Top-level objects with ambiguous nesting (`ClosedBeforeNextObject`).
    pub damaged: Vec<Damage>,
}

/// A top-level object whose missing braces were added where only its author
/// can say they belonged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damage {
    pub key: String,
    /// The object as it was in the file, before any brace was added.
    pub original: String,
    /// How many `}` were added after the last line of `original`.
    pub added: usize,
    /// The lines of `original` (1-based) whose `{` was still open at that point.
    pub open_lines: Vec<usize>,
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
    let mut kinds = Vec::new();
    let mut damaged: Vec<Damage> = Vec::new();
    let mut depth = 0usize;
    let mut open_lines: Vec<usize> = Vec::new();
    let src_lines: Vec<&str> = src.split_inclusive('\n').collect();
    let mut in_string = false;
    // The top-level object currently open, by its first line.
    let mut current: Option<(usize, String)> = None;

    for (n, line) in src.split_inclusive('\n').enumerate() {
        let number = n + 1;
        if !in_string && depth > 0 && starts_top_level_object(line) {
            fixes.push(tnf!("Closed {count} block left open before line {line}", "Closed {count} blocks left open before line {line}", count = depth, line = number));
            kinds.push(FixKind::ClosedBeforeNextObject);
            if let Some((first, key)) = &current
                && !damaged.iter().any(|d| d.key == *key)
            {
                let original = src_lines[first - 1..number - 1].concat().trim_end().to_owned();
                let open_lines = open_lines.iter().filter(|&&l| l >= *first).map(|l| l - first + 1).collect();
                damaged.push(Damage { key: key.clone(), original, added: depth, open_lines });
            }
            open_lines.clear();
            out.push_str(&"}".repeat(depth));
            out.push('\n');
            depth = 0;
            current = None;
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
                '{' => {
                    depth += 1;
                    open_lines.push(number);
                }
                '}' if depth == 0 => {
                    fixes.push(tf!("Removed a '}}' that closed nothing on line {line}", line = number));
                    kinds.push(FixKind::RemovedStray);
                    continue;
                }
                '}' => {
                    depth -= 1;
                    open_lines.pop();
                    if depth == 0 {
                        current = None;
                    }
                }
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
        kinds.push(FixKind::ClosedAtEof);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&"}".repeat(depth));
        out.push('\n');
    }
    script::parse(&out).ok()?;

    let entries = script::parse(&out).ok()?;
    for d in &mut damaged {
        let key = entries.iter().find_map(|e| match e {
            Entry::Assign { key: k, .. } if *k == d.key => crate::merge::conflicts::object_key_for_entry(category, e),
            _ => None,
        });
        if let Some(key) = key {
            d.key = key;
        }
    }
    Some(Repair { text: out, fixes, kinds, damaged })
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

fn non_brace_tokens(text: &str) -> Vec<Token> {
    text.lines()
        .flat_map(|line| {
            let (code, _) = split_line_comment(line);
            tokenize_line(code)
                .into_iter()
                .filter(|t| !matches!(t, Token::Open | Token::Close))
        })
        .collect()
}

/// The comments in `text`, in order, without trailing whitespace.
fn comments(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| split_line_comment(line).1)
        .map(|comment| comment.trim_end().to_owned())
        .collect()
}

/// The entries of `wanted` that `found` doesn't contain, in order.
fn missing_in_order(wanted: &[String], found: &[String]) -> Vec<String> {
    let mut at = 0usize;
    let mut missing = Vec::new();
    for want in wanted {
        match found[at..].iter().position(|have| have == want) {
            Some(offset) => at += offset + 1,
            None => missing.push(want.clone()),
        }
    }
    missing
}

/// Ensures a brace repair only moved `{`/`}` and kept every non-brace token
/// and every comment, in the same order. Added comments and whitespace changes
/// are allowed.
///
/// The message is deliberately untranslated: it also goes to the model as the
/// reason a repair was rejected.
pub fn verify_brace_repair(original: &str, repaired: &str) -> Result<(), String> {
    let orig_tokens = non_brace_tokens(original);
    let new_tokens = non_brace_tokens(repaired);
    let missing_comments = missing_in_order(&comments(original), &comments(repaired));
    if orig_tokens == new_tokens && missing_comments.is_empty() {
        return Ok(());
    }

    if orig_tokens != new_tokens {
        let mut sorted_orig = orig_tokens.clone();
        sorted_orig.sort();
        let mut sorted_new = new_tokens.clone();
        sorted_new.sort();
        if sorted_orig == sorted_new {
            return Err(
                "The repair reordered tokens instead of only moving braces. Keep every line, key and value in its original order."
                    .to_owned(),
            );
        }
    }

    let mut lost = Vec::new();
    let mut pos = 0usize;
    for line in original.lines() {
        let (code, _) = split_line_comment(line);
        let line_tokens: Vec<Token> = tokenize_line(code)
            .into_iter()
            .filter(|t| !matches!(t, Token::Open | Token::Close))
            .collect();
        if line_tokens.is_empty() {
            continue;
        }
        let matched = new_tokens[pos..]
            .windows(line_tokens.len())
            .position(|w| w == line_tokens.as_slice());
        match matched {
            Some(offset) => pos += offset + line_tokens.len(),
            None => lost.push(line.trim().to_owned()),
        }
    }
    lost.extend(missing_comments);

    if lost.is_empty() {
        return Err(
            "The repair changed tokens instead of only moving braces. Keep every line, key and value exactly as it was."
                .to_owned(),
        );
    }
    let sample = lost.iter().take(10).cloned().collect::<Vec<_>>().join("\n");
    let extra = if lost.len() > 10 {
        format!("\n... (+{} more lines)", lost.len() - 10)
    } else {
        String::new()
    };
    Err(format!(
        "The repair dropped or changed this content. Restore it exactly:\n{sample}{extra}"
    ))
}

/// [`verify_brace_repair`] for a repair that also dropped the lines in
/// `removed`, each of which must be a whole line of `original`.
pub fn verify_repair_removing(original: &str, repaired: &str, removed: &[String]) -> Result<(), String> {
    let mut kept: Vec<&str> = original.lines().collect();
    for line in removed.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        match kept.iter().position(|l| l.trim() == line) {
            Some(at) => {
                kept.remove(at);
            }
            None => {
                return Err(format!("`{line}` is listed as removed but is not a line of the object. List only lines that are in the original."));
            }
        }
    }
    verify_brace_repair(&kept.join("\n"), repaired)
}

/// Whether `repaired` holds less than `original` apart from braces and whitespace.
pub fn shrank(original: &str, repaired: &str) -> bool {
    let size = |text: &str| text.chars().filter(|c| !c.is_whitespace() && !matches!(c, '{' | '}')).count();
    size(repaired) < size(original)
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

    fn damaged_keys(r: &Repair) -> Vec<&str> {
        r.damaged.iter().map(|d| d.key.as_str()).collect()
    }

    #[test]
    fn closes_blocks_before_the_next_object() {
        // Shaped like Better Origins' b_origins_buildings.txt.
        let src = "a = {\n\tpotential = {\n\t\towner = { OR = { x = yes }\n\t}\n}\n\nb = {\n\tcost = 1\n}\n\nc = {\n}\n";
        let r = repair("common/buildings", src).unwrap();
        assert_eq!(r.fixes, ["Closed 1 block left open before line 7"]);
        assert_eq!(damaged_keys(&r), ["a"]);
        let entries = script::parse(&r.text).unwrap();
        let keys: Vec<&str> = entries.iter().filter_map(|e| if let Entry::Assign { key, .. } = e { Some(key.as_str()) } else { None }).collect();
        assert_eq!(keys, ["a", "b", "c"], "later objects are top level again");
    }

    #[test]
    fn drops_stray_closers_and_closes_at_end() {
        let src = "a = {\n}\n}\nb = { # comment with } brace\n\tname = \"has } brace\"\n\tc = {\n";
        let r = repair("common/x", src).unwrap();
        assert_eq!(r.fixes, ["Removed a '}' that closed nothing on line 3", "Closed 2 blocks left open at the end of the file"]);
        assert_eq!(r.kinds, [FixKind::RemovedStray, FixKind::ClosedAtEof]);
        assert!(r.damaged.is_empty(), "unambiguous fixes should not mark objects for AI: {:?}", r.damaged);
        assert!(r.text.contains("# comment with } brace") && r.text.contains("\"has } brace\""));
    }

    #[test]
    fn reports_event_ids_and_skips_valid_files() {
        let r = repair("events", "namespace = ns\ncountry_event = {\n\tid = ns.1\n\ttrigger = {\n\ncountry_event = {\n\tid = ns.2\n}\n").unwrap();
        assert_eq!(damaged_keys(&r), ["ns.1"]);
        assert_eq!(r.kinds, [FixKind::ClosedBeforeNextObject]);
        assert_eq!(repair("common/x", "a = { }"), None);
        let fixed = replace_object("events", "ns.1", &r.text, "country_event = {\n\tid = ns.1\n\ttrigger = { }\n}").unwrap();
        assert_eq!(fixed, "namespace = ns\ncountry_event = {\n\tid = ns.1\n\ttrigger = { }\n}\ncountry_event = {\n\tid = ns.2\n}\n");
        assert_eq!(repair("common/x", "a = { name = \"unterminated }"), None);
    }

    #[test]
    fn stray_closer_after_complete_object_is_not_damaged() {
        let src = "a = {\n\tcost = 1\n}\n}\nb = {\n\tcost = 2\n}\n";
        let r = repair("common/buildings", src).unwrap();
        assert_eq!(r.kinds, [FixKind::RemovedStray]);
        assert!(r.damaged.is_empty());
        assert!(script::parse(&r.text).is_ok());
    }

    #[test]
    fn missing_final_brace_at_eof_is_not_damaged() {
        let src = "a = {\n\tcost = 1\n}\nb = {\n\tcost = 2\n";
        let r = repair("common/buildings", src).unwrap();
        assert_eq!(r.kinds, [FixKind::ClosedAtEof]);
        assert!(r.damaged.is_empty());
        assert!(script::parse(&r.text).is_ok());
    }

    #[test]
    fn unclosed_object_before_next_object_flags_only_that_object() {
        let src = "ok_first = {\n\tcost = 1\n}\nbroken = {\n\tpotential = {\n\t\tis_ai = no\n\nok_last = {\n\tcost = 2\n}\n";
        let r = repair("common/buildings", src).unwrap();
        assert_eq!(r.kinds, [FixKind::ClosedBeforeNextObject]);
        assert_eq!(damaged_keys(&r), ["broken"]);
        let d = &r.damaged[0];
        assert_eq!(d.original, "broken = {\n\tpotential = {\n\t\tis_ai = no");
        assert_eq!((d.added, d.open_lines.as_slice()), (2, [1, 2].as_slice()));
    }

    #[test]
    fn closer_left_open_after_a_finished_object_is_not_blamed_on_it() {
        let src = "done = {\n\tcost = 1\n}\n  indented = {\n\tcost = 2\nnext = {\n\tcost = 3\n}\n";
        let r = repair("common/buildings", src).unwrap();
        assert_eq!(r.kinds, [FixKind::ClosedBeforeNextObject]);
        assert!(r.damaged.is_empty(), "blamed an unrelated object: {:?}", r.damaged);
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

    #[test]
    fn brace_repair_accepts_moved_braces_and_complex_tokens() {
        let orig = "sv = {\n\tcomplex_trigger_modifier = {\n\t\ttrigger = check_variable\n\t\tparameters = { who = planet.value:foo|CHAIN|bar| }\n\t\t}\n\t\tmult = 2\n}";
        let moved = "sv = {\n\tcomplex_trigger_modifier = {\n\t\ttrigger = check_variable # moved comment\n\t\tparameters = {\n\t\t\twho = planet.value:foo|CHAIN|bar|\n\t\t}\n\t}\n\tmult = 2\n}";
        assert_eq!(verify_brace_repair(orig, moved), Ok(()));
    }

    #[test]
    fn brace_repair_rejects_deleted_line_and_stub() {
        let orig = "building_imperial_government_sv = {\n\tbase = 0\n\tcomplex_trigger_modifier = {\n\t\ttrigger = num_assigned_jobs\n\t}\n\tmodifier = {\n\t\tadd = 1\n\t}\n\tmult = 2\n}";
        let stub = "building_imperial_government_sv = { base = 0 }";
        let err = verify_brace_repair(orig, stub).unwrap_err();
        assert!(err.contains("dropped or changed"), "{err}");
        assert!(err.contains("mult = 2"), "{err}");

        let empty = "building_imperial_government_sv = {}";
        assert!(verify_brace_repair(orig, empty).is_err());

        let altered = orig.replace("mult = 2", "mult = 3");
        let err = verify_brace_repair(orig, &altered).unwrap_err();
        assert!(err.contains("mult = 2"), "{err}");
    }

    #[test]
    fn repair_removing_allows_only_the_listed_lines() {
        let orig = "sv = {\n\tbase = 0\n\tmult = 2\n\tadd = 1\n}";
        let stub = "sv = {\n\tbase = 0\n\tadd = 1\n}";
        assert!(verify_repair_removing(orig, stub, &["mult = 2".to_owned()]).is_ok());
        assert!(verify_repair_removing(orig, stub, &[]).is_err());
        assert!(verify_repair_removing(orig, stub, &["add = 1".to_owned()]).is_err());
        let invented = verify_repair_removing(orig, stub, &["mult = 3".to_owned()]).unwrap_err();
        assert!(invented.contains("not a line of the object"), "{invented}");
    }

    #[test]
    fn shrank_ignores_braces_and_whitespace() {
        assert!(!shrank("a = {\n\tb = 1\n}", "a = { b = 1 }"));
        assert!(shrank("a = {\n\tb = 1\n\tc = 2\n}", "a = { b = 1 }"));
    }

    #[test]
    fn brace_repair_rejects_reordered_values() {
        let orig = "sv = {\n\ta = 1\n\tb = 2\n}";
        let reordered = "sv = {\n\tb = 2\n\ta = 1\n}";
        let err = verify_brace_repair(orig, reordered).unwrap_err();
        assert!(err.contains("reordered"), "{err}");
        assert!(!err.contains("dropped or changed"), "{err}");
    }

    #[test]
    fn brace_repair_keeps_comments_and_allows_new_ones() {
        let orig = "sv = {\n\ta = 1 # keep\n\tb = 2 # also keep\n}";
        let dropped = "sv = {\n\ta = 1\n\tb = 2 # also keep\n}";
        let err = verify_brace_repair(orig, dropped).unwrap_err();
        assert!(err.contains("# keep"), "{err}");
        assert!(!err.contains("# also keep"), "{err}");

        let added = "sv = {\n\ta = 1 # keep\n\tb = 2 # also keep\n\t# extra\n}";
        assert_eq!(verify_brace_repair(orig, added), Ok(()));

        let swapped = "sv = {\n\tb = 2 # also keep\n\ta = 1 # keep\n}";
        let err = verify_brace_repair(orig, swapped).unwrap_err();
        assert!(err.contains("reordered"), "{err}");
    }

    #[test]
    fn brace_repair_accepts_a_different_nesting() {
        let orig = "sv = {\n\tblock = {\n\t\ta = 1\n\t}\n}";
        let nested = "sv = {\n\tblock = {\n\t}\n\ta = 1\n}";
        assert_eq!(verify_brace_repair(orig, nested), Ok(()));
    }

    #[test]
    fn brace_repair_accepts_the_reported_scripted_value() {
        let orig = "building_imperial_government_sv = {\n\tbase = 0\n\tcomplex_trigger_modifier = {\n\t\tpotential = { NOT = { has_global_flag = PR_installed } }\n\t\ttrigger = count_owned_pop\n\t\tparameters = {\n\t\t\tlimit = {\n\t\t\t\tOR = {\n\t\t\t\t\thas_job = priest\n\t\t\t\t\thas_job = head_maid # rogue maid\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t\tmode = add\n\t}\n\tmodifier = {\n\t\thas_global_flag = PR_installed\n\t\tadd = planet.value:PR_prmt_sv_plnt_JOB_efficiency|CHAIN|culture|JIC|planet_administrators|\n\t}\n\tmult = value:imperial_governance_produces_mult_sv\n}";
        let moved = orig.replace("\t\tparameters = {\n", "\t\tparameters =\n\t\t{\n").replace("\t\tmode = add\n\t}", "\t\tmode = add }");
        assert_ne!(orig, moved);
        assert_eq!(verify_brace_repair(orig, &moved), Ok(()));
    }
}
