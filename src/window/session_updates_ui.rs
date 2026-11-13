//! Which enabled mods Steam updated since the game was last launched from
//! here, with their Workshop change notes.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use adw::prelude::*;
use gtk::{gio, glib};

use super::overview::ago;
use super::{Window, display_name};
use crate::conflicts_view::plural;
use crate::mods::last_session::{self, LastSession};
use crate::paradox::steam;

fn steam_times() -> HashMap<u64, u64> {
    steam::updated_times(&steam::workshop_dirs(&glib::home_dir(), steam::STELLARIS_APP_ID))
}

impl Window {
    fn enabled_workshop_ids(&self) -> Vec<u64> {
        let state = self.state.borrow();
        state.enabled.iter().filter_map(|&i| state.mods[i].descriptor.remote_file_id).collect()
    }

    /// Remembers the enabled mods' versions as the game starts.
    pub(super) fn remember_session(&self) {
        let session = LastSession::starting(crate::mods::saves::now(), &self.enabled_workshop_ids(), &steam_times());
        if let Err(e) = session.save(&last_session::path()) {
            crate::diagnostics::log!("session", "Could not remember the mods' versions: {e}");
        }
        *self.last_session.borrow_mut() = Some(session);
    }

    /// The enabled mods Steam updated since the last launch, by index, with
    /// when; none before the first launch from here.
    pub(super) fn updated_since_last_session(&self) -> Vec<(usize, u64)> {
        let Some(last) = self.last_session.borrow().clone() else { return Vec::new() };
        let ids = self.enabled_workshop_ids();
        let updated = last.updated_since(&ids, &steam_times());
        let state = self.state.borrow();
        updated.into_iter().filter_map(|(id, at)| Some((state.mods.iter().position(|m| m.descriptor.remote_file_id == Some(id))?, at))).collect()
    }

    pub(super) fn show_session_updates(self: &Rc<Self>) {
        let updated = self.updated_since_last_session();
        if updated.is_empty() {
            return;
        }
        let since = self.last_session.borrow().as_ref().map(|s| ago(UNIX_EPOCH + Duration::from_secs(s.at))).unwrap_or_default();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .description(format!("Steam updated {} since you last launched the game, {since}. Read what changed before continuing a campaign: an update can break saves.", plural(updated.len(), "mod", "mods")))
            .build();
        {
            let state = self.state.borrow();
            for &(index, at) in &updated {
                let m = &state.mods[index];
                let row = adw::ActionRow::builder().title(display_name(m)).use_markup(false).subtitle(format!("Updated {}", ago(UNIX_EPOCH + Duration::from_secs(at)))).build();
                let Some(id) = m.descriptor.remote_file_id else { continue };
                let notes = gtk::Button::builder().label("Change Notes").valign(gtk::Align::Center).css_classes(["flat"]).tooltip_text("Open its Workshop change notes in the browser").build();
                let window = self.window.downgrade();
                notes.connect_clicked(move |_| {
                    gtk::UriLauncher::new(&last_session::change_notes_url(id)).launch(window.upgrade().as_ref(), None::<&gio::Cancellable>, |_| {});
                });
                row.add_suffix(&notes);
                group.add(&row);
            }
        }
        page.add(&group);

        let actions = adw::PreferencesGroup::new();
        let frozen = !self.collections.borrow().active().frozen.is_empty();
        if !frozen {
            let freeze = adw::ActionRow::builder()
                .title("Freeze This Collection…")
                .subtitle("Keep the versions you have now, so later updates can't change a campaign in progress")
                .activatable(true)
                .build();
            freeze.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            actions.add(&freeze);
            let weak = Rc::downgrade(self);
            freeze.connect_activated(move |row| {
                if let Some(dialog) = row.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
                    dialog.close();
                }
                if let Some(ui) = weak.upgrade() {
                    ui.freeze_collection();
                }
            });
        }
        let seen = adw::ButtonRow::builder().title("Mark as Seen").build();
        let weak = Rc::downgrade(self);
        let ids: Vec<u64> = {
            let state = self.state.borrow();
            updated.iter().filter_map(|&(i, _)| state.mods[i].descriptor.remote_file_id).collect()
        };
        seen.connect_activated(move |row| {
            if let Some(dialog) = row.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
                dialog.close();
            }
            let Some(ui) = weak.upgrade() else { return };
            let saved = {
                let mut last = ui.last_session.borrow_mut();
                let Some(last) = last.as_mut() else { return };
                last.mark_seen(&ids, &steam_times());
                last.save(&last_session::path())
            };
            if let Err(e) = saved {
                crate::diagnostics::log!("session", "Could not save the seen updates: {e}");
            }
            ui.refresh();
        });
        actions.add(&seen);
        page.add(&actions);

        let header = adw::HeaderBar::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        adw::Dialog::builder().title("Updated Since You Last Played").content_width(560).content_height(520).child(&toolbar).build().present(Some(&self.window));
    }
}
