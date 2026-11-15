//! The load order list: rows, reordering, toggling and undo.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::Window;
use super::display_name;
use super::state::State;
use crate::conflicts_view;
use crate::merge::patch;
use crate::mods::{self, Mod, checks};
use crate::paradox::steam::ItemState;

impl Window {
    pub(super) fn refresh(self: &Rc<Self>) {
        self.apply_requirements();
        self.fetch_requirements();
        let frozen_updates = self.frozen_updates();
        self.check_published();
        let (updated_since, waiting): (Vec<String>, Vec<String>) = {
            let (updated, waiting) = (self.updated_since_last_session(), self.waiting_updates());
            let state = self.state.borrow();
            let names = |found: Vec<(usize, u64)>| found.iter().map(|&(i, _)| display_name(&state.mods[i]).to_owned()).collect();
            (names(updated), names(waiting))
        };
        let state = self.state.borrow();
        self.search_facts.borrow_mut().clear();
        // Moving rows can drop the selection; it's put back at the end.
        self.restoring_marks.set(true);
        // Conflict counts are only shown while they match the load order.
        let counts = self
            .analysis
            .borrow()
            .as_ref()
            .filter(|a| a.revision.get() == state.revision)
            .map(|a| {
                let mut by_position = vec![0; state.enabled.len()];
                let settings = self.settings.borrow();
                let ignored = |k: &str| settings.ignored.contains(k);
                let rules = crate::merge::ignore_rules::Rules::parse(&settings.ignore_rules);
                let ignored = conflicts_view::Ignores {
                    keys: &ignored,
                    rules: &rules,
                    names: &a.names,
                    dependencies: (!settings.show_dependency_conflicts).then_some(&a.depends_on[..]),
                };
                let counts = a.report.counts_by_mod_where(
                    a.names.len(),
                    |c| !conflicts_view::file_ignored(&ignored, c),
                    |c| !conflicts_view::object_ignored(&ignored, c),
                );
                for (scanned, n) in counts.into_iter().enumerate() {
                    if let Some(slot) = by_position.get_mut(a.load_positions[scanned]) {
                        *slot = n;
                    }
                }
                by_position
            });
        let game_version = self.game.as_ref().and_then(|g| g.version.as_deref());
        let issues = checks::check(&state.mods, &state.enabled, game_version);
        {
            let mut conflicts = self.last_conflicts.borrow_mut();
            conflicts.clear();
            if let Some(counts) = &counts {
                conflicts.extend(state.enabled.iter().zip(counts).map(|(&i, &n)| (i, n)));
            }
        }
        self.last_issues.replace(issues.clone());
        // Each enabled mod's Smart Sort category, and the runs it makes,
        // before the rows go in, since their headers ask for them.
        let placed = self.smart_plan(&state);
        *self.category_runs.borrow_mut() = mods::smart_sort::runs(&state.enabled, &placed).into_iter().map(|run| (state.enabled[run.start], run)).collect();
        let none = Vec::new();
        let no_achievements: Vec<String> = state
            .enabled
            .iter()
            .map(|&i| &state.mods[i])
            .filter(|m| self.changes_checksum(m))
            .map(|m| display_name(m).to_owned())
            .collect();
        let mut rows = self.rows.borrow_mut();
        let mut shown = std::collections::HashSet::new();
        let mut enabled_rows = Vec::with_capacity(state.enabled.len());
        for (position, &index) in state.enabled.iter().enumerate() {
            let conflicts = counts
                .as_ref()
                .and_then(|c| c.get(position))
                .copied()
                .unwrap_or(0);
            let m = &state.mods[index];
            let data = self.row_data(m, true, conflicts, issues.get(&index).unwrap_or(&none));
            self.search_facts.borrow_mut().insert(index, data.facts());
            shown.insert(index);
            enabled_rows.push(self.keep_or_build(&mut rows, m, index, Some(position), data));
        }
        {
            let mut facts = self.search_facts.borrow_mut();
            for p in &placed {
                if let Some(f) = facts.get_mut(&p.index) {
                    f.category = Some(p.category);
                }
            }
        }
        self.missing_list.remove_all();
        let placeholders = self.collections.borrow().active().placeholders(&state.mods);
        let show_missing = !placeholders.is_empty() && self.view.get() == mods::filter::View::All && self.category.get().is_none();
        self.missing_heading.set_visible(show_missing);
        self.missing_list.set_visible(show_missing);
        for p in placeholders {
            self.missing_list.append(&self.placeholder_row(p));
        }
        let disabled = {
            let settings = self.settings.borrow();
            state.disabled_in(settings.disabled_order, &settings.aliases)
        };
        let mut disabled_rows = Vec::new();
        for index in disabled {
            let m = &state.mods[index];
            let data = self.row_data(m, false, 0, issues.get(&index).unwrap_or(&none));
            self.search_facts.borrow_mut().insert(index, data.facts());
            shown.insert(index);
            disabled_rows.push(self.keep_or_build(&mut rows, m, index, None, data));
        }
        rows.retain(|i, _| shown.contains(i));
        // The enabled list first: a mod that moved between lists has a new
        // row, and the old one is dropped by whichever list still holds it.
        place(&self.enabled_list, &enabled_rows);
        place(&self.disabled_list, &disabled_rows);
        drop(rows);
        let misordered = issues
            .values()
            .flatten()
            .filter(|i| i.fixed_by_sorting)
            .count();
        let missing_required: std::collections::HashSet<&str> = state
            .enabled
            .iter()
            .filter_map(|i| issues.get(i))
            .flatten()
            .filter(|i| super::overview::is_missing_requirement(i))
            .map(|i| i.message.as_str())
            .collect();
        let outdated = game_version.map_or(0, |game| {
            state.enabled.iter().filter(|&&i| {
                let v = state.mods[i].descriptor.game_version();
                !v.is_empty() && !checks::version_matches(v, game)
            }).count()
        });
        let health = super::overview::Health {
            enabled: state.enabled.len(),
            total: state.mods.len(),
            dirty: state.dirty,
            version: game_version.map(str::to_owned),
            outdated,
            missing_required: missing_required.len(),
            misordered,
            no_achievements,
            stale_merges: self.stale.borrow().len(),
            frozen_updates,
            updated_since,
            waiting,
        };
        let (enabled, total) = (state.enabled.len(), state.mods.len());
        self.syncing.set(true);
        self.all_toggle.set_active(total > 0 && enabled == total);
        self.all_toggle
            .set_inconsistent(enabled > 0 && enabled < total);
        self.all_toggle.set_sensitive(total > 0);
        self.syncing.set(false);
        let (can_undo, can_redo) = (state.can_undo(), state.can_redo());
        drop(state);
        self.refresh_health(health);
        self.refresh_view_counts(enabled);
        self.refresh_category_counts();
        // Reused rows aren't added again, so GTK doesn't run the filter and
        // header functions for them by itself; facts and Smart Sort runs may
        // have changed.
        self.enabled_list.invalidate_filter();
        self.disabled_list.invalidate_filter();
        self.enabled_list.invalidate_headers();
        // Keep showing the selected mod, in its new place.
        if let Some(index) = self.selected.get() {
            let name = format!("mod-{index}");
            let found = [&self.enabled_list, &self.disabled_list].into_iter().find_map(|list| {
                let mut child = list.first_child();
                while let Some(c) = child {
                    if c.widget_name() == name {
                        return Some((list, c.downcast::<gtk::ListBoxRow>().ok()?));
                    }
                    child = c.next_sibling();
                }
                None
            });
            if let Some((list, row)) = found
                && self.marked.borrow().len() < 2
            {
                list.select_row(Some(&row));
            }
            self.show_inspector();
        }
        self.restoring_marks.set(false);
        self.restore_marks();
        self.update_bulk_bar();
        self.undo_action.set_enabled(can_undo);
        self.redo_action.set_enabled(can_redo);
        self.scan_if_stale();
    }

