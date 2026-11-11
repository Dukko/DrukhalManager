//! Launching the game, its error report, and window-level dialogs.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use super::display_name;
use crate::conflicts_view;
use crate::error_report_dialog;
use crate::merge::patch;
use crate::mods::archive;
use crate::paradox::{error_log, session};

/// Makes the game load the last save instead of the main menu.
const CONTINUE: &str = "--continuelastsave";

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
        // A zipped mod: show the folder holding the zip.
        let dir = if dir.is_file() { dir.parent().map_or(dir.clone(), |p| p.to_path_buf()) } else { dir };
        gtk::FileLauncher::new(Some(&gio::File::for_path(dir))).launch(
            Some(&self.window),
            None::<&gio::Cancellable>,
            |_| {},
        );
    }

    fn workshop_id(&self, index: usize) -> Option<u64> {
        self.state.borrow().mods.get(index).and_then(|m| m.descriptor.remote_file_id)
    }

    /// Opens the mod's Workshop page in the web browser.
    pub(super) fn open_workshop(&self, index: usize) {
        let Some(id) = self.workshop_id(index) else { return };
        gtk::UriLauncher::new(&workshop_url(id)).launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
    }

    /// Opens the mod's Workshop page in the Steam client, where the player
    /// is signed in; in the browser if Steam doesn't take the link.
    pub(super) fn open_in_steam(&self, index: usize) {
        let Some(id) = self.workshop_id(index) else { return };
        let window = self.window.downgrade();
        gtk::UriLauncher::new(&format!("steam://url/CommunityFilePage/{id}")).launch(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            if result.is_err() {
                let window = window.upgrade();
                gtk::UriLauncher::new(&workshop_url(id)).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
            }
        });
    }

    /// Steam can't be asked to unsubscribe directly, so this opens the page
    /// in Steam and says what to do there.
    pub(super) fn unsubscribe_in_steam(&self, index: usize) {
        let Some(name) = self.state.borrow().mods.get(index).map(|m| super::display_name(m).to_owned()) else { return };
        self.open_in_steam(index);
        self.add_toast(adw::Toast::builder().title(format!("Click Unsubscribe on {name}'s page in Steam, then reload the mods (F5)")).timeout(10).build());
    }

    pub(super) fn copy_workshop_link(&self, index: usize) {
        let Some(id) = self.workshop_id(index) else { return };
        self.window.clipboard().set_text(&workshop_url(id));
        self.add_toast(adw::Toast::new("Copied the Workshop link"));
    }

    /// Offers to continue the last save in the Launch menu, if there is one.
    pub(super) fn update_continue(&self) {
        let last = self.game.as_ref().and_then(|_| session::last_save(&self.paths.user_dir));
        if let Some(action) = self.window.lookup_action("launch-continue").and_downcast::<gio::SimpleAction>() {
            action.set_enabled(last.is_some());
        }
        let label = match &last {
            Some(save) if save.date.is_empty() => format!("_Continue {}", save.empire),
            Some(save) => format!("_Continue {}, {}", save.empire, save.date),
            None => "_Continue Last Save (none found)".to_owned(),
        };
        self.launch_menu.remove_all();
        self.launch_menu.append(Some(&label), Some("win.launch-continue"));
    }

    /// Starts the game; `resume` loads the last save straight away. Asks
    /// first if the last save was played with other mods.
    pub(super) fn launch(self: &Rc<Self>, resume: bool) {
        if self.game.is_none() {
            return;
        }
        if self.hunting() {
            // Tests don't continue saves, and change the mods on purpose.
            return self.start_game(resume);
        }
        self.check_save_mods(move |ui| ui.start_game(resume));
    }

    fn start_game(self: &Rc<Self>, resume: bool) {
        let Some(game) = &self.game else { return };
        let options = self.settings.borrow().game.clone();
        let extra = if options.launch_args.is_empty() {
            Vec::new()
        } else {
            match glib::shell_parse_argv(&options.launch_args) {
                Ok(args) => args.into_iter().map(|a| a.to_string_lossy().into_owned()).collect(),
                Err(e) => {
                    return self.add_toast(
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
        let played = self.playing();
        if crate::sandbox::is_flatpak() {
            // A sandboxed app can't start the game itself; Steam can, and
            // the launcher playset carries the load order.
            let id = crate::paradox::steam::STELLARIS_APP_ID;
            // Steam asks before passing arguments on.
            let uri = if resume { format!("steam://run/{id}//{CONTINUE}/") } else { format!("steam://rungameid/{id}") };
            gtk::UriLauncher::new(&uri)
            .launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
            self.add_toast(adw::Toast::new("Starting Stellaris through Steam…"));
            if options.close_after_launch {
                self.window.close();
            } else {
                self.follow_session(std::time::SystemTime::now(), played);
            }
            return;
        }
        let started = std::time::SystemTime::now();
        let mut extra = extra;
        if resume {
            extra.push(CONTINUE.to_owned());
        }
        let mut child = match game.launch(&extra) {
            Ok(child) => child,
            Err(e) => {
                return self.add_toast(
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
        self.add_toast(adw::Toast::new("Starting Stellaris…"));
        // When the game exits, point out errors it logged about the mods.
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || child.wait()).await;
            if let Some(ui) = weak.upgrade() {
                ui.record_session(started, played);
                let crashed = session::ended(&ui.paths.user_dir, started) == Some(session::Ended::Crashed);
                ui.report_session(started, crashed);
            }
        });
    }

    /// Watches the game's files for a session started through Steam to end,
    /// since the sandbox can't wait on the game. Gives up if it doesn't
    /// start within half an hour (the launch was cancelled).
    fn follow_session(self: &Rc<Self>, launched: std::time::SystemTime, played: crate::mods::saves::Played) {
        const GIVE_UP: std::time::Duration = std::time::Duration::from_secs(30 * 60);
        let weak = Rc::downgrade(self);
        let mut running = false;
        glib::timeout_add_seconds_local(5, move || {
            let Some(ui) = weak.upgrade() else { return glib::ControlFlow::Break };
            let user_dir = &ui.paths.user_dir;
            if !running {
                running = session::started(user_dir, launched);
                if !running && launched.elapsed().unwrap_or_default() > GIVE_UP {
                    return glib::ControlFlow::Break;
                }
                return glib::ControlFlow::Continue;
            }
            match session::ended(user_dir, launched) {
                Some(how) => {
                    ui.record_session(launched, played.clone());
                    ui.report_session(launched, how == session::Ended::Crashed);
                    glib::ControlFlow::Break
                }
                None => glib::ControlFlow::Continue,
            }
        });
    }

    /// After a session that started at `started`, points out errors the
    /// game logged about the mods, and that it crashed.
    fn report_session(self: &Rc<Self>, started: std::time::SystemTime, crashed: bool) {
        if self.hunting() {
            return self.ask_hunt_result(crashed);
        }
        let log = error_log::path(&self.paths.user_dir);
        let fresh = std::fs::metadata(&log)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= started);
        let report = if fresh { Some(self.error_report()) } else { None };
        let from_mods = report.as_ref().map_or(0, |r| r.from_mods());
        if from_mods == 0 && !crashed {
            return;
        }
        let patch = report
            .iter()
            .flat_map(|r| &r.groups)
            .find(|g| g.is_patch)
            .map_or(0, |g| g.total());
        let errors = conflicts_view::plural(from_mods, "error", "errors");
        let mut title = match (crashed, from_mods) {
            (true, 0) => "Stellaris crashed".to_owned(),
            (true, _) => format!("Stellaris crashed and logged {errors} from your mods"),
            (false, _) => format!("Stellaris logged {errors} from your mods"),
        };
        if patch > 0 {
            title.push_str(&format!(", {patch} from the merged patch"));
        }
        let mut toast = adw::Toast::builder().title(title).timeout(0);
        if fresh {
            toast = toast.button_label("Show").action_name("win.errors");
        }
        self.add_toast(toast.build());
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
                    root: m.content.as_deref().and_then(|c| archive::files(c).ok()),
                    steam_id: m.descriptor.remote_file_id,
                    is_patch: patch::is_patch(&m.id),
                }
            })
            .collect();
        error_log::analyze(&src, &mods)
    }

    /// The Multiplayer Check dialog, for the enabled mods in load order.
    /// The merged patch is left out: each player's is their own.
    pub(super) fn show_multiplayer(self: &Rc<Self>) {
        let mods = {
            let state = self.state.borrow();
            state
                .enabled
                .iter()
                .map(|&i| &state.mods[i])
                .filter(|m| !patch::is_patch(&m.id))
                .map(|m| (display_name(m).to_owned(), m.descriptor.remote_file_id, m.content.clone()))
                .collect()
        };
        let request = crate::multiplayer_dialog::Request {
            mods,
            game_dir: self.game.as_ref().map(|g| g.dir.clone()),
            game_version: self.game.as_ref().and_then(|g| g.version.clone()),
        };
        crate::multiplayer_dialog::present(&self.window, request);
    }

    pub(super) fn show_errors(self: &Rc<Self>) {
        let log = error_log::path(&self.paths.user_dir);
        if !log.is_file() {
            return self.add_toast(adw::Toast::new(
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
                .filter_map(|m| Some((m.id.clone(), archive::files(m.content.as_deref()?).ok()?)))
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

fn workshop_url(id: u64) -> String {
    format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}")
}
