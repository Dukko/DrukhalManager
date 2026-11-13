//! Where things live on this system, and the few things done differently
//! per platform. Code outside the UI goes through here rather than through
//! glib, so it stays free of GTK.
//!
//! On Linux, inside a Flatpak sandbox the usual XDG directories point into
//! the app's private folder, so the game's data, the Steam libraries and
//! this app's own settings (shared with a native install) are found through
//! the host's directories instead, which Flatpak passes in as `HOST_XDG_*`
//! variables. Caches and the app's private data stay in the sandbox.
//!
//! On Windows, settings roam with the user (`%APPDATA%`); everything else
//! stays on this machine (`%LOCALAPPDATA%`).

use std::path::{Path, PathBuf};

const APP_DIR: &str = "DrukhalManager";

pub fn is_flatpak() -> bool {
    cfg!(target_os = "linux") && Path::new("/.flatpak-info").exists()
}

/// The user's home folder.
pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

#[cfg(unix)]
fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home_dir().join(fallback))
}

/// An XDG directory as seen outside the sandbox.
#[cfg(unix)]
fn host_xdg_dir(host_var: &str, var: &str, fallback: &str) -> PathBuf {
    if is_flatpak() { xdg_dir(host_var, fallback) } else { xdg_dir(var, fallback) }
}

#[cfg(windows)]
fn local_app_data() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| home_dir().join("AppData/Local"))
}

/// `~/.local/share` on the host: where the Paradox games keep their user
/// data on Linux.
#[cfg(unix)]
pub fn host_data_dir() -> PathBuf {
    host_xdg_dir("HOST_XDG_DATA_HOME", "XDG_DATA_HOME", ".local/share")
}

/// `~/.config` on the host.
#[cfg(unix)]
pub fn host_config_dir() -> PathBuf {
    host_xdg_dir("HOST_XDG_CONFIG_HOME", "XDG_CONFIG_HOME", ".config")
}

/// The app's settings folder, shared between a Flatpak and a native install.
pub fn config_dir() -> PathBuf {
    #[cfg(unix)]
    let base = host_config_dir();
    #[cfg(windows)]
    let base = dirs::config_dir().unwrap_or_else(|| home_dir().join("AppData/Roaming"));
    base.join(APP_DIR)
}

/// The app's data the user would want kept with a native install too:
/// backups, logs, collections.
pub fn data_dir() -> PathBuf {
    #[cfg(unix)]
    let base = host_data_dir();
    #[cfg(windows)]
    let base = local_app_data();
    base.join(APP_DIR)
}

/// The app's private data: inside the sandbox when there is one.
pub fn private_data_dir() -> PathBuf {
    #[cfg(unix)]
    let base = xdg_dir("XDG_DATA_HOME", ".local/share");
    #[cfg(windows)]
    let base = local_app_data();
    base.join(APP_DIR)
}

/// Caches that can be rebuilt: inside the sandbox when there is one.
pub fn cache_dir() -> PathBuf {
    #[cfg(unix)]
    return xdg_dir("XDG_CACHE_HOME", ".cache").join(APP_DIR);
    #[cfg(windows)]
    return local_app_data().join(APP_DIR).join("cache");
}

/// The Paradox games' user data folder: `~/.local/share/Paradox Interactive`
/// on Linux, `Documents\Paradox Interactive` on Windows (wherever Documents
/// was moved to, OneDrive included).
pub fn paradox_dir() -> PathBuf {
    #[cfg(unix)]
    let base = host_data_dir();
    #[cfg(windows)]
    let base = dirs::document_dir().unwrap_or_else(|| home_dir().join("Documents"));
    base.join("Paradox Interactive")
}

/// A folder as a descriptor's `path=` (or `archive=`) holds it. On Windows
/// that's with `/` separators, as the Paradox launcher writes them.
pub fn descriptor_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) { text.replace('\\', "/") } else { text.into_owned() }
}

/// A path relative to a mod's or the game's folder as the game names
/// files, and as conflicts, patches and reports key them: `/`-separated on
/// every platform.
pub fn game_path(relative: &Path) -> String {
    relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// `std::fs::rename`, except that on Windows a rename refused because
/// something briefly has the file open is tried again for a moment. That
/// happens when an antivirus or the search indexer opens a file that was just
/// written.
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> std::io::Result<()> {
    let (from, to) = (from.as_ref(), to.as_ref());
    #[cfg(windows)]
    {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        for _ in 0..10 {
            match std::fs::rename(from, to) {
                Err(e) if matches!(e.raw_os_error(), Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                result => return result,
            }
        }
    }
    std::fs::rename(from, to)
}

/// Where `program` is installed, found through `PATH`.
pub fn find_program(program: &str) -> Option<PathBuf> {
    let candidate = |dir: &Path| -> Option<PathBuf> {
        let path = dir.join(program);
        if path.is_file() {
            return Some(path);
        }
        // `code` is `code.cmd`, `meld` is `meld.exe`…
        #[cfg(windows)]
        for ext in std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into()).split(';').filter(|e| !e.is_empty()) {
            let path = dir.join(format!("{program}{ext}"));
            if path.is_file() {
                return Some(path);
            }
        }
        None
    };
    if program.contains(['/', std::path::MAIN_SEPARATOR]) {
        return candidate(Path::new(""));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| candidate(&dir))
}

/// The operating system's name, for diagnostics: the distribution from
/// os-release (the host's, in the Flatpak), or the Windows version.
pub fn os_name() -> String {
    #[cfg(unix)]
    {
        let files: &[&Path] = if is_flatpak() { &[Path::new("/run/host/os-release"), Path::new("/run/host/usr/lib/os-release")] } else { &[Path::new("/etc/os-release"), Path::new("/usr/lib/os-release")] };
        files
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .find_map(|text| text.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_owned()))
            .unwrap_or_else(|| "unknown".to_owned())
    }
    #[cfg(windows)]
    {
        let Ok(key) = windows_registry::LOCAL_MACHINE.open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else { return "Windows".to_owned() };
        // `ProductName` still says Windows 10 on Windows 11; the build tells.
        let build = key.get_string("CurrentBuild").unwrap_or_default();
        let name = if build.parse::<u32>().is_ok_and(|b| b >= 22000) { "Windows 11" } else { "Windows 10" };
        match key.get_string("DisplayVersion") {
            Ok(version) => format!("{name} {version} (build {build})"),
            Err(_) => format!("{name} (build {build})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_folders_are_named_for_the_app() {
        for dir in [config_dir(), data_dir(), private_data_dir()] {
            assert!(dir.ends_with(APP_DIR), "{}", dir.display());
        }
        assert!(cache_dir().components().any(|c| c.as_os_str() == APP_DIR));
        assert!(paradox_dir().ends_with("Paradox Interactive"));
    }

    #[test]
    fn finds_programs_on_the_path() {
        #[cfg(unix)]
        assert!(find_program("sh").is_some());
        #[cfg(windows)]
        assert!(find_program("cmd").is_some());
        assert!(find_program("drukhal-no-such-program").is_none());
    }
}
