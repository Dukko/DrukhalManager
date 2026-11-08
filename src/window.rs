//! The main window: a load order editor.

mod collections_ui;

use std::cell::{Cell, RefCell};
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::conflicts_view::{self, Analysis, MergeHooks};
use crate::merge::conflicts::{self, ModInput};
use crate::merge::patch::{self, Patch};
use crate::file_merge_dialog;
use crate::fix_dialog;
use crate::merge_dialog;
use crate::mods::collections::Collections;
use crate::mods::{self, Mod, checks};
use crate::preferences;
use crate::settings::{self, Settings};
use crate::paradox::game::GameInstall;
use crate::error_report_dialog;
use crate::paradox::error_log;
use crate::paradox::launcher_db;

pub struct Paths {
    pub user_dir: PathBuf,
    pub backup_dir: PathBuf,
}

struct State {
    mods: Vec<Mod>,
    /// Indices into `mods`, in load order.
    enabled: Vec<usize>,
    dirty: bool,
    /// Bumped on every load order change, so conflict reports can tell
    /// whether they still describe it.
    revision: u64,
}

impl State {
    fn new(mods: Vec<Mod>) -> Self {
        let mut enabled: Vec<usize> = (0..mods.len()).filter(|&i| mods[i].load_position.is_some()).collect();
        enabled.sort_by_key(|&i| mods[i].load_position);
        let mut state = Self { mods, enabled, dirty: false, revision: 0 };
        // A saved order with the patch elsewhere gets fixed on the next save.
        state.dirty = state.pin_patch();
        state
    }

    /// Keeps the merged patch at the very end of the load order: its merged
    /// files and fixes only win when it loads after every mod. Every change
    /// to the order goes through here. Returns whether it had to move.
    fn pin_patch(&mut self) -> bool {
        let Some(at) = self.enabled.iter().position(|&i| self.mods[i].id == patch::ID) else { return false };
        if at + 1 == self.enabled.len() {
            return false;
        }
        let patch = self.enabled.remove(at);
        self.enabled.push(patch);
        true
    }

    fn disabled(&self) -> Vec<usize> {
        let mut disabled: Vec<usize> = (0..self.mods.len()).filter(|i| !self.enabled.contains(i)).collect();
        disabled.sort_by_cached_key(|&i| self.mods[i].descriptor.name.to_lowercase());
        disabled
    }

    /// Enabling appends to the end of the load order.
    fn set_enabled(&mut self, index: usize, on: bool) {
        match (on, self.enabled.iter().position(|&i| i == index)) {
            (true, None) => self.enabled.push(index),
            (false, Some(p)) => {
                self.enabled.remove(p);
            }
            _ => return,
        }
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
    }

    /// Moves the mod at load position `from` in front of the mod currently at
    /// `before` (`before == len` moves it to the end).
    /// Moves the mod at load position `from` so it ends up at position `to`
    /// (both 0-based; `to` is clamped to the list).
    fn move_to(&mut self, from: usize, to: usize) {
        let to = to.min(self.enabled.len().saturating_sub(1));
        self.move_enabled(from, if to > from { to + 1 } else { to });
    }

    fn move_enabled(&mut self, from: usize, mut before: usize) {
        if from >= self.enabled.len() {
            return;
        }
        if before > from {
            before -= 1;
        }
        if before == from {
            return;
        }
        let item = self.enabled.remove(from);
        self.enabled.insert(before.min(self.enabled.len()), item);
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
    }

    /// Enables every mod (newly enabled ones go to the end, alphabetically)
    /// or disables them all. Returns the previous load order for undo, or
    /// `None` if nothing changed.
    fn set_all(&mut self, on: bool) -> Option<(Vec<usize>, bool)> {
        let previous = (self.enabled.clone(), self.dirty);
        if on {
            let disabled = self.disabled();
            if disabled.is_empty() {
                return None;
            }
            self.enabled.extend(disabled);
        } else {
            if self.enabled.is_empty() {
                return None;
            }
            self.enabled.clear();
        }
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
        Some(previous)
    }

    fn restore(&mut self, (enabled, dirty): (Vec<usize>, bool)) {
        self.enabled = enabled;
        self.dirty = dirty;
        self.pin_patch();
        self.revision += 1;
    }

    /// Drops mods by id, keeping the load order of the rest. Removing an
    /// enabled mod counts as a change.
    fn remove_mods(&mut self, ids: &[String]) {
        let enabled_ids = self.enabled_ids();
        let before = enabled_ids.len();
        self.mods.retain(|m| !ids.contains(&m.id));
        let index: std::collections::HashMap<&str, usize> = self.mods.iter().enumerate().map(|(i, m)| (m.id.as_str(), i)).collect();
        self.enabled = enabled_ids.iter().filter_map(|id| index.get(id.as_str()).copied()).collect();
        self.pin_patch();
        if self.enabled.len() != before {
            self.dirty = true;
        }
        self.revision += 1;
    }

    /// Replaces the load order with `ids`; returns those not installed.
    fn set_enabled_ids(&mut self, ids: &[String]) -> Vec<String> {
        let index: std::collections::HashMap<&str, usize> = self.mods.iter().enumerate().map(|(i, m)| (m.id.as_str(), i)).collect();
        let mut missing = Vec::new();
        let mut enabled = Vec::new();
        for id in ids {
            match index.get(id.as_str()) {
                Some(&i) if !enabled.contains(&i) => enabled.push(i),
                Some(_) => {}
                None => missing.push(id.clone()),
            }
        }
        self.enabled = enabled;
        self.pin_patch();
        self.dirty = true;
        self.revision += 1;
        missing
    }

    fn enabled_ids(&self) -> Vec<String> {
        self.enabled.iter().map(|&i| self.mods[i].id.clone()).collect()
    }
}

struct Window {
    window: adw::ApplicationWindow,
    paths: Paths,
    game: Option<GameInstall>,
    state: RefCell<State>,
    status: gtk::Label,
    /// Checked when every mod is enabled, mixed when some are.
    all_toggle: gtk::CheckButton,
    /// Set while `refresh` updates `all_toggle`, so it isn't taken as a click.
    syncing: Cell<bool>,
    /// Shown when some mods load before mods they depend on.
    sort_button: gtk::Button,
    search: gtk::SearchEntry,
    /// Offers to repair descriptors with stale paths or no mod behind them.
    repair_banner: adw::Banner,
    stack: adw::ViewStack,
    conflicts_page: adw::Bin,
    analysis: RefCell<Option<Rc<Analysis>>>,
    scanning: Cell<bool>,
    /// Whether the Conflicts page shows ignored conflicts (dimmed).
    show_ignored: Cell<bool>,
    /// Merges and fixes in the patch whose sources changed since.
    stale: RefCell<Vec<patch::Stale>>,
    settings: RefCell<Settings>,
    /// `None` if the patch's manifest couldn't be read.
    patch: RefCell<Option<Patch>>,
    enabled_list: gtk::ListBox,
    disabled_list: gtk::ListBox,
    toasts: adw::ToastOverlay,
    collections: RefCell<Collections>,
    collection_button: gtk::MenuButton,
    /// Offers to restore the active collection when the game's load order
    /// was changed elsewhere.
    restore_banner: adw::Banner,
}

