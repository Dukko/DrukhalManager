//! Launching the game, its error report, and window-level dialogs.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use super::display_name;
use crate::conflicts_view;
use crate::error_report_dialog;
use crate::merge::patch;
use crate::paradox::error_log;

impl Window {
    /// In a Flatpak, Steam installed outside the home folder (reached through
    /// a link) is invisible, so the game can't be found. Explains the
    /// one-time permission that fixes it.
    pub(super) fn explain_sandbox_access(&self) {
        let missing = crate::paradox::steam::unreachable_roots(&gtk::glib::home_dir());
        if missing.is_empty() {
            return;
        }
        let app_id = self
            .window
            .application()
            .and_then(|a| a.application_id())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let paths: Vec<String> = missing
            .iter()
            .map(|p| format!("--filesystem={}", p.display()))
            .collect();
        let command = format!("flatpak override --user {} {app_id}", paths.join(" "));
        let dialog = adw::AlertDialog::new(
            Some("Allow Access to Steam"),
            Some(&format!(
                "Steam is installed at {}, outside your home folder, so DrukhalManager can't see it from its sandbox and can't find Stellaris. Run this once in a terminal, then restart DrukhalManager:",
                missing
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )),
        );
        let label = gtk::Label::builder()
            .label(&command)
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::Char)
            .css_classes(["monospace", "card"])
            .margin_top(6)
            .build();
        dialog.set_extra_child(Some(&label));
        dialog.add_responses(&[("close", "_Close"), ("copy", "_Copy Command")]);
        dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("copy"));
        dialog.set_close_response("close");
        dialog.connect_response(Some("copy"), move |d, _| d.clipboard().set_text(&command));
        dialog.present(Some(&self.window));
    }

    pub(super) fn open_folder(&self, index: usize) {
        let Some(dir) = self
            .state
            .borrow()
            .mods
            .get(index)
            .and_then(|m| m.content.clone())
        else {
            return;
        };
        gtk::FileLauncher::new(Some(&gio::File::for_path(dir))).launch(
            Some(&self.window),
            None::<&gio::Cancellable>,
            |_| {},
        );
    }

    pub(super) fn open_workshop(&self, index: usize) {
        let Some(id) = self
            .state
            .borrow()
            .mods
            .get(index)
            .and_then(|m| m.descriptor.remote_file_id)
        else {
            return;
        };
        gtk::UriLauncher::new(&format!(
            "https://steamcommunity.com/sharedfiles/filedetails/?id={id}"
        ))
        .launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
    }

    pub(super) fn launch(self: &Rc<Self>) {
        let Some(game) = &self.game else { return };
        let options = self.settings.borrow().game.clone();
        let extra = if options.launch_args.is_empty() {
            Vec::new()
        } else {
            match glib::shell_parse_argv(&options.launch_args) {
                Ok(args) => args.into_iter().map(|a| a.to_string_lossy().into_owned()).collect(),
                Err(e) => {
                    return self.toasts.add_toast(
                        adw::Toast::builder()
                            .title(format!("Check the game launch options in Preferences: {}", e.message()))
                            .timeout(0)
                            .build(),
                    );
                }
            }
        };
        if options.refresh_descriptors {
            let stale = crate::mods::repair::find(&self.paths.user_dir, &self.state.borrow().mods).stale;
            if !stale.is_empty() {
                self.repair(&stale, &[]);
            }
        }
        if self.state.borrow().dirty && !self.save() {
            return;
        }
        if crate::sandbox::is_flatpak() {
            // A sandboxed app can't start the game itself; Steam can, and
            // the launcher playset carries the load order.
            gtk::UriLauncher::new(&format!(
                "steam://rungameid/{}",
                crate::paradox::steam::STELLARIS_APP_ID
            ))
            .launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
            self.toasts
                .add_toast(adw::Toast::new("Starting Stellaris through Steam…"));
            if options.close_after_launch {
                self.window.close();
            }
            return;
        }
        let started = std::time::SystemTime::now();
        let mut child = match game.launch(&extra) {
            Ok(child) => child,
            Err(e) => {
                return self.toasts.add_toast(
                    adw::Toast::builder()
                        .title(format!("Could not start Stellaris: {e}"))
                        .timeout(0)
                        .build(),
                );
            }
        };
        if options.close_after_launch {
            // The game keeps running on its own.
            self.window.close();
            return;
        }
        self.toasts
            .add_toast(adw::Toast::new("Starting Stellaris…"));
        // When the game exits, point out errors it logged about the mods.
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || child.wait()).await;
            let Some(ui) = weak.upgrade() else { return };
            let log = error_log::path(&ui.paths.user_dir);
            let fresh = std::fs::metadata(&log)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= started);
            if !fresh {
                return;
            }
            let report = ui.error_report();
            let from_mods = report.from_mods();
            if from_mods == 0 {
                return;
            }
            let patch = report
                .groups
                .iter()
                .find(|g| g.is_patch)
                .map_or(0, |g| g.total());
            let mut title = format!(
                "Stellaris logged {} from your mods",
                conflicts_view::plural(from_mods, "error", "errors")
            );
            if patch > 0 {
                title.push_str(&format!(", {patch} from the merged patch"));
            }
            let toast = adw::Toast::builder()
                .title(title)
                .button_label("Show")
                .action_name("win.errors")
                .timeout(0)
                .build();
            ui.toasts.add_toast(toast);
        });
    }

    /// The last session's error.log, credited to the enabled mods.
    pub(super) fn error_report(&self) -> error_log::Report {
        let src = std::fs::read(error_log::path(&self.paths.user_dir))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let state = self.state.borrow();
        let mods: Vec<error_log::LogMod> = state
            .enabled
            .iter()
            .map(|&i| {
                let m = &state.mods[i];
                error_log::LogMod {
                    id: m.id.clone(),
                    name: display_name(m).to_owned(),
                    root: m.content.clone(),
                    steam_id: m.descriptor.remote_file_id,
                    is_patch: m.id == patch::ID,
                }
            })
            .collect();
        error_log::analyze(&src, &mods)
    }

    pub(super) fn show_errors(self: &Rc<Self>) {
        let log = error_log::path(&self.paths.user_dir);
        if !log.is_file() {
            return self.toasts.add_toast(adw::Toast::new(
                "No error.log yet; it's written when the game runs",
            ));
        }
        let modified = std::fs::metadata(&log)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| {
                glib::DateTime::from_unix_local(
                    t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64,
                )
                .ok()
            })
            .and_then(|d| d.format("%A %H:%M").ok())
            .map(|s| s.to_string());
        let roots = {
            let state = self.state.borrow();
            state
                .mods
                .iter()
                .filter_map(|m| Some((m.id.clone(), m.content.clone()?)))
                .collect()
        };
        error_report_dialog::present(&self.window, &self.error_report(), log, modified, roots);
    }

    pub(super) fn confirm_close(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some("Save Changes?"),
            Some("The load order has unsaved changes. Unsaved changes will be lost if you close."),
        );
        dialog.add_responses(&[
            ("cancel", "_Cancel"),
            ("discard", "_Discard"),
            ("save", "_Save"),
        ]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            let close = match response {
                "save" => ui.save(),
                "discard" => true,
                _ => false,
            };
            if close {
                ui.state.borrow_mut().dirty = false;
                ui.window.close();
            }
        });
        dialog.present(Some(&self.window));
    }
}
