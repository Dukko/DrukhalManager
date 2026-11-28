//! Text rules that hide conflicts, like Irony Mod Manager's Ignore Rules.
//!
//! One rule per line:
//!
//! ```text
//! # Comments start with #.
//! gfx/**                          every file override under gfx
//! localisation/*                  every localisation key
//! common/defines                  a folder, and everything below it
//! interface/*.gui | Mod A | Mod B only when exactly these mods conflict
//! common/traditions | won by Mod  only while Mod's version is the one used
//! !gfx/portraits/**               shows these again
//! ```
//!
//! A file override is matched by its path. An object conflict is matched
//! by `<folder>/<key>` (e.g. `common/buildings/building_lab`), or by its
//! files when every one of them matches. Names and paths compare ignoring
//! case. Later rules win over earlier ones, so a `!` rule after a broad
//! rule brings some conflicts back.

use std::collections::BTreeSet;

use super::conflicts::{FileConflict, ObjectConflict};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// The line it's on, from 0.
    line: usize,
    /// Whether it shows conflicts again rather than hiding them.
    negated: bool,
    /// Lowercased, without a trailing `/`.
    pattern: String,
    /// The exact set of mods it applies to, lowercased; empty for any.
    mods: BTreeSet<String>,
    /// The mod that has to win, lowercased.
    winner: Option<String>,
    /// The rule as written, for showing it.
    shown: Shown,
}

/// A rule's parts as written, for showing it in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The line it's on, from 0.
    pub line: usize,
    /// Whether it hides conflicts, rather than showing them again.
    pub hides: bool,
    pub pattern: String,
    /// The exact set of mods it applies to; empty for any.
    pub mods: Vec<String>,
    /// The mod whose version has to be the one used.
    pub winner: Option<String>,
}

/// What a rule's part after `|` starts with to name the winning mod.
const WON_BY: &str = "won by ";

/// The rule hiding the conflicts under `folder` that `mod_name` wins.
pub fn won_by(folder: &str, mod_name: &str) -> String {
    format!("{} | {WON_BY}{}", folder.trim_matches('/'), mod_name.trim())
}

