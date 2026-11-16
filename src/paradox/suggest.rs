//! Suggestions a player can send to the developers from their error log, as
//! a pre-filled GitHub issue they review and submit themselves.

/// The script keyword an error says the game doesn't know, if its wording is
/// one we recognise.
pub fn unknown_keyword(message: &str) -> Option<String> {
    const PHRASES: [&str; 2] = ["Invalid scripted effect:", "Unexpected token:"];
    PHRASES.iter().find_map(|phrase| {
        let rest = message[message.find(phrase)? + phrase.len()..].trim_start_matches(' ');
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        (end > 0).then(|| rest[..end].to_owned())
    })
}

/// `text` with the player's home folder replaced by `~`, in both `/` and `\`
/// spellings, case-insensitively on the drive letter. `home` is the home
/// folder as a string.
pub fn scrub(text: &str, home: &str) -> String {
    let home = home.trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return text.to_owned();
    }
    let mut out = text.to_owned();
    for spelling in [home.replace('\\', "/"), home.replace('/', "\\")] {
        out = replace_ignoring_case(&out, &spelling, "~");
    }
    out
}

/// Replaces every ASCII-case-insensitive occurrence of `needle`.
fn replace_ignoring_case(text: &str, needle: &str, with: &str) -> String {
    let (lower, needle) = (text.to_ascii_lowercase(), needle.to_ascii_lowercase());
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(&needle) {
        out.push_str(&text[at..at + found]);
        out.push_str(with);
        at += found + needle.len();
    }
    out.push_str(&text[at..]);
    out
}

/// Percent-encodes everything except `A-Z a-z 0-9 - _ . ~` (RFC 3986
/// unreserved), byte by byte over UTF-8.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub const NEW_ISSUE_URL: &str = "https://github.com/Dukko/DrukhalManager/issues/new";

/// The longest link GitHub reliably accepts, in bytes.
const MAX_URL: usize = 7000;

const CUT_LINE: &str = "…(cut to fit the link)";

fn issue_head(title: &str) -> String {
    format!("{NEW_ISSUE_URL}?title={}&body=", encode(title))
}

/// The body as `issue_url` keeps it: whole when it fits, otherwise cut at a
/// line end with a final line saying so.
pub fn fitted_body(title: &str, body: &str) -> String {
    let budget = MAX_URL.saturating_sub(issue_head(title).len());
    if encode(body).len() <= budget {
        return body.to_owned();
    }
    let budget = budget.saturating_sub(encode(&format!("\n{CUT_LINE}")).len());
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for (i, line) in body.split('\n').enumerate() {
        let cost = encode(line).len() + if i > 0 { 3 } else { 0 };
        if used + cost > budget {
            break;
        }
        kept.push(line);
        used += cost;
    }
    kept.push(CUT_LINE);
    kept.join("\n")
}

/// The link that opens a new issue with `title` and `body` filled in. The
/// body is cut at a line end so the whole link stays under 7,000 bytes.
pub fn issue_url(title: &str, body: &str) -> String {
    issue_head(title) + &encode(&fitted_body(title, body))
}

pub struct NoteSuggestion<'a> {
    pub keyword: &'a str,
    pub error: &'a str,
    pub mod_name: Option<&'a str>,
    pub game_version: &'a str,
    pub app_version: &'a str,
    pub ai_notes: Option<&'a str>,
}

pub struct HarmlessSuggestion<'a> {
    pub pattern: &'a str,
    pub mod_name: Option<&'a str>,
    pub game_version: &'a str,
    pub app_version: &'a str,
}

fn versions(game: &str, app: &str, mod_name: Option<&str>) -> String {
    let mut out = format!("### Game version\n\n{game}\n\n### DrukhalManager version\n\n{app}\n\n");
    if let Some(name) = mod_name {
        out.push_str(&format!("### Mod\n\n{name}\n\n"));
    }
    out
}

impl NoteSuggestion<'_> {
    pub fn title(&self) -> String {
        format!("Game note: {} no longer exists", self.keyword)
    }

    pub fn body(&self) -> String {
        let mut out = format!("### Keyword\n\n`{}`\n\n", self.keyword);
        out.push_str(&versions(self.game_version, self.app_version, self.mod_name));
        out.push_str(&format!("### Error\n\n```\n{}\n```\n\n", self.error.trim()));
        if let Some(notes) = self.ai_notes {
            out.push_str(&format!("### What Fix with AI suggested\n\n{}\n\n", notes.trim()));
        }
        out.push_str(&format!("Checked by DrukhalManager: `{}` appears nowhere in this game's script files.", self.keyword));
        out
    }
}