    /// Where Smart Sort would put each enabled mod, in its order. Reads the
    /// folders of mods it hasn't seen yet.
    fn smart_plan(&self, state: &State) -> Vec<mods::smart_sort::Placement> {
        let mut cache = self.profiles.borrow_mut();
        let mut profiles = std::collections::HashMap::new();
        for &i in &state.enabled {
            let Some(content) = &state.mods[i].content else { continue };
            if !cache.contains_key(content) {
                let Some(files) = mods::archive::cached(content) else { continue };
                cache.insert(content.clone(), mods::smart_sort::Profile::read(&files));
            }
            profiles.insert(i, cache[content].clone());
        }
        let overrides = self.settings.borrow().sort_categories.iter().filter_map(|(id, key)| Some((id.clone(), mods::smart_sort::Category::from_key(key)?))).collect();
        mods::smart_sort::plan(&state.mods, &state.enabled, &profiles, &overrides, &patch::is_patch)
    }

    /// Whether the mod changes checksummed files, remembered until the
    /// mods' files may have changed (see `forget_mod_files`). Zipped mods
    /// count once their unpacked copy exists.
    fn changes_checksum(&self, m: &Mod) -> bool {
        let Some(content) = &m.content else { return false };
        if let Some(&known) = self.checksummed.borrow().get(content) {
            return known;
        }
        // Not unpacked yet: answer no, and ask again next time.
        let Some(root) = mods::archive::cached(content) else { return false };
        let changes = crate::multiplayer::changes_checksum(&root);
        self.checksummed.borrow_mut().insert(content.clone(), changes);
        changes
    }