impl Rules {
    pub fn parse(text: &str) -> Self {
        let rules = text
            .lines()
            .enumerate()
            .filter_map(|(line, raw)| {
                let raw = raw.trim();
                if raw.is_empty() || raw.starts_with('#') {
                    return None;
                }
                let (negated, raw) = match raw.strip_prefix('!') {
                    Some(rest) => (true, rest.trim_start()),
                    None => (false, raw),
                };
                let mut parts = raw.split('|').map(str::trim);
                let shown_pattern = parts.next()?.trim_matches('/').replace('\\', "/");
                let (mut shown_mods, mut shown_winner) = (Vec::new(), None);
                for part in parts.filter(|m| !m.is_empty()) {
                    match part.get(..WON_BY.len()).filter(|p| p.eq_ignore_ascii_case(WON_BY)) {
                        Some(_) => shown_winner = Some(part[WON_BY.len()..].trim().to_owned()),
                        None => shown_mods.push(part.to_owned()),
                    }
                }
                let pattern = shown_pattern.to_lowercase();
                let mods = shown_mods.iter().map(|m| m.to_lowercase()).collect();
                let winner = shown_winner.as_deref().map(str::to_lowercase);
                let shown = Shown { line, hides: !negated, pattern: shown_pattern, mods: shown_mods, winner: shown_winner };
                (!pattern.is_empty()).then_some(Rule { line, negated, pattern, mods, winner, shown })
            })
            .collect();
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The line of the rule that hides the file override, if one does.
    /// `names` are the mod names by scanned position.
    pub fn hiding_file(&self, c: &FileConflict, names: &[String]) -> Option<usize> {
        self.deciding_file(c, names).filter(|&(_, hides)| hides).map(|(line, _)| line)
    }

    /// The line of the rule that hides the object conflict, if one does.
    pub fn hiding_object(&self, c: &ObjectConflict, names: &[String]) -> Option<usize> {
        self.deciding_object(c, names).filter(|&(_, hides)| hides).map(|(line, _)| line)
    }

    /// The line of the last rule matching the file override, and whether
    /// it hides it (rather than showing it again).
    pub fn deciding_file(&self, c: &FileConflict, names: &[String]) -> Option<(usize, bool)> {
        if self.rules.is_empty() {
            return None;
        }
        let winner = || c.mods.last().and_then(|&p| names.get(p)).map(|n| n.to_lowercase());
        self.decide(&[c.path.as_str()], &[], || mods_of(c.mods.iter().copied(), names), winner)
    }

    pub fn deciding_object(&self, c: &ObjectConflict, names: &[String]) -> Option<(usize, bool)> {
        if self.rules.is_empty() {
            return None;
        }
        let name = format!("{}/{}", c.category, c.key);
        let files: Vec<&str> = c.definitions.iter().map(|d| d.file.as_str()).collect();
        let winner = || c.winner().and_then(|(i, _)| names.get(c.definitions[i].mod_position)).map(|n| n.to_lowercase());
        self.decide(&[name.as_str()], &files, || mods_of(c.definitions.iter().map(|d| d.mod_position), names), winner)
    }

    /// The last rule matching `names` (any of them) or `files` (all of
    /// them): its line, and whether it hides. `mods` and `winner` are only
    /// worked out if a rule needs them.
    fn decide(&self, names: &[&str], files: &[&str], mods: impl FnOnce() -> BTreeSet<String>, winner: impl FnOnce() -> Option<String>) -> Option<(usize, bool)> {
        // Lowercased once, not once per rule: this runs for every conflict
        // whenever the mod list's counts are worked out.
        let names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let files: Vec<String> = files.iter().map(|f| f.to_lowercase()).collect();
        let mods = if self.rules.iter().any(|r| !r.mods.is_empty()) { mods() } else { BTreeSet::new() };
        let winner = if self.rules.iter().any(|r| r.winner.is_some()) { winner() } else { None };
        let last = self.rules.iter().rev().find(|r| {
            (r.mods.is_empty() || r.mods == mods)
                && (r.winner.is_none() || r.winner == winner)
                && (names.iter().any(|n| matches(&r.pattern, n)) || (!files.is_empty() && files.iter().all(|f| matches(&r.pattern, f))))
        })?;
        Some((last.line, !last.negated))
    }

    /// Each rule as written, in order.
    pub fn shown(&self) -> impl Iterator<Item = &Shown> + '_ {
        self.rules.iter().map(|r| &r.shown)
    }
}

fn mods_of(positions: impl Iterator<Item = usize>, names: &[String]) -> BTreeSet<String> {
    positions.filter_map(|p| names.get(p)).map(|n| n.to_lowercase()).collect()
}

/// Whether `path` matches `pattern`: `*` is any run of characters within a
/// folder, `**` any run across folders, and `?` one character. A pattern
/// with no wildcards also matches everything below it.
fn matches(pattern: &str, path: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return path == pattern || path.strip_prefix(pattern).is_some_and(|rest| rest.starts_with('/'));
    }
    glob(pattern.as_bytes(), path.as_bytes())
}

