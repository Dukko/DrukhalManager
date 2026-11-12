//! Freeze This Collection: copies its Workshop mods into local ones so
//! Steam updates can't change a campaign, and Unfreeze to go back.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::conflicts_view::plural;
use crate::mods::freeze::{self, Frozen};
use crate::mods::{self, collections};
use crate::paradox::steam;

/// A Workshop mod to copy: id, Steam id, name, descriptor, files.
type ToCopy = (String, u64, mods::Descriptor, std::path::PathBuf);

impl Window {
    pub(super) fn freeze_collection(self: &Rc<Self>) {
        if self.state.borrow().dirty && !self.save() {
            return;
        }
        let name = self.collections.borrow().active.clone();
        let to_copy: Vec<ToCopy> = {
            let state = self.state.borrow();
            state
                .enabled
                .iter()
                .map(|&i| &state.mods[i])
                .filter(|m| m.source == mods::Source::Workshop)
                .filter_map(|m| Some((m.id.clone(), m.descriptor.remote_file_id.or_else(|| collections::steam_id_of(&m.id))?, m.descriptor.clone(), m.content.clone()?)))
                .collect()
        };
        if to_copy.is_empty() {
            return self.toast_error_message(&format!("“{name}” has no Workshop mods to freeze"));
        }
        let size = freeze::size(&to_copy.iter().map(|m| m.3.as_path()).collect::<Vec<_>>());
        let body = format!(
            "Copies its {} ({}) into local mods, and switches “{name}” to the copies, so Steam updates can't change it in the middle of a campaign.\n\nFixes their authors publish are missed too, until you unfreeze it. Unfreezing switches back to the Workshop mods and deletes the copies.",
            plural(to_copy.len(), "Workshop mod", "Workshop mods"),
            glib::format_size(size)
        );
        let dialog = adw::AlertDialog::new(Some(&format!("Freeze “{name}”?")), Some(&body));
        dialog.add_responses(&[("cancel", "_Cancel"), ("freeze", "_Freeze")]);
        dialog.set_response_appearance("freeze", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("freeze"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let (Some(ui), "freeze") = (weak.upgrade(), response) else { return };
            ui.copy_frozen(name.clone(), to_copy.clone());
        });
        dialog.present(Some(&self.window));
    }