impl HarmlessSuggestion<'_> {
    pub fn title(&self) -> String {
        format!("Harmless error: {}", self.pattern.trim().chars().take(60).collect::<String>())
    }

    pub fn body(&self) -> String {
        let mut out = format!("### Error pattern\n\n```\n{}\n```\n\n", self.pattern.trim());
        out.push_str(&versions(self.game_version, self.app_version, self.mod_name));
        out.push_str("The player thinks this isn't a real problem.");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_unknown_keywords() {
        let effect = "[15:28:13][effect_impl.cpp:825]: Script Error: Invalid scripted effect: every_system_colony at  common/scripted_effects/uiod_psionic_auras_scripted_effects.txt:10";
        assert_eq!(unknown_keyword(effect).as_deref(), Some("every_system_colony"));
        let token = "[15:28:00][persistent.cpp:41]: Error: \"Unexpected token: ai_update_type, near line: 7 \" in file: \"common/technology/ui_tech_ascension_theory_repeatable.txt\" near line: 7";
        assert_eq!(unknown_keyword(token).as_deref(), Some("ai_update_type"));
        let other = "[15:28:00][scripted_effects.cpp:100]: Compiling source for  failed for missing args: foo";
        assert_eq!(unknown_keyword(other), None);
        assert_eq!(unknown_keyword("Unexpected token: , near"), None);
    }

    #[test]
    fn scrub_replaces_home() {
        let text = "/home/alice/.local/share/x and C:\\Users\\alice\\Documents\\x";
        assert_eq!(scrub(text, "/home/alice"), "~/.local/share/x and C:\\Users\\alice\\Documents\\x");
        let win = "c:\\users\\ALICE\\Documents\\x and C:/Users/alice/y";
        assert_eq!(scrub(win, "C:\\Users\\alice"), "~\\Documents\\x and ~/y");
        assert_eq!(scrub("abc", ""), "abc");
    }

    #[test]
    fn encodes_unreserved_only() {
        assert_eq!(encode("a b&c=é/"), "a%20b%26c%3D%C3%A9%2F");
    }

    #[test]
    fn issue_url_is_capped() {
        let body = "x".repeat(100) + "\n";
        let url = issue_url("Title here", &body.repeat(200));
        assert!(url.len() < 7000, "{}", url.len());
        assert!(url.starts_with(&format!("{NEW_ISSUE_URL}?title=")));
        let long = issue_url("t", &"é".repeat(20_000));
        assert!(long.len() < 7000);
        let short = issue_url("a b", "one\ntwo");
        assert!(short.ends_with("&body=one%0Atwo"));
        assert_eq!(fitted_body("a b", "one\ntwo"), "one\ntwo");
        let many = body.repeat(200);
        let fitted = fitted_body("Title here", &many);
        assert!(fitted.ends_with(&format!("\n{CUT_LINE}")));
        assert_eq!(issue_url("Title here", &many), issue_url("Title here", &fitted));
    }

    #[test]
    fn note_body_has_the_evidence() {
        let mut s = NoteSuggestion { keyword: "every_system_colony", error: "Invalid scripted effect: every_system_colony", mod_name: Some("Mod"), game_version: "4.0", app_version: "1.1.0", ai_notes: None };
        let body = s.body();
        assert!(body.contains("every_system_colony"));
        assert!(body.contains("```\nInvalid scripted effect"));
        assert!(body.contains("appears nowhere in this game's script files."));
        assert!(!body.contains("What Fix with AI suggested"));
        s.ai_notes = Some("renamed");
        assert!(s.body().contains("What Fix with AI suggested"));
        assert_eq!(s.title(), "Game note: every_system_colony no longer exists");
    }

    #[test]
    fn harmless_title_is_short() {
        let h = HarmlessSuggestion { pattern: &"p".repeat(100), mod_name: None, game_version: "unknown", app_version: "1.1.0" };
        assert_eq!(h.title().len(), "Harmless error: ".len() + 60);
        assert!(h.body().contains("isn't a real problem"));
    }
}
