//! The About dialog, and finding and installing new versions.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::preferences::UPDATES_PAGE;
use crate::update::{self, Progress};

type Action = fn(&Rc<Window>);

impl Window {
    pub(super) fn add_update_actions(self: &Rc<Self>) {
        let actions: [(&str, Action); 6] = [
            ("about", Self::show_about),
            ("copy-diagnostics", Self::copy_diagnostics),
            ("open-log-folder", Self::open_log_folder),
            ("install-update", Self::install_update),
            ("restart", Self::restart),
            ("show-update", |ui| ui.open_preferences(Some(UPDATES_PAGE))),
        ];
        for (name, f) in actions {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    f(&ui);
                }
            });
            self.window.add_action(&action);
        }
    }

    fn show_about(self: &Rc<Self>) {
        let about = adw::AboutDialog::builder()
            .application_name("DrukhalManager")
            .application_icon(crate::APP_ID)
            .version(update::CURRENT)
            .comments("Manage Stellaris mods and merge their conflicts")
            .developer_name("Dukko")
            .license_type(gtk::License::MitX11)
            .website("https://github.com/Dukko/DrukhalManager")
            .issue_url("https://github.com/Dukko/DrukhalManager/issues")
            .debug_info(self.diagnostics())
            .debug_info_filename("DrukhalManager diagnostics.txt")
            .build();
        about.add_acknowledgement_section(Some("A Fork Of"), &["Irony Mod Manager by Mario (bcssov) https://github.com/bcssov/IronyModManager"]);
        about.add_legal_section("Irony Mod Manager", Some("© 2020 Mario"), gtk::License::MitX11, None);
        about.present(Some(&self.window));
    }

    /// What to attach to a bug report: versions, the system, and the end of
    /// the log. No keys, prompts or file contents.
    fn diagnostics(&self) -> String {
        let state = self.state.borrow();
        let provider = match &self.settings.borrow().provider {
            crate::merge::ai::Provider::Anthropic { model, .. } => format!("Claude, {model}"),
            crate::merge::ai::Provider::OpenAiCompatible { model, .. } => format!("OpenAI-compatible, {model}"),
        };
        let env = |name: &str| std::env::var(name).unwrap_or_else(|_| "unknown".to_owned());
        let facts = [
            ("DrukhalManager", format!("{} ({})", update::CURRENT, if crate::sandbox::is_flatpak() { "Flatpak" } else { "native" })),
            ("Stellaris", self.game.as_ref().and_then(|g| g.version.clone()).unwrap_or_else(|| "not found".to_owned())),
            ("System", crate::diagnostics::distro()),
            ("Desktop", format!("{} ({})", env("XDG_CURRENT_DESKTOP"), env("XDG_SESSION_TYPE"))),
            ("GTK", format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version())),
            ("libadwaita", format!("{}.{}.{}", adw::major_version(), adw::minor_version(), adw::micro_version())),
            ("AI provider", provider),
            ("Mods", format!("{} installed, {} enabled", state.mods.len(), state.enabled.len())),
            ("Collection", self.collections.borrow().active.clone()),
        ];
        crate::diagnostics::report(&facts, 200)
    }

    fn copy_diagnostics(self: &Rc<Self>) {
        self.window.clipboard().set_text(&self.diagnostics());
        self.add_toast(adw::Toast::new("Copied the diagnostics; paste them into your bug report"));
    }

    fn open_log_folder(self: &Rc<Self>) {
        let dir = crate::diagnostics::dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.add_toast(adw::Toast::builder().title(format!("Could not open the log folder: {e}")).timeout(0).build());
        }
        gtk::FileLauncher::new(Some(&gio::File::for_path(dir))).launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
    }

    /// At startup, if enabled: tells the user about a newer release they
    /// haven't chosen to ignore.
    pub(super) fn check_for_updates_at_startup(self: &Rc<Self>) {
        let options = self.settings.borrow().updates.clone();
        if !options.check_at_startup {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let prereleases = options.prereleases;
            let found = gio::spawn_blocking(move || update::latest(prereleases)).await;
            match &found {
                Ok(Ok(latest)) => crate::diagnostics::log!("update", "Checked for updates: the latest is {}", latest.version),
                Ok(Err(e)) => crate::diagnostics::log!("update", "Could not check for updates: {e}"),
                Err(_) => crate::diagnostics::log!("update", "Could not check for updates: the check crashed"),
            }
            if let Ok(Ok(latest)) = found
                && update::is_newer(&latest.version, update::CURRENT)
                && options.skipped_version.as_deref() != Some(latest.version.as_str())
                && let Some(ui) = weak.upgrade()
            {
                ui.announce_update(Some(&latest));
            }
            // While it runs, Flatpak also reports versions published later.
            if crate::sandbox::is_flatpak()
                && let Some(ui) = weak.upgrade()
                && let Some(monitor) = ui.monitor().await
            {
                let weak = Rc::downgrade(&ui);
                monitor.on_available(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.announce_update(None);
                    }
                });
            }
        });
    }

    fn announce_update(&self, release: Option<&update::Release>) {
        if self.update_announced.replace(true) {
            return;
        }
        let title = match release {
            Some(r) if r.prerelease => format!("Test version {} of DrukhalManager is available", r.version),
            Some(r) => format!("DrukhalManager {} is available", r.version),
            None => "A new version of DrukhalManager is available".to_owned(),
        };
        // Test versions aren't in the Flatpak repository; Preferences says
        // how to get one.
        let installable = crate::sandbox::is_flatpak() && !release.is_some_and(|r| r.prerelease);
        let (label, action) = if installable { ("Update", "win.install-update") } else { ("Details", "win.show-update") };
        self.add_toast(adw::Toast::builder().title(title).button_label(label).action_name(action).timeout(0).build());
    }

    /// Flatpak's update monitor, created on first use.
    async fn monitor(self: &Rc<Self>) -> Option<Rc<update::Monitor>> {
        if let Some(m) = self.update_monitor.borrow().clone() {
            return Some(m);
        }
        let monitor = match update::Monitor::new().await {
            Ok(m) => Rc::new(m),
            Err(e) => {
                self.add_toast(adw::Toast::builder().title(format!("Could not reach Flatpak to update: {}", e.message())).timeout(0).build());
                return None;
            }
        };
        let weak = Rc::downgrade(self);
        monitor.on_progress(move |progress| {
            if let Some(ui) = weak.upgrade() {
                ui.show_update_progress(progress);
            }
        });
        self.update_monitor.replace(Some(monitor.clone()));
        Some(monitor)
    }

    fn install_update(self: &Rc<Self>) {
        if !crate::sandbox::is_flatpak() {
            gtk::UriLauncher::new(update::RELEASES_URL).launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
            return;
        }
        // Progress shows as toasts on the window, which Preferences would
        // cover.
        if let Some(dialog) = self.window.visible_dialog() {
            dialog.close();
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let Some(monitor) = ui.monitor().await else { return };
            ui.show_update_progress(Progress::Running(0));
            if let Err(e) = monitor.update().await {
                // Flatpak asks once whether the app may update itself; on
                // desktops without a portal dialog for that (niri, sway…) the
                // question can't be shown.
                let message = if e.matches(gio::DBusError::ServiceUnknown) || e.message().contains("ServiceUnknown") {
                    "Flatpak could not ask whether DrukhalManager may update itself. Run “flatpak update io.github.dukko.DrukhalManager” instead".to_owned()
                } else {
                    e.message().to_owned()
                };
                ui.show_update_progress(Progress::Failed(message));
            }
        });
    }

    fn show_update_progress(&self, progress: Progress) {
        // Bound first: the borrow must end before a new toast is stored.
        let current = self.update_toast.borrow().clone();
        let toast = current.unwrap_or_else(|| {
            let toast = adw::Toast::builder().timeout(0).build();
            self.update_toast.replace(Some(toast.clone()));
            self.show_progress(&toast);
            toast
        });
        match progress {
            Progress::Running(percent) => {
                toast.set_title(&format!("Updating DrukhalManager… {percent}%"));
                return;
            }
            Progress::Done => {
                toast.set_title("DrukhalManager was updated. Restart to use the new version.");
                toast.set_button_label(Some("Restart"));
                toast.set_action_name(Some("win.restart"));
            }
            Progress::NothingToDo => {
                toast.set_title("No update to install yet. A new release can take a few minutes to reach Flatpak.");
                toast.set_timeout(8);
            }
            Progress::Failed(message) => toast.set_title(&failure(&message)),
        }
        // Finished: the next update starts a new toast. Re-adding shows the
        // finished state even if the user dismissed the progress toast.
        self.update_toast.replace(None);
        self.add_toast(toast);
    }

    /// Starts the updated version and quits.
    fn restart(self: &Rc<Self>) {
        if self.state.borrow().dirty && !self.save() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = update::relaunch().await;
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(()) => {
                    if let Some(app) = ui.window.application() {
                        app.quit();
                    }
                }
                Err(e) => ui.add_toast(
                    adw::Toast::builder()
                        .title(format!("Could not restart: {}. Close DrukhalManager and open it again.", e.message()))
                        .timeout(0)
                        .build(),
                ),
            }
        });
    }
}

/// What to say when an update fails. Flatpak won't let an app update itself
/// into a version that asks for more permissions; only the user can approve
/// those, from a terminal or a software centre.
fn failure(message: &str) -> String {
    if message.to_lowercase().contains("permission") {
        return "This update needs a new permission, which only you can approve. Run “flatpak update io.github.dukko.DrukhalManager” in a terminal, or update from your software centre".to_owned();
    }
    format!("Could not update: {message}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn explains_updates_that_need_new_permissions() {
        assert!(super::failure("New version requires new permissions").contains("flatpak update io.github.dukko.DrukhalManager"));
        assert_eq!(super::failure("no space"), "Could not update: no space");
    }
}
