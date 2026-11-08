//! Running inside a Flatpak sandbox. There, the usual XDG directories point
//! into the app's private folder, so the game's data, the Steam libraries
//! and this app's own settings (shared with a native install) are found
//! through the host's directories instead, which Flatpak passes in as
//! `HOST_XDG_*` variables.

use std::path::{Path, PathBuf};

pub fn is_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

fn host_dir(host_var: &str, var: &str, fallback: &str) -> PathBuf {
    let from = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let preferred = if is_flatpak() { from(host_var) } else { from(var) };
    preferred.unwrap_or_else(|| gtk::glib::home_dir().join(fallback))
}

/// `~/.local/share` on the host.
pub fn data_dir() -> PathBuf {
    host_dir("HOST_XDG_DATA_HOME", "XDG_DATA_HOME", ".local/share")
}

/// `~/.config` on the host.
pub fn config_dir() -> PathBuf {
    host_dir("HOST_XDG_CONFIG_HOME", "XDG_CONFIG_HOME", ".config")
}