pub fn build(app: &adw::Application, paths: Paths, game: Option<GameInstall>, found: io::Result<Vec<Mod>>) {
    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("DrukhalManager")
        .default_width(960)
        .default_height(720)
        .content(&toasts)
        .build();

    let found = match found {
        Ok(found) => found,
        Err(e) => {
            toolbar.set_content(Some(&status_page("Could Not Read Mods", &format!("{}: {e}", paths.user_dir.display()))));
            window.present();
            return;
        }
    };

    let launch = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().icon_name("media-playback-start-symbolic").label("Launch").build())
        .action_name("win.launch")
        .css_classes(["suggested-action"])
        .build();
    let save = gtk::Button::builder().label("Save").action_name("win.save").tooltip_text("Save the load order for the game and the Paradox Launcher (Ctrl+S)").build();
    let menu = gio::Menu::new();
    menu.append(Some("Last Game's _Errors…"), Some("win.errors"));
    menu.append(Some("_Preferences"), Some("win.preferences"));
    let menu_button = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Main Menu").primary(true).build();
    header.pack_end(&menu_button);
    header.pack_end(&launch);
    header.pack_end(&save);
    let collection_button = gtk::MenuButton::builder().tooltip_text("Collections: switch between saved load orders").build();
    header.pack_start(&collection_button);
    if game.is_none() {
        launch.set_tooltip_text(Some("Stellaris was not found in any Steam library"));
    }

    let enabled_list = boxed_list("No mods enabled");
    let disabled_list = boxed_list("Every mod is enabled");
    let status = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let all_toggle = gtk::CheckButton::builder().label("Enable All").tooltip_text("Enable or disable every mod").halign(gtk::Align::End).build();
    let sort_button = gtk::Button::builder()
        .label("Sort by Dependencies")
        .tooltip_text("Move mods after the mods they depend on")
        .css_classes(["pill", "suggested-action"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status.set_hexpand(true);
    top.append(&status);
    top.append(&sort_button);
    top.append(&all_toggle);
    let search = gtk::SearchEntry::builder().placeholder_text("Search mods by name or tag (Ctrl+F)").build();
    let repair_banner = adw::Banner::builder().button_label("Repair…").build();
    toolbar.add_top_bar(&repair_banner);
    let restore_banner = adw::Banner::builder().button_label("Restore").build();
    toolbar.add_top_bar(&restore_banner);
    content.append(&top);
    content.append(&search);
    for list in [&enabled_list, &disabled_list] {
        let search = search.clone();
        list.set_filter_func(move |row| {
            let query = search.text().to_lowercase();
            if query.is_empty() {
                return true;
            }
            row.downcast_ref::<adw::ActionRow>().is_none_or(|r| format!("{} {}", r.title(), r.subtitle().unwrap_or_default()).to_lowercase().contains(&query))
        });
    }
    search.connect_search_changed(glib::clone!(
        #[weak]
        enabled_list,
        #[weak]
        disabled_list,
        move |_| {
            enabled_list.invalidate_filter();
            disabled_list.invalidate_filter();
        }
    ));
    content.append(&heading("Load Order"));
    content.append(&enabled_list);
    content.append(&heading("Disabled"));
    content.append(&disabled_list);
    stack.add_titled_with_icon(&scrolled_page(&content), Some("load-order"), "Load Order", "view-list-symbolic");
    let conflicts_page = adw::Bin::new();
    stack.add_titled_with_icon(&scrolled_page(&conflicts_page), Some("conflicts"), "Conflicts", "dialog-warning-symbolic");
    toolbar.set_content(Some(&stack));

    let save_action = gio::SimpleAction::new("save", None);
    let launch_action = gio::SimpleAction::new("launch", None);
    launch_action.set_enabled(game.is_some());
    window.add_action(&save_action);
    window.add_action(&launch_action);
    app.set_accels_for_action("win.save", &["<Control>s"]);
    let preferences_action = gio::SimpleAction::new("preferences", None);
    window.add_action(&preferences_action);
    app.set_accels_for_action("win.preferences", &["<Control>comma"]);
    let page_action = gio::SimpleAction::new("page", Some(glib::VariantTy::STRING));
    page_action.connect_activate(glib::clone!(
        #[weak]
        stack,
        move |_, page| {
            if let Some(page) = page.and_then(|p| p.str()) {
                stack.set_visible_child_name(page);
            }
        }
    ));
    window.add_action(&page_action);
    app.set_accels_for_action("win.page::load-order", &["<Alt>1"]);
    app.set_accels_for_action("win.page::conflicts", &["<Alt>2"]);

    let ui = Rc::new(Window {
        window: window.clone(),
        paths,
        game,
        collections: RefCell::new(Collections::starting_with(Vec::new())),
        collection_button,
        restore_banner,
        state: RefCell::new(State::new(found)),
        status,
        all_toggle: all_toggle.clone(),
        syncing: Cell::new(false),
        sort_button: sort_button.clone(),
        search: search.clone(),
        repair_banner: repair_banner.clone(),
        stack: stack.clone(),
        conflicts_page,
        analysis: RefCell::new(None),
        scanning: Cell::new(false),
        show_ignored: Cell::new(false),
        stale: RefCell::new(Vec::new()),
        settings: RefCell::new(Settings::load(&settings::path())),
        patch: RefCell::new(None),
        enabled_list,
        disabled_list,
        toasts,
    });

    let weak = Rc::downgrade(&ui);
    save_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.save();
        }
    });
    let weak = Rc::downgrade(&ui);
    launch_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.launch();
        }
    });
    match Patch::open(&ui.paths.user_dir) {
        Ok(p) => {
            ui.patch.replace(Some(p));
        }
        Err(e) => ui.toasts.add_toast(adw::Toast::builder().title(format!("Could not read the merged patch: {e}")).timeout(0).build()),
    }
    let weak = Rc::downgrade(&ui);
    repair_banner.connect_button_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.confirm_repair();
        }
    });
    ui.update_repair_banner();
    ui.setup_collections();
    let weak = Rc::downgrade(&ui);
    sort_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.sort_by_dependencies();
        }
    });
    for (name, open) in [("open-folder", Window::open_folder as fn(&Window, usize)), ("open-workshop", Window::open_workshop)] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
        let weak = Rc::downgrade(&ui);
        action.connect_activate(move |_, index| {
            if let (Some(ui), Some(index)) = (weak.upgrade(), index.and_then(|i| i.get::<u32>())) {
                open(&ui, index as usize);
            }
        });
        window.add_action(&action);
    }
    for (name, to_top) in [("move-top", true), ("move-bottom", false)] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
        let weak = Rc::downgrade(&ui);
        action.connect_activate(move |_, index| {
            let (Some(ui), Some(index)) = (weak.upgrade(), index.and_then(|i| i.get::<u32>())) else { return };
            ui.update(move |state| {
                if let Some(from) = state.enabled.iter().position(|&i| i == index as usize) {
                    let to = if to_top { 0 } else { state.enabled.len() };
                    state.move_to(from, to);
                }
            });
        });
        window.add_action(&action);
    }
    let errors = gio::SimpleAction::new("errors", None);
    let weak = Rc::downgrade(&ui);
    errors.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.show_errors();
        }
    });
    window.add_action(&errors);
    let find = gio::SimpleAction::new("find", None);
    let weak = Rc::downgrade(&ui);
    find.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.stack.set_visible_child_name("load-order");
            ui.search.grab_focus();
        }
    });
    window.add_action(&find);
    app.set_accels_for_action("win.find", &["<Control>f"]);
    let weak = Rc::downgrade(&ui);
    all_toggle.connect_toggled(move |_| {
        if let Some(ui) = weak.upgrade()
            && !ui.syncing.get()
        {
            ui.toggle_all();
        }
    });
    ui.check_stale();
    let weak = Rc::downgrade(&ui);
    preferences_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.open_preferences();
        }
    });
    let weak = Rc::downgrade(&ui);
    stack.connect_visible_child_name_notify(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.scan_if_stale();
        }
    });
    ui.refresh();
    // The window owns the only strong reference through this handler; every
    // other closure holds a weak one. Destroying the window drops it.
    let owner = ui.clone();
    window.connect_close_request(move |_| {
        if owner.state.borrow().dirty {
            owner.confirm_close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });

    window.present();
}

