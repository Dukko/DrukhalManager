//! The collection switcher and its actions: new, duplicate, rename, delete,
//! import from a launcher playset or a file, and export.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::conflicts_view::plural;
use crate::mods::collections::{self, Collections};
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
                    self.toast_error(&format!("Could not save collections: {e}"));
                }
                c
            }
            Err(e) => {
                // Keep the unreadable file for inspection instead of
                // overwriting it.
                let _ = std::fs::rename(&path, path.with_extension("json.unreadable"));
                self.toast_error(&format!("Collections could not be read ({e}) and were reset; the old file was kept next to it"));
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
        action("collection-import-playset", Window::import_playset);
        action("collection-import-file", Window::import_file);
        action("collection-import-workshop", Window::import_workshop);
        action("collection-export", Window::export_collection);

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
        let saved = self.collections.borrow().active().mods.clone();
        if saved != current && !saved.is_empty() {
            self.restore_banner.set_title(&format!("The game's load order doesn't match the collection “{active}”. Something else changed it, like the Paradox Launcher."));
            self.restore_banner.set_revealed(true);
        }
        self.rebuild_collection_menu();
    }

    fn toast_error(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(message).timeout(0).build());
    }

    fn rebuild_collection_menu(&self) {
        let collections = self.collections.borrow();
        let menu = gio::Menu::new();
        let list = gio::Menu::new();
        for c in &collections.list {
            let item = gio::MenuItem::new(Some(&c.name), None);
            item.set_action_and_target_value(Some("win.collection"), Some(&c.name.to_variant()));
            list.append_item(&item);
        }
        menu.append_section(Some("Collections"), &list);
        let manage = gio::Menu::new();
        manage.append(Some("_New Collection…"), Some("win.collection-new"));
        manage.append(Some("_Duplicate…"), Some("win.collection-duplicate"));
        manage.append(Some("_Rename…"), Some("win.collection-rename"));
        if collections.list.len() > 1 {
            manage.append(Some("De_lete"), Some("win.collection-delete"));
        }
        menu.append_section(None, &manage);
        let share = gio::Menu::new();
        share.append(Some("Import from Launcher _Playset…"), Some("win.collection-import-playset"));
        share.append(Some("_Import from File…"), Some("win.collection-import-file"));
        share.append(Some("Import _Workshop Collection…"), Some("win.collection-import-workshop"));
        share.append(Some("_Export to File…"), Some("win.collection-export"));
        menu.append_section(None, &share);

        self.collection_button.set_menu_model(Some(&menu));
        self.collection_button.set_label(&collections.active);
        if let Some(action) = self.window.lookup_action("collection") {
            action.change_state(&collections.active.to_variant());
        }
    }

    fn save_collections(&self) {
        if let Err(e) = self.collections.borrow().save(&collections::path()) {
            self.toast_error(&format!("Could not save collections: {e}"));
        }
    }

    /// Makes `mods` the load order and saves it everywhere.
    fn apply_collection_mods(self: &Rc<Self>, mods: &[String]) {
        let missing = self.state.borrow_mut().set_enabled_ids(mods);
        self.restore_banner.set_revealed(false);
        if self.save() && !missing.is_empty() {
            self.toast_error(&format!("{} in this collection aren't installed: {}", plural(missing.len(), "mod", "mods"), missing.join(", ")));
        }
    }

    fn switch_collection(self: &Rc<Self>, name: String) {
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
        let dialog = adw::AlertDialog::new(Some("Save Changes?"), Some(&format!("“{current}” has unsaved changes. Save them before switching to “{name}”?")));
        dialog.add_responses(&[("cancel", "_Cancel"), ("discard", "_Discard"), ("save", "_Save")]);
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
        self.ask_text(title, "Name", initial, confirm, done);
    }

    /// Asks for one line of text, then calls `done` with it.
    fn ask_text(self: &Rc<Self>, title: &str, field: &str, initial: &str, confirm: &str, done: impl Fn(&Rc<Window>, String) + 'static) {
        let entry = adw::EntryRow::builder().title(field).text(initial).build();
        let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
        list.append(&entry);
        let dialog = adw::AlertDialog::new(Some(title), None);
        dialog.set_extra_child(Some(&list));
        dialog.add_responses(&[("cancel", "_Cancel"), ("ok", confirm)]);
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
        let suggested = if duplicate { self.collections.borrow().unique_name(&format!("{active} copy")) } else { self.collections.borrow().unique_name("New Collection") };
        let title = if duplicate { "Duplicate Collection" } else { "New Collection" };
        self.ask_name(title, &suggested, "_Create", move |ui, name| {
            let mods = if duplicate { ui.state.borrow().enabled_ids() } else { Vec::new() };
            let name = ui.collections.borrow_mut().add(&name, mods.clone());
            ui.collections.borrow_mut().active = name.clone();
            ui.save_collections();
            ui.rebuild_collection_menu();
            ui.apply_collection_mods(&mods);
            ui.toasts.add_toast(adw::Toast::new(&format!("Created “{name}”")));
        });
    }

    fn rename_collection(self: &Rc<Self>) {
        let active = self.collections.borrow().active.clone();
        self.ask_name("Rename Collection", &active.clone(), "_Rename", move |ui, name| {
            let Some(new) = ui.collections.borrow_mut().rename(&active, &name) else { return };
            if new == active {
                return;
            }
            ui.save_collections();
            ui.rebuild_collection_menu();
            if let Err(e) = launcher_db::rename(&ui.paths.user_dir, &collections::playset_name(&active), &collections::playset_name(&new)) {
                ui.toast_error(&format!("Renamed, but the Paradox Launcher's playset kept its old name: {e}"));
            }
        });
    }

    fn delete_collection(self: &Rc<Self>) {
        let active = self.collections.borrow().active.clone();
        let dialog = adw::AlertDialog::new(
            Some(&format!("Delete “{active}”?")),
            Some("The collection's load order is forgotten. Its playset stays in the Paradox Launcher, and your mods aren't touched."),
        );
        dialog.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            if response != "delete" || !ui.collections.borrow_mut().remove(&active) {
                return;
            }
            let mods = ui.collections.borrow().active().mods.clone();
            ui.save_collections();
            ui.rebuild_collection_menu();
            ui.state.borrow_mut().dirty = false;
            ui.apply_collection_mods(&mods);
        });
        dialog.present(Some(&self.window));
    }

    fn import_playset(self: &Rc<Self>) {
        let playsets = match launcher_db::playsets(&self.paths.user_dir) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => return self.toast_error("The Paradox Launcher has no playsets to import"),
            Err(e) => return self.toast_error(&format!("Could not read the Paradox Launcher's playsets: {e}")),
        };
        let names: Vec<String> = playsets.iter().map(|(n, mods)| format!("{n} ({})", plural(mods.len(), "mod", "mods"))).collect();
        let choice = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let dialog = adw::AlertDialog::new(Some("Import Playset"), Some("Creates a collection with the playset's mods, in its order."));
        dialog.set_extra_child(Some(&choice));
        dialog.add_responses(&[("cancel", "_Cancel"), ("import", "_Import")]);
        dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("import"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let (Some(ui), "import") = (weak.upgrade(), response) else { return };
            let Some((name, mods)) = playsets.get(choice.selected() as usize) else { return };
            let name = name.strip_prefix("DrukhalManager – ").unwrap_or(name);
            ui.add_and_switch(name, mods.clone(), Vec::new());
        });
        dialog.present(Some(&self.window));
    }

    /// Adds a collection, switches to it, and reports mods that weren't
    /// found. Returns the name it was added under.
    fn add_and_switch(self: &Rc<Self>, name: &str, mods: Vec<String>, missing: Vec<String>) -> Option<String> {
        if self.state.borrow().dirty {
            // Unsaved edits belong to the current collection; keep them.
            if !self.save() {
                return None;
            }
        }
        let name = self.collections.borrow_mut().add(name, mods.clone());
        self.collections.borrow_mut().active = name.clone();
        self.save_collections();
        self.rebuild_collection_menu();
        self.apply_collection_mods(&mods);
        if missing.is_empty() {
            self.toasts.add_toast(adw::Toast::new(&format!("Imported “{name}”")));
        } else {
            self.toast_error(&format!("Imported “{name}”. {} aren't installed: {}", plural(missing.len(), "mod", "mods"), missing.join(", ")));
        }
        Some(name)
    }

    fn import_workshop(self: &Rc<Self>) {
        self.ask_text("Import Workshop Collection", "Collection link or ID", "", "_Import", |ui, text| {
            let Some(id) = workshop::parse_id(&text) else {
                return ui.toast_error("That isn't a Workshop collection link or ID");
            };
            let toast = adw::Toast::builder().title("Reading the collection from Steam…").timeout(0).build();
            ui.toasts.add_toast(toast.clone());
            let weak = Rc::downgrade(ui);
            glib::spawn_future_local(async move {
                let fetched = gio::spawn_blocking(move || workshop::fetch(id)).await.unwrap_or_else(|_| Err("The request failed".to_owned()));
                toast.dismiss();
                let Some(ui) = weak.upgrade() else { return };
                let shared = match fetched {
                    Ok(shared) => shared,
                    Err(e) => return ui.toast_error(&format!("Could not import the collection: {e}")),
                };
                let (mods, missing) = collections::resolve(&shared, &ui.state.borrow().mods);
                if mods.is_empty() && missing.is_empty() {
                    return ui.toast_error(&format!("“{}” has no mods", shared.name));
                }
                if let Some(name) = ui.add_and_switch(&shared.name, mods, Vec::new())
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
                let open = gtk::Button::builder().icon_name("adw-external-link-symbolic").tooltip_text("Open on the Steam Workshop").valign(gtk::Align::Center).css_classes(["flat"]).build();
                let window = self.window.downgrade();
                open.connect_clicked(move |_| open_page(&window, id));
                row.add_suffix(&open);
                row.set_activatable_widget(Some(&open));
            }
            list.append(&row);
        }
        let all = gtk::Button::builder().label("Open the Collection on the Workshop").halign(gtk::Align::Center).css_classes(["pill"]).build();
        let window = self.window.downgrade();
        all.connect_clicked(move |_| open_page(&window, collection_id));
        let scroll = gtk::ScrolledWindow::builder().child(&list).propagate_natural_height(true).max_content_height(320).hscrollbar_policy(gtk::PolicyType::Never).build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&scroll);
        content.append(&all);

        let dialog = adw::AlertDialog::new(
            Some(&format!("{} Not Installed", plural(missing.len(), "Mod", "Mods"))),
            Some(&format!("Subscribe to them on the Steam Workshop. Once Steam has downloaded them, reload to add them to “{name}”.")),
        );
        dialog.set_extra_child(Some(&content));
        dialog.add_responses(&[("later", "_Later"), ("reload", "_Reload Mods")]);
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
        let (mods, missing) = collections::resolve(&shared, &self.state.borrow().mods);
        self.collections.borrow_mut().set_mods(&name, mods.clone());
        self.save_collections();
        if self.collections.borrow().active == name {
            if self.state.borrow().dirty {
                // Don't throw away edits; let the user choose.
                self.restore_banner.set_title(&format!("“{name}” has new mods from the Workshop that aren't in the load order yet."));
                self.restore_banner.set_revealed(true);
            } else {
                self.apply_collection_mods(&mods);
            }
        }
        if missing.is_empty() {
            self.toasts.add_toast(adw::Toast::new(&format!("Every mod in “{name}” is installed")));
        } else {
            self.offer_missing(collection_id, name, shared);
        }
    }

    fn import_file(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("DrukhalManager collections"));
        filter.add_pattern("*.json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title("Import Collection").filters(&filters).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.window), None::<&gio::Cancellable>, move |file| {
            let (Some(ui), Ok(file)) = (weak.upgrade(), file) else { return };
            let Some(path) = file.path() else { return };
            let shared: collections::Shared = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
                Ok(s) => s,
                Err(e) => return ui.toast_error(&format!("Not a collection file: {e}")),
            };
            let (mods, missing) = collections::resolve(&shared, &ui.state.borrow().mods);
            ui.add_and_switch(&shared.name, mods, missing);
        });
    }

    fn export_collection(self: &Rc<Self>) {
        let name = self.collections.borrow().active.clone();
        let dialog = gtk::FileDialog::builder().title("Export Collection").initial_name(format!("{name}.json")).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), None::<&gio::Cancellable>, move |file| {
            let (Some(ui), Ok(file)) = (weak.upgrade(), file) else { return };
            let Some(path) = file.path() else { return };
            let shared = {
                let state = ui.state.borrow();
                collections::export(&name, &state.enabled_ids(), &state.mods)
            };
            match serde_json::to_vec_pretty(&shared).map_err(|e| e.to_string()).and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string())) {
                Ok(()) => ui.toasts.add_toast(adw::Toast::new(&format!("Exported “{name}” with {}", plural(shared.mods.len(), "mod", "mods")))),
                Err(e) => ui.toast_error(&format!("Could not export: {e}")),
            }
        });
    }
}

fn open_page(window: &glib::WeakRef<adw::ApplicationWindow>, id: u64) {
    gtk::UriLauncher::new(&workshop::page_url(id)).launch(window.upgrade().as_ref(), None::<&gio::Cancellable>, |_| {});
}
