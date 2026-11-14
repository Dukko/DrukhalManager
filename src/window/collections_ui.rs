//! The collection switcher and its actions: new, duplicate, rename, delete,
//! import from the game, a launcher playset or a file, and export.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::merge::{bundle, collection};
use crate::mods::archive;
use crate::mods::collections::{self, Collections};
use crate::mods::load_order::DlcLoad;
use crate::paradox::{launcher_db, workshop};

impl Window {
    /// Loads the collections (creating the default one from the current
    /// load order on first run), adds the actions, and checks whether the
    /// game's load order still matches the active collection.
    pub(super) fn setup_collections(self: &Rc<Self>) {
        let path = collections::path();
        let current = self.state.borrow().enabled_ids();
        let loaded = match Collections::load(&path) {
            Ok(Some(c)) => c,
            Ok(None) => {
                let c = Collections::starting_with(current.clone());
                if let Err(e) = c.save(&path) {
                    self.toast_error(&tf!("Could not save collections: {error}", error = e));
                }
                c
            }
            Err(e) => {
                // Keep the unreadable file for inspection instead of
                // overwriting it.
                let _ = crate::platform::rename(&path, path.with_extension("json.unreadable"));
                self.toast_error(&tf!("Collections could not be read ({error}) and were reset; the old file was kept next to it", error = e));
                Collections::starting_with(current.clone())
            }
        };
        self.collections.replace(loaded);

        let action = |name: &str, f: fn(&Rc<Window>)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    f(&ui);
                }
            });
            self.window.add_action(&a);
        };
        action("collection-new", |ui| ui.new_collection(false));
        action("collection-duplicate", |ui| ui.new_collection(true));
        action("collection-rename", Window::rename_collection);
        action("collection-delete", Window::delete_collection);
        action("collection-import-game", Window::import_game);
        action("collection-import-playset", Window::import_playset);
        action("collection-import-file", Window::import_file);
        action("collection-import-workshop", Window::import_workshop);
        action("collection-merge", Window::merge_collection);
        action("collection-freeze", Window::freeze_collection);
        action("collection-unfreeze", Window::unfreeze_collection);
        action("collection-move-patch", Window::move_patch);
        action("collection-export", Window::export_collection);
        action("collection-export-launcher", Window::export_launcher);
        action("collection-export-bundle", Window::export_bundle);
        action("collection-copy", Window::copy_collection);
        action("collection-paste", Window::paste_collection);

        let active = self.collections.borrow().active.clone();
        let switch = gio::SimpleAction::new_stateful("collection", Some(glib::VariantTy::STRING), &active.to_variant());
        let weak = Rc::downgrade(self);
        switch.connect_activate(move |_, name| {
            if let (Some(ui), Some(name)) = (weak.upgrade(), name.and_then(|n| n.str().map(str::to_owned))) {
                ui.switch_collection(name);
            }
        });
        self.window.add_action(&switch);

        let weak = Rc::downgrade(self);
        self.restore_banner.connect_button_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let mods = ui.collections.borrow().active().mods.clone();
                ui.apply_collection_mods(&mods);
            }
        });
        let saved = self.collections.borrow().active().installed(&self.state.borrow().mods);
        if saved != current && !saved.is_empty() {
            self.restore_banner.set_title(&tf!("The game's load order doesn't match the collection “{collection}”. Something else changed it, like the Paradox Launcher.", collection = active));
            self.restore_banner.set_revealed(true);
        }
        self.rebuild_collection_menu();
    }

    fn toast_error(&self, message: &str) {
        self.add_toast(adw::Toast::builder().title(message).timeout(0).build());
    }

    /// Moves the active collection's merged patch to the Extra Mod Folder,
    /// or back to the game's mod folder. Its descriptor follows, so the
    /// game, backups and export keep finding it.
    fn move_patch(self: &Rc<Self>) {
        let from = self.patch_target();
        if !from.root(&self.paths.user_dir).is_dir() {
            return self.toast_error_message(&t!("This collection has no merged patch yet"));
        }
        let to_dir = if from.dir.is_some() {
            None
        } else {
            match self.settings.borrow().game.extra_mod_dir.clone() {
                Some(dir) => Some(dir),
                None => return self.toast_error_message(&t!("Choose an Extra Mod Folder in Preferences → Game first")),
            }
        };
        let to = from.clone().in_dir(to_dir.clone());
        // Closed while it moves, and opened again from its new place.
        self.patch.replace(None);
        let user_dir = self.paths.user_dir.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let moved = {
                let (from, to) = (from.clone(), to.clone());
                gio::spawn_blocking(move || crate::merge::patch::relocate(&user_dir, &from, &to)).await.unwrap_or_else(|_| Err(std::io::Error::other(t!("the move failed"))))
            };
            let Some(ui) = weak.upgrade() else { return };
            if let Err(e) = moved {
                return ui.toast_error_message(&tf!("Could not move the merged patch: {error}", error = e));
            }
            {
                let mut collections = ui.collections.borrow_mut();
                let active = collections.active.clone();
                if let Some(c) = collections.get_mut(&active) {
                    c.patch_dir = to_dir.clone();
                }
            }
            ui.save_collections();
            ui.rebuild_collection_menu();
            let place = match &to_dir {
                Some(dir) => dir.display().to_string(),
                None => t!("the game's mod folder"),
            };
            // The mod list reads where the patch now is.
            ui.reload(move |ui| ui.add_toast(adw::Toast::new(&tf!("Moved {name} to {place}", name = to.name, place = place))));
        });
    }

    pub(super) fn rebuild_collection_menu(&self) {
        let collections = self.collections.borrow();
        let menu = gio::Menu::new();
        let list = gio::Menu::new();
        for c in &collections.list {
            let item = gio::MenuItem::new(Some(&c.name), None);
            item.set_action_and_target_value(Some("win.collection"), Some(&c.name.to_variant()));
            list.append_item(&item);
        }
        menu.append_section(Some(&t!("Collections")), &list);
        let manage = gio::Menu::new();
        manage.append(Some(&t!("_New Collection…")), Some("win.collection-new"));
        manage.append(Some(&t!("_Duplicate…")), Some("win.collection-duplicate"));
        manage.append(Some(&t!("_Rename…")), Some("win.collection-rename"));
        if collections.list.len() > 1 {
            manage.append(Some(&t!("De_lete")), Some("win.collection-delete"));
        }
        menu.append_section(None, &manage);
        let share = gio::Menu::new();
        let import = gio::Menu::new();
        import.append(Some(&t!("From the _Game's Load Order")), Some("win.collection-import-game"));
        import.append(Some(&t!("From a Launcher _Playset…")), Some("win.collection-import-playset"));
        import.append(Some(&t!("From a _File…")), Some("win.collection-import-file"));
        import.append(Some(&t!("From a _Workshop Collection…")), Some("win.collection-import-workshop"));
        share.append_submenu(Some(&t!("_Import")), &import);
        let export = gio::Menu::new();
        export.append(Some(&t!("To a DrukhalManager _File…")), Some("win.collection-export"));
        export.append(Some(&t!("With the Merged _Patch…")), Some("win.collection-export-bundle"));
        export.append(Some(&t!("To a Paradox _Launcher File…")), Some("win.collection-export-launcher"));
        share.append_submenu(Some(&t!("_Export")), &export);
        share.append(Some(&t!("_Copy Load Order")), Some("win.collection-copy"));
        share.append(Some(&t!("_Paste Load Order")), Some("win.collection-paste"));
        share.append(Some(&t!("_Merge into One Mod…")), Some("win.collection-merge"));
        if collections.active().frozen.is_empty() {
            share.append(Some(&t!("_Freeze This Collection…")), Some("win.collection-freeze"));
        } else {
            share.append(Some(&t!("Un_freeze This Collection…")), Some("win.collection-unfreeze"));
        }
        menu.append_section(None, &share);
        // Only once there's an Extra Mod Folder, or to bring it back.
        let active = collections.active();
        let has_extra = self.settings.borrow().game.extra_mod_dir.is_some();
        if active.patch.is_some() && (active.patch_dir.is_some() || has_extra) {
            let place = gio::Menu::new();
            place.append(Some(&if active.patch_dir.is_some() { t!("Move Merged Patch to the _Mod Folder") } else { t!("Move Merged Patch to the E_xtra Mod Folder") }), Some("win.collection-move-patch"));
            menu.append_section(None, &place);
        }

        self.collection_button.set_menu_model(Some(&menu));
        self.collection_button.set_label(&collections.active);
        if let Some(action) = self.window.lookup_action("collection") {
            action.change_state(&collections.active.to_variant());
        }
    }

    /// Saves the collections, and opens the active one's merged patch.
    pub(super) fn save_collections(&self) {
        if let Err(e) = self.collections.borrow().save(&collections::path()) {
            self.toast_error(&tf!("Could not save collections: {error}", error = e));
        }
        if let Err(e) = self.sync_patch() {
            self.toast_error(&e);
        }
    }

    /// Makes `mods` the load order and saves it everywhere. Mods that
    /// aren't installed keep their place in the collection.
    pub(super) fn apply_collection_mods(self: &Rc<Self>, mods: &[String]) {
        let missing = self.state.borrow_mut().set_enabled_ids(mods);
        self.restore_banner.set_revealed(false);
        if self.save() && !missing.is_empty() {
            self.toast_error(&tnf!(
                "{count} mod in this collection isn't installed; it's kept under Not Installed",
                "{count} mods in this collection aren't installed; they're kept under Not Installed",
                count = missing.len()
            ));
        }
    }

    /// Takes a mod that isn't installed out of the active collection.
    pub(super) fn remove_placeholder(self: &Rc<Self>, id: &str) {
        {
            let mut collections = self.collections.borrow_mut();
            let active = collections.active.clone();
            if let Some(c) = collections.get_mut(&active) {
                c.mods.retain(|m| m != id);
                c.names.remove(id);
            }
        }
        self.save_collections();
        self.refresh();
    }

    pub(super) fn switch_collection(self: &Rc<Self>, name: String) {
        if name == self.collections.borrow().active {
            return;
        }
        let go = {
            let name = name.clone();
            move |ui: &Rc<Window>| {
                ui.collections.borrow_mut().active = name.clone();
                let mods = ui.collections.borrow().active().mods.clone();
                ui.save_collections();
                ui.rebuild_collection_menu();
                ui.apply_collection_mods(&mods);
            }
        };
        if !self.state.borrow().dirty {
            go(self);
            return;
        }
        let current = self.collections.borrow().active.clone();
        let dialog = adw::AlertDialog::new(
            Some(&t!("Save Changes?")),
            Some(&tf!("“{current}” has unsaved changes. Save them before switching to “{next}”?", current = current, next = name)),
        );
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("discard", &t!("_Discard")), ("save", &t!("_Save"))]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            match response {
                "save" if ui.save() => go(&ui),
                "discard" => go(&ui),
                _ => ui.rebuild_collection_menu(), // put the radio back
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Asks for a name, then calls `done` with it.
    fn ask_name(self: &Rc<Self>, title: &str, initial: &str, confirm: &str, done: impl Fn(&Rc<Window>, String) + 'static) {
        self.ask_text(title, &t!("Name"), initial, confirm, done);
    }

    /// Asks for one line of text, then calls `done` with it.
    pub(super) fn ask_text(self: &Rc<Self>, title: &str, field: &str, initial: &str, confirm: &str, done: impl Fn(&Rc<Window>, String) + 'static) {
        let entry = adw::EntryRow::builder().title(field).text(initial).build();
        let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
        list.append(&entry);
        let dialog = adw::AlertDialog::new(Some(title), None);
        dialog.set_extra_child(Some(&list));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("ok", confirm)]);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("cancel");
        entry.connect_entry_activated(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.emit_by_name::<()>("response", &[&"ok"])
        ));
        let weak = Rc::downgrade(self);
        let field = entry.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "ok"
                && let Some(ui) = weak.upgrade()
            {
                done(&ui, field.text().trim().to_owned());
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    /// Creates a collection, empty or holding the current list, and switches
    /// to it. A duplicate carries any unsaved changes with it.
    fn new_collection(self: &Rc<Self>, duplicate: bool) {
        let active = self.collections.borrow().active.clone();
        let suggested = if duplicate { self.collections.borrow().unique_name(&tf!("{name} copy", name = active)) } else { self.collections.borrow().unique_name(&t!("New Collection")) };
        let title = if duplicate { t!("Duplicate Collection") } else { t!("New Collection") };
        self.ask_name(&title, &suggested, &t!("_Create"), move |ui, name| {
            // A copy gets its own patch once something is merged in it.
            let mods: Vec<String> = if duplicate { ui.state.borrow().enabled_ids().into_iter().filter(|id| !crate::merge::patch::is_patch(id)).collect() } else { Vec::new() };
            let name = ui.collections.borrow_mut().add(&name, mods.clone());
            ui.collections.borrow_mut().active = name.clone();
            ui.save_collections();
            ui.rebuild_collection_menu();
            ui.apply_collection_mods(&mods);
            ui.add_toast(adw::Toast::new(&tf!("Created “{name}”", name = name)));
        });
    }

    fn rename_collection(self: &Rc<Self>) {
        let active = self.collections.borrow().active.clone();
        self.ask_name(&t!("Rename Collection"), &active.clone(), &t!("_Rename"), move |ui, name| {
            let Some(new) = ui.collections.borrow_mut().rename(&active, &name) else { return };
            if new == active {
                return;
            }
            ui.save_collections();
            ui.rebuild_collection_menu();
            if let Err(e) = launcher_db::rename(&ui.paths.user_dir, &collections::playset_name(&active), &collections::playset_name(&new)) {
                ui.toast_error(&tf!("Renamed, but the Paradox Launcher's playset kept its old name: {error}", error = e));
            }
        });
    }

    fn delete_collection(self: &Rc<Self>) {
        let active = self.collections.borrow().active.clone();
        let patch = {
            let collections = self.collections.borrow();
            let c = collections.active();
            c.patch.clone().map(|folder| crate::merge::patch::Target::new(&folder, &active).in_dir(c.patch_dir.clone())).filter(|t| t.root(&self.paths.user_dir).is_dir())
        };
        let body = if patch.is_some() {
            t!("The collection's load order and its merged patch are deleted; the patch is kept in DrukhalManager's backups folder. Its playset stays in the Paradox Launcher, and your mods aren't touched.")
        } else {
            t!("The collection's load order is forgotten. Its playset stays in the Paradox Launcher, and your mods aren't touched.")
        };
        let dialog = adw::AlertDialog::new(Some(&tf!("Delete “{collection}”?", collection = &active)), Some(&body));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("delete", &t!("_Delete"))]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            if response != "delete" || ui.collections.borrow().list.len() <= 1 {
                return;
            }
            if let Some(target) = &patch {
                // Backed up first; if that fails, nothing is deleted.
                let deleted = crate::merge::snapshot::take(&ui.paths.user_dir, target, &ui.paths.backup_dir, crate::merge::snapshot::BEFORE_DELETING).and_then(|_| crate::merge::patch::delete(&ui.paths.user_dir, target));
                if let Err(e) = deleted {
                    return ui.toast_error(&tf!("Could not delete “{collection}”'s merged patch, so the collection was kept: {error}", collection = &active, error = e));
                }
            }
            ui.collections.borrow_mut().remove(&active);
            let mods = ui.collections.borrow().active().mods.clone();
            ui.save_collections();
            ui.rebuild_collection_menu();
            ui.state.borrow_mut().dirty = false;
            ui.apply_collection_mods(&mods);
        });
        dialog.present(Some(&self.window));
    }

    /// Makes a collection of the load order in the game's `dlc_load.json`,
    /// e.g. after the Paradox Launcher or another tool set it.
    fn import_game(self: &Rc<Self>) {
        let ids = match DlcLoad::read(&self.paths.user_dir) {
            Ok(d) if !d.enabled_mods.is_empty() => d.enabled_mods,
            Ok(_) => return self.toast_error(&t!("The game's load order has no mods")),
            Err(e) => return self.toast_error(&tf!("Could not read the game's load order: {error}", error = e)),
        };
        let missing = {
            let state = self.state.borrow();
            ids.iter().filter(|id| !state.mods.iter().any(|m| &m.id == *id)).cloned().collect()
        };
        self.add_and_switch(collections::Collection::new(&t!("Game Load Order"), ids), missing);
    }

    fn import_playset(self: &Rc<Self>) {
        let playsets = match launcher_db::playsets(&self.paths.user_dir) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => return self.toast_error(&t!("The Paradox Launcher has no playsets to import")),
            Err(e) => return self.toast_error(&tf!("Could not read the Paradox Launcher's playsets: {error}", error = e)),
        };
        let names: Vec<String> = playsets.iter().map(|(n, mods)| tf!("{name} ({mods})", name = n, mods = tnf!("{count} mod", "{count} mods", count = mods.len()))).collect();
        let choice = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let dialog = adw::AlertDialog::new(Some(&t!("Import Playset")), Some(&t!("Creates a collection with the playset's mods, in its order.")));
        dialog.set_extra_child(Some(&choice));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("import", &t!("_Import"))]);
        dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("import"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let (Some(ui), "import") = (weak.upgrade(), response) else { return };
            let Some((name, mods)) = playsets.get(choice.selected() as usize) else { return };
            let name = name.strip_prefix("DrukhalManager – ").unwrap_or(name);
            ui.add_and_switch(collections::Collection::new(name, mods.clone()), Vec::new());
        });
        dialog.present(Some(&self.window));
    }

    /// Adds a collection, switches to it, and reports mods that weren't
    /// found. Returns the name it was added under.
    fn add_and_switch(self: &Rc<Self>, collection: collections::Collection, missing: Vec<String>) -> Option<String> {
        if self.state.borrow().dirty {
            // Unsaved edits belong to the current collection; keep them.
            if !self.save() {
                return None;
            }
        }
        let mods = collection.mods.clone();
        let name = self.collections.borrow_mut().add_collection(collection);
        self.collections.borrow_mut().active = name.clone();
        self.save_collections();
        self.rebuild_collection_menu();
        let installed = self.state.borrow_mut().set_enabled_ids(&mods).is_empty();
        self.restore_banner.set_revealed(false);
        if !self.save() {
            return Some(name);
        }
        if missing.is_empty() && installed {
            self.add_toast(adw::Toast::new(&tf!("Imported “{name}”", name = &name)));
        } else if !missing.is_empty() {
            self.toast_error(&tnf!(
                "Imported “{name}”. {count} mod isn't installed, so it's listed under Not Installed: {mods}",
                "Imported “{name}”. {count} mods aren't installed, so they're listed under Not Installed: {mods}",
                count = missing.len(),
                name = &name,
                mods = missing.join(", ")
            ));
        }
        Some(name)
    }

    fn import_workshop(self: &Rc<Self>) {
        self.ask_text(&t!("Import Workshop Collection"), &t!("Collection link or ID"), "", &t!("_Import"), |ui, text| {
            let Some(id) = workshop::parse_id(&text) else {
                return ui.toast_error(&t!("That isn't a Workshop collection link or ID"));
            };
            let toast = adw::Toast::builder().title(t!("Reading the collection from Steam…")).timeout(0).build();
            ui.show_progress(&toast);
            let weak = Rc::downgrade(ui);
            glib::spawn_future_local(async move {
                let fetched = gio::spawn_blocking(move || workshop::fetch(id)).await.unwrap_or_else(|_| Err(t!("The request failed")));
                toast.dismiss();
                let Some(ui) = weak.upgrade() else { return };
                let shared = match fetched {
                    Ok(shared) => shared,
                    Err(e) => return ui.toast_error(&tf!("Could not import the collection: {error}", error = e)),
                };
                let (collection, missing) = collections::import(&shared, &ui.state.borrow().mods);
                if collection.mods.is_empty() {
                    return ui.toast_error(&tf!("“{name}” has no mods", name = shared.name));
                }
                if let Some(name) = ui.add_and_switch(collection, Vec::new())
                    && !missing.is_empty()
                {
                    ui.offer_missing(id, name, shared);
                }
            });
        });
    }

    /// Lists the collection's mods that aren't installed, with links to
    /// subscribe, and reloads once they're downloaded.
    fn offer_missing(self: &Rc<Self>, collection_id: u64, name: String, shared: collections::Shared) {
        let missing: Vec<&collections::SharedMod> = {
            let installed = &self.state.borrow().mods;
            shared.mods.iter().filter(|s| collections::find(s, installed).is_none()).collect()
        };
        let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
        for m in &missing {
            let row = adw::ActionRow::builder().title(&m.name).use_markup(false).build();
            if let Some(id) = m.steam_id {
                let open = gtk::Button::builder().icon_name("adw-external-link-symbolic").tooltip_text(t!("Open on the Steam Workshop")).valign(gtk::Align::Center).css_classes(["flat"]).build();
                let window = self.window.downgrade();
                open.connect_clicked(move |_| open_page(&window, id));
                row.add_suffix(&open);
                row.set_activatable_widget(Some(&open));
            }
            list.append(&row);
        }
        let all = gtk::Button::builder().label(t!("Open the Collection on the Workshop")).halign(gtk::Align::Center).css_classes(["pill"]).build();
        let window = self.window.downgrade();
        all.connect_clicked(move |_| open_page(&window, collection_id));
        let scroll = gtk::ScrolledWindow::builder().child(&list).propagate_natural_height(true).max_content_height(320).hscrollbar_policy(gtk::PolicyType::Never).build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&scroll);
        content.append(&all);

        let dialog = adw::AlertDialog::new(
            Some(&tnf!("{count} Mod Not Installed", "{count} Mods Not Installed", count = missing.len())),
            Some(&tf!("Subscribe to them on the Steam Workshop. Once Steam has downloaded them, reload to add them to “{name}”.", name = &name)),
        );
        dialog.set_extra_child(Some(&content));
        dialog.add_responses(&[("later", &t!("_Later")), ("reload", &t!("_Reload Mods"))]);
        dialog.set_response_appearance("reload", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("reload"));
        dialog.set_close_response("later");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let (Some(ui), "reload") = (weak.upgrade(), response) else { return };
            let (name, shared) = (name.clone(), shared.clone());
            ui.reload(move |ui| ui.refill_collection(collection_id, name, shared));
        });
        dialog.present(Some(&self.window));
    }

    /// After a reload, matches the imported collection again so newly
    /// downloaded mods join it.
    fn refill_collection(self: &Rc<Self>, collection_id: u64, name: String, shared: collections::Shared) {
        if self.collections.borrow().get(&name).is_none() {
            return; // deleted meanwhile
        }
        let (fresh, missing) = collections::import(&shared, &self.state.borrow().mods);
        let mods = fresh.mods.clone();
        if let Some(c) = self.collections.borrow_mut().get_mut(&name) {
            (c.mods, c.names) = (fresh.mods, fresh.names);
        }
        self.save_collections();
        if self.collections.borrow().active == name {
            if self.state.borrow().dirty {
                // Don't throw away edits; let the user choose.
                self.restore_banner.set_title(&tf!("“{name}” has new mods from the Workshop that aren't in the load order yet.", name = &name));
                self.restore_banner.set_revealed(true);
            } else {
                self.apply_collection_mods(&mods);
            }
        }
        if missing.is_empty() {
            self.add_toast(adw::Toast::new(&tf!("Every mod in “{name}” is installed", name = name)));
        } else {
            self.offer_missing(collection_id, name, shared);
        }
    }

    fn import_file(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&t!("Collections (DrukhalManager or Paradox Launcher)")));
        filter.add_pattern("*.json");
        filter.add_pattern("*.zip");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(t!("Import Collection")).filters(&filters).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.window), None::<&gio::Cancellable>, move |file| {
            let (Some(ui), Ok(file)) = (weak.upgrade(), file) else { return };
            let Some(path) = file.path() else { return };
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                return ui.import_bundle(&path);
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => return ui.toast_error(&tf!("Could not read the file: {error}", error = e)),
            };
            let shared = match serde_json::from_str::<collections::Shared>(&text) {
                Ok(s) => s,
                Err(e) => match collections::from_launcher_json(&text) {
                    Some(s) => s,
                    None => return ui.toast_error(&tf!("Not a collection or playset file: {error}", error = e)),
                },
            };
            ui.adopt_notes(&shared);
            let (collection, missing) = collections::import(&shared, &ui.state.borrow().mods);
            ui.add_and_switch(collection, missing);
        });
    }

    /// Copies the load order's files into one local mod (Irony's "Merge
    /// (Basic)") and switches to a collection holding just that mod.
    fn merge_collection(self: &Rc<Self>) {
        let active = self.collections.borrow().active.clone();
        let (inputs, left_out): (Vec<(std::path::PathBuf, Vec<String>)>, Vec<String>) = {
            let state = self.state.borrow();
            let mut left_out = Vec::new();
            let inputs = state
                .enabled
                .iter()
                .map(|&i| &state.mods[i])
                .filter_map(|m| match &m.content {
                    Some(content) => Some((content.clone(), m.descriptor.replace_path.clone())),
                    None => {
                        left_out.push(super::display_name(m).to_owned());
                        None
                    }
                })
                .collect();
            (inputs, left_out)
        };
        if inputs.is_empty() {
            return self.toast_error(&t!("Enable some mods to merge first"));
        }
        let name = collection::mod_name(&active, self.settings.borrow().game.merged_name.as_deref());
        let mut body = tnf!(
            "Copies the files of {count} mod in the load order into a new local mod, “{name}”, the way the game would load them, and switches to a collection holding just that mod. Your mods and “{active}” aren't changed. The merge takes as much disk space as the mods.",
            "Copies the files of {count} mods in the load order into a new local mod, “{name}”, the way the game would load them, and switches to a collection holding just that mod. Your mods and “{active}” aren't changed. The merge takes as much disk space as the mods.",
            count = inputs.len(),
            name = &name,
            active = &active
        );
        if self.collections.borrow().get(&name).is_some() {
            body.push_str(&tf!("\n\nIt replaces the earlier merge in “{name}”.", name = &name));
        }
        if !left_out.is_empty() {
            body.push_str(&tf!("\n\nThese mods' files weren't found, so they're left out: {mods}", mods = left_out.join(", ")));
        }
        let dialog = adw::AlertDialog::new(Some(&t!("Merge into One Mod?")), Some(&body));
        let as_zip = gtk::CheckButton::builder()
            .label(t!("Write One Zip File"))
            .tooltip_text(t!("Easier to move, back up and share than thousands of files. The game reads it like a folder."))
            .active(self.settings.borrow().game.merge_to_zip)
            .halign(gtk::Align::Center)
            .build();
        dialog.set_extra_child(Some(&as_zip));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("merge", &t!("_Merge"))]);
        dialog.set_response_appearance("merge", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("merge"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let (Some(ui), "merge") = (weak.upgrade(), response) else { return };
            let output = if as_zip.is_active() { collection::Output::Zip } else { collection::Output::Folder };
            if ui.settings.borrow().game.merge_to_zip != as_zip.is_active() {
                ui.settings.borrow_mut().game.merge_to_zip = as_zip.is_active();
                ui.save_settings("the merge format");
            }
            let toast = adw::Toast::builder().title(tf!("Merging into “{name}”…", name = &name)).timeout(0).build();
            ui.show_progress(&toast);
            let user_dir = ui.paths.user_dir.clone();
            let out_dir = ui.settings.borrow().game.merge_dir(&user_dir);
            let version = crate::merge::patch::supported_version(ui.game.as_ref().and_then(|g| g.version.as_deref()));
            let (active, inputs, name) = (active.clone(), inputs.clone(), name.clone());
            let mod_name = name.clone();
            let weak = Rc::downgrade(&ui);
            glib::spawn_future_local(async move {
                let merged = gio::spawn_blocking(move || {
                    // Zipped mods are copied from their unpacked files.
                    let inputs = inputs
                        .into_iter()
                        .map(|(content, replace_path)| Ok(collection::Input { root: archive::files(&content)?, replace_path }))
                        .collect::<std::io::Result<Vec<_>>>()?;
                    collection::write(&user_dir, &out_dir, &active, &mod_name, &version, &inputs, output)
                })
                .await
                .unwrap_or_else(|_| Err(std::io::Error::other(t!("the merge failed"))));
                toast.dismiss();
                let Some(ui) = weak.upgrade() else { return };
                let merged = match merged {
                    Ok(merged) => merged,
                    Err(e) => return ui.toast_error(&tf!("Could not merge the collection: {error}", error = e)),
                };
                ui.add_toast(adw::Toast::new(&tnf!("Merged {count} file into “{name}”", "Merged {count} files into “{name}”", count = merged.files, name = &name)));
                // The new mod shows up once the mod list is read again.
                ui.reload(move |ui| ui.use_merged(name, merged.id));
            });
        });
        dialog.present(Some(&self.window));
    }

    /// Puts the merged mod alone in its collection and switches to it.
    fn use_merged(self: &Rc<Self>, name: String, id: String) {
        let exists = self.collections.borrow().get(&name).is_some();
        if exists {
            self.collections.borrow_mut().set_mods(&name, vec![id.clone()]);
        } else {
            self.collections.borrow_mut().add(&name, vec![id.clone()]);
        }
        self.save_collections();
        self.rebuild_collection_menu();
        if self.collections.borrow().active == name {
            self.apply_collection_mods(&[id]);
        } else {
            self.switch_collection(name);
        }
    }

    fn export_collection(self: &Rc<Self>) {
        self.ask_include_notes(|ui, notes| ui.export_collection_file(notes));
    }

    /// Asks whether to share the user's notes on the active collection's
    /// mods, then calls `go` with the answer. Doesn't ask when none has one.
    fn ask_include_notes(self: &Rc<Self>, go: impl Fn(&Rc<Window>, bool) + 'static) {
        let name = self.collections.borrow().active.clone();
        let noted = {
            let settings = self.settings.borrow();
            self.shared_collection(&name, false).mods.iter().filter(|m| settings.notes.contains_key(&m.id)).count()
        };
        if noted == 0 {
            return go(self, false);
        }
        let body = tnf!(
            "{count} mod in “{name}” has your notes. Whoever imports the file sees them on mods they haven't noted themselves.",
            "{count} mods in “{name}” have your notes. Whoever imports the file sees them on mods they haven't noted themselves.",
            count = noted,
            name = name
        );
        let dialog = adw::AlertDialog::new(Some(&t!("Include Your Notes?")), Some(&body));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("without", &t!("_Leave Out")), ("with", &t!("_Include"))]);
        dialog.set_response_appearance("with", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("with"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            match response {
                "with" => go(&ui, true),
                "without" => go(&ui, false),
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    fn export_collection_file(self: &Rc<Self>, notes: bool) {
        let name = self.collections.borrow().active.clone();
        let dialog = gtk::FileDialog::builder().title(t!("Export Collection")).initial_name(format!("{name}.json")).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), None::<&gio::Cancellable>, move |file| {
            let (Some(ui), Ok(file)) = (weak.upgrade(), file) else { return };
            let Some(path) = file.path() else { return };
            let shared = ui.shared_collection_with(&name, notes);
            match serde_json::to_vec_pretty(&shared).map_err(|e| e.to_string()).and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string())) {
                Ok(()) => ui.add_toast(adw::Toast::new(&tnf!("Exported “{name}” with {count} mod", "Exported “{name}” with {count} mods", count = shared.mods.len(), name = name))),
                Err(e) => ui.toast_error(&tf!("Could not export: {error}", error = e)),
            }
        });
    }
}