impl Window {
    fn refresh(self: &Rc<Self>) {
        let state = self.state.borrow();
        self.enabled_list.remove_all();
        self.disabled_list.remove_all();
        // Conflict counts are only shown while they match the load order.
        let counts = self.analysis.borrow().as_ref().filter(|a| a.revision.get() == state.revision).map(|a| {
            let mut by_position = vec![0; state.enabled.len()];
            let settings = self.settings.borrow();
            let ignored = |k: &str| settings.ignored.contains(k);
            let counts = a.report.counts_by_mod_where(a.names.len(), |c| !conflicts_view::file_ignored(&ignored, c), |c| !conflicts_view::object_ignored(&ignored, c));
            for (scanned, n) in counts.into_iter().enumerate() {
                if let Some(slot) = by_position.get_mut(a.load_positions[scanned]) {
                    *slot = n;
                }
            }
            by_position
        });
        let game_version = self.game.as_ref().and_then(|g| g.version.as_deref());
        let issues = checks::check(&state.mods, &state.enabled, game_version);
        let none = Vec::new();
        for (position, &index) in state.enabled.iter().enumerate() {
            let conflicts = counts.as_ref().and_then(|c| c.get(position)).copied().unwrap_or(0);
            self.enabled_list.append(&self.row(&state.mods[index], index, Some(position), conflicts, issues.get(&index).unwrap_or(&none)));
        }
        for index in state.disabled() {
            self.disabled_list.append(&self.row(&state.mods[index], index, None, 0, issues.get(&index).unwrap_or(&none)));
        }
        let misordered = issues.values().flatten().filter(|i| i.fixed_by_sorting).count();
        self.sort_button.set_visible(misordered > 0);

        let mut subtitle = Vec::new();
        if let Some(version) = self.game.as_ref().and_then(|g| g.version.as_deref()) {
            subtitle.push(format!("Stellaris {version}"));
        }
        subtitle.push(format!("{} of {} mods enabled", state.enabled.len(), state.mods.len()));
        if state.dirty {
            subtitle.push("Unsaved changes".to_owned());
        }
        self.status.set_label(&subtitle.join(" · "));
        let (enabled, total) = (state.enabled.len(), state.mods.len());
        self.syncing.set(true);
        self.all_toggle.set_active(total > 0 && enabled == total);
        self.all_toggle.set_inconsistent(enabled > 0 && enabled < total);
        self.all_toggle.set_sensitive(total > 0);
        self.syncing.set(false);
        drop(state);
        self.scan_if_stale();
    }

    /// Rescans when the Conflicts page is showing a different load order.
    fn scan_if_stale(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() != Some("conflicts") {
            return;
        }
        let revision = self.state.borrow().revision;
        if self.analysis.borrow().as_ref().is_none_or(|a| a.revision.get() != revision) {
            self.scan();
        }
    }

