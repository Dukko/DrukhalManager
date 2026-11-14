//! After a game update: which enabled mods haven't caught up, with a way
//! to set them aside until they do.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::{Window, display_name};
use crate::mods::game_update::{self, Outdated};

impl Window {
    /// Notices a new game version since the last run, and shows the mods
    /// that don't support it yet, if any.
    pub(super) fn check_game_update(self: &Rc<Self>) {
        let Some(now) = self.game.as_ref().and_then(|g| g.version.clone()) else { return };
        let last = self.settings.borrow().last_game_version.clone();
        if last.as_deref() == Some(now.as_str()) {
            return;
        }
        self.settings.borrow_mut().last_game_version = Some(now.clone());
        self.save_settings("the game version");
        if game_update::is_update(last.as_deref(), &now) {
            self.show_outdated_mods(true);
        }
    }

    /// The enabled mods that don't support the installed game version.
    /// `after_update` words it for a game that was just updated; otherwise
    /// a list without any says so in a toast.
    pub(super) fn show_outdated_mods(self: &Rc<Self>, after_update: bool) {
        let Some(game) = self.game.clone() else { return };
        let Some(version) = game.version.clone() else {
            return self.add_toast(adw::Toast::new(&t!("The game's version couldn't be read")));
        };
        // When Steam last updated the game.
        let game_updated = std::fs::metadata(&game.exe).and_then(|m| m.modified()).ok();
        let outdated = {
            let state = self.state.borrow();
            game_update::outdated(&state.mods, &state.enabled, &version, game_updated)
        };
        let short = format!("v{}", game_update::major_minor(&version));
        if outdated.is_empty() {
            if !after_update {
                self.add_toast(adw::Toast::new(&tf!("Every enabled mod supports Stellaris {version}", version = &short)));
            }
            return;
        }
        let stale_merges = self.stale.borrow().iter().filter(|s| s.base_game).count();
        let mut body = tnf!(
            "{count} enabled mod doesn't say it supports Stellaris {version} yet. Mods often still work, but some break saves or crash the game until their authors update them.",
            "{count} enabled mods don't say they support Stellaris {version} yet. Mods often still work, but some break saves or crash the game until their authors update them.",
            count = outdated.len(),
            version = &short
        );
        if stale_merges > 0 {
            body.push_str(&tnf!(
                " The update also changed files behind {count} merge in the merged patch; they're listed on the Conflicts page.",
                " The update also changed files behind {count} merges in the merged patch; they're listed on the Conflicts page.",
                count = stale_merges
            ));
        }
        body.push_str(&t!("\n\nTo keep playing as before, you can roll the game back in Steam: Stellaris › Properties › Betas lists the previous versions."));
        let dialog = adw::AlertDialog::builder()
            .heading(if after_update {
                tf!("Stellaris Updated to {version}", version = &short)
            } else {
                tf!("Mods Not Updated for {version}", version = &short)
            })
            .body(body)
            .build();
        dialog.set_extra_child(Some(&self.outdated_list(&outdated)));
        dialog.add_responses(&[("close", &t!("_Keep Them Enabled")), ("disable", &t!("_Disable Them for Now"))]);
        dialog.set_response_appearance("disable", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("close"));
        dialog.set_close_response("close");
        let weak = Rc::downgrade(self);
        let indices: Vec<usize> = outdated.iter().map(|o| o.index).collect();
        dialog.connect_response(None, move |_, response| {
            if let (Some(ui), "disable") = (weak.upgrade(), response) {
                ui.disable_outdated(&indices, &short);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn outdated_list(self: &Rc<Self>, outdated: &[Outdated]) -> gtk::ScrolledWindow {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        let state = self.state.borrow();
        for o in outdated {
            let m = &state.mods[o.index];
            let mut subtitle = tf!("Supports {version}", version = &o.supported);
            if o.updated_since {
                subtitle.push_str(&t!(" · Updated since the game was"));
            }
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(display_name(m))).subtitle(subtitle).build();
            if m.descriptor.remote_file_id.is_some() {
                let button = gtk::Button::builder().icon_name("web-browser-symbolic").tooltip_text(t!("Open Workshop Page")).valign(gtk::Align::Center).css_classes(["flat"]).build();
                let weak = Rc::downgrade(self);
                let index = o.index;
                button.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.open_workshop(index);
                    }
                });
                row.add_suffix(&button);
            }
            list.append(&row);
        }
        gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).max_content_height(320).build()
    }

    /// Keeps the current load order as a collection, then disables
    /// `indices` and saves.
    fn disable_outdated(self: &Rc<Self>, indices: &[usize], short: &str) {
        let ids = self.state.borrow().enabled_ids();
        let kept = self.collections.borrow_mut().add(&tf!("Before Stellaris {version}", version = &short), ids);
        self.save_collections();
        self.rebuild_collection_menu();
        {
            let mut state = self.state.borrow_mut();
            let order: Vec<usize> = state.enabled.iter().copied().filter(|i| !indices.contains(i)).collect();
            state.reorder(order);
        }
        if self.save() {
            self.add_toast(adw::Toast::new(&tnf!("Disabled {count} mod; “{name}” has the full load order", "Disabled {count} mods; “{name}” has the full load order", count = indices.len(), name = kept)));
        }
    }
}
