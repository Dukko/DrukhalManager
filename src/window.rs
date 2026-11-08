//! The main window: a load order editor.

use std::cell::{Cell, RefCell};
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::conflicts_view::{self, Analysis, MergeHooks};
use crate::merge::conflicts::{self, ModInput};
use crate::merge::patch::{self, Patch};
use crate::merge_dialog;
use crate::mods::{self, Mod};
use crate::preferences;
use crate::settings::{self, Settings};
use crate::paradox::game::GameInstall;
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
        Self { mods, enabled, dirty: false, revision: 0 }
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
        self.dirty = true;
        self.revision += 1;
    }

    /// Moves the mod at load position `from` in front of the mod currently at
    /// `before` (`before == len` moves it to the end).
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
        self.dirty = true;
        self.revision += 1;
        Some(previous)
    }

    fn restore(&mut self, (enabled, dirty): (Vec<usize>, bool)) {
        self.enabled = enabled;
        self.dirty = dirty;
        self.revision += 1;
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
    stack: adw::ViewStack,
    conflicts_page: adw::Bin,
    analysis: RefCell<Option<Rc<Analysis>>>,
    scanning: Cell<bool>,
    settings: RefCell<Settings>,
    /// `None` if the patch's manifest couldn't be read.
    patch: RefCell<Option<Patch>>,
    enabled_list: gtk::ListBox,
    disabled_list: gtk::ListBox,
    toasts: adw::ToastOverlay,
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
    menu.append(Some("_Preferences"), Some("win.preferences"));
    let menu_button = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Main Menu").primary(true).build();
    header.pack_end(&menu_button);
    header.pack_end(&launch);
    header.pack_end(&save);
    if game.is_none() {
        launch.set_tooltip_text(Some("Stellaris was not found in any Steam library"));
    }

    let enabled_list = boxed_list("No mods enabled");
    let disabled_list = boxed_list("Every mod is enabled");
    let status = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let all_toggle = gtk::CheckButton::builder().label("Enable All").tooltip_text("Enable or disable every mod").halign(gtk::Align::End).build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status.set_hexpand(true);
    top.append(&status);
    top.append(&all_toggle);
    content.append(&top);
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
        state: RefCell::new(State::new(found)),
        status,
        all_toggle: all_toggle.clone(),
        syncing: Cell::new(false),
        stack: stack.clone(),
        conflicts_page,
        analysis: RefCell::new(None),
        scanning: Cell::new(false),
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
    all_toggle.connect_toggled(move |_| {
        if let Some(ui) = weak.upgrade()
            && !ui.syncing.get()
        {
            ui.toggle_all();
        }
    });
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
            for (scanned, n) in a.report.counts_by_mod(a.names.len()).into_iter().enumerate() {
                if let Some(slot) = by_position.get_mut(a.load_positions[scanned]) {
                    *slot = n;
                }
            }
            by_position
        });
        for (position, &index) in state.enabled.iter().enumerate() {
            let conflicts = counts.as_ref().and_then(|c| c.get(position)).copied().unwrap_or(0);
            self.enabled_list.append(&self.row(&state.mods[index], index, Some(position), conflicts));
        }
        for index in state.disabled() {
            self.disabled_list.append(&self.row(&state.mods[index], index, None, 0));
        }

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
        let (revision, names, roots, load_positions, inputs) = {
            let state = self.state.borrow();
            // The merged patch is left out: its objects are resolutions, not
            // conflicts.
            let scanned: Vec<(usize, &Mod)> = state.enabled.iter().map(|&i| &state.mods[i]).enumerate().filter(|(_, m)| m.id != patch::ID).collect();
            let names = scanned.iter().map(|(_, m)| display_name(m).to_owned()).collect();
            let roots = scanned.iter().map(|(_, m)| m.content.clone()).collect();
            let positions = scanned.iter().map(|(p, _)| *p).collect();
            let inputs: Vec<ModInput> = scanned.iter().map(|(_, m)| ModInput { root: m.content.clone(), replace_path: m.descriptor.replace_path.clone() }).collect();
            (state.revision, names, roots, positions, inputs)
        };

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || conflicts::analyze(&inputs)).await;
            let Some(ui) = weak.upgrade() else { return };
            ui.scanning.set(false);
            match result {
                Ok(report) => {
                    let analysis = Rc::new(Analysis { revision: Cell::new(revision), names, roots, load_positions, report });
                    let hooks = ui.merge_hooks(&analysis);
                    ui.conflicts_page.set_child(Some(&conflicts_view::build(&analysis, ui.rescan_button(), hooks)));
                    ui.analysis.replace(Some(analysis));
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

    fn merge_hooks(self: &Rc<Self>, analysis: &Rc<Analysis>) -> Rc<MergeHooks> {
        let weak = Rc::downgrade(self);
        let is_merged = Box::new(move |category: &str, key: &str| {
            weak.upgrade().is_some_and(|ui| ui.patch.borrow().as_ref().is_some_and(|p| p.get(category, key).is_some()))
        });
        let weak = Rc::downgrade(self);
        let analysis = analysis.clone();
        let open = Box::new(move |index: usize, mark_merged: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_merge(&analysis, index, mark_merged);
            }
        });
        Rc::new(MergeHooks { is_merged, open })
    }

    fn open_merge(self: &Rc<Self>, analysis: &Rc<Analysis>, index: usize, mark_merged: Box<dyn Fn()>) {
        let conflict = &analysis.report.objects[index];
        let origins = conflict
            .definitions
            .iter()
            .filter_map(|d| {
                let root = analysis.roots[d.mod_position].clone()?;
                Some(merge_dialog::Origin { label: analysis.name(d.mod_position).to_owned(), root, file: d.file.clone() })
            })
            .collect();
        let (category, key) = (conflict.category.clone(), conflict.key.clone());
        let existing = self.patch.borrow().as_ref().and_then(|p| p.get(&category, &key).cloned());

        let weak = Rc::downgrade(self);
        let save = {
            let (category, key) = (category.clone(), key.clone());
            Box::new(move |script: &str, sources: Vec<String>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                ui.save_merge(&category, &key, script, sources)
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

    fn save_merge(&self, category: &str, key: &str, script: &str, sources: Vec<String>) -> Result<(), String> {
        let mut patch = self.patch.borrow_mut();
        if patch.is_none() {
            *patch = Some(Patch::open(&self.paths.user_dir).map_err(|e| format!("Could not read the merged patch: {e}"))?);
        }
        let version = self.game.as_ref().and_then(|g| g.version.as_deref());
        patch.as_mut().unwrap().save(&self.paths.user_dir, version, category, key, script, sources).map_err(|e| format!("Could not save the merged patch: {e}"))
    }

    /// Makes sure the merged patch is in the load order once it has content.
    fn patch_saved(self: &Rc<Self>) {
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
                state.set_enabled(index, true);
                // The patch isn't scanned, so the current report still holds.
                if let Some(a) = self.analysis.borrow().as_ref() {
                    a.revision.set(state.revision);
                }
            }
            !was_enabled
        };
        self.refresh();
        let toast = adw::Toast::builder().title(format!("Saved to {}", patch::NAME));
        let toast = if added { toast.title(format!("Saved to {}, which was added to the load order", patch::NAME)).button_label("Save Load Order").action_name("win.save") } else { toast };
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
        let toast = adw::Toast::builder().title(format!("{verb} {}", conflicts_view::plural(changed, "mod", "mods"))).button_label("Undo").build();
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

    fn row(self: &Rc<Self>, m: &Mod, index: usize, position: Option<usize>, conflicts: usize) -> adw::ActionRow {
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
        if let Some(error) = &m.error {
            let icon = gtk::Image::builder().icon_name("dialog-warning-symbolic").tooltip_text(error).css_classes(["warning"]).build();
            row.add_suffix(&icon);
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

        if let Some(position) = position {
            let handle = gtk::Image::builder().icon_name("list-drag-handle-symbolic").css_classes(["dim-label"]).build();
            let number = gtk::Label::builder().label((position + 1).to_string()).width_chars(3).xalign(1.0).css_classes(["dim-label", "numeric"]).build();
            row.add_prefix(&handle);
            row.add_prefix(&number);
            self.make_reorderable(&row, position);
        }
        row
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
        match launcher_db::sync(&self.paths.user_dir, &self.paths.backup_dir, &playset) {
            Ok(None) => adw::Toast::new("Load order saved"),
            Ok(Some(synced)) if synced.missing.is_empty() => adw::Toast::new(&format!("Load order saved and set as the “{}” playset in the Paradox Launcher", launcher_db::PLAYSET_NAME)),
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
        match game.launch() {
            Ok(()) => self.toasts.add_toast(adw::Toast::new("Starting Stellaris…")),
            Err(e) => self.toasts.add_toast(adw::Toast::builder().title(format!("Could not start Stellaris: {e}")).timeout(0).build()),
        }
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
    fn toggling_appends_and_removes() {
        let mut s = state(&[2, 0]);
        assert_eq!(s.disabled(), [1, 3]);
        s.set_enabled(3, true);
        s.set_enabled(2, false);
        assert_eq!(s.enabled, [0, 3]);
        assert_eq!(s.enabled_ids(), ["mod/0.mod", "mod/3.mod"]);
    }
}
