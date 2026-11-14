//! Selecting several mods: Ctrl+click and Shift+click in either list, or
//! Select All Shown while a view, category or search narrows the list.
//! With two or more selected, a bar at the bottom changes them all at
//! once, each change one step to undo.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::Window;
use crate::mods::smart_sort::Category;

/// The bar shown while several mods are selected.
pub(super) struct BulkBar {
    pub revealer: gtk::Revealer,
    count: gtk::Label,
    collections: gio::Menu,
    moves: [gtk::Button; 2],
}

impl BulkBar {
    pub(super) fn new() -> Self {
        let count = gtk::Label::builder().css_classes(["heading"]).build();
        let button = |label: &str, action: &str| gtk::Button::builder().label(label).action_name(action).build();
        let enable = button(&t!("Enable"), "win.bulk-enable");
        let disable = button(&t!("Disable"), "win.bulk-disable");
        // Icons keep the bar narrow, so it never makes the window wider.
        let icon = |icon: &str, tip: &str, action: &str| gtk::Button::builder().icon_name(icon).tooltip_text(tip).action_name(action).build();
        let top = icon("go-top-symbolic", &t!("Move to Top"), "win.bulk-top");
        let bottom = icon("go-bottom-symbolic", &t!("Move to Bottom"), "win.bulk-bottom");

        let categories = gio::Menu::new();
        let item = gio::MenuItem::new(Some(&t!("Let Smart Sort Decide")), None);
        item.set_action_and_target_value(Some("win.bulk-category"), Some(&"".to_variant()));
        categories.append_item(&item);
        let named = gio::Menu::new();
        for c in Category::ALL {
            let item = gio::MenuItem::new(Some(&c.title()), None);
            item.set_action_and_target_value(Some("win.bulk-category"), Some(&c.key().to_variant()));
            named.append_item(&item);
        }
        categories.append_section(None, &named);
        let collections = gio::Menu::new();
        let more_menu = gio::Menu::new();
        more_menu.append_submenu(Some(&t!("_Category")), &categories);
        more_menu.append_submenu(Some(&t!("_Add to Collection")), &collections);
        let more = gtk::MenuButton::builder().icon_name("view-more-symbolic").menu_model(&more_menu).tooltip_text(t!("Category and Collections")).build();
        let clear = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text(t!("Clear the Selection")).action_name("win.bulk-clear").css_classes(["flat"]).build();

        let bar = gtk::ActionBar::new();
        bar.pack_start(&count);
        bar.pack_start(&enable);
        bar.pack_start(&disable);
        bar.pack_start(&top);
        bar.pack_start(&bottom);
        bar.pack_end(&clear);
        bar.pack_end(&more);
        let revealer = gtk::Revealer::builder().child(&bar).transition_type(gtk::RevealerTransitionType::SlideUp).build();
        Self { revealer, count, collections, moves: [top, bottom] }
    }
}

/// What a bar button does to the window.
type Run = fn(&Rc<Window>);

/// The mod index a row shows, from its `mod-N` name.
fn row_index(row: &gtk::ListBoxRow) -> Option<usize> {
    row.widget_name().strip_prefix("mod-")?.parse().ok()
}

/// Whether the widget at `(x, y)` in `list` is a control inside a row
/// (its switch, a button), whose clicks aren't about selecting.
fn on_control(list: &gtk::ListBox, x: f64, y: f64) -> bool {
    let mut widget = list.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(w) = widget {
        if w.is::<gtk::ListBoxRow>() {
            return false;
        }
        if w.is::<gtk::Switch>() || w.is::<gtk::CheckButton>() || w.is::<gtk::Button>() {
            return true;
        }
        widget = w.parent();
    }
    false
}

/// Whether Ctrl or Shift is held, which adds to a selection rather than
/// starting a new one.
fn extending(widget: &impl IsA<gtk::Widget>) -> bool {
    widget
        .display()
        .default_seat()
        .and_then(|seat| seat.keyboard())
        .is_some_and(|k| k.modifier_state().intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK))
}

