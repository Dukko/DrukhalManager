//! The host's GTK 4 stylesheet, in the Flatpak.

use crate::platform::is_flatpak;

/// Applies the host's GTK 4 stylesheet (`~/.config/gtk-4.0/gtk.css`), as
/// GTK would outside the sandbox. In the Flatpak, GTK looks for it in the
/// app's private config folder instead, so custom libadwaita colour schemes
/// (DankMaterialShell, matugen, Gradience…) wouldn't apply. It's reloaded
/// when the folder changes, as those tools rewrite it when the palette does.
pub fn load_host_stylesheet() {
    if !is_flatpak() {
        return;
    }
    let dir = crate::platform::host_config_dir().join("gtk-4.0");
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