    fn copy_frozen(self: &Rc<Self>, name: String, to_copy: Vec<ToCopy>) {
        // Unfreezing deletes a collection's folder, so one in use exists.
        let base = self.paths.user_dir.join("mod").join(freeze::FOLDER);
        let folder = freeze::folder_name(&name, |f| base.join(f).exists());
        let toast = adw::Toast::builder().title(format!("Copying {}…", plural(to_copy.len(), "mod", "mods"))).timeout(0).build();
        self.show_progress(&toast);
        let user_dir = self.paths.user_dir.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let copied = gio::spawn_blocking(move || {
                let updated = steam::updated_times(&steam::workshop_dirs(&glib::home_dir(), steam::STELLARIS_APP_ID));
                let sources: Vec<freeze::Source> = to_copy.iter().map(|(id, steam_id, descriptor, content)| freeze::Source { id, steam_id: *steam_id, descriptor, content }).collect();
                freeze::freeze(&user_dir, &folder, &sources, &updated).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|_| Err("Copying failed".into()));
            toast.dismiss();
            let Some(ui) = weak.upgrade() else { return };
            let frozen = match copied {
                Ok(f) => f,
                Err(e) => return ui.toast_error_message(&format!("Could not freeze “{name}”, so nothing changed: {e}")),
            };
            let count = frozen.len();
            let map: HashMap<&str, &str> = frozen.iter().map(|(copy, f)| (f.workshop.as_str(), copy.as_str())).collect();
            let mods = {
                let mut collections = ui.collections.borrow_mut();
                let Some(c) = collections.get_mut(&name) else { return };
                freeze::swap(&mut c.mods, &map);
                c.frozen.extend(frozen.clone());
                c.mods.clone()
            };
            ui.save_collections();
            ui.rebuild_collection_menu();
            // The copies' descriptors show up once the mods are read again.
            ui.reload(move |ui| {
                ui.apply_collection_mods(&mods);
                ui.add_toast(adw::Toast::new(&format!("Froze “{name}”: {} copied", plural(count, "Workshop mod", "Workshop mods"))));
            });
        });
    }

    pub(super) fn unfreeze_collection(self: &Rc<Self>) {
        let (name, count) = {
            let collections = self.collections.borrow();
            (collections.active.clone(), collections.active().frozen.len())
        };
        if count == 0 {
            return;
        }
        let body = format!("“{name}” switches back to the {}, as Steam has them now, and the copies are deleted.", plural(count, "Workshop mod", "Workshop mods"));
        let dialog = adw::AlertDialog::new(Some(&format!("Unfreeze “{name}”?")), Some(&body));
        dialog.add_responses(&[("cancel", "_Cancel"), ("unfreeze", "_Unfreeze")]);
        dialog.set_default_response(Some("unfreeze"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let (Some(ui), "unfreeze") = (weak.upgrade(), response) else { return };
            ui.unfreeze(&name);
        });
        dialog.present(Some(&self.window));
    }

    fn unfreeze(self: &Rc<Self>, name: &str) {
        if self.state.borrow().dirty && !self.save() {
            return;
        }
        let (frozen, mods) = {
            let mut collections = self.collections.borrow_mut();
            let Some(c) = collections.get_mut(name) else { return };
            let frozen = std::mem::take(&mut c.frozen);
            let map: HashMap<&str, &str> = frozen.iter().map(|(copy, f)| (copy.as_str(), f.workshop.as_str())).collect();
            freeze::swap(&mut c.mods, &map);
            (frozen, c.mods.clone())
        };
        if let Err(e) = freeze::unfreeze(&self.paths.user_dir, &frozen) {
            // The collection is back on the Workshop mods either way; only
            // copies may be left over.
            self.toast_error_message(&format!("Some frozen copies couldn't be deleted: {e}"));
        }
        self.save_collections();
        self.rebuild_collection_menu();
        let (name, count) = (name.to_owned(), frozen.len());
        self.reload(move |ui| {
            ui.apply_collection_mods(&mods);
            ui.add_toast(adw::Toast::new(&format!("Unfroze “{name}”: back to {}", plural(count, "Workshop mod", "Workshop mods"))));
        });
    }

    /// The active collection's frozen mods that Steam has updated since,
    /// by name.
    pub(super) fn frozen_updates(&self) -> Vec<String> {
        let frozen: BTreeMap<String, Frozen> = self.collections.borrow().active().frozen.clone();
        if frozen.is_empty() {
            return Vec::new();
        }
        let times = steam::updated_times(&steam::workshop_dirs(&glib::home_dir(), steam::STELLARIS_APP_ID));
        let state = self.state.borrow();
        frozen
            .iter()
            .filter(|(_, f)| matches!((f.updated, times.get(&f.steam_id)), (Some(then), Some(&now)) if now > then))
            .map(|(copy, f)| state.mods.iter().find(|m| m.id == *copy).map_or_else(|| f.steam_id.to_string(), |m| m.descriptor.name.clone()))
            .collect()
    }

    /// Lists the frozen mods updated on the Workshop since, offering to
    /// unfreeze.
    pub(super) fn show_frozen_updates(self: &Rc<Self>) {
        let updated = self.frozen_updates();
        let name = self.collections.borrow().active.clone();
        let body = format!(
            "Steam has newer versions of {} than “{name}” uses:\n\n{}\n\nThe collection keeps its copies until you unfreeze it.",
            if updated.len() == 1 { "this mod" } else { "these mods" },
            updated.iter().map(|n| format!("• {n}")).collect::<Vec<_>>().join("\n")
        );
        let dialog = adw::AlertDialog::new(Some("Updated on the Workshop"), Some(&body));
        dialog.add_responses(&[("close", "_Close"), ("unfreeze", "_Unfreeze…")]);
        dialog.set_close_response("close");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let (Some(ui), "unfreeze") = (weak.upgrade(), response) {
                ui.unfreeze_collection();
            }
        });
        dialog.present(Some(&self.window));
    }
}