fn glob(pattern: &[u8], path: &[u8]) -> bool {
    match pattern {
        [] => path.is_empty(),
        [b'*', b'*', rest @ ..] => {
            // `**/` also matches no folders at all.
            let rest_after_slash = rest.strip_prefix(b"/").unwrap_or(rest);
            (0..=path.len()).any(|i| glob(rest, &path[i..]) || glob(rest_after_slash, &path[i..]))
        }
        [b'*', rest @ ..] => (0..=path.len()).take_while(|&i| i == 0 || path[i - 1] != b'/').any(|i| glob(rest, &path[i..])),
        [b'?', rest @ ..] => path.first().is_some_and(|&c| c != b'/') && glob(rest, &path[1..]),
        [c, rest @ ..] => path.first() == Some(c) && glob(rest, &path[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::conflicts::Definition;

    #[test]
    fn globs_match_like_paths() {
        assert!(matches("gfx/**", "gfx/models/ships/a.mesh"));
        assert!(matches("gfx/*", "gfx/a.dds") && !matches("gfx/*", "gfx/models/a.dds"));
        assert!(matches("**/*.gui", "interface/main.gui") && matches("**/*.gui", "a.gui"));
        assert!(matches("common/defines", "common/defines/00_defines.txt") && !matches("common/def", "common/defines/x.txt"));
        assert!(matches("events/?.txt", "events/a.txt") && !matches("events/?.txt", "events/ab.txt"));
        assert!(matches("gfx/**/ships", "gfx/ships") && matches("gfx/**/ships", "gfx/a/b/ships"));
        // Multi-byte names don't break matching.
        assert!(matches("localisation/*", "localisation/l_english:ödön"));
    }

    #[test]
    fn later_rules_win_and_mod_sets_are_exact() {
        let names: Vec<String> = ["Mod A", "Mod B", "Mod C"].map(String::from).into();
        let file = |path: &str, mods: Vec<usize>| FileConflict { path: path.into(), mods };
        let rules = Rules::parse("# junk\ngfx/**\n\n!gfx/portraits/**\nInterface/*.gui | mod a | MOD B\n");
        assert_eq!(rules.hiding_file(&file("gfx/models/x.mesh", vec![0, 1]), &names), Some(1));
        assert_eq!(rules.hiding_file(&file("GFX/portraits/x.dds", vec![0, 1]), &names), None, "shown again");
        assert_eq!(rules.hiding_file(&file("interface/main.gui", vec![0, 1]), &names), Some(4));
        assert_eq!(rules.hiding_file(&file("interface/main.gui", vec![0, 1, 2]), &names), None, "a different set of mods");
        assert_eq!(rules.hiding_file(&file("common/x.txt", vec![0, 1]), &names), None);

        let object = |category: &str, key: &str, files: &[&str]| ObjectConflict {
            category: category.into(),
            key: key.into(),
            definitions: files.iter().enumerate().map(|(i, f)| Definition { mod_position: i, file: (*f).into() }).collect(),
        };
        let rules = Rules::parse("localisation/*\ncommon/buildings/building_lab\ncommon/**/zz_compat_*.txt");
        assert_eq!(rules.hiding_object(&object("localisation", "l_english:x", &["localisation/a_l_english.yml"]), &names), Some(0));
        assert_eq!(rules.hiding_object(&object("common/buildings", "building_lab", &["common/buildings/a.txt"]), &names), Some(1));
        let both = object("common/buildings", "farm", &["common/buildings/zz_compat_a.txt", "common/buildings/zz_compat_b.txt"]);
        assert_eq!(rules.hiding_object(&both, &names), Some(2), "every file matches");
        let one = object("common/buildings", "farm", &["common/buildings/zz_compat_a.txt", "common/buildings/b.txt"]);
        assert_eq!(rules.hiding_object(&one, &names), None, "only one does");
        assert!(Rules::parse("# nothing\n\n  ").is_empty());
    }

    #[test]
    fn won_by_rules_need_the_mod_to_win() {
        let names: Vec<String> = ["More AP", "Sartek Merger"].map(String::from).into();
        let line = won_by("common/ascension_perks/", "Sartek Merger");
        assert_eq!(line, "common/ascension_perks | won by Sartek Merger");
        let rules = Rules::parse(&format!("# x\n{}", line.replace("won by", "Won By").replace("Sartek", "sartek")));
        let shown: Vec<_> = rules.shown().collect();
        assert_eq!((shown[0].line, shown[0].pattern.as_str(), shown[0].winner.as_deref()), (1, "common/ascension_perks", Some("sartek Merger")));
        assert!(shown[0].mods.is_empty() && shown[0].hides);

        // Objects in a LIOS folder: the later file name wins.
        let object = |files: [&str; 2]| ObjectConflict {
            category: "common/ascension_perks".into(),
            key: "ap_x".into(),
            definitions: files.iter().enumerate().map(|(i, f)| Definition { mod_position: i, file: (*f).into() }).collect(),
        };
        assert_eq!(rules.hiding_object(&object(["common/ascension_perks/a.txt", "common/ascension_perks/z.txt"]), &names), Some(1));
        assert_eq!(rules.hiding_object(&object(["common/ascension_perks/z.txt", "common/ascension_perks/a.txt"]), &names), None, "More AP wins");
        // Files: the last mod in load order wins.
        let file = |mods: Vec<usize>| FileConflict { path: "common/ascension_perks/00_perks.txt".into(), mods };
        assert_eq!(rules.hiding_file(&file(vec![0, 1]), &names), Some(1));
        assert_eq!(rules.hiding_file(&file(vec![1, 0]), &names), None);
        assert_eq!(rules.hiding_file(&FileConflict { path: "common/traditions/x.txt".into(), mods: vec![0, 1] }, &names), None, "another folder");
    }
}
