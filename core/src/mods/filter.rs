//! The mod list's search syntax.
//!
//! Words are matched anywhere in a mod's name, alias, tags or Workshop ID,
//! and all of them must match. Besides words, a query can hold:
//! - `enabled:yes|no` (also `selected:`), `achievements:yes|no`,
//!   `source:workshop|steam|local`, `version:3.12` and `conflicts:yes|no`
//! - `--` before a word or value to negate it, e.g. `source:--local`
//! - `||` between alternatives, binding looser than the implied "and"
//!   (`&&` may be written but changes nothing)
//! - `"quoted words"`, matched as one phrase

use super::Source;

/// What a query can match a mod on.
#[derive(Debug, Clone)]
pub struct Facts {
    /// Name, alias, tags and Workshop ID, lowercased.
    pub text: String,
    pub enabled: bool,
    /// Whether achievements stay on with this mod.
    pub achievements: bool,
    pub source: Source,
    /// The game version the mod supports, e.g. `3.12.*`.
    pub version: String,
    pub conflicts: bool,
    /// Doesn't declare support for the installed game version.
    pub outdated: bool,
    /// Has a warning: a missing requirement, a bad load order, a broken
    /// descriptor and the like.
    pub attention: bool,
    /// The Smart Sort section of an enabled mod, once known.
    pub category: Option<super::smart_sort::Category>,
}

/// The sidebar's lists of mods, each a filter over the same rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    All,
    Attention,
    Outdated,
    Conflicts,
    NoAchievements,
    Disabled,
}

impl View {
    pub const ALL: [View; 6] = [View::All, View::Attention, View::Outdated, View::Conflicts, View::NoAchievements, View::Disabled];

    pub fn title(self) -> String {
        match self {
            View::All => t!("Load Order"),
            View::Attention => t!("Needs Attention"),
            View::Outdated => t!("Not Updated for This Game"),
            View::Conflicts => t!("Has Conflicts"),
            View::NoAchievements => t!("Turns Off Achievements"),
            View::Disabled => t!("Disabled"),
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            View::All => "view-list-symbolic",
            View::Attention => "dialog-warning-symbolic",
            View::Outdated => "software-update-available-symbolic",
            View::Conflicts => "edit-copy-symbolic",
            View::NoAchievements => "action-unavailable-symbolic",
            View::Disabled => "list-remove-symbolic",
        }
    }

    pub fn matches(self, f: &Facts) -> bool {
        match self {
            View::All => true,
            View::Attention => f.attention,
            View::Outdated => f.enabled && f.outdated,
            View::Conflicts => f.conflicts,
            View::NoAchievements => f.enabled && !f.achievements,
            View::Disabled => !f.enabled,
        }
    }
}

/// The filter keys, in the order the filter popover shows them.
pub const KEYS: [&str; 4] = ["enabled", "source", "achievements", "conflicts"];

#[derive(Debug, Clone, PartialEq)]
enum Test {
    Text(String),
    Enabled(bool),
    Achievements(bool),
    Source(Source),
    Version(String),
    Conflicts(bool),
}

#[derive(Debug, Clone, PartialEq)]
struct Term {
    negated: bool,
    test: Test,
}

/// A parsed query: any of the groups, each matching when all its terms do.
#[derive(Debug, Default)]
pub struct Query {
    groups: Vec<Vec<Term>>,
}

