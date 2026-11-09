mod conflicts_view;
mod error_report_dialog;
mod file_merge_dialog;
mod fix_dialog;
mod merge;
mod merge_dialog;
mod mods;
mod multiplayer;
mod multiplayer_dialog;
mod paradox;
mod sandbox;
mod secrets;
mod preferences;
mod settings;
mod thinking_view;
mod update;
mod window;

use adw::prelude::*;

use paradox::{game, steam};

pub const APP_ID: &str = "io.github.dukko.DrukhalManager";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    // Picks up the installed icon for the window.
    gtk::Window::set_default_icon_name(APP_ID);
    let home = gtk::glib::home_dir();
    // Folders chosen in Preferences replace the detected ones.
    let options = settings::Settings::load(&settings::path()).game;
    let user_dir = options.data_dir.clone().unwrap_or_else(steam::stellaris_user_dir);
    let libraries = steam::library_folders(&home);
    let workshop_dirs = steam::workshop_dirs(&home, steam::STELLARIS_APP_ID);
    let found = mods::discover(&user_dir, &workshop_dirs);
    let paths = window::Paths { user_dir, backup_dir: sandbox::data_dir().join("DrukhalManager/backups") };
    let game = match &options.executable {
        Some(exe) => game::from_executable(exe).ok(),
        None => game::find(&libraries),
    };
    window::build(app, paths, game, found);
}
