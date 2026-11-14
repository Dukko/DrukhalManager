// Release builds on Windows open without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod conflicts_view;
mod dds;
mod diagnostics;
mod diff;
mod diff_view;
mod error_report_dialog;
mod file_merge_dialog;
mod fix_dialog;
mod folder_merge_dialog;
#[cfg(unix)]
mod host_stylesheet;
#[macro_use]
mod i18n;
mod merge;
mod merge_dialog;
mod mods;
mod multiplayer;
mod multiplayer_dialog;
mod paradox;
mod platform;
mod secrets;
mod preferences;
mod resize_grip;
mod settings;
mod thinking_view;
mod update;
mod window;

use adw::prelude::*;

use paradox::{game, steam};

pub const APP_ID: &str = "io.github.dukko.DrukhalManager";

fn main() -> gtk::glib::ExitCode {
    // Before any UI, so the first string the app shows is translated too.
    i18n::init();
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    // Picks up the installed icon for the window.
    gtk::Window::set_default_icon_name(APP_ID);
    // The app's own icons, when run from the source tree; the Flatpak
    // installs them with the theme's.
    let icons = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    if icons.is_dir()
        && let Some(display) = gtk::gdk::Display::default()
    {
        gtk::IconTheme::for_display(&display).add_search_path(icons);
    }
    diagnostics::log!("app", "DrukhalManager {} started ({})", update::CURRENT, if platform::is_flatpak() { "Flatpak" } else { "native" });
    let home = platform::home_dir();
    // Folders chosen in Preferences replace the detected ones.
    let options = settings::Settings::load(&settings::path()).game;
    let user_dir = options.data_dir.clone().unwrap_or_else(steam::stellaris_user_dir);
    let libraries = steam::library_folders(&home);
    let workshop_dirs = steam::workshop_dirs(&home, steam::STELLARIS_APP_ID);
    // Mods subscribed or updated since the Paradox Launcher last ran.
    for error in mods::sync::sync(&user_dir, &workshop_dirs).errors {
        diagnostics::log!("mods", "{error}");
    }
    for error in mods::extra::link(&user_dir, options.extra_mod_dir.as_deref()).errors {
        diagnostics::log!("mods", "{error}");
    }
    let found = mods::discover(&user_dir, &workshop_dirs);
    let paths = window::Paths { user_dir, backup_dir: platform::data_dir().join("backups") };
    let game = match &options.executable {
        Some(exe) => game::from_executable(exe).ok(),
        None => game::find(&libraries),
    };
    match &found {
        Ok(mods) => diagnostics::log!("app", "{} mods found; game {}", mods.len(), game.as_ref().and_then(|g| g.version.as_deref()).unwrap_or("not found")),
        Err(e) => diagnostics::log!("app", "Could not read the mods: {e}"),
    }
    window::build(app, paths, game, found);
}
