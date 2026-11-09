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
}
