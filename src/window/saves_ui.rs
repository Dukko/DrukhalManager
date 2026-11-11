//! Remembering which mods each campaign was played with, and warning
//! before continuing one with a different load order.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::{Window, display_name};
use crate::conflicts_view::plural;
use crate::mods::saves::{self, Played, Saves};
use crate::paradox::session;

impl Window {
    /// The load order about to be played, to record when the session ends.
    pub(super) fn playing(&self) -> Played {
        let state = self.state.borrow();
        let mods = state.enabled_ids();
        let names = state.enabled.iter().map(|&i| (state.mods[i].id.clone(), display_name(&state.mods[i]).to_owned())).collect();
        Played { empire: String::new(), date: String::new(), collection: self.collections.borrow().active.clone(), mods, names, at: 0 }
    }

    /// After a session that started at `started`, records the campaign it
    /// saved, if it saved one.
    pub(super) fn record_session(&self, started: std::time::SystemTime, mut played: Played) {
        let user_dir = &self.paths.user_dir;
        // A crash hunt's tests aren't how the campaign is played.
        if self.hunting() || !session::saved_since(user_dir, started) {
            return;
        }
        let Some(last) = session::last_save(user_dir) else { return };
        played.empire = last.empire;
        played.date = last.date;
        played.at = saves::now();
        let path = saves::path();
        let mut saves = Saves::load(&path);
        saves.record(&last.folder, played);
        if let Err(e) = saves.save(&path) {
            eprintln!("Could not remember the save's mods: {e}");
        }
        self.update_continue();
    }

    /// Before launching: if the save the game will continue was played
    /// with a different load order, asks first. Calls `go` to launch.
    pub(super) fn check_save_mods(self: &Rc<Self>, go: impl Fn(&Rc<Window>) + 'static) {
        let found = session::last_save(&self.paths.user_dir).and_then(|last| Some((Saves::load(&saves::path()).campaigns.get(&last.folder)?.clone(), last)));
        let Some((played, last)) = found else { return go(self) };
        let now = self.playing();
        let difference = saves::compare(&played.mods, &now.mods);
        if difference.is_empty() {
            return go(self);
        }
        let empire = glib::markup_escape_text(&last.empire);
        let mut body = if played.collection == now.collection {
            format!("{empire} was last played with “{}”, which has changed since:", glib::markup_escape_text(&played.collection))
        } else {
            format!("{empire} was last played with “{}”. “{}” is active:", glib::markup_escape_text(&played.collection), glib::markup_escape_text(&now.collection))
        };
        let name = |id: &String| glib::markup_escape_text(played.names.get(id).or_else(|| now.names.get(id)).map_or(id.as_str(), String::as_str)).to_string();
        let list = |ids: &[String]| {
            let mut names: Vec<String> = ids.iter().take(5).map(name).collect();
            if ids.len() > 5 {
                names.push(format!("{} more", ids.len() - 5));
            }
            names.join(", ")
        };
        if !difference.added.is_empty() {
            body.push_str(&format!("\n• {} added: {}", plural(difference.added.len(), "mod", "mods"), list(&difference.added)));
        }
        if !difference.removed.is_empty() {
            body.push_str(&format!("\n• {} missing: {}", plural(difference.removed.len(), "mod", "mods"), list(&difference.removed)));
        }
        if difference.reordered {
            body.push_str("\n• The load order is different");
        }
        body.push_str("\n\nLoading a save with other mods can break it.");
        let dialog = adw::AlertDialog::builder().heading("Different Mods Than Last Time").body(body).body_use_markup(true).build();
        dialog.add_response("cancel", "_Cancel");
        let can_switch = played.collection != now.collection && self.collections.borrow().get(&played.collection).is_some();
        if can_switch {
            dialog.add_response("switch", &format!("_Switch to “{}”", played.collection));
            dialog.set_response_appearance("switch", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("switch"));
        } else {
            dialog.set_default_response(Some("cancel"));
        }
        dialog.add_response("launch", "_Launch Anyway");
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            match response {
                "launch" => go(&ui),
                "switch" => {
                    ui.switch_collection(played.collection.clone());
                    // Switching may ask to save first; launch only once
                    // it's done.
                    if ui.collections.borrow().active == played.collection && !ui.state.borrow().dirty {
                        go(&ui);
                    }
                }
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// The campaigns played from DrukhalManager, with their mods.
    pub(super) fn show_saves(self: &Rc<Self>) {
        let saves = Saves::load(&saves::path());
        if saves.campaigns.is_empty() {
            return self.add_toast(adw::Toast::new("No saves yet: games played from DrukhalManager are listed here"));
        }
        let active = self.collections.borrow().active.clone();
        let current = self.playing().mods;
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        for (_, played) in saves.recent() {
            let difference = saves::compare(&played.mods, &current);
            let mut subtitle = format!("{} · {}", played.collection, plural(played.mods.len(), "mod", "mods"));
            if !difference.is_empty() {
                subtitle.push_str(if played.collection == active { " · Changed since" } else { " · Not active" });
            }
            let title = if played.date.is_empty() { played.empire.clone() } else { format!("{}, {}", played.empire, played.date) };
            let row = adw::ExpanderRow::builder().title(glib::markup_escape_text(&title)).subtitle(glib::markup_escape_text(&subtitle)).build();
            for id in &played.mods {
                let name = played.names.get(id).map_or(id.as_str(), String::as_str);
                row.add_row(&adw::ActionRow::builder().title(glib::markup_escape_text(name)).css_classes(["property"]).build());
            }
            list.append(&row);
        }
        let scrolled = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).max_content_height(420).build();
        let dialog = adw::AlertDialog::builder().heading("Saves").body("The mods each campaign was last played with from DrukhalManager.").build();
        dialog.set_extra_child(Some(&scrolled));
        dialog.add_response("close", "_Close");
        dialog.present(Some(&self.window));
    }
}
