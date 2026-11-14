//! Workshop required items: fetched in the background for enabled mods,
//! checked like dependencies, and fixed from a mod's menu.

use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::mods::requirements::Cache;
use crate::paradox::workshop;
use crate::secrets;

/// After Steam refused or couldn't be reached, wait this long.
const RETRY: Duration = Duration::from_secs(30 * 60);
/// Between batches of a first fetch of many mods.
const BETWEEN_BATCHES: Duration = Duration::from_secs(5);
/// Without a key, look again this much later (or when one is set).
const NO_KEY: Duration = Duration::from_secs(24 * 3600);

impl Window {
    /// Fills in the mods' requirements from the cache.
    pub(super) fn apply_requirements(&self) {
        self.requirements.borrow().apply(&mut self.state.borrow_mut().mods, &self.settings.borrow().ignored_requirements);
    }

    /// Stops treating a Workshop item as a requirement of any mod, or
    /// starts again (`ignore` false, for Undo).
    pub(super) fn ignore_requirement(self: &Rc<Self>, id: u64, ignore: bool) {
        let name = self.state.borrow().mods.iter().flat_map(|m| &m.requires).find(|r| r.steam_id == id).map(|r| r.name.clone());
        {
            let mut settings = self.settings.borrow_mut();
            if ignore { settings.ignored_requirements.insert(id) } else { settings.ignored_requirements.remove(&id) };
        }
        self.save_settings("ignored requirements");
        self.refresh();
        if ignore {
            let name = name.unwrap_or_else(|| tf!("Workshop item {id}", id = id));
            self.add_toast(
                adw::Toast::builder()
                    .title(tf!("“{name}” is no longer treated as required", name = name))
                    .button_label(t!("_Undo"))
                    .action_name("win.unignore-requirement")
                    .action_target(&id.to_variant())
                    .build(),
            );
        }
    }

    /// Fetches the requirements of enabled Workshop mods that aren't known
    /// or are old, a batch at a time, unless a fetch is running or Steam
    /// refused lately.
    pub(super) fn fetch_requirements(self: &Rc<Self>) {
        if self.fetching_requirements.get() || self.requirements_not_before.get().is_some_and(|at| Instant::now() < at) {
            return;
        }
        let ids = {
            let state = self.state.borrow();
            let enabled: Vec<_> = state.enabled.iter().map(|&i| state.mods[i].clone()).collect();
            self.requirements.borrow().stale(&enabled)
        };
        if ids.is_empty() {
            return;
        }
        self.fetching_requirements.set(true);
        let weak = Rc::downgrade(self);
        // Where the key is kept while there's no keyring.
        let in_file = self.settings.borrow().steam_key.clone();
        glib::spawn_future_local(async move {
            let fetched = gio::spawn_blocking(move || {
                // Without the user's own key, required items can't be read.
                let Some(key) = steam_key(in_file) else { return Ok(None) };
                workshop::requirements(&key, &ids).map(Some)
            })
            .await
            .unwrap_or_else(|_| Err("the fetch failed".into()));
            let Some(ui) = weak.upgrade() else { return };
            ui.fetching_requirements.set(false);
            let found = match fetched {
                Ok(Some(found)) => found,
                // Until a key is set in Preferences.
                Ok(None) => return ui.requirements_not_before.set(Some(Instant::now() + NO_KEY)),
                Err(e) => {
                    crate::diagnostics::log!("requirements", "Could not read Workshop requirements: {e}");
                    return ui.requirements_not_before.set(Some(Instant::now() + RETRY));
                }
            };
            // More may be left; fetched after a pause.
            ui.requirements_not_before.set(Some(Instant::now() + BETWEEN_BATCHES));
            let weak = Rc::downgrade(&ui);
            glib::timeout_add_local_once(BETWEEN_BATCHES, move || {
                if let Some(ui) = weak.upgrade() {
                    ui.fetch_requirements();
                }
            });
            ui.requirements.borrow_mut().update(found);
            if let Err(e) = ui.requirements.borrow().save(&Cache::path()) {
                crate::diagnostics::log!("requirements", "Could not save the Workshop requirements: {e}");
            }
            ui.refresh();
        });
    }

    /// Stores a Steam Web API key typed in Preferences (empty removes it)
    /// and fetches requirements with it. It goes in the keyring, or with no
    /// keyring in the (private) settings file, as the provider's key does.
    pub(super) fn store_steam_key(self: &Rc<Self>, key: String) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let for_keyring = key.clone();
            let stored = gio::spawn_blocking(move || secrets::set(secrets::STEAM, &for_keyring)).await.unwrap_or_else(|_| Err(t!("The keyring call failed")));
            let Some(ui) = weak.upgrade() else { return };
            let in_file = if stored.is_ok() { String::new() } else { key };
            if ui.settings.borrow().steam_key != in_file {
                ui.settings.borrow_mut().steam_key = in_file;
                ui.save_settings("the Steam key");
            }
            ui.requirements_not_before.set(None);
            ui.fetch_requirements();
        });
    }

    /// Enables the mod's required items that are installed, sorts so they
    /// load first, and opens the Workshop pages of the ones that aren't.
    pub(super) fn get_requirements(self: &Rc<Self>, index: usize) {
        let (to_enable, missing) = {
            let state = self.state.borrow();
            let Some(m) = state.mods.get(index) else { return };
            let mut to_enable = Vec::new();
            let mut missing = Vec::new();
            for r in &m.requires {
                match state.mods.iter().position(|x| x.descriptor.remote_file_id == Some(r.steam_id)) {
                    Some(i) if !state.enabled.contains(&i) => to_enable.push(i),
                    Some(_) => {}
                    None => missing.push(r.steam_id),
                }
            }
            (to_enable, missing)
        };
        if !to_enable.is_empty() {
            let count = to_enable.len();
            {
                // One step to undo: enabled, then placed before the mods
                // that need them.
                let mut state = self.state.borrow_mut();
                let mut order = state.enabled.clone();
                order.extend(&to_enable);
                let sorted = crate::mods::checks::sort_by_dependencies(&state.mods, &order);
                state.reorder(sorted);
            }
            self.refresh();
            self.offer_undo(&tnf!("Enabled {count} mod this mod requires", "Enabled {count} mods this mod requires", count = count));
        }
        for id in &missing {
            gtk::UriLauncher::new(&format!("steam://url/CommunityFilePage/{id}")).launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
        }
        if !missing.is_empty() {
            self.add_toast(adw::Toast::new(&tnf!(
                "Opened {count} Workshop page in Steam: subscribe there, then reload (F5)",
                "Opened {count} Workshop pages in Steam: subscribe there, then reload (F5)",
                count = missing.len()
            )));
        }
    }
}

/// The user's Steam Web API key: from the keyring, or else `in_file`, the
/// settings file's copy while there's no keyring. Blocks; call off the
/// main thread.
pub(super) fn steam_key(in_file: String) -> Option<String> {
    secrets::get(secrets::STEAM).ok().flatten().into_iter().chain([in_file]).map(|k| k.trim().to_owned()).find(|k| !k.is_empty())
}
