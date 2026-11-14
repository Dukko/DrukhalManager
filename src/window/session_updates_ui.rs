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
use crate::paradox::{steam, workshop};

/// How often the Workshop is asked for newer versions.
const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);

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

    /// Asks the Workshop when the enabled mods were last updated, at most
    /// every [`CHECK_EVERY`], in the background; the lists are refreshed
    /// when the answer changes what's waiting.
    pub(super) fn check_published(self: &Rc<Self>) {
        if self.published_checked.get().is_some_and(|at| at.elapsed() < CHECK_EVERY) {
            return;
        }
        let ids = self.enabled_workshop_ids();
        if ids.is_empty() {
            return;
        }
        self.published_checked.set(Some(std::time::Instant::now()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let found = gio::spawn_blocking(move || workshop::published_times(&ids)).await.unwrap_or_else(|_| Err("the check failed".to_owned()));
            let Some(ui) = weak.upgrade() else { return };
            match found {
                Ok(times) => {
                    let before = ui.waiting_updates();
                    ui.published.borrow_mut().extend(times);
                    if ui.waiting_updates() != before {
                        ui.refresh();
                    }
                }
                Err(e) => crate::diagnostics::log!("session", "Could not check the Workshop for updates: {e}"),
            }
        });
    }

    /// The enabled mods with a newer version on the Workshop than Steam
    /// has downloaded, by index, with when it came out.
    pub(super) fn waiting_updates(&self) -> Vec<(usize, u64)> {
        let ids = self.enabled_workshop_ids();
        let published: HashMap<u64, u64> = self.published.borrow().iter().filter(|(id, _)| ids.contains(id)).map(|(id, t)| (*id, *t)).collect();
        if published.is_empty() {
            return Vec::new();
        }
        let waiting = last_session::waiting(&steam_times(), &published);
        let state = self.state.borrow();
        waiting.into_iter().filter_map(|(id, at)| Some((state.mods.iter().position(|m| m.descriptor.remote_file_id == Some(id))?, at))).collect()
    }

    fn notes_button(&self, id: u64) -> gtk::Button {
        let notes = gtk::Button::builder().label(t!("Change Notes")).valign(gtk::Align::Center).css_classes(["flat"]).tooltip_text(t!("Open its Workshop change notes in the browser")).build();
        let window = self.window.downgrade();
        notes.connect_clicked(move |_| {
            gtk::UriLauncher::new(&last_session::change_notes_url(id)).launch(window.upgrade().as_ref(), None::<&gio::Cancellable>, |_| {});
        });
        notes
    }

    pub(super) fn show_session_updates(self: &Rc<Self>) {
        let updated = self.updated_since_last_session();
        let waiting = self.waiting_updates();
        if updated.is_empty() && waiting.is_empty() {
            return;
        }
        let since = self.last_session.borrow().as_ref().map(|s| ago(UNIX_EPOCH + Duration::from_secs(s.at))).unwrap_or_default();
        let page = adw::PreferencesPage::new();
        if !waiting.is_empty() {
            let group = adw::PreferencesGroup::builder()
                .title(t!("Not Downloaded Yet"))
                .description(format!("The Workshop has newer versions of {} than Steam has downloaded. Steam usually fetches them while it's open and the game isn't running, so they may change before your next session.", plural(waiting.len(), "mod", "mods")))
                .build();
            let downloads = gtk::Button::builder().label(t!("Steam Downloads")).valign(gtk::Align::Center).css_classes(["flat"]).tooltip_text(t!("Open Steam's Downloads page")).build();
            let window = self.window.downgrade();
            downloads.connect_clicked(move |_| {
                gtk::UriLauncher::new("steam://open/downloads").launch(window.upgrade().as_ref(), None::<&gio::Cancellable>, |_| {});
            });
            group.set_header_suffix(Some(&downloads));
            let state = self.state.borrow();
            for &(index, at) in &waiting {
                let m = &state.mods[index];
                let row = adw::ActionRow::builder().title(display_name(m)).use_markup(false).subtitle(format!("Updated on the Workshop {}", ago(UNIX_EPOCH + Duration::from_secs(at)))).build();
                if let Some(id) = m.descriptor.remote_file_id {
                    row.add_suffix(&self.notes_button(id));
                }
                group.add(&row);
            }
            drop(state);
            page.add(&group);
        }
        let group = adw::PreferencesGroup::builder()
            .title(t!("Updated Since You Last Played"))
            .description(format!("Steam updated {} since you last launched the game, {since}. Read what changed before continuing a campaign: an update can break saves.", plural(updated.len(), "mod", "mods")))
            .build();
        {
            let state = self.state.borrow();
            for &(index, at) in &updated {
                let m = &state.mods[index];
                let row = adw::ActionRow::builder().title(display_name(m)).use_markup(false).subtitle(format!("Updated {}", ago(UNIX_EPOCH + Duration::from_secs(at)))).build();
                let Some(id) = m.descriptor.remote_file_id else { continue };
                row.add_suffix(&self.notes_button(id));
                group.add(&row);
            }
        }
        if !updated.is_empty() {
            page.add(&group);
        }

        let actions = adw::PreferencesGroup::new();
        let frozen = !self.collections.borrow().active().frozen.is_empty();
        if !frozen {
            let freeze = adw::ActionRow::builder()
                .title(t!("Freeze This Collection…"))
                .subtitle(t!("Keep the versions you have now, so later updates can't change a campaign in progress"))
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
        let seen = adw::ButtonRow::builder().title(t!("Mark as Seen")).visible(!updated.is_empty()).build();
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
        adw::Dialog::builder().title(t!("Mod Updates")).content_width(560).content_height(520).child(&toolbar).build().present(Some(&self.window));
    }
}
