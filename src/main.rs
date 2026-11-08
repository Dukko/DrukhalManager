mod merge;
mod mods;
mod paradox;
mod window;

use adw::prelude::*;

use paradox::{game, steam};

const APP_ID: &str = "io.github.dukkokun.DrukhalManager";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    let home = gtk::glib::home_dir();
    let user_dir = steam::stellaris_user_dir(&home);
    let libraries = steam::library_folders(&home);
    let workshop_dirs = steam::workshop_dirs(&home, steam::STELLARIS_APP_ID);
    let found = mods::discover(&user_dir, &workshop_dirs);
    let paths = window::Paths { user_dir, backup_dir: gtk::glib::user_data_dir().join("DrukhalManager/backups") };
    window::build(app, paths, game::find(&libraries), found);
}
