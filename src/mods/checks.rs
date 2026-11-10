//! Load order checks: missing or late dependencies and mods made for an
//! older game version. Dependency handling (by mod name, dependencies first)
//! follows Irony Mod Manager's `ModService`
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::collections::HashMap;

use super::Mod;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub message: String,
    /// The issue is fixed by sorting by dependencies.
    pub fixed_by_sorting: bool,
}

/// Whether a descriptor's `supported_version` (`v4.5.*`, `4.*`, `v4.5*`,
/// `*`, `v4.4.6`…) covers the game version (`v4.5.3`). Like the launcher,
/// only major and minor versions count.
pub fn version_matches(supported: &str, game: &str) -> bool {
    let parts = |s: &str| s.trim().trim_start_matches(['v', 'V']).split('.').map(str::to_owned).collect::<Vec<_>>();
    let (supported, game) = (parts(supported), parts(game));
    for i in 0..2 {
        let Some(want) = supported.get(i) else { return true };
        if want.starts_with('*') || want.is_empty() {
            return true;
        }
        let have = game.get(i).map_or("", String::as_str);
        if let Some(prefix) = want.strip_suffix('*') {
            return have.starts_with(prefix);
        }
        if want != have {
            return false;
        }
    }
    true
}

fn by_name(mods: &[Mod]) -> HashMap<String, usize> {
    mods.iter().enumerate().map(|(i, m)| (m.descriptor.name.trim().to_lowercase(), i)).collect()
}

/// Issues per mod index.
pub fn check(mods: &[Mod], enabled: &[usize], game_version: Option<&str>) -> HashMap<usize, Vec<Issue>> {
    let names = by_name(mods);
    let position: HashMap<usize, usize> = enabled.iter().enumerate().map(|(p, &i)| (i, p)).collect();
    let mut issues: HashMap<usize, Vec<Issue>> = HashMap::new();
    let mut add = |i: usize, severity, message: String, fixed_by_sorting| issues.entry(i).or_default().push(Issue { severity, message, fixed_by_sorting });

    for (p, &i) in enabled.iter().enumerate() {
        for dep in &mods[i].descriptor.dependencies {
            match names.get(&dep.trim().to_lowercase()) {
                // Mods also list optional compatibility targets here, so a
                // missing dependency isn't necessarily a problem.
                None => add(i, Severity::Info, format!("Lists “{dep}” as a dependency, but it isn't installed (it may be optional)"), false),
                Some(d) => match position.get(d) {
                    None => add(i, Severity::Info, format!("Lists “{dep}” as a dependency, but it's disabled (it may be optional)"), false),
                    Some(&dp) if dp > p => add(i, Severity::Warning, format!("Should load after “{dep}”"), true),
                    _ => {}
                },
            }
        }
    }
    if let Some(game) = game_version {
        for (i, m) in mods.iter().enumerate() {
            let supported = m.descriptor.game_version();
            if !supported.is_empty() && !version_matches(supported, game) {
                add(i, Severity::Info, format!("Made for Stellaris {supported}; you have {game}. It may still work."), false);
            }
        }
    }
    issues
}

/// `enabled` reordered so every mod comes after the enabled mods it
/// depends on, otherwise keeping the current order.
pub fn sort_by_dependencies(mods: &[Mod], enabled: &[usize]) -> Vec<usize> {
    let names = by_name(mods);
    let is_enabled: std::collections::HashSet<usize> = enabled.iter().copied().collect();
    let mut placed = Vec::with_capacity(enabled.len());
    let mut state: HashMap<usize, bool> = HashMap::new(); // false = visiting, true = placed

    fn visit(i: usize, mods: &[Mod], names: &HashMap<String, usize>, is_enabled: &std::collections::HashSet<usize>, state: &mut HashMap<usize, bool>, placed: &mut Vec<usize>) {
        if state.contains_key(&i) {
            return; // placed already, or a dependency cycle
        }
        state.insert(i, false);
        for dep in &mods[i].descriptor.dependencies {
            if let Some(&d) = names.get(&dep.trim().to_lowercase())
                && is_enabled.contains(&d)
            {
                visit(d, mods, names, is_enabled, state, placed);
            }
        }
        state.insert(i, true);
        placed.push(i);
    }
    for &i in enabled {
        visit(i, mods, &names, &is_enabled, &mut state, &mut placed);
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::{Descriptor, Source};

    fn m(name: &str, deps: &[&str], version: &str) -> Mod {
        Mod {
            id: format!("mod/{name}.mod"),
            descriptor: Descriptor { name: name.into(), dependencies: deps.iter().map(|s| s.to_string()).collect(), supported_version: version.into(), ..Default::default() },
            content: None,
            source: Source::Local,
            load_position: None,
            error: None,
            steam: None,
        }
    }

    #[test]
    fn matches_versions_like_the_launcher() {
        for ok in ["v4.5.*", "4.5.*", "v4.*", "v4.*.*", "*", "v4.5*", "v4.5.0", "v4.5.3", "4"] {
            assert!(version_matches(ok, "v4.5.3"), "{ok}");
        }
        for old in ["v4.4.*", "v4.3.*", "3.*.*", "4.2.*", "v4.4.6", "v4.1.*"] {
            assert!(!version_matches(old, "v4.5.3"), "{old}");
        }
    }

    #[test]
    fn finds_dependency_problems_and_sorts() {
        let mods = vec![m("Patch", &["UI Overhaul Dynamic", "Base"], "v4.5.*"), m("ui overhaul dynamic", &[], "v4.3.*"), m("Base", &["Missing Mod"], "*"), m("Off", &[], "v4.5.*"), m("Needs Off", &["off"], "")];
        let enabled = [0, 1, 2, 4];
        let issues = check(&mods, &enabled, Some("v4.5.3"));
        let messages = |i: usize| issues.get(&i).map(|v| v.iter().map(|x| x.message.as_str()).collect::<Vec<_>>()).unwrap_or_default();
        assert_eq!(messages(0), ["Should load after “UI Overhaul Dynamic”", "Should load after “Base”"]);
        assert_eq!(messages(2), ["Lists “Missing Mod” as a dependency, but it isn't installed (it may be optional)"]);
        assert_eq!(messages(4), ["Lists “off” as a dependency, but it's disabled (it may be optional)"]);
        assert_eq!(issues[&2][0].severity, Severity::Info);
        assert_eq!(issues[&1][0].severity, Severity::Info);
        assert!(issues[&0].iter().all(|i| i.fixed_by_sorting));

        let sorted = sort_by_dependencies(&mods, &enabled);
        assert_eq!(sorted, [1, 2, 0, 4]);
        assert!(check(&mods, &sorted, None).get(&0).is_none(), "sorting fixes the order issues");
    }

    #[test]
    fn sorting_survives_cycles() {
        let mods = vec![m("A", &["B"], ""), m("B", &["A"], "")];
        let mut sorted = sort_by_dependencies(&mods, &[0, 1]);
        sorted.sort();
        assert_eq!(sorted, [0, 1]);
    }
}