    /// Undoes the last load order change, unless the user is typing in a
    /// text field (where Ctrl+Z belongs to the text).
    pub(super) fn undo(self: &Rc<Self>) {
        if self.entry_has_focus() {
            return;
        }
        if self.state.borrow_mut().undo() {
            self.refresh();
        }
    }

    /// Redoes an undone change, unless the user is typing in a text field.
    pub(super) fn redo(self: &Rc<Self>) {
        if self.entry_has_focus() {
            return;
        }
        if self.state.borrow_mut().redo() {
            self.refresh();
        }
    }

    fn entry_has_focus(&self) -> bool {
        gtk::prelude::GtkWindowExt::focus(&self.window).is_some_and(|w| {
            w.is::<gtk::Entry>() || w.is::<gtk::SearchEntry>()
        })
    }

    /// Like a select-all checkbox: enables everything unless everything is
    /// already enabled, in which case it disables everything. Offers undo,
    /// since disabling all forgets the load order.
    pub(super) fn toggle_all(self: &Rc<Self>) {
        let (enable, changed) = {
            let state = self.state.borrow();
            let enable = state.enabled.len() < state.mods.len();
            let changed = if enable {
                state.disabled().len()
            } else {
                state.enabled.len()
            };
            (enable, changed)
        };
        if changed == 0 {
            return;
        }
        self.state.borrow_mut().set_all(enable);
        self.refresh();
        let undo = if enable {
            tnf!("Enabled {count} mod", "Enabled {count} mods", count = changed)
        } else {
            tnf!("Disabled {count} mod", "Disabled {count} mods", count = changed)
        };
        self.offer_undo(&undo);
    }

    pub(super) fn sort_by_dependencies(self: &Rc<Self>) {
        if !self.state.borrow_mut().sort_by_dependencies() {
            return;
        }
        self.refresh();
        self.offer_undo(&t!("Sorted mods after their dependencies"));
    }

    pub(super) fn sort_alphabetically(self: &Rc<Self>) {
        if !self.state.borrow_mut().sort_alphabetically() {
            return self.add_toast(adw::Toast::new(&t!("Already in alphabetical order")));
        }
        self.refresh();
        self.offer_undo(&t!("Sorted mods alphabetically, after their dependencies"));
    }

    /// Shows `title` with an Undo button that undoes the last change.
    pub(super) fn offer_undo(self: &Rc<Self>, title: &str) {
        // `win.undo` does the work when the toast's button is clicked. The
        // bell tells Undo toasts apart by that action rather than the
        // translated label.
        let toast = adw::Toast::builder()
            .title(title)
            .button_label(t!("Undo"))
            .action_name("win.undo")
            .build();
        self.add_toast(toast);
    }