    /// Scans the enabled mods' files in the background.
    fn scan(self: &Rc<Self>) {
        if self.scanning.replace(true) {
            return;
        }
        self.conflicts_page.set_child(Some(&conflicts_view::scanning()));
        let (revision, names, ids, roots, load_positions, inputs) = {
            let state = self.state.borrow();
            // The merged patch is left out: its objects are resolutions, not
            // conflicts.
            let scanned: Vec<(usize, &Mod)> = state.enabled.iter().map(|&i| &state.mods[i]).enumerate().filter(|(_, m)| m.id != patch::ID).collect();
            let names = scanned.iter().map(|(_, m)| display_name(m).to_owned()).collect();
            let ids = scanned.iter().map(|(_, m)| m.id.clone()).collect();
            let roots = scanned.iter().map(|(_, m)| m.content.clone()).collect();
            let positions = scanned.iter().map(|(p, _)| *p).collect();
            let inputs: Vec<ModInput> = scanned.iter().map(|(_, m)| ModInput { root: m.content.clone(), replace_path: m.descriptor.replace_path.clone() }).collect();
            (state.revision, names, ids, roots, positions, inputs)
        };

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || conflicts::analyze(&inputs)).await;
            let Some(ui) = weak.upgrade() else { return };
            ui.scanning.set(false);
            match result {
                Ok(report) => {
                    ui.check_stale();
                    ui.analysis.replace(Some(Rc::new(Analysis { revision: Cell::new(revision), names, ids, roots, load_positions, report })));
                    ui.render_conflicts();
                    // Shows the per-mod counts, and rescans if the load order
                    // changed while this scan ran.
                    ui.refresh();
                }
                Err(_) => {
                    ui.conflicts_page.set_child(Some(&status_page("Scan Failed", "Something went wrong while reading the mods' files")));
                }
            }
        });
    }

    /// Compares the patch's fingerprints with the enabled mods' files.
    fn check_stale(&self) {
        let stale = {
            let state = self.state.borrow();
            let mut roots: std::collections::HashMap<String, std::path::PathBuf> =
                state.enabled.iter().filter_map(|&i| Some((state.mods[i].id.clone(), state.mods[i].content.clone()?))).collect();
            if let Some(game) = &self.game {
                roots.insert(patch::VANILLA.to_owned(), game.dir.clone());
            }
            self.patch.borrow().as_ref().map(|p| p.stale(&roots)).unwrap_or_default()
        };
        self.stale.replace(stale);
    }

    fn remove_merge(self: &Rc<Self>, stale: &patch::Stale) {
        let result = self.with_patch(|p, _, _| match &stale.object {
            Some((category, key)) => p.remove(category, key),
            None => p.remove_file(&stale.path),
        });
        match result {
            Ok(()) => {
                self.check_stale();
                self.refresh();
                let what = stale.object.as_ref().map_or(stale.path.as_str(), |(_, k)| k.as_str());
                self.toasts.add_toast(adw::Toast::new(&format!("Removed {what} from the patch")));
            }
            Err(e) => self.toast_error_message(&e),
        }
    }

    fn toast_error_message(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(message).timeout(0).build());
    }

    /// Rebuilds the Conflicts page from the current analysis.
    fn render_conflicts(self: &Rc<Self>) {
        let Some(analysis) = self.analysis.borrow().clone() else { return };
        let hooks = self.merge_hooks(&analysis);
        self.conflicts_page.set_child(Some(&conflicts_view::build(&analysis, self.rescan_button(), hooks)));
    }

    fn set_ignored(self: &Rc<Self>, key: &str, ignore: bool) {
        {
            let mut settings = self.settings.borrow_mut();
            if ignore {
                settings.ignored.insert(key.to_owned());
            } else {
                settings.ignored.remove(key);
            }
            if let Err(e) = settings.save(&settings::path()) {
                self.toasts.add_toast(adw::Toast::builder().title(format!("Could not save ignored conflicts: {e}")).timeout(0).build());
            }
        }
        // Updates the per-mod counts; the page updates itself.
        self.refresh();
    }

    fn merge_hooks(self: &Rc<Self>, analysis: &Rc<Analysis>) -> Rc<MergeHooks> {
        let weak = Rc::downgrade(self);
        let is_merged = Box::new(move |category: &str, key: &str| {
            weak.upgrade().is_some_and(|ui| ui.patch.borrow().as_ref().is_some_and(|p| p.get(category, key).is_some()))
        });
        let weak = Rc::downgrade(self);
        let objects = analysis.clone();
        let open = Box::new(move |index: usize, mark_merged: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_merge(&objects, index, mark_merged);
            }
        });
        let weak = Rc::downgrade(self);
        let is_file_merged = Box::new(move |path: &str| weak.upgrade().is_some_and(|ui| ui.patch.borrow().as_ref().is_some_and(|p| p.get_file(path).is_some())));
        let weak = Rc::downgrade(self);
        let files = analysis.clone();
        let open_file = Box::new(move |index: usize, mark_merged: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_file_merge(&files, index, mark_merged);
            }
        });
        let weak = Rc::downgrade(self);
        let errors = analysis.clone();
        let open_fix = Box::new(move |index: usize, mark_fixed: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_fix(&errors, index, mark_fixed);
            }
        });
        let weak = Rc::downgrade(self);
        let is_ignored = Box::new(move |key: &str| weak.upgrade().is_some_and(|ui| ui.settings.borrow().ignored.contains(key)));
        let weak = Rc::downgrade(self);
        let set_ignored = Box::new(move |key: &str, ignore: bool| {
            if let Some(ui) = weak.upgrade() {
                ui.set_ignored(key, ignore);
            }
        });
        let weak = Rc::downgrade(self);
        let toggle_show_ignored = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.show_ignored.set(!ui.show_ignored.get());
                // After the click handler returns, since this replaces the page.
                glib::idle_add_local_once(move || ui.render_conflicts());
            }
        });
        let weak = Rc::downgrade(self);
        let remove_merge = Box::new(move |stale: &patch::Stale| {
            if let Some(ui) = weak.upgrade() {
                ui.remove_merge(stale);
            }
        });
        Rc::new(MergeHooks {
            is_merged,
            open,
            is_file_merged,
            open_file,
            open_fix,
            is_ignored,
            set_ignored,
            show_ignored: self.show_ignored.get(),
            toggle_show_ignored,
            stale: self.stale.borrow().clone(),
            remove_merge,
        })
    }

    fn open_fix(self: &Rc<Self>, analysis: &Rc<Analysis>, index: usize, mark_fixed: Box<dyn Fn()>) {
        let e = &analysis.report.errors[index];
        let Some(root) = analysis.roots[e.mod_position].clone() else { return };
        let path = e.file.clone();
        let weak = Rc::downgrade(self);
        let save = {
            let path = path.clone();
            Box::new(move |contents: &str, prints: Vec<patch::Print>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                ui.with_patch(|patch, user_dir, version| patch.save_file(user_dir, version, &path, contents, prints))
            })
        };
        let weak = Rc::downgrade(self);
        let open_preferences = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_preferences();
            }
        });
        let weak = Rc::downgrade(self);
        let on_saved = Box::new(move || {
            mark_fixed();
            if let Some(ui) = weak.upgrade() {
                ui.patch_saved();
            }
        });
        let request = fix_dialog::Request {
            path,
            mod_id: analysis.ids[e.mod_position].clone(),
            mod_name: analysis.name(e.mod_position).to_owned(),
            root,
            error: e.message.clone(),
            provider: self.settings.borrow().ready_provider(),
            save,
            open_preferences,
            on_saved,
        };
        fix_dialog::present(&self.window, request);
    }

    fn open_file_merge(self: &Rc<Self>, analysis: &Rc<Analysis>, index: usize, mark_merged: Box<dyn Fn()>) {
        let conflict = &analysis.report.files[index];
        let origins = conflict.mods.iter().filter_map(|&m| Some((analysis.ids[m].clone(), analysis.name(m).to_owned(), analysis.roots[m].clone()?))).collect();
        let path = conflict.path.clone();
        let merged_before = self.patch.borrow().as_ref().is_some_and(|p| p.get_file(&path).is_some());

        let weak = Rc::downgrade(self);
        let save = {
            let path = path.clone();
            Box::new(move |contents: &str, prints: Vec<patch::Print>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                ui.with_patch(|patch, user_dir, version| patch.save_file(user_dir, version, &path, contents, prints))
            })
        };
        let weak = Rc::downgrade(self);
        let open_preferences = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_preferences();
            }
        });
        let weak = Rc::downgrade(self);
        let on_saved = Box::new(move || {
            mark_merged();
            if let Some(ui) = weak.upgrade() {
                ui.patch_saved();
            }
        });
        let request = file_merge_dialog::Request {
            path,
            origins,
            game_dir: self.game.as_ref().map(|g| g.dir.clone()),
            provider: self.settings.borrow().ready_provider(),
            merged_before,
            save,
            open_preferences,
            on_saved,
        };
        file_merge_dialog::present(&self.window, request);
    }

    /// Runs `f` on the merged patch, opening it first if needed.
    fn with_patch(&self, f: impl FnOnce(&mut Patch, &std::path::Path, Option<&str>) -> std::io::Result<()>) -> Result<(), String> {
        let mut patch = self.patch.borrow_mut();
        if patch.is_none() {
            *patch = Some(Patch::open(&self.paths.user_dir).map_err(|e| format!("Could not read the merged patch: {e}"))?);
        }
        let version = self.game.as_ref().and_then(|g| g.version.as_deref());
        f(patch.as_mut().unwrap(), &self.paths.user_dir, version).map_err(|e| format!("Could not save the merged patch: {e}"))
    }

    fn open_merge(self: &Rc<Self>, analysis: &Rc<Analysis>, index: usize, mark_merged: Box<dyn Fn()>) {
        let conflict = &analysis.report.objects[index];
        let origins = conflict
            .definitions
            .iter()
            .filter_map(|d| {
                let root = analysis.roots[d.mod_position].clone()?;
                Some(merge_dialog::Origin { mod_id: analysis.ids[d.mod_position].clone(), label: analysis.name(d.mod_position).to_owned(), root, file: d.file.clone() })
            })
            .collect();
        let (category, key) = (conflict.category.clone(), conflict.key.clone());
        let existing = self.patch.borrow().as_ref().and_then(|p| p.get(&category, &key).cloned());

        let weak = Rc::downgrade(self);
        let save = {
            let (category, key) = (category.clone(), key.clone());
            Box::new(move |script: &str, prints: Vec<patch::Print>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                ui.save_merge(&category, &key, script, prints)
            })
        };
        let weak = Rc::downgrade(self);
        let open_preferences = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_preferences();
            }
        });
        let weak = Rc::downgrade(self);
        let on_saved = Box::new(move || {
            mark_merged();
            if let Some(ui) = weak.upgrade() {
                ui.patch_saved();
            }
        });

        let request = merge_dialog::Request {
            category,
            key,
            origins,
            game_dir: self.game.as_ref().map(|g| g.dir.clone()),
            provider: self.settings.borrow().ready_provider(),
            existing,
            save,
            open_preferences,
            on_saved,
        };
        merge_dialog::present(&self.window, request);
    }

    fn save_merge(&self, category: &str, key: &str, script: &str, prints: Vec<patch::Print>) -> Result<(), String> {
        self.with_patch(|patch, user_dir, version| patch.save(user_dir, version, category, key, script, prints))
    }

    /// Makes sure the merged patch is in the load order once it has content.
    fn patch_saved(self: &Rc<Self>) {
        self.check_stale();
        let added = {
            let mut state = self.state.borrow_mut();
            let index = match state.mods.iter().position(|m| m.id == patch::ID) {
                Some(i) => i,
                None => {
                    let descriptor = std::fs::read_to_string(self.paths.user_dir.join(patch::ID)).ok().and_then(|src| mods::Descriptor::parse(&src).ok()).unwrap_or_default();
                    state.mods.push(Mod {
                        id: patch::ID.to_owned(),
                        descriptor,
                        content: Some(Patch::root(&self.paths.user_dir)),
                        source: mods::Source::Local,
                        load_position: None,
                        error: None,
                    });
                    state.mods.len() - 1
                }
            };
            let was_enabled = state.enabled.contains(&index);
            if !was_enabled {
                // Pinned to the end, like every change.
                state.set_enabled(index, true);
                // The patch isn't scanned, so the current report still holds.
                if let Some(a) = self.analysis.borrow().as_ref() {
                    a.revision.set(state.revision);
                }
            }
            (!was_enabled).then_some("added to the end of the load order")
        };
        self.refresh();
        let toast = adw::Toast::builder().title(format!("Saved to {}", patch::NAME));
        let toast = match added {
            Some(change) => toast.title(format!("Saved to {}, which was {change}", patch::NAME)).button_label("Save Load Order").action_name("win.save"),
            None => toast,
        };
        self.toasts.add_toast(toast.build());
    }

    fn open_preferences(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        preferences::present(&self.window, self.settings.borrow().clone(), move |settings| {
            let Some(ui) = weak.upgrade() else { return };
            if *ui.settings.borrow() == settings {
                return;
            }
            if let Err(e) = settings.save(&settings::path()) {
                ui.toasts.add_toast(adw::Toast::builder().title(format!("Could not save preferences: {e}")).timeout(0).build());
            }
            ui.settings.replace(settings);
        });
    }

    /// Like a select-all checkbox: enables everything unless everything is
    /// already enabled, in which case it disables everything. Offers undo,
    /// since disabling all forgets the load order.
    fn toggle_all(self: &Rc<Self>) {
        let enable = {
            let state = self.state.borrow();
            state.enabled.len() < state.mods.len()
        };
        let previous = self.state.borrow_mut().set_all(enable);
        self.refresh();
        let Some(previous) = previous else { return };
        let changed = if enable { previous.0.len().abs_diff(self.state.borrow().enabled.len()) } else { previous.0.len() };
        let verb = if enable { "Enabled" } else { "Disabled" };
        self.offer_undo(&format!("{verb} {}", conflicts_view::plural(changed, "mod", "mods")), previous);
    }

    fn sort_by_dependencies(self: &Rc<Self>) {
        let previous = {
            let mut state = self.state.borrow_mut();
            let sorted = checks::sort_by_dependencies(&state.mods, &state.enabled);
            if sorted == state.enabled {
                return;
            }
            let previous = (std::mem::replace(&mut state.enabled, sorted), state.dirty);
            state.pin_patch();
            state.dirty = true;
            state.revision += 1;
            previous
        };
        self.refresh();
        self.offer_undo("Sorted mods after their dependencies", previous);
    }

    /// Shows `title` with an Undo button that restores `previous`.
    fn offer_undo(self: &Rc<Self>, title: &str, previous: (Vec<usize>, bool)) {
        let toast = adw::Toast::builder().title(title).button_label("Undo").build();
        let previous = RefCell::new(Some(previous));
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let (Some(ui), Some(previous)) = (weak.upgrade(), previous.take()) {
                ui.state.borrow_mut().restore(previous);
                ui.refresh();
            }
        });
        self.toasts.add_toast(toast);
    }

    fn update_repair_banner(&self) {
        let findings = mods::repair::find(&self.paths.user_dir, &self.state.borrow().mods);
        let mut parts = Vec::new();
        if !findings.stale.is_empty() {
            parts.push(format!("{} point to folders that no longer exist", conflicts_view::plural(findings.stale.len(), "mod descriptor", "mod descriptors")));
        }
        if !findings.orphans.is_empty() {
            parts.push(format!("{} belong to mods whose files are gone", conflicts_view::plural(findings.orphans.len(), "descriptor", "descriptors")));
        }
        self.repair_banner.set_title(&glib::markup_escape_text(&parts.join("; ")));
        self.repair_banner.set_revealed(!findings.is_empty());
    }

    fn confirm_repair(self: &Rc<Self>) {
        let findings = mods::repair::find(&self.paths.user_dir, &self.state.borrow().mods);
        if findings.is_empty() {
            self.update_repair_banner();
            return;
        }
        let mut body = Vec::new();
        if !findings.stale.is_empty() {
            body.push(format!(
                "{} will point to where the mods actually are, so the game can find them even when Steam doesn't say.",
                conflicts_view::plural(findings.stale.len(), "descriptor", "descriptors")
            ));
        }
        if !findings.orphans.is_empty() {
            let names: Vec<&str> = findings.orphans.iter().take(8).map(|o| o.name.as_str()).collect();
            let more = findings.orphans.len().saturating_sub(names.len());
            body.push(format!(
                "These descriptors have no mod files behind them and can be removed: {}{}.",
                names.join(", "),
                if more > 0 { format!(" and {more} more") } else { String::new() }
            ));
        }
        body.push("Changed or removed descriptors are backed up first.".to_owned());

        let dialog = adw::AlertDialog::new(Some("Repair Mod Descriptors?"), Some(&body.join("\n\n")));
        dialog.add_response("cancel", "_Cancel");
        match (findings.stale.is_empty(), findings.orphans.is_empty()) {
            (false, false) => {
                dialog.add_response("fix", "_Fix Paths Only");
                dialog.add_response("all", "Fix and _Remove");
                dialog.set_response_appearance("all", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("all"));
            }
            (false, true) => {
                dialog.add_response("fix", "_Fix Paths");
                dialog.set_response_appearance("fix", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("fix"));
            }
            (true, _) => {
                dialog.add_response("all", "_Remove");
                dialog.set_response_appearance("all", adw::ResponseAppearance::Destructive);
            }
        }
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            let remove: &[mods::repair::Orphan] = match response {
                "all" => &findings.orphans,
                "fix" => &[],
                _ => return,
            };
            ui.repair(&findings.stale, remove);
        });
        dialog.present(Some(&self.window));
    }

    fn repair(self: &Rc<Self>, stale: &[mods::repair::StalePath], remove: &[mods::repair::Orphan]) {
        match mods::repair::apply(&self.paths.user_dir, &self.paths.backup_dir, stale, remove) {
            Ok(backup) => {
                {
                    let mut state = self.state.borrow_mut();
                    for s in stale {
                        if let Some(m) = state.mods.iter_mut().find(|m| m.id == s.id) {
                            m.descriptor.path = Some(s.new.to_string_lossy().into_owned());
                        }
                    }
                    let ids: Vec<String> = remove.iter().map(|o| o.id.clone()).collect();
                    if !ids.is_empty() {
                        state.remove_mods(&ids);
                    }
                }
                self.refresh();
                self.update_repair_banner();
                let mut done = Vec::new();
                if !stale.is_empty() {
                    done.push(format!("fixed {}", conflicts_view::plural(stale.len(), "path", "paths")));
                }
                if !remove.is_empty() {
                    done.push(format!("removed {}", conflicts_view::plural(remove.len(), "descriptor", "descriptors")));
                }
                let toast = adw::Toast::builder().title(format!("Repaired descriptors: {}. Backup in {}", done.join(", "), backup.display())).timeout(8).build();
                self.toasts.add_toast(toast);
            }
            Err(e) => self.toasts.add_toast(adw::Toast::builder().title(format!("Could not repair descriptors: {e}")).timeout(0).build()),
        }
    }

    fn open_folder(&self, index: usize) {
        let Some(dir) = self.state.borrow().mods.get(index).and_then(|m| m.content.clone()) else { return };
        gtk::FileLauncher::new(Some(&gio::File::for_path(dir))).launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
    }

    fn open_workshop(&self, index: usize) {
        let Some(id) = self.state.borrow().mods.get(index).and_then(|m| m.descriptor.remote_file_id) else { return };
        gtk::UriLauncher::new(&format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}")).launch(Some(&self.window), None::<&gio::Cancellable>, |_| {});
    }

    fn rescan_button(self: &Rc<Self>) -> gtk::Button {
        let button = gtk::Button::builder()
            .child(&adw::ButtonContent::builder().icon_name("view-refresh-symbolic").label("Rescan").build())
            .tooltip_text("Scan again, e.g. after mods were updated")
            .build();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.scan();
            }
        });
        button
    }

    /// Applies a state change after the current signal handler returns, since
    /// refreshing destroys the widget that emitted it.
    fn update(self: &Rc<Self>, change: impl FnOnce(&mut State) + 'static) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                change(&mut ui.state.borrow_mut());
                ui.refresh();
            }
        });
    }

    fn row(self: &Rc<Self>, m: &Mod, index: usize, position: Option<usize>, conflicts: usize, issues: &[checks::Issue]) -> adw::ActionRow {
        let d = &m.descriptor;
        let name = display_name(m);
        let source = match m.source {
            mods::Source::Workshop => "Workshop",
            mods::Source::Local => "Local",
        };
        let mut subtitle = vec![source.to_owned()];
        if !d.game_version().is_empty() {
            subtitle.push(format!("Stellaris {}", d.game_version()));
        }
        if !d.tags.is_empty() {
            subtitle.push(d.tags.join(", "));
        }

        let row = adw::ActionRow::builder().title(name).subtitle(subtitle.join(" · ")).use_markup(false).build();
        if let Some(content) = &m.content {
            row.set_tooltip_text(Some(&content.display().to_string()));
        }

        if conflicts > 0 {
            let badge = gtk::Label::builder()
                .label(conflicts_view::plural(conflicts, "conflict", "conflicts"))
                .tooltip_text("See the Conflicts page (Alt+2) for details")
                .valign(gtk::Align::Center)
                .css_classes(["dim-label", "caption"])
                .build();
            row.add_suffix(&badge);
        }
        let is_patch = m.id == patch::ID;
        // One icon for everything wrong with this mod, worst first.
        let mut problems: Vec<(checks::Severity, String)> = Vec::new();
        if let Some(error) = &m.error {
            problems.push((checks::Severity::Warning, error.clone()));
        }
        let outdated = if is_patch { self.stale.borrow().len() } else { 0 };
        if outdated > 0 {
            problems.push((checks::Severity::Warning, format!("{} out of date because mods changed; see the Conflicts page", conflicts_view::plural(outdated, "merge or fix is", "merges or fixes are"))));
        }
        problems.extend(issues.iter().map(|i| (i.severity, i.message.clone())));
        problems.sort_by(|a, b| b.0.cmp(&a.0));
        if let Some((worst, _)) = problems.first() {
            let (icon, class) = match worst {
                checks::Severity::Warning => ("dialog-warning-symbolic", "warning"),
                checks::Severity::Info => ("dialog-information-symbolic", "dim-label"),
            };
            let tooltip = problems.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>().join("\n");
            row.add_suffix(&gtk::Image::builder().icon_name(icon).tooltip_text(tooltip).css_classes([class]).build());
        }

        let menu = gio::Menu::new();
        let index_variant = (index as u32).to_variant();
        if position.is_some() && !is_patch {
            let moves = gio::Menu::new();
            for (label, action) in [("Move to _Top", "win.move-top"), ("Move to _Bottom", "win.move-bottom")] {
                let item = gio::MenuItem::new(Some(label), None);
                item.set_action_and_target_value(Some(action), Some(&index_variant));
                moves.append_item(&item);
            }
            menu.append_section(None, &moves);
        }
        if m.content.is_some() {
            let item = gio::MenuItem::new(Some("Open Folder"), None);
            item.set_action_and_target_value(Some("win.open-folder"), Some(&index_variant));
            menu.append_item(&item);
        }
        if m.descriptor.remote_file_id.is_some() {
            let item = gio::MenuItem::new(Some("Open Workshop Page"), None);
            item.set_action_and_target_value(Some("win.open-workshop"), Some(&index_variant));
            menu.append_item(&item);
        }

        let toggle = gtk::Switch::builder().active(position.is_some()).valign(gtk::Align::Center).build();
        let weak = Rc::downgrade(self);
        toggle.connect_active_notify(move |toggle| {
            let on = toggle.is_active();
            if let Some(ui) = weak.upgrade() {
                ui.update(move |state| state.set_enabled(index, on));
            }
        });
        row.add_suffix(&toggle);
        row.set_activatable_widget(Some(&toggle));
        if menu.n_items() > 0 {
            row.add_suffix(&gtk::MenuButton::builder().icon_name("view-more-symbolic").menu_model(&menu).valign(gtk::Align::Center).tooltip_text("More").css_classes(["flat"]).build());
        }

        if let (Some(position), true) = (position, is_patch) {
            // Pinned: no handle, no editable position.
            let lock = gtk::Image::builder().icon_name("changes-prevent-symbolic").tooltip_text("Always loads last, so its merges and fixes win").css_classes(["dim-label"]).build();
            let number = gtk::Label::builder().label((position + 1).to_string()).width_chars(3).xalign(1.0).css_classes(["dim-label", "numeric"]).build();
            row.add_prefix(&lock);
            row.add_prefix(&number);
            row.set_subtitle(&format!("{} · Always loads last", row.subtitle().unwrap_or_default()));
        } else if let Some(position) = position {
            let handle = gtk::Image::builder().icon_name("list-drag-handle-symbolic").css_classes(["dim-label"]).build();
            row.add_prefix(&handle);
            row.add_prefix(&self.position_entry(position));
            self.make_reorderable(&row, position);
        }
        row
    }

    /// The load position as an editable number: type a position and press
    /// Enter, or leave the field, to move the mod there.
    fn position_entry(self: &Rc<Self>, position: usize) -> gtk::Entry {
        let entry = gtk::Entry::builder()
            .text((position + 1).to_string())
            .width_chars(3)
            .max_width_chars(4)
            .xalign(1.0)
            .input_purpose(gtk::InputPurpose::Digits)
            .valign(gtk::Align::Center)
            .tooltip_text("Load position: type a number to move this mod there")
            .css_classes(["numeric", "flat"])
            .build();
        let apply = {
            let weak = Rc::downgrade(self);
            move |entry: &gtk::Entry| {
                let Some(ui) = weak.upgrade() else { return };
                match entry.text().trim().parse::<usize>() {
                    // Positions are shown from 1.
                    Ok(n) if n.saturating_sub(1) != position => {
                        // Applying rebuilds the list, which can also end
                        // this field's focus; reset it so that's a no-op.
                        entry.set_text(&(position + 1).to_string());
                        ui.update(move |state| state.move_to(position, n.saturating_sub(1)));
                    }
                    Ok(_) => {}
                    Err(_) => entry.set_text(&(position + 1).to_string()),
                }
            }
        };
        let on_leave = apply.clone();
        entry.connect_activate(apply);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak]
            entry,
            move |_| on_leave(&entry)
        ));
        entry.add_controller(focus);
        entry
    }

    fn make_reorderable(self: &Rc<Self>, row: &adw::ActionRow, position: usize) {
        let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&(position as u32).to_value())));
        source.connect_drag_begin(glib::clone!(
            #[weak]
            row,
            move |source, _| {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            }
        ));
        row.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = Rc::downgrade(self);
        target.connect_drop(glib::clone!(
            #[weak]
            row,
            #[upgrade_or]
            false,
            move |_, value, _, y| {
                let (Ok(from), Some(ui)) = (value.get::<u32>(), weak.upgrade()) else {
                    return false;
                };
                // Dropping on the lower half of a row places the mod after it.
                let before = position + usize::from(y > f64::from(row.height()) / 2.0);
                ui.update(move |state| state.move_enabled(from as usize, before));
                true
            }
        ));
        row.add_controller(target);
    }

    fn save(self: &Rc<Self>) -> bool {
        let ids = self.state.borrow().enabled_ids();
        match mods::load_order::save(&self.paths.user_dir, &self.paths.backup_dir, &ids) {
            Ok(_) => {
                self.state.borrow_mut().dirty = false;
                {
                    let mut collections = self.collections.borrow_mut();
                    let active = collections.active.clone();
                    collections.set_mods(&active, ids);
                    if let Err(e) = collections.save(&mods::collections::path()) {
                        self.toasts.add_toast(adw::Toast::builder().title(format!("Could not save collections: {e}")).timeout(0).build());
                    }
                }
                self.restore_banner.set_revealed(false);
                self.refresh();
                self.toasts.add_toast(self.sync_launcher());
                true
            }
            Err(e) => {
                self.toasts.add_toast(adw::Toast::builder().title(format!("Could not save load order: {e}")).timeout(0).build());
                false
            }
        }
    }

    /// Mirrors the saved load order into the Paradox Launcher's playsets and
    /// returns a toast describing the outcome.
    fn sync_launcher(&self) -> adw::Toast {
        let playset: Vec<launcher_db::PlaysetMod> = {
            let state = self.state.borrow();
            state
                .enabled
                .iter()
                .map(|&i| {
                    let m = &state.mods[i];
                    launcher_db::PlaysetMod {
                        registry_id: m.id.clone(),
                        steam_id: m.descriptor.remote_file_id,
                        name: display_name(m).to_owned(),
                        dir: m.content.clone(),
                        tags: m.descriptor.tags.clone(),
                        supported_version: m.descriptor.game_version().to_owned(),
                    }
                })
                .collect()
        };
        let playset_name = mods::collections::playset_name(&self.collections.borrow().active);
        match launcher_db::sync(&self.paths.user_dir, &self.paths.backup_dir, &playset_name, &playset) {
            Ok(None) => adw::Toast::new("Load order saved"),
            Ok(Some(synced)) if synced.missing.is_empty() => adw::Toast::new(&format!("Load order saved and set as the “{playset_name}” playset in the Paradox Launcher")),
            Ok(Some(synced)) => adw::Toast::builder()
                .title(format!(
                    "Load order saved. The Paradox Launcher doesn't know {} yet, so its playset leaves {} out",
                    synced.missing.join(", "),
                    if synced.missing.len() == 1 { "it" } else { "them" }
                ))
                .timeout(0)
                .build(),
            Err(e) => adw::Toast::builder().title(format!("Load order saved, but the Paradox Launcher's playset wasn't updated: {e}")).timeout(0).build(),
        }
    }

    fn launch(self: &Rc<Self>) {
        let Some(game) = &self.game else { return };
        if self.state.borrow().dirty && !self.save() {
            return;
        }
        let started = std::time::SystemTime::now();
        let mut child = match game.launch() {
            Ok(child) => child,
            Err(e) => return self.toasts.add_toast(adw::Toast::builder().title(format!("Could not start Stellaris: {e}")).timeout(0).build()),
        };
        self.toasts.add_toast(adw::Toast::new("Starting Stellaris…"));
        // When the game exits, point out errors it logged about the mods.
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || child.wait()).await;
            let Some(ui) = weak.upgrade() else { return };
            let log = error_log::path(&ui.paths.user_dir);
            let fresh = std::fs::metadata(&log).and_then(|m| m.modified()).is_ok_and(|t| t >= started);
            if !fresh {
                return;
            }
            let report = ui.error_report();
            let from_mods = report.from_mods();
            if from_mods == 0 {
                return;
            }
            let patch = report.groups.iter().find(|g| g.is_patch).map_or(0, |g| g.total());
            let mut title = format!("Stellaris logged {} from your mods", conflicts_view::plural(from_mods, "error", "errors"));
            if patch > 0 {
                title.push_str(&format!(", {patch} from the merged patch"));
            }
            let toast = adw::Toast::builder().title(title).button_label("Show").action_name("win.errors").timeout(0).build();
            ui.toasts.add_toast(toast);
        });
    }

    /// The last session's error.log, credited to the enabled mods.
    fn error_report(&self) -> error_log::Report {
        let src = std::fs::read(error_log::path(&self.paths.user_dir)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let state = self.state.borrow();
        let mods: Vec<error_log::LogMod> = state
            .enabled
            .iter()
            .map(|&i| {
                let m = &state.mods[i];
                error_log::LogMod { id: m.id.clone(), name: display_name(m).to_owned(), root: m.content.clone(), steam_id: m.descriptor.remote_file_id, is_patch: m.id == patch::ID }
            })
            .collect();
        error_log::analyze(&src, &mods)
    }

    fn show_errors(self: &Rc<Self>) {
        let log = error_log::path(&self.paths.user_dir);
        if !log.is_file() {
            return self.toasts.add_toast(adw::Toast::new("No error.log yet; it's written when the game runs"));
        }
        let modified = std::fs::metadata(&log)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| glib::DateTime::from_unix_local(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64).ok())
            .and_then(|d| d.format("%A %H:%M").ok())
            .map(|s| s.to_string());
        let roots = {
            let state = self.state.borrow();
            state.mods.iter().filter_map(|m| Some((m.id.clone(), m.content.clone()?))).collect()
        };
        error_report_dialog::present(&self.window, &self.error_report(), log, modified, roots);
    }

    fn confirm_close(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(Some("Save Changes?"), Some("The load order has unsaved changes. Unsaved changes will be lost if you close."));
        dialog.add_responses(&[("cancel", "_Cancel"), ("discard", "_Discard"), ("save", "_Save")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            let close = match response {
                "save" => ui.save(),
                "discard" => true,
                _ => false,
            };
            if close {
                ui.state.borrow_mut().dirty = false;
                ui.window.close();
            }
        });
        dialog.present(Some(&self.window));
    }
}

