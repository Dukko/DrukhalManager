//! Finding the installed game and launching it directly, bypassing the
//! Paradox launcher so it can't overwrite the load order we wrote.
//!
//! The executable and arguments come from the game's `launcher-settings.json`,
//! as Irony Mod Manager's `GameService` does
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::steam::STELLARIS_APP_ID;

#[derive(Debug, Clone)]
pub struct GameInstall {
    pub dir: PathBuf,
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// e.g. `v4.5.3`
    pub version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LauncherSettings {
    exe_path: String,
    #[serde(default)]
    exe_args: Vec<String>,
    raw_version: Option<String>,
}

/// Looks for Stellaris in each Steam library.
pub fn find(libraries: &[PathBuf]) -> Option<GameInstall> {
    libraries.iter().map(|lib| lib.join("steamapps/common/Stellaris")).find_map(|dir| read_install(&dir).ok())
}

/// The game at a chosen executable. Its arguments and version still come
/// from `launcher-settings.json` beside it, when there is one.
pub fn from_executable(exe: &Path) -> io::Result<GameInstall> {
    if !exe.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found", exe.display())));
    }
    let dir = exe.parent().unwrap_or(Path::new("/")).to_path_buf();
    let settings = std::fs::read_to_string(dir.join("launcher-settings.json")).ok().and_then(|src| serde_json::from_str::<LauncherSettings>(&src).ok());
    Ok(GameInstall {
        exe: exe.to_path_buf(),
        args: settings.as_ref().map(|s| s.exe_args.clone()).unwrap_or_default(),
        version: settings.and_then(|s| s.raw_version),
        dir,
    })
}

fn read_install(dir: &Path) -> io::Result<GameInstall> {
    let src = std::fs::read_to_string(dir.join("launcher-settings.json"))?;
    let settings: LauncherSettings = serde_json::from_str(&src).map_err(io::Error::other)?;
    // Drops the `./` that `exePath` starts with.
    let exe: PathBuf = dir.join(&settings.exe_path).components().collect();
    if !exe.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found", exe.display())));
    }
    Ok(GameInstall { dir: dir.to_path_buf(), exe, args: settings.exe_args, version: settings.raw_version })
}

impl GameInstall {
    /// Starts the game and returns its process; wait on it to know when
    /// the game exits (and so it doesn't linger as a zombie). Steam must be
    /// running.
    /// `extra` arguments follow the launcher's.
    pub fn launch(&self, extra: &[String]) -> io::Result<std::process::Child> {
        Command::new(&self.exe)
            .args(&self.args)
            .args(extra)
            .current_dir(&self.dir)
            .env("SteamAppId", STELLARIS_APP_ID.to_string())
            .env("SteamGameId", STELLARIS_APP_ID.to_string())
            .spawn()
    }
}

/// The game's argument to continue the last save.
pub const CONTINUE: &str = "--continuelastsave";

/// The game launch options from Preferences as arguments, split the way
/// the platform's shell would.
pub fn parse_launch_args(text: &str) -> Result<Vec<String>, String> {
    #[cfg(not(windows))]
    {
        shlex::split(text).ok_or_else(|| "a quote isn't closed".to_owned())
    }
    #[cfg(windows)]
    {
        Ok(windows_args(text))
    }
}

/// Splits `text` the way Windows hands a command line to a program
/// (`CommandLineToArgvW`): whitespace separates arguments, quotes group
/// them, and backslashes are literal except before a quote.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut count = 1;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    count += 1;
                }
                if chars.peek() == Some(&'"') {
                    // 2n backslashes leave n and let the quote group;
                    // 2n+1 leave n and make a literal quote.
                    current.extend(std::iter::repeat_n('\\', count / 2));
                    if count % 2 == 1 {
                        current.push('"');
                        chars.next();
                    }
                } else {
                    current.extend(std::iter::repeat_n('\\', count));
                }
                started = true;
            }
            '"' => {
                started = true;
                if quoted && chars.peek() == Some(&'"') {
                    // Two quotes inside a quoted argument mean one.
                    current.push('"');
                    chars.next();
                } else {
                    quoted = !quoted;
                }
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_windows_paths_whole() {
        assert_eq!(windows_args(r"C:\dir\file -debug"), ["C:\\dir\\file", "-debug"]);
        assert_eq!(windows_args(r#""C:\Program Files\Steam" -x"#), [r"C:\Program Files\Steam", "-x"]);
        assert_eq!(windows_args(r#"pa\"th "" "a""b""#), ["pa\"th", "", "a\"b"]);
    }

    #[test]
    fn reads_launcher_settings() {
        let lib = std::env::temp_dir().join(format!("drukhal-game-{}", std::process::id()));
        let dir = lib.join("steamapps/common/Stellaris");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stellaris"), "").unwrap();
        std::fs::write(
            dir.join("launcher-settings.json"),
            r#"{ "gameId": "stellaris", "rawVersion": "v4.5.3", "exePath": "./stellaris", "exeArgs": [ "-gdpr-compliant" ]
                ,"alternativeExecutables": [] }"#,
        )
        .unwrap();

        let game = find(&[PathBuf::from("/nonexistent"), lib.clone()]).unwrap();
        assert_eq!(game.exe, dir.join("./stellaris"));
        assert_eq!(game.args, ["-gdpr-compliant"]);
        assert_eq!(game.version.as_deref(), Some("v4.5.3"));

        let chosen = from_executable(&dir.join("stellaris")).unwrap();
        assert_eq!((chosen.dir, chosen.args, chosen.version.as_deref()), (dir.clone(), vec!["-gdpr-compliant".to_owned()], Some("v4.5.3")));
        assert!(from_executable(&dir.join("missing")).is_err());

        std::fs::remove_dir_all(lib).unwrap();
    }
}
