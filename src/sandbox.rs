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

/// Applies the host's GTK 4 stylesheet (`~/.config/gtk-4.0/gtk.css`), as
/// GTK would outside the sandbox. In the Flatpak, GTK looks for it in the
/// app's private config folder instead, so custom libadwaita colour schemes
/// (DankMaterialShell, matugen, Gradience…) wouldn't apply. It's reloaded
/// when the folder changes, as those tools rewrite it when the palette does.
pub fn load_host_stylesheet() {
    if !is_flatpak() {
        return;
    }
    let dir = config_dir().join("gtk-4.0");
    let css = dir.join("gtk.css");
    // Nothing to do when the app's own folder has one, or the host has none.
    if gtk::glib::user_config_dir().join("gtk-4.0/gtk.css").exists() || !css.exists() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    let file = gtk::gio::File::for_path(&css);
    provider.load_from_file(&file);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER);

    use gtk::gio::prelude::*;
    let Ok(monitor) = gtk::gio::File::for_path(&dir).monitor_directory(gtk::gio::FileMonitorFlags::NONE, gtk::gio::Cancellable::NONE) else { return };
    let pending: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> = Default::default();
    monitor.connect_changed(move |_, _, _, _| {
        // A palette change rewrites several files at once; reload once.
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
        }
        let (provider, file, pending_ref) = (provider.clone(), file.clone(), pending.clone());
        *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            pending_ref.borrow_mut().take();
            provider.load_from_file(&file);
        }));
    });
    // Lives as long as the app.
    std::mem::forget(monitor);
}
