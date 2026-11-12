//! Reloading the mod list (F5 / Ctrl+R), for mods subscribed to or added
//! while the app is open.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::conflicts_view::plural;
use crate::mods;
use crate::paradox::steam;

impl Window {
    pub(super) fn add_reload_action(self: &Rc<Self>, app: &adw::Application) {
        let action = gio::SimpleAction::new("reload", None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.reload(|_| {});
            }
        });
        self.window.add_action(&action);
        app.set_accels_for_action("win.reload", &["F5", "<Control>r"]);
    }

    /// Reads the descriptors and Workshop folders again in the background,
    /// keeping the load order, collection and unsaved changes, then calls
    /// `then` if it worked.
    pub(super) fn reload(self: &Rc<Self>, then: impl FnOnce(&Rc<Window>) + 'static) {
        let Some(action) = self.window.lookup_action("reload").and_downcast::<gio::SimpleAction>() else { return };
        if !action.is_enabled() {
            return; // already reloading
        }
        action.set_enabled(false);
        let user_dir = self.paths.user_dir.clone();
        let extra = self.settings.borrow().game.extra_mod_dir.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let found = gio::spawn_blocking(move || {
                // Mods added to or removed from the extra mod folder.
                let linked = mods::extra::link(&user_dir, extra.as_deref());
                // Libraries may have been added since startup.
                let workshop_dirs = steam::workshop_dirs(&glib::home_dir(), steam::STELLARIS_APP_ID);
                // Registers mods Steam downloaded since, as the launcher would.
                let synced = mods::sync::sync(&user_dir, &workshop_dirs);
                for error in &synced.errors {
                    crate::diagnostics::log!("mods", "{error}");
                }
                mods::discover(&user_dir, &workshop_dirs).map(|found| (found, synced.updated, linked.errors)).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|_| Err("the reload failed".to_owned()));
            action.set_enabled(true);
            let Some(ui) = weak.upgrade() else { return };
            let (found, updated, extra_errors) = match found {
                Ok(found) => found,
                Err(e) => return ui.toast_error_message(&format!("Could not reload mods: {e}")),
            };
            if let Some(error) = extra_errors.first() {
                ui.toast_error_message(error);
            }
            let before: std::collections::HashSet<String> = ui.state.borrow().mods.iter().map(|m| m.id.clone()).collect();
            let (added, removed) = ui.state.borrow_mut().reload(found);
            // Mods the collection kept a place for go back in it.
            let returned: std::collections::HashSet<String> = ui.state.borrow().mods.iter().map(|m| m.id.clone()).filter(|id| !before.contains(id)).collect();
            let collection = ui.collections.borrow().active().mods.clone();
            let restored = ui.state.borrow_mut().restore_returned(&collection, &returned);
            // Updated mods may have new thumbnails and files.
            *ui.thumbnails.borrow_mut() = Default::default();
            ui.profiles.borrow_mut().clear();
            ui.check_stale();
            ui.update_repair_banner();
            let rescan = ui.analysis.borrow().is_some();
            ui.refresh();
            if rescan {
                ui.scan();
            }
            let mut title = summary(&added, &updated, &removed);
            if !restored.is_empty() {
                title.push_str(&format!(" · back in the load order: {}", names(&restored)));
            }
            ui.add_toast(adw::Toast::builder().title(title).timeout(8).build());
            then(&ui);
        });
    }
}

/// "2 new mods: A, B · 1 updated: C · Gigastructural Engineering was removed"
fn summary(added: &[String], updated: &[String], removed: &[String]) -> String {
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("{}: {}", plural(added.len(), "new mod", "new mods"), names(added)));
    }
    if !updated.is_empty() {
        parts.push(format!("{} updated: {}", updated.len(), names(updated)));
    }
    match removed.len() {
        0 => {}
        1 => parts.push(format!("{} was removed", removed[0])),
        n => parts.push(format!("{n} mods were removed: {}", names(removed))),
    }
    if parts.is_empty() {
        "Mods reloaded, none added or removed".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// Up to three names, then how many more.
fn names(names: &[String]) -> String {
    match names {
        [first, second, third, rest @ ..] if !rest.is_empty() => {
            format!("{first}, {second}, {third} and {} more", rest.len())
        }
        _ => names.join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn summarises_what_changed() {
        assert_eq!(summary(&[], &[], &[]), "Mods reloaded, none added or removed");
        assert_eq!(summary(&strings(&["A", "B"]), &[], &[]), "2 new mods: A, B");
        assert_eq!(summary(&[], &strings(&["C"]), &[]), "1 updated: C");
        assert_eq!(summary(&[], &[], &strings(&["Giga"])), "Giga was removed");
        assert_eq!(
            summary(&strings(&["A", "B", "C", "D", "E"]), &[], &strings(&["X", "Y"])),
            "5 new mods: A, B, C and 2 more · 2 mods were removed: X, Y"
        );
    }
}
