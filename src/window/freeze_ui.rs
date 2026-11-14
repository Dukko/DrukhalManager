//! Freeze This Collection: copies its Workshop mods into local ones so
//! Steam updates can't change a campaign, and Unfreeze to go back.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
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
            return self.toast_error_message(&tf!("“{name}” has no Workshop mods to freeze", name = &name));
        }
        let size = glib::format_size(freeze::size(&to_copy.iter().map(|m| m.3.as_path()).collect::<Vec<_>>()));
        let body = tf!("Copies its {mods} ({size}) into local mods, and switches “{name}” to the copies, so Steam updates can't change it in the middle of a campaign.\n\nFixes their authors publish are missed too, until you unfreeze it. Unfreezing switches back to the Workshop mods and deletes the copies.", mods = tnf!("{count} Workshop mod", "{count} Workshop mods", count = to_copy.len()), size = size, name = &name);
        let dialog = adw::AlertDialog::new(Some(&tf!("Freeze “{name}”?", name = &name)), Some(&body));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("freeze", &t!("_Freeze"))]);
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
        let toast = adw::Toast::builder().title(tf!("Copying {mods}…", mods = tnf!("{count} mod", "{count} mods", count = to_copy.len()))).timeout(0).build();
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
            .unwrap_or_else(|_| Err(t!("Copying failed")));
            toast.dismiss();
            let Some(ui) = weak.upgrade() else { return };
            let frozen = match copied {
                Ok(f) => f,
                Err(e) => return ui.toast_error_message(&tf!("Could not freeze “{name}”, so nothing changed: {e}", name = &name, e = e)),
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
                ui.add_toast(adw::Toast::new(&tf!("Froze “{name}”: {mods} copied", name = &name, mods = tnf!("{count} Workshop mod", "{count} Workshop mods", count = count))));
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
        let body = tf!("“{name}” switches back to the {mods}, as Steam has them now, and the copies are deleted.", name = &name, mods = tnf!("{count} Workshop mod", "{count} Workshop mods", count = count));
        let dialog = adw::AlertDialog::new(Some(&tf!("Unfreeze “{name}”?", name = &name)), Some(&body));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("unfreeze", &t!("_Unfreeze"))]);
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
            self.toast_error_message(&tf!("Some frozen copies couldn't be deleted: {e}", e = e));
        }
        self.save_collections();
        self.rebuild_collection_menu();
        let (name, count) = (name.to_owned(), frozen.len());
        self.reload(move |ui| {
            ui.apply_collection_mods(&mods);
            ui.add_toast(adw::Toast::new(&tf!("Unfroze “{name}”: back to {mods}", name = &name, mods = tnf!("{count} Workshop mod", "{count} Workshop mods", count = count))));
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
        let mods = if updated.len() == 1 { t!("this mod") } else { t!("these mods") };
        let list = updated.iter().map(|n| format!("• {n}")).collect::<Vec<_>>().join("\n");
        let body = tf!("Steam has newer versions of {mods} than “{name}” uses:\n\n{list}\n\nThe collection keeps its copies until you unfreeze it.", mods = &mods, name = &name, list = &list);
        let dialog = adw::AlertDialog::new(Some(&t!("Updated on the Workshop")), Some(&body));
        dialog.add_responses(&[("close", &t!("_Close")), ("unfreeze", &t!("_Unfreeze…"))]);
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