impl Query {
    pub fn parse(src: &str) -> Self {
        let mut groups = vec![Vec::new()];
        for token in tokens(src) {
            match token.as_str() {
                "||" => groups.push(Vec::new()),
                "&&" => {}
                _ => {
                    if let Some(term) = term(&token) {
                        groups.last_mut().expect("never empty").push(term);
                    }
                }
            }
        }
        groups.retain(|g| !g.is_empty());
        Self { groups }
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub fn matches(&self, facts: &Facts) -> bool {
        self.is_empty() || self.groups.iter().any(|g| g.iter().all(|t| t.matches(facts)))
    }
}

impl Term {
    fn matches(&self, f: &Facts) -> bool {
        let yes = match &self.test {
            Test::Text(s) => f.text.contains(s.as_str()),
            Test::Enabled(b) => f.enabled == *b,
            Test::Achievements(b) => f.achievements == *b,
            Test::Source(s) => f.source == *s,
            Test::Version(v) => version_matches(&f.version, v),
            Test::Conflicts(b) => f.conflicts == *b,
        };
        yes != self.negated
    }
}

/// Splits on whitespace, keeping quoted phrases (quotes removed) together.
fn tokens(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in src.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn term(token: &str) -> Option<Term> {
    let token = token.to_lowercase();
    let (mut negated, token) = match token.strip_prefix("--") {
        Some(rest) => (true, rest.to_owned()),
        None => (false, token),
    };
    let Some((key, value)) = token.split_once(':') else {
        return (!token.is_empty()).then_some(Term { negated, test: Test::Text(token) });
    };
    let value = match value.strip_prefix("--") {
        Some(rest) => {
            negated = !negated;
            rest
        }
        None => value,
    };
    // A key being typed, with no value yet, doesn't filter.
    if value.is_empty() && is_key(key) {
        return None;
    }
    let test = match key {
        "enabled" | "selected" => yes_no(value).map(Test::Enabled),
        "achievements" => yes_no(value).map(Test::Achievements),
        "conflicts" => yes_no(value).map(Test::Conflicts),
        "source" => match value {
            "workshop" | "steam" => Some(Test::Source(Source::Workshop)),
            "local" => Some(Test::Source(Source::Local)),
            _ => None,
        },
        "version" => Some(Test::Version(value.trim_start_matches('v').to_owned())),
        _ => None,
    }
    // Not a key or value we know, so plain text, e.g. a name with a colon.
    .unwrap_or_else(|| Test::Text(token.clone()));
    Some(Term { negated, test })
}

fn is_key(key: &str) -> bool {
    KEYS.contains(&key) || matches!(key, "selected" | "version")
}

fn yes_no(value: &str) -> Option<bool> {
    match value {
        "yes" | "y" | "true" | "on" => Some(true),
        "no" | "n" | "false" | "off" => Some(false),
        _ => None,
    }
}

/// Whether a supported version such as `3.12.*` or `v3.12.4` covers the
/// version asked for, which may be partial (`3.12`).
fn version_matches(supported: &str, asked: &str) -> bool {
    let supported: Vec<&str> = supported.trim_start_matches('v').split('.').collect();
    asked.split('.').enumerate().all(|(i, part)| match supported.get(i) {
        Some(&"*") => true,
        Some(s) => *s == part || part == "*",
        None => false,
    })
}

/// The value a query sets `key` to, if one term does, for the popover.
pub fn value_of(src: &str, key: &str) -> Option<String> {
    tokens(src).into_iter().find_map(|t| {
        let (k, v) = t.split_once(':')?;
        let k = k.to_lowercase();
        let k = if k == "selected" { "enabled" } else { &k };
        (k == key).then(|| v.to_lowercase())
    })
}

/// The query with every `key:` term replaced by `key:value`, or removed
/// when `value` is `None`.
pub fn with_value(src: &str, key: &str, value: Option<&str>) -> String {
    let mut out: Vec<String> = tokens(src)
        .into_iter()
        .filter(|t| t.split_once(':').is_none_or(|(k, _)| {
            let k = if k.eq_ignore_ascii_case("selected") { "enabled" } else { k };
            !k.eq_ignore_ascii_case(key)
        }))
        .map(|t| if t.contains(char::is_whitespace) { format!("\"{t}\"") } else { t })
        .collect();
    if let Some(value) = value {
        out.push(format!("{key}:{value}"));
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            text: "gigastructural engineering 1121692237 gameplay".into(),
            enabled: true,
            achievements: false,
            source: Source::Workshop,
            version: "v3.12.*".into(),
            conflicts: true,
            outdated: false,
            attention: true,
            category: None,
        }
    }

    fn matches(q: &str) -> bool {
        Query::parse(q).matches(&facts())
    }

    #[test]
    fn matches_words_and_keys() {
        assert!(matches(""));
        assert!(matches("GIGA"));
        assert!(matches("giga engineering"));
        assert!(!matches("giga ethics"));
        assert!(matches("1121692237"));
        assert!(matches("\"structural engineering\""));
        assert!(!matches("\"engineering structural\""));
        assert!(matches("enabled:yes source:steam achievements:no conflicts:yes"));
        assert!(matches("selected:yes"));
        assert!(!matches("source:local"));
        assert!(matches("source:--local"));
        assert!(matches("--source:local"));
        assert!(!matches("--giga"));
    }

    #[test]
    fn matches_versions() {
        assert!(matches("version:3.12"));
        assert!(matches("version:3.12.4"));
        assert!(matches("version:v3"));
        assert!(!matches("version:3.11"));
        assert!(!matches("version:4"));
        assert!(matches("version:"), "an unfinished key matches everything");
    }

    #[test]
    fn combines_with_or() {
        assert!(matches("source:local || giga"));
        assert!(matches("ethics || enabled:yes && giga"));
        assert!(!matches("ethics || enabled:no"));
        assert!(!matches("ethics ||"), "an unfinished alternative is ignored");
        assert!(!matches("enabled:maybe"), "a value it doesn't know is text");
    }

    #[test]
    fn views_pick_mods() {
        let f = facts();
        assert!(View::All.matches(&f) && View::Attention.matches(&f) && View::Conflicts.matches(&f) && View::NoAchievements.matches(&f));
        assert!(!View::Outdated.matches(&f) && !View::Disabled.matches(&f));
        let off = Facts { enabled: false, ..facts() };
        assert!(View::Disabled.matches(&off) && !View::NoAchievements.matches(&off), "only enabled mods cost achievements");
    }

    #[test]
    fn edits_keys() {
        assert_eq!(value_of("giga Selected:No", "enabled").as_deref(), Some("no"));
        assert_eq!(with_value("giga enabled:no \"two words\"", "enabled", Some("yes")), "giga \"two words\" enabled:yes");
        assert_eq!(with_value("giga source:local", "source", None), "giga");
    }
}