impl Window {
    pub(super) fn setup_selection(self: &Rc<Self>, app: &adw::Application) {
        for (list, other) in [(&self.enabled_list, &self.disabled_list), (&self.disabled_list, &self.enabled_list)] {
            let weak = Rc::downgrade(self);
            let other = other.downgrade();
            let other_for_click = other.clone();
            list.connect_selected_rows_changed(move |list| {
                let Some(ui) = weak.upgrade() else { return };
                if ui.restoring_marks.get() {
                    return;
                }
                // A plain click starts over, in both lists.
                if !list.selected_rows().is_empty()
                    && !extending(list)
                    && let Some(other) = other.upgrade()
                {
                    ui.restoring_marks.set(true);
                    other.unselect_all();
                    ui.restoring_marks.set(false);
                }
                ui.read_marks();
            });

            // A plain click selects just that row, whatever GTK did with
            // it: it doesn't add to the selection everywhere.
            let click = gtk::GestureClick::builder().button(gdk::BUTTON_PRIMARY).propagation_phase(gtk::PropagationPhase::Capture).build();
            let pressed: Rc<std::cell::RefCell<Option<gtk::ListBoxRow>>> = Rc::default();
            let (list_ref, held) = (list.downgrade(), pressed.clone());
            click.connect_pressed(move |gesture, _, x, y| {
                let Some(list) = list_ref.upgrade() else { return };
                let plain = !gesture.current_event_state().intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
                *held.borrow_mut() = (plain && !on_control(&list, x, y)).then(|| list.row_at_y(y as i32)).flatten();
            });
            let (weak, list_ref, other) = (Rc::downgrade(self), list.downgrade(), other_for_click);
            click.connect_released(move |_, _, _, _| {
                let Some(row) = pressed.borrow_mut().take() else { return };
                let (weak, list_ref, other) = (weak.clone(), list_ref.clone(), other.clone());
                // After the list has handled the click itself.
                glib::idle_add_local_once(move || {
                    let (Some(ui), Some(list)) = (weak.upgrade(), list_ref.upgrade()) else { return };
                    let only_it = list.selected_rows() == [row.clone()] && other.upgrade().is_none_or(|o| o.selected_rows().is_empty());
                    if only_it || row.parent().as_ref() != Some(list.upcast_ref()) {
                        return;
                    }
                    ui.restoring_marks.set(true);
                    if let Some(other) = other.upgrade() {
                        other.unselect_all();
                    }
                    list.unselect_all();
                    list.select_row(Some(&row));
                    ui.restoring_marks.set(false);
                    ui.read_marks();
                });
            });
            list.add_controller(click);
        }

        let actions: [(&str, Run); 6] = [
            ("bulk-enable", |ui| ui.bulk_enable(true)),
            ("bulk-disable", |ui| ui.bulk_enable(false)),
            ("bulk-top", |ui| ui.bulk_move(true)),
            ("bulk-bottom", |ui| ui.bulk_move(false)),
            ("bulk-clear", |ui| ui.clear_marks()),
            ("select-shown", |ui| ui.select_shown()),
        ];
        for (name, run) in actions {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    run(&ui);
                }
            });
            self.window.add_action(&action);
        }
        for (name, run) in [("bulk-category", Window::bulk_category as fn(&Rc<Window>, &str)), ("bulk-add-to", Window::bulk_add_to)] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, target| {
                if let (Some(ui), Some(target)) = (weak.upgrade(), target.and_then(|t| t.get::<String>())) {
                    run(&ui, &target);
                }
            });
            self.window.add_action(&action);
        }
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.update_select_shown();
            }
        });
        app.set_accels_for_action("win.bulk-clear", &["Escape"]);
        self.update_bulk_bar();
    }

    /// The selected mods' indices: enabled ones in load order, then
    /// disabled ones as listed.
    fn marked_indices(&self) -> Vec<usize> {
        let state = self.state.borrow();
        let ids = self.marked.borrow();
        let mut indices: Vec<usize> = state.mods.iter().enumerate().filter(|(_, m)| ids.contains(&m.id)).map(|(i, _)| i).collect();
        indices.sort_by_key(|i| (state.enabled.iter().position(|e| e == i).unwrap_or(usize::MAX), *i));
        indices
    }

    /// Takes the selection from the lists, and shows the selected mod
    /// when there's one.
    fn read_marks(self: &Rc<Self>) {
        let indices: Vec<usize> = [&self.enabled_list, &self.disabled_list].iter().flat_map(|l| l.selected_rows()).filter_map(|r| row_index(&r)).collect();
        {
            let state = self.state.borrow();
            *self.marked.borrow_mut() = indices.iter().filter_map(|&i| state.mods.get(i).map(|m| m.id.clone())).collect();
        }
        if let [index] = indices[..] {
            let changed = self.selected.replace(Some(index)) != Some(index);
            if changed || self.inspector.child().is_none() {
                self.show_inspector();
            }
            if self.inspector_split.is_collapsed() {
                self.inspector_split.set_show_sidebar(true);
            }
        }
        self.update_bulk_bar();
    }

    /// Selects the marked mods' rows again after the list was rebuilt.
    pub(super) fn restore_marks(&self) {
        let wanted: Vec<String> = self.marked.borrow().clone();
        if wanted.is_empty() {
            return;
        }
        self.restoring_marks.set(true);
        for list in [&self.enabled_list, &self.disabled_list] {
            let mut child = list.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                let Ok(row) = c.downcast::<gtk::ListBoxRow>() else { continue };
                let id = row_index(&row).and_then(|i| self.state.borrow().mods.get(i).map(|m| m.id.clone()));
                if id.is_some_and(|id| wanted.contains(&id)) {
                    list.select_row(Some(&row));
                }
            }
        }
        self.restoring_marks.set(false);
        // Mods that are gone, after a reload, are no longer selected.
        let state = self.state.borrow();
        self.marked.borrow_mut().retain(|id| state.mods.iter().any(|m| &m.id == id));
    }

    /// Shows the bar while two or more mods are selected.
    pub(super) fn update_bulk_bar(&self) {
        let indices = self.marked_indices();
        let bar = &self.bulk;
        bar.revealer.set_reveal_child(indices.len() > 1);
        // Escape clears the selection only while there is one, so it still
        // reaches the search and dialogs otherwise.
        if let Some(clear) = self.window.lookup_action("bulk-clear").and_downcast::<gio::SimpleAction>() {
            clear.set_enabled(indices.len() > 1);
        }
        bar.count.set_label(&tnf!("{count} selected", "{count} selected", count = indices.len()));
        let any_enabled = {
            let state = self.state.borrow();
            indices.iter().any(|i| state.enabled.contains(i))
        };
        for b in &bar.moves {
            b.set_sensitive(any_enabled);
        }
        bar.collections.remove_all();
        let collections = self.collections.borrow();
        let active = collections.active().name.clone();
        for c in collections.list.iter().filter(|c| c.name != active) {
            let item = gio::MenuItem::new(Some(&c.name), None);
            item.set_action_and_target_value(Some("win.bulk-add-to"), Some(&c.name.to_variant()));
            bar.collections.append_item(&item);
        }
        if bar.collections.n_items() == 0 {
            // No action, so it shows greyed out.
            bar.collections.append(Some("No Other Collections"), None);
        }
    }

    fn clear_marks(self: &Rc<Self>) {
        if self.marked.borrow().len() < 2 {
            return;
        }
        self.restoring_marks.set(true);
        self.enabled_list.unselect_all();
        self.disabled_list.unselect_all();
        self.restoring_marks.set(false);
        self.marked.borrow_mut().clear();
        self.update_bulk_bar();
    }

    /// Selects every mod the current view, category or search shows.
    fn select_shown(self: &Rc<Self>) {
        self.restoring_marks.set(true);
        self.enabled_list.select_all();
        self.disabled_list.select_all();
        self.restoring_marks.set(false);
        self.read_marks();
    }

    /// Shows Select All Shown while something narrows the list.
    pub(super) fn update_select_shown(&self) {
        let narrowed = self.view.get() != crate::mods::filter::View::All || self.category.get().is_some() || !self.search.text().trim().is_empty();
        self.select_shown.set_visible(narrowed);
    }

    fn bulk_enable(self: &Rc<Self>, on: bool) {
        let indices = self.marked_indices();
        let changed = indices.iter().filter(|i| self.state.borrow().enabled.contains(i) != on).count();
        if !self.state.borrow_mut().set_enabled_many(&indices, on) {
            return;
        }
        self.refresh();
        let mods = tnf!("{count} mod", "{count} mods", count = changed);
        self.offer_undo(&if on { tf!("Enabled {mods}", mods = &mods) } else { tf!("Disabled {mods}", mods = &mods) });
    }

    fn bulk_move(self: &Rc<Self>, to_top: bool) {
        let indices = self.marked_indices();
        if !self.state.borrow_mut().move_many(&indices, to_top) {
            return;
        }
        self.refresh();
        let moved = indices.iter().filter(|i| self.state.borrow().enabled.contains(i)).count();
        let mods = tnf!("{count} mod", "{count} mods", count = moved);
        let to = if to_top { t!("top") } else { t!("bottom") };
        self.offer_undo(&tf!("Moved {mods} to the {to}", mods = &mods, to = &to));
    }

    /// Puts the selected mods in a Smart Sort section, or lets Smart Sort
    /// decide again (`key` empty).
    fn bulk_category(self: &Rc<Self>, key: &str) {
        let ids = self.marked.borrow().clone();
        {
            let mut settings = self.settings.borrow_mut();
            for id in &ids {
                if key.is_empty() {
                    settings.sort_categories.remove(id);
                } else {
                    settings.sort_categories.insert(id.clone(), key.to_owned());
                }
            }
        }
        self.save_settings("the mods' categories");
        self.refresh();
        self.add_toast(adw::Toast::new(&match Category::from_key(key) {
            Some(c) => tnf!(
                "Smart Sort will put {count} mod in {category}",
                "Smart Sort will put {count} mods in {category}",
                count = ids.len(),
                category = c.title()
            ),
            None => tnf!(
                "Smart Sort will choose where {count} mod goes",
                "Smart Sort will choose where {count} mods go",
                count = ids.len()
            ),
        }));
    }

    /// Adds the selected mods to another collection, after its own.
    fn bulk_add_to(self: &Rc<Self>, name: &str) {
        let ids = self.marked.borrow().clone();
        let added = {
            let mut collections = self.collections.borrow_mut();
            let Some(c) = collections.get_mut(name) else { return };
            let new: Vec<String> = ids.into_iter().filter(|id| !c.mods.contains(id)).collect();
            c.mods.extend(new.iter().cloned());
            new.len()
        };
        if added == 0 {
            return self.add_toast(adw::Toast::new(&tf!("“{name}” already has them", name = name)));
        }
        self.save_collections();
        self.add_toast(adw::Toast::new(&tnf!("Added {count} mod to “{name}”", "Added {count} mods to “{name}”", count = added, name = name)));
    }
}
