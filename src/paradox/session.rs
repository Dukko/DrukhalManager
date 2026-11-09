//! Following a game session from its files, for when the game can't be
//! waited on (the Flatpak starts it through Steam, outside the sandbox).
//!
//! Stellaris rewrites its logs as it starts. It writes `settings.txt` as it
//! quits, but not as it starts, and leaves a folder in `crashes` when it
//! crashes.

use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Quit,
    Crashed,
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Whether a session started at or after `since`.
pub fn started(user_dir: &Path, since: SystemTime) -> bool {
    modified(&user_dir.join("logs/system.log")).is_some_and(|t| t >= since)
}

/// How the session that started at or after `since` ended, if it has.
pub fn ended(user_dir: &Path, since: SystemTime) -> Option<Ended> {
    let crashed = std::fs::read_dir(user_dir.join("crashes"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= since));
    if crashed {
        return Some(Ended::Crashed);
    }
    modified(&user_dir.join("settings.txt")).is_some_and(|t| t >= since).then_some(Ended::Quit)
}

/// The save the game continues with `--continuelastsave`, from
/// `continue_game.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastSave {
    /// The empire's name.
    pub empire: String,
    /// The in-game date, e.g. `2370.07.01`.
    pub date: String,
}

/// The save the game would continue, if it's still there.
pub fn last_save(user_dir: &Path) -> Option<LastSave> {
    let src = std::fs::read_to_string(user_dir.join("continue_game.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&src).ok()?;
    let field = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty());
    // "save games/<empire>_<id>/<name>", without the extension.
    let save = field("filename").or_else(|| field("title"))?;
    let path = user_dir.join(save);
    // Not `with_extension`: the name's date has dots in it.
    if !path.is_file() && !user_dir.join(format!("{save}.sav")).is_file() {
        return None;
    }
    Some(LastSave { empire: field("desc").unwrap_or("your empire").to_owned(), date: field("date").unwrap_or_default().to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn follows_a_session() {
        let dir = std::env::temp_dir().join(format!("drukhal-session-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("crashes")).unwrap();
        std::fs::write(dir.join("settings.txt"), "").unwrap();
        std::fs::write(dir.join("logs/system.log"), "").unwrap();
        std::fs::create_dir(dir.join("crashes/stellaris_old")).unwrap();
        // Everything above is from an earlier session.
        let since = SystemTime::now() + Duration::from_millis(50);
        assert!(!started(&dir, since));
        assert_eq!(ended(&dir, since), None);

        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.join("logs/system.log"), "starting").unwrap();
        assert!(started(&dir, since));
        assert_eq!(ended(&dir, since), None, "still running");

        std::fs::write(dir.join("settings.txt"), "quit").unwrap();
        assert_eq!(ended(&dir, since), Some(Ended::Quit));
        std::fs::create_dir(dir.join("crashes/stellaris_new")).unwrap();
        assert_eq!(ended(&dir, since), Some(Ended::Crashed));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_the_save_to_continue() {
        let dir = std::env::temp_dir().join(format!("drukhal-continue-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("save games/omnian_1")).unwrap();
        assert_eq!(super::last_save(&dir), None);
        std::fs::write(
            dir.join("continue_game.json"),
            "{\n\t\"title\":\t\"save games/omnian_1/autosave_2370.07.01\",\n\t\"desc\":\t\"Omnian Animus\",\n\t\"date\":\t\"2370.07.01\"\n}\n",
        )
        .unwrap();
        assert_eq!(super::last_save(&dir), None, "the save was deleted");
        std::fs::write(dir.join("save games/omnian_1/autosave_2370.07.01.sav"), "").unwrap();
        assert_eq!(super::last_save(&dir), Some(super::LastSave { empire: "Omnian Animus".into(), date: "2370.07.01".into() }));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