    /// Applies a state change after the current signal handler returns, since
    /// refreshing destroys the widget that emitted it.
    pub(super) fn update(self: &Rc<Self>, change: impl FnOnce(&mut State) + 'static) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                change(&mut ui.state.borrow_mut());
                ui.refresh();
            }
        });
    }

    /// A mod in the collection that isn't installed: where it would load,
    /// a link to get it, and a button to drop it from the collection.
    fn placeholder_row(self: &Rc<Self>, p: mods::collections::Placeholder) -> adw::ActionRow {
        let subtitle = match &p.after {
            Some(after) => tf!("Loads after {name} once it's installed", name = after),
            None => t!("Loads first once it's installed"),
        };
        let row = adw::ActionRow::builder().title(&p.name).subtitle(subtitle).use_markup(false).css_classes(["dim-label"]).build();
        row.add_prefix(&gtk::Image::builder().icon_name("action-unavailable-symbolic").tooltip_text(t!("Not installed, so it's left out of the game's load order")).build());
        if let Some(steam_id) = p.steam_id {
            let open = gtk::Button::builder().icon_name("adw-external-link-symbolic").tooltip_text(t!("Open on the Steam Workshop to subscribe")).valign(gtk::Align::Center).css_classes(["flat"]).build();
            let window = self.window.downgrade();
            open.connect_clicked(move |_| {
                gtk::UriLauncher::new(&crate::paradox::workshop::page_url(steam_id)).launch(window.upgrade().as_ref(), None::<&gio::Cancellable>, |_| {});
            });
            row.add_suffix(&open);
        }
        let remove = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text(t!("Remove from Collection")).valign(gtk::Align::Center).css_classes(["flat"]).build();
        let weak = Rc::downgrade(self);
        let id = p.id.clone();
        remove.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.remove_placeholder(&id);
            }
        });
        row.add_suffix(&remove);
        row
    }

    /// Asks for a name to show for the mod at `index` instead of its own.
    /// An empty name, or the mod's own, goes back to the real one.
    pub(super) fn rename_mod(self: &Rc<Self>, index: usize) {
        let (id, real) = {
            let state = self.state.borrow();
            let Some(m) = state.mods.get(index) else { return };
            (m.id.clone(), display_name(m).trim().to_owned())
        };
        let current = self.settings.borrow().aliases.get(&id).cloned().unwrap_or_else(|| real.clone());
        self.ask_text(&t!("Rename Mod"), &t!("Name"), &current, &t!("_Rename"), move |ui, name| {
            {
                let mut settings = ui.settings.borrow_mut();
                if name.is_empty() || name == real {
                    settings.aliases.remove(&id);
                } else {
                    settings.aliases.insert(id.clone(), name);
                }
            }
            ui.save_settings("the mod's name");
            ui.refresh();
        });
    }

    /// Asks for a note on the mod at `index`, shown on its row and in the
    /// inspector. An empty note removes it.
    pub(super) fn edit_note(self: &Rc<Self>, index: usize) {
        let (id, name) = {
            let state = self.state.borrow();
            let Some(m) = state.mods.get(index) else { return };
            let name = self.settings.borrow().aliases.get(&m.id).cloned().unwrap_or_else(|| display_name(m).trim().to_owned());
            (m.id.clone(), name)
        };
        let current = self.settings.borrow().notes.get(&id).cloned();
        let text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .build();
        text.buffer().set_text(current.as_deref().unwrap_or(""));
        text.update_property(&[gtk::accessible::Property::Label(&t!("Note"))]);
        let scrolled = gtk::ScrolledWindow::builder().child(&text).hscrollbar_policy(gtk::PolicyType::Never).min_content_height(110).max_content_height(260).propagate_natural_height(true).css_classes(["card"]).build();
        let dialog = adw::AlertDialog::new(Some(&if current.is_some() { t!("Edit Note") } else { t!("Add Note") }), Some(&name));
        dialog.set_extra_child(Some(&scrolled));
        dialog.add_response("cancel", &t!("_Cancel"));
        if current.is_some() {
            dialog.add_response("remove", &t!("_Remove"));
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        }
        dialog.add_response("save", &t!("_Save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        // Enter starts a new line; Ctrl+Enter saves.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            dialog,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                    dialog.emit_by_name::<()>("response", &[&"save"]);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        text.add_controller(keys);
        let weak = Rc::downgrade(self);
        let field = text.clone();
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            let buffer = field.buffer();
            let note = match response {
                "save" => buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().to_owned(),
                "remove" => String::new(),
                _ => return,
            };
            if note.as_str() == current.as_deref().unwrap_or("") {
                return;
            }
            {
                let mut settings = ui.settings.borrow_mut();
                if note.is_empty() {
                    settings.notes.remove(&id);
                } else {
                    settings.notes.insert(id.clone(), note);
                }
            }
            ui.save_settings("the note");
            ui.refresh();
        });
        dialog.present(Some(&self.window));
        text.grab_focus();
    }

    /// The row for the mod at `index`, kept from the last rebuild when it
    /// would be built the same, with its position updated.
    fn keep_or_build(self: &Rc<Self>, rows: &mut HashMap<usize, CachedRow>, m: &Mod, index: usize, position: Option<usize>, data: RowData) -> adw::ActionRow {
        if let Some(cached) = rows.get(&index)
            && cached.data == data
        {
            if let Some(p) = position
                && p != cached.position.get()
            {
                cached.position.set(p);
                match &cached.number {
                    Some(PositionWidget::Entry(entry)) => entry.set_text(&(p + 1).to_string()),
                    Some(PositionWidget::Label(label)) => label.set_label(&(p + 1).to_string()),
                    None => {}
                }
            }
            return cached.row.clone();
        }
        let position = Rc::new(Cell::new(position.unwrap_or(0)));
        let (row, number) = self.row(m, index, &data, &position);
        rows.insert(index, CachedRow { data, row: row.clone(), position, number });
        row
    }

    /// Everything `row` shows, read from the mod and the settings.
    fn row_data(&self, m: &Mod, enabled: bool, conflicts: usize, issues: &[checks::Issue]) -> RowData {
        let d = &m.descriptor;
        let alias = self.settings.borrow().aliases.get(&m.id).cloned();
        let game = self.game.as_ref().and_then(|g| g.version.as_deref());
        let outdated = game.is_some_and(|game| !d.game_version().is_empty() && !checks::version_matches(d.game_version(), game));
        // One icon for everything wrong with this mod, worst first.
        let mut problems = self.extra_problems(m);
        problems.extend(issues.iter().map(|i| (i.severity, i.message.clone(), false)));
        problems.sort_by_key(|a| std::cmp::Reverse(a.0));
        RowData {
            id: m.id.clone(),
            enabled,
            is_patch: patch::is_patch(&m.id),
            aliased: alias.is_some(),
            name: alias.unwrap_or_else(|| display_name(m).to_owned()),
            real_name: display_name(m).to_owned(),
            source: m.source,
            tags: d.tags.clone(),
            content: m.content.clone(),
            note: self.settings.borrow().notes.get(&m.id).cloned(),
            game_version: d.game_version().to_owned(),
            outdated,
            conflicts,
            no_achievements: self.changes_checksum(m),
            steam: m.steam,
            remote_file_id: d.remote_file_id,
            problems,
            has_requirement: issues.iter().any(|i| i.requirement.is_some()),
        }
    }

    /// Builds a mod's row from `data`. Only the thumbnail comes from `m`.
    /// `position` is read when the row is used, so it can be updated
    /// without building the row again. Also returns the widget showing the
    /// number, if the row has one.
    fn row(self: &Rc<Self>, m: &Mod, index: usize, data: &RowData, position: &Rc<Cell<usize>>) -> (adw::ActionRow, Option<PositionWidget>) {
        let mut tooltip = Vec::new();
        if data.aliased {
            // The real name, which search also matches.
            tooltip.push(data.real_name.trim().to_owned());
        }
        tooltip.push(data.source_label());
        if !data.tags.is_empty() {
            tooltip.push(data.tags.join(", "));
        }
        let mut tooltip = tooltip.join(" · ");
        if let Some(content) = &data.content {
            tooltip.push('\n');
            tooltip.push_str(&content.display().to_string());
        }

        let row = adw::ActionRow::builder()
            .title(&data.name)
            .title_lines(1)
            .tooltip_text(tooltip)
            .use_markup(false)
            .css_classes(["mod-row"])
            .build();
        // First, so it ends up nearest the name: later prefixes go before it.
        row.add_prefix(&self.thumbnail_sized(m, 28));

        if let Some(note) = &data.note {
            row.add_suffix(&gtk::Image::builder().icon_name("document-edit-symbolic").tooltip_text(note).valign(gtk::Align::Center).css_classes(["dim-label"]).build());
        }

        let outdated = data.outdated;
        if !data.game_version.is_empty() {
            let version = gtk::Label::builder()
                .label(&data.game_version)
                .width_chars(7)
                .xalign(0.0)
                .valign(gtk::Align::Center)
                .tooltip_text(if outdated { t!("Doesn't say it supports this game version") } else { t!("The game version it supports") })
                .css_classes(["caption", "numeric", if outdated { "warning" } else { "dim-label" }])
                .build();
            row.add_suffix(&version);
        }
        let conflicts = data.conflicts;
        if conflicts > 0 {
            let badge = gtk::Label::builder()
                .label(conflicts.to_string())
                .tooltip_text(tnf!("{count} conflict; see the Conflicts page (Alt+2)", "{count} conflicts; see the Conflicts page (Alt+2)", count = conflicts))
                .valign(gtk::Align::Center)
                .css_classes(["caption", "numeric", "conflict-count"])
                .build();
            row.add_suffix(&badge);
        }
        row.set_widget_name(&format!("mod-{index}"));
        if data.no_achievements {
            row.add_suffix(
                &gtk::Image::builder()
                    .icon_name("action-unavailable-symbolic")
                    .tooltip_text(t!("Turns off achievements: it changes files the game checksums"))
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        if let Some((label, tooltip, class)) = steam_badge(data.steam) {
            let badge = gtk::Label::builder()
                .label(&label)
                .tooltip_text(&tooltip)
                .valign(gtk::Align::Center)
                .css_classes([class, "caption"])
                .build();
            row.add_suffix(&badge);
        }
        if let Some((worst, _, _)) = data.problems.first() {
            let (icon, class) = match worst {
                checks::Severity::Warning => ("dialog-warning-symbolic", "warning"),
                checks::Severity::Info => ("dialog-information-symbolic", "dim-label"),
            };
            let tooltip = data
                .problems
                .iter()
                .map(|(_, m, _)| m.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            row.add_suffix(
                &gtk::Image::builder()
                    .icon_name(icon)
                    .tooltip_text(tooltip)
                    .css_classes([class])
                    .build(),
            );
        }
        self.row_menu(&row, data, index);

        let toggle = gtk::Switch::builder()
            .active(data.enabled)
            .valign(gtk::Align::Center)
            .build();
        let weak = Rc::downgrade(self);
        toggle.connect_active_notify(move |toggle| {
            let on = toggle.is_active();
            if let Some(ui) = weak.upgrade() {
                ui.update(move |state| state.set_enabled(index, on));
            }
        });
        row.add_suffix(&toggle);

        let mut number = None;
        if data.enabled && data.is_patch {
            // Pinned: no handle, no editable position.
            let lock = gtk::Image::builder()
                .icon_name("changes-prevent-symbolic")
                .tooltip_text(t!("Always loads last, so its merges and fixes win"))
                .css_classes(["dim-label"])
                .build();
            let label = gtk::Label::builder()
                .label((position.get() + 1).to_string())
                .width_chars(3)
                .xalign(1.0)
                .css_classes(["dim-label", "numeric"])
                .build();
            row.add_prefix(&lock);
            row.add_prefix(&label);
            number = Some(PositionWidget::Label(label));
        } else if data.enabled {
            let handle = gtk::Image::builder()
                .icon_name("list-drag-handle-symbolic")
                .css_classes(["dim-label"])
                .build();
            row.add_prefix(&handle);
            let entry = self.position_entry(position.clone());
            row.add_prefix(&entry);
            self.make_reorderable(&row, position.clone());
            number = Some(PositionWidget::Entry(entry));
        }
        (row, number)
    }

    /// Problems with the mod that aren't load order checks: an unreadable
    /// descriptor, and for the merged patch, merges that need a look.
    /// Warnings about a mod. The flag says whether the Conflicts page has
    /// the details, so the inspector can offer to open it without reading
    /// the translated message.
    pub(super) fn extra_problems(&self, m: &Mod) -> Vec<(checks::Severity, String, bool)> {
        let mut problems = Vec::new();
        if let Some(error) = &m.error {
            problems.push((checks::Severity::Warning, error.clone(), false));
        }
        let is_patch = patch::is_patch(&m.id);
        let (outdated, untracked) = if is_patch {
            let stale = self.stale.borrow();
            let untracked = stale.iter().filter(|s| s.untracked).count();
            (stale.len() - untracked, untracked)
        } else {
            (0, 0)
        };
        if untracked > 0 {
            problems.push((
                checks::Severity::Warning,
                tnf!(
                    "{count} merge saved by an older version needs checking; see the Conflicts page",
                    "{count} merges saved by an older version need checking; see the Conflicts page",
                    count = untracked
                ),
                true,
            ));
        }
        if outdated > 0 {
            problems.push((
                checks::Severity::Warning,
                tnf!(
                    "{count} merge or fix is out of date because mods changed; see the Conflicts page",
                    "{count} merges or fixes are out of date because mods changed; see the Conflicts page",
                    count = outdated
                ),
                true,
            ));
        }
        problems
    }

    /// The row's menu, opened by right-clicking it: the same actions as the
    /// inspector, for when it's out of the way.
    fn row_menu(self: &Rc<Self>, row: &adw::ActionRow, data: &RowData, index: usize) {
        let menu = gio::Menu::new();
        let index_variant = (index as u32).to_variant();
        if data.enabled && !data.is_patch {
            let moves = gio::Menu::new();
            for (label, action) in [
                (t!("Move to _Top"), "win.move-top"),
                (t!("Move to _Bottom"), "win.move-bottom"),
            ] {
                let item = gio::MenuItem::new(Some(&label), None);
                item.set_action_and_target_value(Some(action), Some(&index_variant));
                moves.append_item(&item);
            }
            menu.append_section(None, &moves);
        }
        if data.enabled && data.has_requirement {
            let item = gio::MenuItem::new(Some(&t!("_Get Required Mods")), None);
            item.set_action_and_target_value(Some("win.get-requirements"), Some(&index_variant));
            menu.append_item(&item);
        }
        let item = gio::MenuItem::new(Some(&t!("_Rename…")), None);
        item.set_action_and_target_value(Some("win.rename-mod"), Some(&index_variant));
        menu.append_item(&item);
        let has_note = data.note.is_some();
        let item = gio::MenuItem::new(Some(&if has_note { t!("Edit _Note…") } else { t!("Add _Note…") }), None);
        item.set_action_and_target_value(Some("win.edit-note"), Some(&index_variant));
        menu.append_item(&item);
        if data.content.is_some() {
            let item = gio::MenuItem::new(Some(&t!("Open Folder")), None);
            item.set_action_and_target_value(Some("win.open-folder"), Some(&index_variant));
            menu.append_item(&item);
        }
        if data.conflicts > 0 {
            let item = gio::MenuItem::new(Some(&t!("Show _Conflicts")), None);
            item.set_action_and_target_value(Some("win.show-conflicts"), Some(&index_variant));
            menu.append_item(&item);
        }
        if data.remote_file_id.is_some() {
            let workshop = gio::Menu::new();
            for (label, action) in [
                (t!("Open in _Steam"), "win.open-in-steam"),
                (t!("Open Workshop Page in _Browser"), "win.open-workshop"),
                (t!("Copy _Workshop Link"), "win.copy-workshop-link"),
                (t!("_Unsubscribe in Steam…"), "win.unsubscribe"),
            ] {
                if action == "win.unsubscribe" && data.steam == Some(ItemState::Unsubscribed) {
                    continue;
                }
                let item = gio::MenuItem::new(Some(&label), None);
                item.set_action_and_target_value(Some(action), Some(&index_variant));
                workshop.append_item(&item);
            }
            menu.append_section(None, &workshop);
        }
        let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        let row_weak = row.downgrade();
        click.connect_pressed(move |_, _, x, y| {
            let Some(row) = row_weak.upgrade() else { return };
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_parent(&row);
            popover.set_has_arrow(false);
            popover.set_halign(gtk::Align::Start);
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.connect_closed(|p| {
                let p = p.clone();
                glib::idle_add_local_once(move || p.unparent());
            });
            popover.popup();
        });
        row.add_controller(click);
    }

    /// The load position as an editable number: type a position and press
    /// Enter, or leave the field, to move the mod there.
    fn position_entry(self: &Rc<Self>, position: Rc<Cell<usize>>) -> gtk::Entry {
        let entry = gtk::Entry::builder()
            .text((position.get() + 1).to_string())
            .width_chars(3)
            .max_width_chars(4)
            .xalign(1.0)
            .input_purpose(gtk::InputPurpose::Digits)
            .valign(gtk::Align::Center)
            .tooltip_text(t!("Load position: type a number to move this mod there"))
            .css_classes(["numeric", "flat"])
            .build();
        let apply = {
            let weak = Rc::downgrade(self);
            move |entry: &gtk::Entry| {
                let Some(ui) = weak.upgrade() else { return };
                let position = position.get();
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

    fn make_reorderable(self: &Rc<Self>, row: &adw::ActionRow, position: Rc<Cell<usize>>) {
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        source.connect_prepare({
            let position = position.clone();
            move |_, _, _| Some(gdk::ContentProvider::for_value(&(position.get() as u32).to_value()))
        });
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
                let before = position.get() + usize::from(y > f64::from(row.height()) / 2.0);
                ui.update(move |state| state.move_enabled(from as usize, before));
                true
            }
        ));
        row.add_controller(target);
    }
}

/// A label for what Steam is doing with a Workshop mod, if it's worth
/// knowing before playing: its text, tooltip and style class.
fn steam_badge(state: Option<ItemState>) -> Option<(String, String, &'static str)> {
    match state? {
        ItemState::Installed => None,
        ItemState::UpdatePending => Some((
            t!("Update pending"),
            t!("Steam has a newer version it hasn't finished downloading. Let it finish before playing, or the game may load a half-updated mod."),
            "warning",
        )),
        ItemState::Downloading => Some((t!("Downloading"), t!("Steam hasn't finished downloading this mod"), "warning")),
        ItemState::Unsubscribed => Some((
            t!("Unsubscribed"),
            t!("You're no longer subscribed to this mod in Steam. Once Steam removes its files, Repair in the banner above the list can remove it from the list."),
            "dim-label",
        )),
    }
}

/// The widget showing an enabled row's load position.
pub(super) enum PositionWidget {
    Entry(gtk::Entry),
    /// The merged patch's position can't be edited.
    Label(gtk::Label),
}

/// Everything a mod's row shows or offers, apart from its position and
/// thumbnail. Two equal values build identical rows, so a row is kept
/// while its `RowData` stays the same.
#[derive(Clone, PartialEq)]
pub(super) struct RowData {
    id: String,
    enabled: bool,
    is_patch: bool,
    /// The alias, or the mod's own name.
    name: String,
    /// The mod's own name.
    real_name: String,
    aliased: bool,
    source: mods::Source,
    tags: Vec<String>,
    content: Option<std::path::PathBuf>,
    note: Option<String>,
    game_version: String,
    outdated: bool,
    conflicts: usize,
    no_achievements: bool,
    steam: Option<ItemState>,
    remote_file_id: Option<u64>,
    /// Severity, message, whether the Conflicts page has details; worst first.
    problems: Vec<(checks::Severity, String, bool)>,
    has_requirement: bool,
}

impl RowData {
    /// The translated name of where the mod comes from.
    fn source_label(&self) -> String {
        match self.source {
            mods::Source::Workshop => t!("Workshop"),
            mods::Source::Local => t!("Local"),
        }
    }

    /// What search and the filters know about this mod.
    fn facts(&self) -> mods::filter::Facts {
        let mut text = vec![self.name.clone(), self.real_name.clone(), self.source_label(), self.tags.join(" ")];
        text.extend(self.remote_file_id.map(|id| id.to_string()));
        text.extend(self.note.clone());
        mods::filter::Facts {
            text: text.join(" ").to_lowercase(),
            enabled: self.enabled,
            achievements: !self.no_achievements,
            source: self.source,
            version: self.game_version.clone(),
            conflicts: self.conflicts > 0,
            outdated: self.outdated,
            attention: self.problems.iter().any(|(s, _, _)| *s == checks::Severity::Warning),
            // Set by `refresh`, from the Smart Sort plan.
            category: None,
        }
    }
}

/// A row kept between rebuilds, with what it was built from.
pub(super) struct CachedRow {
    data: RowData,
    row: adw::ActionRow,
    position: Rc<Cell<usize>>,
    number: Option<PositionWidget>,
}

/// Makes `list` hold exactly `rows`, in order, moving only rows that are
/// out of place instead of emptying it.
fn place(list: &gtk::ListBox, rows: &[adw::ActionRow]) {
    for (i, row) in rows.iter().enumerate() {
        if list.row_at_index(i as i32).as_ref() == Some(row.upcast_ref::<gtk::ListBoxRow>()) {
            continue;
        }
        // It may be further down this list, or in the other one.
        if let Some(parent) = row.parent().and_then(|p| p.downcast::<gtk::ListBox>().ok()) {
            parent.remove(row);
        }
        list.insert(row, i as i32);
    }
    while let Some(extra) = list.row_at_index(rows.len() as i32) {
        list.remove(&extra);
    }
    debug_assert!(rows.iter().enumerate().all(|(i, r)| list.row_at_index(i as i32).as_ref() == Some(r.upcast_ref::<gtk::ListBoxRow>())));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> RowData {
        RowData {
            id: "ugc_1.mod".into(),
            enabled: true,
            is_patch: false,
            name: "My Alias".into(),
            real_name: "Real Name".into(),
            aliased: true,
            source: mods::Source::Local,
            tags: vec!["Fixes".into()],
            content: None,
            note: Some("Remember This".into()),
            game_version: "4.0.*".into(),
            outdated: true,
            conflicts: 2,
            no_achievements: true,
            steam: None,
            remote_file_id: Some(1234567),
            problems: vec![(checks::Severity::Warning, "Broken".into(), false)],
            has_requirement: false,
        }
    }

    #[test]
    fn row_facts_match_what_search_reads() {
        let mut data = data();
        let facts = data.facts();
        for part in ["my alias", "real name", "fixes", "remember this", "1234567"] {
            assert!(facts.text.contains(part), "{part} missing from {}", facts.text);
        }
        assert!(facts.enabled);
        assert!(!facts.achievements);
        assert!(facts.conflicts);
        assert!(facts.outdated);
        assert!(facts.attention);
        assert!(facts.category.is_none());
        data.problems[0].0 = checks::Severity::Info;
        assert!(!data.facts().attention);
    }
}