impl Window {
    /// Exports the load order with the merged patch in one zip, so a friend
    /// gets the same merges, e.g. to play together.
    fn export_bundle(self: &Rc<Self>) {
        self.ask_include_notes(|ui, notes| ui.export_bundle_file(notes));
    }

    fn export_bundle_file(self: &Rc<Self>, notes: bool) {
        let name = self.collections.borrow().active.clone();
        let dialog = gtk::FileDialog::builder().title(t!("Export with the Merged Patch")).initial_name(format!("{name}.zip")).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), None::<&gio::Cancellable>, move |file| {
            let (Some(ui), Ok(file)) = (weak.upgrade(), file) else { return };
            let Some(path) = file.path() else { return };
            let shared = ui.shared_collection_with(&name, notes);
            match bundle::export(&path, &shared, &ui.paths.user_dir, &ui.patch_target()) {
                Ok(0) => ui.add_toast(adw::Toast::new(&tnf!("Exported “{name}” with {count} mod; there's no merged patch yet", "Exported “{name}” with {count} mods; there's no merged patch yet", count = shared.mods.len(), name = name))),
                Ok(files) => ui.add_toast(adw::Toast::new(&tf!(
                    "{mods} and the merged patch ({files})",
                    mods = tnf!("Exported “{name}” with {count} mod", "Exported “{name}” with {count} mods", count = shared.mods.len(), name = name),
                    files = tnf!("{count} file", "{count} files", count = files)
                ))),
                Err(e) => ui.toast_error(&tf!("Could not export: {error}", error = e)),
            }
        });
    }

    /// Imports a collection exported with its merged patch: asks before
    /// replacing the patch, then imports the load order.
    fn import_bundle(self: &Rc<Self>, path: &std::path::Path) {
        let bundle = match bundle::read(path) {
            Ok(b) => b,
            Err(e) => return self.toast_error(&tf!("Could not import: {error}", error = e)),
        };
        if bundle.patch_files == 0 {
            return self.import_shared(&bundle.collection, None);
        }
        let name = bundle.collection.name.clone();
        let body = tf!("“{name}” comes with its merged patch, which becomes the new collection's. Your other collections' patches aren't touched.", name = name);
        let dialog = adw::AlertDialog::new(Some(&t!("Install the Merged Patch?")), Some(&body));
        dialog.add_responses(&[("cancel", &t!("_Cancel")), ("order", &t!("Load Order _Only")), ("patch", &t!("_Install"))]);
        dialog.set_response_appearance("patch", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("patch"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            match response {
                "order" => ui.import_shared(&bundle.collection, None),
                "patch" => {
                    // Installed as the new collection's patch, in a folder
                    // of its own.
                    let (name, folder) = {
                        let collections = ui.collections.borrow();
                        let name = collections.unique_name(&bundle.collection.name);
                        let user_dir = &ui.paths.user_dir;
                        let folder = crate::merge::patch::folder_for(&name, |f| user_dir.join("mod").join(f).exists() || collections.list.iter().any(|c| c.patch.as_deref() == Some(f)));
                        (name, folder)
                    };
                    let target = crate::merge::patch::Target::new(&folder, &name);
                    if let Err(e) = bundle::install_patch(&bundle, &ui.paths.user_dir, &target, &ui.paths.backup_dir) {
                        return ui.toast_error(&tf!("Could not install the merged patch, so nothing was imported: {error}", error = e));
                    }
                    // The sharer's patch stands for this one in the load order.
                    let mut shared = bundle.collection.clone();
                    for m in shared.mods.iter_mut().filter(|m| crate::merge::patch::is_patch(&m.id)) {
                        m.id = target.id();
                        m.name = target.name.clone();
                    }
                    // The patch's descriptor shows up once the mods are read again.
                    ui.reload(move |ui| {
                        ui.import_shared(&shared, Some(folder));
                        ui.patch_saved();
                    });
                }
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Adds `shared` as a collection, with the patch in `patch` if it came
    /// with one. Without one, the sharer's patch is left out.
    fn import_shared(self: &Rc<Self>, shared: &collections::Shared, patch: Option<String>) {
        let mut shared = shared.clone();
        if patch.is_none() {
            shared.mods.retain(|m| !crate::merge::patch::is_patch(&m.id));
        }
        self.adopt_notes(&shared);
        let (mut collection, missing) = collections::import(&shared, &self.state.borrow().mods);
        collection.patch = patch;
        self.add_and_switch(collection, missing);
    }

    /// Exports the load order as a Paradox Launcher playset file, for
    /// players who don't use DrukhalManager.
    fn export_launcher(self: &Rc<Self>) {
        let name = self.collections.borrow().active.clone();
        let dialog = gtk::FileDialog::builder().title(t!("Export for the Paradox Launcher")).initial_name(format!("{name} (launcher).json")).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), None::<&gio::Cancellable>, move |file| {
            let (Some(ui), Ok(file)) = (weak.upgrade(), file) else { return };
            let Some(path) = file.path() else { return };
            let shared = ui.shared_collection(&name, true);
            let (json, skipped) = collections::to_launcher_json(&shared);
            if let Err(e) = std::fs::write(&path, json) {
                return ui.toast_error(&tf!("Could not export: {error}", error = e));
            }
            let exported = tnf!("Exported “{name}” with {count} mod", "Exported “{name}” with {count} mods", count = shared.mods.len() - skipped.len(), name = name);
            if skipped.is_empty() {
                ui.add_toast(adw::Toast::new(&exported));
            } else {
                ui.toast_error(&tnf!(
                    "{exported}. The launcher only knows Workshop mods, so this local mod was left out: {mods}",
                    "{exported}. The launcher only knows Workshop mods, so these local mods were left out: {mods}",
                    count = skipped.len(),
                    exported = exported,
                    mods = skipped.join(", ")
                ));
            }
        });
    }

    /// `shared_collection` with the merged patch, and the user's notes if
    /// `notes`.
    fn shared_collection_with(&self, name: &str, notes: bool) -> collections::Shared {
        let mut shared = self.shared_collection(name, false);
        if notes {
            collections::attach_notes(&mut shared, &self.settings.borrow().notes);
        }
        shared
    }

    /// Takes the notes an imported collection came with, for mods that
    /// don't have one here.
    fn adopt_notes(&self, shared: &collections::Shared) {
        let notes = collections::imported_notes(shared, &self.state.borrow().mods, &self.settings.borrow().notes);
        if notes.is_empty() {
            return;
        }
        self.settings.borrow_mut().notes.extend(notes);
        self.save_settings("the imported notes");
    }

    /// The current load order to share as the collection `name`, with its
    /// mods that aren't installed in their places. `without_patch` leaves
    /// out the merged patch, which only exists on this machine.
    fn shared_collection(&self, name: &str, without_patch: bool) -> collections::Shared {
        let state = self.state.borrow();
        let mut c = self.collections.borrow().get(name).cloned().unwrap_or_else(|| collections::Collection::new(name, Vec::new()));
        c.update(state.enabled_ids(), &state.mods);
        if without_patch {
            c.mods.retain(|id| !crate::merge::patch::is_patch(id));
        }
        collections::export(name, &c.mods, &state.mods, &c.names)
    }

    /// Copies the load order as a numbered list with Workshop links, for
    /// pasting in a chat or forum post.
    fn copy_collection(self: &Rc<Self>) {
        let name = self.collections.borrow().active.clone();
        let shared = self.shared_collection(&name, true);
        if shared.mods.is_empty() {
            return self.toast_error(&t!("No mods are enabled"));
        }
        self.window.clipboard().set_text(&collections::to_text(&shared));
        self.add_toast(adw::Toast::new(&tnf!("Copied “{name}” with {count} mod", "Copied “{name}” with {count} mods", count = shared.mods.len(), name = name)));
    }

    /// Imports a load order from the clipboard as a new collection, like
    /// Import from File.
    fn paste_collection(self: &Rc<Self>) {
        let clipboard = self.window.clipboard();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let text = clipboard.read_text_future().await;
            let Some(ui) = weak.upgrade() else { return };
            let Some(shared) = text.ok().flatten().and_then(|t| collections::from_text(&t)) else {
                return ui.toast_error(&t!("The clipboard doesn't hold a load order"));
            };
            ui.adopt_notes(&shared);
            let (collection, missing) = collections::import(&shared, &ui.state.borrow().mods);
            ui.add_and_switch(collection, missing);
        });
    }
}

fn open_page(window: &glib::WeakRef<adw::ApplicationWindow>, id: u64) {
    gtk::UriLauncher::new(&workshop::page_url(id)).launch(window.upgrade().as_ref(), None::<&gio::Cancellable>, |_| {});
}