fn boxed_list(placeholder: &str) -> gtk::ListBox {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.set_placeholder(Some(&gtk::Label::builder().label(placeholder).margin_top(12).margin_bottom(12).css_classes(["dim-label"]).build()));
    list
}

fn display_name(m: &Mod) -> &str {
    if m.descriptor.name.is_empty() { &m.id } else { &m.descriptor.name }
}

fn scrolled_page(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder().maximum_size(900).child(child).margin_top(12).margin_bottom(24).margin_start(12).margin_end(12).build();
    gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build()
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).css_classes(["heading"]).build()
}

fn status_page(title: &str, description: &str) -> gtk::Widget {
    adw::StatusPage::builder().icon_name("applications-games-symbolic").title(title).description(description).build().upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(enabled: &[usize]) -> State {
        let mods = (0..4)
            .map(|i| Mod {
                id: format!("mod/{i}.mod"),
                descriptor: mods::Descriptor { name: format!("{i}"), ..Default::default() },
                content: None,
                source: mods::Source::Local,
                load_position: enabled.iter().position(|&e| e == i),
                error: None,
            })
            .collect();
        State::new(mods)
    }

    #[test]
    fn moves_within_load_order() {
        let mut s = state(&[0, 1, 2]);
        s.move_enabled(0, 2);
        assert_eq!(s.enabled, [1, 0, 2]);
        s.move_enabled(2, 0);
        assert_eq!(s.enabled, [2, 1, 0]);
        s.move_enabled(0, 3);
        assert_eq!(s.enabled, [1, 0, 2]);
        assert!(s.dirty);

        let mut s = state(&[0, 1]);
        s.move_enabled(0, 1);
        assert_eq!(s.enabled, [0, 1]);
        assert!(!s.dirty, "dropping a mod onto its own slot is a no-op");
    }

    #[test]
    fn enables_and_disables_all_with_undo() {
        let mut s = state(&[2, 0]);
        let previous = s.set_all(true).unwrap();
        assert_eq!(s.enabled, [2, 0, 1, 3], "new mods go to the end, keeping the existing order");
        assert!(s.set_all(true).is_none());
        s.restore(previous);
        assert_eq!(s.enabled, [2, 0]);
        assert!(!s.dirty, "undo restores the saved state");
        s.set_all(false).unwrap();
        assert!(s.enabled.is_empty() && s.dirty);
        assert!(s.set_all(false).is_none());
    }

    #[test]
    fn removing_mods_keeps_the_order() {
        let mut s = state(&[3, 1, 0]);
        s.remove_mods(&["mod/1.mod".into()]);
        assert_eq!(s.enabled_ids(), ["mod/3.mod", "mod/0.mod"]);
        assert!(s.dirty);
        let mut s = state(&[0]);
        s.remove_mods(&["mod/2.mod".into()]);
        assert_eq!(s.enabled_ids(), ["mod/0.mod"]);
        assert!(!s.dirty, "removing a disabled mod doesn't change the load order");
    }

    #[test]
    fn patch_always_loads_last() {
        let mut s = state(&[0, 1]);
        s.mods.push(Mod { id: patch::ID.into(), descriptor: Default::default(), content: None, source: mods::Source::Local, load_position: None, error: None });
        s.set_enabled(4, true);
        s.set_enabled(2, true);
        assert_eq!(s.enabled, [0, 1, 2, 4], "enabling another mod still puts it before the patch");
        s.move_to(3, 0);
        assert_eq!(s.enabled, [0, 1, 2, 4], "the patch can't be moved up");
        s.move_to(0, 99);
        assert_eq!(s.enabled, [1, 2, 0, 4], "moving to the bottom stops above the patch");
        s.set_enabled_ids(&[patch::ID.into(), "mod/3.mod".into()]);
        assert_eq!(s.enabled, [3, 4], "collections and imports can't put it first");
        s.restore((vec![4, 0], false));
        assert_eq!(s.enabled, [0, 4]);
        s.set_all(true);
        assert_eq!(*s.enabled.last().unwrap(), 4);

        let mods = vec![
            Mod { id: patch::ID.into(), descriptor: Default::default(), content: None, source: mods::Source::Local, load_position: Some(0), error: None },
            Mod { id: "mod/a.mod".into(), descriptor: Default::default(), content: None, source: mods::Source::Local, load_position: Some(1), error: None },
        ];
        let s = State::new(mods);
        assert_eq!(s.enabled, [1, 0], "a saved order with the patch first is fixed on load");
        assert!(s.dirty);
    }

    #[test]
    fn moves_to_a_position() {
        let mut s = state(&[0, 1, 2, 3]);
        s.move_to(0, 2);
        assert_eq!(s.enabled, [1, 2, 0, 3], "the mod ends up at the typed position");
        s.move_to(3, 0);
        assert_eq!(s.enabled, [3, 1, 2, 0]);
        s.move_to(1, 99);
        assert_eq!(s.enabled, [3, 2, 0, 1], "past the end moves to the bottom");
        let before = s.revision;
        s.move_to(2, 2);
        assert_eq!(s.revision, before, "same position is a no-op");
    }

    #[test]
    fn toggling_appends_and_removes() {
        let mut s = state(&[2, 0]);
        assert_eq!(s.disabled(), [1, 3]);
        s.set_enabled(3, true);
        s.set_enabled(2, false);
        assert_eq!(s.enabled, [0, 3]);
        assert_eq!(s.enabled_ids(), ["mod/0.mod", "mod/3.mod"]);
    }
}
