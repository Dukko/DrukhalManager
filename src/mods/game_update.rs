//! After a game update: which enabled mods don't support the new version
//! yet, and whether their authors have updated them since.

use std::time::SystemTime;

use super::Mod;
use super::checks::version_matches;

/// A game version's major and minor parts, which are what mods declare
/// support for: `v4.1.3` -> `4.1`.
pub fn major_minor(version: &str) -> String {
    version.trim().trim_start_matches(['v', 'V']).split('.').take(2).collect::<Vec<_>>().join(".")
}

/// Whether going from `last` to `now` is an update mods have to catch up
/// with: a new major or minor version. Unknown before means no.
pub fn is_update(last: Option<&str>, now: &str) -> bool {
    last.is_some_and(|last| major_minor(last) != major_minor(now))
}

/// An enabled mod whose `supported_version` doesn't cover the game.
#[derive(Debug, PartialEq, Eq)]
pub struct Outdated {
    pub index: usize,
    pub supported: String,
    /// Its files changed after the game did: the author may have updated
    /// it without changing `supported_version`, or it may need a reload.
    pub updated_since: bool,
}

/// The enabled mods that don't declare support for `game`, in load order.
/// `game_updated` is when the game's files last changed.
pub fn outdated(mods: &[Mod], enabled: &[usize], game: &str, game_updated: Option<SystemTime>) -> Vec<Outdated> {
    enabled
        .iter()
        .filter_map(|&i| {
            let supported = mods[i].descriptor.game_version();
            // Mods that don't say aren't counted, like the launcher does.
            (!supported.is_empty() && !version_matches(supported, game)).then(|| Outdated {
                index: i,
                supported: supported.to_owned(),
                updated_since: game_updated.zip(mods[i].updated()).is_some_and(|(game, m)| m > game),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::{Descriptor, Source};

    #[test]
    fn notices_updates_by_major_and_minor() {
        assert!(is_update(Some("v4.0.23"), "v4.1.0"));
        assert!(!is_update(Some("v4.1.0"), "v4.1.2"));
        assert!(!is_update(None, "v4.1.0"));
        assert_eq!(major_minor("v3.14.15926"), "3.14");
    }

    #[test]
    fn lists_enabled_mods_that_dont_support_the_game() {
        let m = |name: &str, version: &str| Mod { id: format!("mod/{name}.mod"), descriptor: Descriptor { name: name.into(), supported_version: version.into(), ..Default::default() }, content: None, source: Source::Local, load_position: None, error: None, steam: None, requires: Vec::new() };
        let mods = vec![m("Old", "v4.0.*"), m("New", "v4.1.*"), m("Any", "*"), m("Unsaid", ""), m("Off", "v3.*")];
        let found = outdated(&mods, &[0, 1, 2, 3], "v4.1.2", None);
        assert_eq!(found, [Outdated { index: 0, supported: "v4.0.*".into(), updated_since: false }]);
    }
}
