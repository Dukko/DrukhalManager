//! The load order list: rows, reordering, toggling and undo.

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
        // Rebuilding drops the selection; it's put back at the end.
        self.restoring_marks.set(true);
        self.enabled_list.remove_all();
        self.disabled_list.remove_all();
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
            .filter(|m| changes_checksum(m))
            .map(|m| display_name(m).to_owned())
            .collect();
        for (position, &index) in state.enabled.iter().enumerate() {
            let conflicts = counts
                .as_ref()
                .and_then(|c| c.get(position))
                .copied()
                .unwrap_or(0);
            self.enabled_list.append(&self.row(
                &state.mods[index],
                index,
                Some(position),
                conflicts,
                issues.get(&index).unwrap_or(&none),
            ));
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
        for index in disabled {
            self.disabled_list.append(&self.row(
                &state.mods[index],
                index,
                None,
                0,
                issues.get(&index).unwrap_or(&none),
            ));
        }
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
        // The facts the filter reads came after the rows.
        if self.category.get().is_some() {
            self.enabled_list.invalidate_filter();
        }
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
                let Ok(files) = mods::archive::files(content) else { continue };
                cache.insert(content.clone(), mods::smart_sort::Profile::read(&files));
            }
            profiles.insert(i, cache[content].clone());
        }
        let overrides = self.settings.borrow().sort_categories.iter().filter_map(|(id, key)| Some((id.clone(), mods::smart_sort::Category::from_key(key)?))).collect();
        mods::smart_sort::plan(&state.mods, &state.enabled, &profiles, &overrides, &patch::is_patch)
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
        let verb = if enable { "Enabled" } else { "Disabled" };
        self.offer_undo(&format!(
            "{verb} {}",
            conflicts_view::plural(changed, "mod", "mods")
        ));
    }

    pub(super) fn sort_by_dependencies(self: &Rc<Self>) {
        if !self.state.borrow_mut().sort_by_dependencies() {
            return;
        }
        self.refresh();
        self.offer_undo("Sorted mods after their dependencies");
    }

    pub(super) fn sort_alphabetically(self: &Rc<Self>) {
        if !self.state.borrow_mut().sort_alphabetically() {
            return self.add_toast(adw::Toast::new("Already in alphabetical order"));
        }
        self.refresh();
        self.offer_undo("Sorted mods alphabetically, after their dependencies");
    }

    /// Shows `title` with an Undo button that undoes the last change.
    pub(super) fn offer_undo(self: &Rc<Self>, title: &str) {
        let toast = adw::Toast::builder()
            .title(title)
            .button_label("Undo")
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.state.borrow_mut().undo();
                ui.refresh();
            }
        });
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
            Some(after) => format!("Loads after {after} once it's installed"),
            None => "Loads first once it's installed".to_owned(),
        };
        let row = adw::ActionRow::builder().title(&p.name).subtitle(subtitle).use_markup(false).css_classes(["dim-label"]).build();
        row.add_prefix(&gtk::Image::builder().icon_name("action-unavailable-symbolic").tooltip_text("Not installed, so it's left out of the game's load order").build());
        if let Some(steam_id) = p.steam_id {
            let open = gtk::Button::builder().icon_name("adw-external-link-symbolic").tooltip_text("Open on the Steam Workshop to subscribe").valign(gtk::Align::Center).css_classes(["flat"]).build();
            let window = self.window.downgrade();
            open.connect_clicked(move |_| {
                gtk::UriLauncher::new(&crate::paradox::workshop::page_url(steam_id)).launch(window.upgrade().as_ref(), None::<&gio::Cancellable>, |_| {});
            });
            row.add_suffix(&open);
        }
        let remove = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Remove from Collection").valign(gtk::Align::Center).css_classes(["flat"]).build();
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
        self.ask_text("Rename Mod", "Name", &current, "_Rename", move |ui, name| {
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
        text.update_property(&[gtk::accessible::Property::Label("Note")]);
        let scrolled = gtk::ScrolledWindow::builder().child(&text).hscrollbar_policy(gtk::PolicyType::Never).min_content_height(110).max_content_height(260).propagate_natural_height(true).css_classes(["card"]).build();
        let dialog = adw::AlertDialog::new(Some(if current.is_some() { "Edit Note" } else { "Add Note" }), Some(&name));
        dialog.set_extra_child(Some(&scrolled));
        dialog.add_response("cancel", "_Cancel");
        if current.is_some() {
            dialog.add_response("remove", "_Remove");
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        }
        dialog.add_response("save", "_Save");
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

    pub(super) fn row(
        self: &Rc<Self>,
        m: &Mod,
        index: usize,
        position: Option<usize>,
        conflicts: usize,
        issues: &[checks::Issue],
    ) -> adw::ActionRow {
        let d = &m.descriptor;
        let alias = self.settings.borrow().aliases.get(&m.id).cloned();
        let name = alias.as_deref().unwrap_or(display_name(m));
        let source = match m.source {
            mods::Source::Workshop => "Workshop",
            mods::Source::Local => "Local",
        };
        let mut tooltip = Vec::new();
        if alias.is_some() {
            // The real name, which search also matches.
            tooltip.push(display_name(m).trim().to_owned());
        }
        tooltip.push(source.to_owned());
        if !d.tags.is_empty() {
            tooltip.push(d.tags.join(", "));
        }
        let mut tooltip = tooltip.join(" · ");
        if let Some(content) = &m.content {
            tooltip.push('\n');
            tooltip.push_str(&content.display().to_string());
        }

        let row = adw::ActionRow::builder()
            .title(name)
            .title_lines(1)
            .tooltip_text(tooltip)
            .use_markup(false)
            .css_classes(["mod-row"])
            .build();
        // First, so it ends up nearest the name: later prefixes go before it.
        row.add_prefix(&self.thumbnail_sized(m, 28));

        let note = self.settings.borrow().notes.get(&m.id).cloned();
        if let Some(note) = &note {
            row.add_suffix(&gtk::Image::builder().icon_name("document-edit-symbolic").tooltip_text(note).valign(gtk::Align::Center).css_classes(["dim-label"]).build());
        }

        let game = self.game.as_ref().and_then(|g| g.version.as_deref());
        let outdated = game.is_some_and(|game| !d.game_version().is_empty() && !checks::version_matches(d.game_version(), game));
        if !d.game_version().is_empty() {
            let version = gtk::Label::builder()
                .label(d.game_version())
                .width_chars(7)
                .xalign(0.0)
                .valign(gtk::Align::Center)
                .tooltip_text(if outdated { "Doesn't say it supports this game version" } else { "The game version it supports" })
                .css_classes(["caption", "numeric", if outdated { "warning" } else { "dim-label" }])
                .build();
            row.add_suffix(&version);
        }
        if conflicts > 0 {
            let badge = gtk::Label::builder()
                .label(conflicts.to_string())
                .tooltip_text(format!("{}; see the Conflicts page (Alt+2)", conflicts_view::plural(conflicts, "conflict", "conflicts")))
                .valign(gtk::Align::Center)
                .css_classes(["caption", "numeric", "conflict-count"])
                .build();
            row.add_suffix(&badge);
        }
        let no_achievements = changes_checksum(m);
        let mut text = vec![name.to_owned(), display_name(m).to_owned(), source.to_owned(), d.tags.join(" ")];
        text.extend(d.remote_file_id.map(|id| id.to_string()));
        text.extend(note);
        self.search_facts.borrow_mut().insert(
            index,
            mods::filter::Facts {
                text: text.join(" ").to_lowercase(),
                enabled: position.is_some(),
                achievements: !no_achievements,
                source: m.source,
                version: d.game_version().to_owned(),
                conflicts: conflicts > 0,
                outdated,
                // Set below, once the problems are known.
                attention: false,
                // Set by `refresh`, from the Smart Sort plan.
                category: None,
            },
        );
        row.set_widget_name(&format!("mod-{index}"));
        if no_achievements {
            row.add_suffix(
                &gtk::Image::builder()
                    .icon_name("action-unavailable-symbolic")
                    .tooltip_text("Turns off achievements: it changes files the game checksums")
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        if let Some((label, tooltip, class)) = steam_badge(m.steam) {
            let badge = gtk::Label::builder()
                .label(label)
                .tooltip_text(tooltip)
                .valign(gtk::Align::Center)
                .css_classes([class, "caption"])
                .build();
            row.add_suffix(&badge);
        }
        let is_patch = patch::is_patch(&m.id);
        // One icon for everything wrong with this mod, worst first.
        let mut problems = self.extra_problems(m);
        problems.extend(issues.iter().map(|i| (i.severity, i.message.clone())));
        problems.sort_by(|a, b| b.0.cmp(&a.0));
        if let Some(f) = self.search_facts.borrow_mut().get_mut(&index) {
            f.attention = problems.iter().any(|(s, _)| *s == checks::Severity::Warning);
        }
        if let Some((worst, _)) = problems.first() {
            let (icon, class) = match worst {
                checks::Severity::Warning => ("dialog-warning-symbolic", "warning"),
                checks::Severity::Info => ("dialog-information-symbolic", "dim-label"),
            };
            let tooltip = problems
                .iter()
                .map(|(_, m)| m.as_str())
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
        self.row_menu(&row, m, index, position, conflicts, issues);

        let toggle = gtk::Switch::builder()
            .active(position.is_some())
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

        if let (Some(position), true) = (position, is_patch) {
            // Pinned: no handle, no editable position.
            let lock = gtk::Image::builder()
                .icon_name("changes-prevent-symbolic")
                .tooltip_text("Always loads last, so its merges and fixes win")
                .css_classes(["dim-label"])
                .build();
            let number = gtk::Label::builder()
                .label((position + 1).to_string())
                .width_chars(3)
                .xalign(1.0)
                .css_classes(["dim-label", "numeric"])
                .build();
            row.add_prefix(&lock);
            row.add_prefix(&number);
        } else if let Some(position) = position {
            let handle = gtk::Image::builder()
                .icon_name("list-drag-handle-symbolic")
                .css_classes(["dim-label"])
                .build();
            row.add_prefix(&handle);
            row.add_prefix(&self.position_entry(position));
            self.make_reorderable(&row, position);
        }
        row
    }

    /// Problems with the mod that aren't load order checks: an unreadable
    /// descriptor, and for the merged patch, merges that need a look.
    pub(super) fn extra_problems(&self, m: &Mod) -> Vec<(checks::Severity, String)> {
        let mut problems = Vec::new();
        if let Some(error) = &m.error {
            problems.push((checks::Severity::Warning, error.clone()));
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
                format!(
                    "{} saved by an older version {} checking; see the Conflicts page",
                    conflicts_view::plural(untracked, "merge", "merges"),
                    if untracked == 1 { "needs" } else { "need" }
                ),
            ));
        }
        if outdated > 0 {
            problems.push((
                checks::Severity::Warning,
                format!(
                    "{} out of date because mods changed; see the Conflicts page",
                    conflicts_view::plural(outdated, "merge or fix is", "merges or fixes are")
                ),
            ));
        }
        problems
    }

    /// The row's menu, opened by right-clicking it: the same actions as the
    /// inspector, for when it's out of the way.
    fn row_menu(self: &Rc<Self>, row: &adw::ActionRow, m: &Mod, index: usize, position: Option<usize>, conflicts: usize, issues: &[checks::Issue]) {
        let is_patch = patch::is_patch(&m.id);
        let menu = gio::Menu::new();
        let index_variant = (index as u32).to_variant();
        if position.is_some() && !is_patch {
            let moves = gio::Menu::new();
            for (label, action) in [
                ("Move to _Top", "win.move-top"),
                ("Move to _Bottom", "win.move-bottom"),
            ] {
                let item = gio::MenuItem::new(Some(label), None);
                item.set_action_and_target_value(Some(action), Some(&index_variant));
                moves.append_item(&item);
            }
            menu.append_section(None, &moves);
        }
        if position.is_some() && issues.iter().any(|i| i.message.starts_with("Requires “")) {
            let item = gio::MenuItem::new(Some("_Get Required Mods"), None);
            item.set_action_and_target_value(Some("win.get-requirements"), Some(&index_variant));
            menu.append_item(&item);
        }
        let item = gio::MenuItem::new(Some("_Rename…"), None);
        item.set_action_and_target_value(Some("win.rename-mod"), Some(&index_variant));
        menu.append_item(&item);
        let has_note = self.settings.borrow().notes.contains_key(&m.id);
        let item = gio::MenuItem::new(Some(if has_note { "Edit _Note…" } else { "Add _Note…" }), None);
        item.set_action_and_target_value(Some("win.edit-note"), Some(&index_variant));
        menu.append_item(&item);
        if m.content.is_some() {
            let item = gio::MenuItem::new(Some("Open Folder"), None);
            item.set_action_and_target_value(Some("win.open-folder"), Some(&index_variant));
            menu.append_item(&item);
        }
        if conflicts > 0 {
            let item = gio::MenuItem::new(Some("Show _Conflicts"), None);
            item.set_action_and_target_value(Some("win.show-conflicts"), Some(&index_variant));
            menu.append_item(&item);
        }
        if m.descriptor.remote_file_id.is_some() {
            let workshop = gio::Menu::new();
            for (label, action) in [
                ("Open in _Steam", "win.open-in-steam"),
                ("Open Workshop Page in _Browser", "win.open-workshop"),
                ("Copy _Workshop Link", "win.copy-workshop-link"),
                ("_Unsubscribe in Steam…", "win.unsubscribe"),
            ] {
                if action == "win.unsubscribe" && m.steam == Some(ItemState::Unsubscribed) {
                    continue;
                }
                let item = gio::MenuItem::new(Some(label), None);
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
    pub(super) fn position_entry(self: &Rc<Self>, position: usize) -> gtk::Entry {
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

    pub(super) fn make_reorderable(self: &Rc<Self>, row: &adw::ActionRow, position: usize) {
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        source.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(
                &(position as u32).to_value(),
            ))
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
                let before = position + usize::from(y > f64::from(row.height()) / 2.0);
                ui.update(move |state| state.move_enabled(from as usize, before));
                true
            }
        ));
        row.add_controller(target);
    }
}

/// Whether the mod changes checksummed files (zipped mods are unpacked once,
/// and cached).
fn changes_checksum(m: &Mod) -> bool {
    m.content
        .as_deref()
        .and_then(|c| if c.is_file() { mods::archive::files(c).ok() } else { Some(c.to_path_buf()) })
        .is_some_and(|root| crate::multiplayer::changes_checksum(&root))
}

/// A label for what Steam is doing with a Workshop mod, if it's worth
/// knowing before playing: its text, tooltip and style class.
fn steam_badge(state: Option<ItemState>) -> Option<(&'static str, &'static str, &'static str)> {
    match state? {
        ItemState::Installed => None,
        ItemState::UpdatePending => Some((
            "Update pending",
            "Steam has a newer version it hasn't finished downloading. Let it finish before playing, or the game may load a half-updated mod.",
            "warning",
        )),
        ItemState::Downloading => Some(("Downloading", "Steam hasn't finished downloading this mod", "warning")),
        ItemState::Unsubscribed => Some((
            "Unsubscribed",
            "You're no longer subscribed to this mod in Steam. Once Steam removes its files, Repair in the banner above the list can remove it from the list.",
            "dim-label",
        )),
    }
}
