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

impl Window {
    pub(super) fn refresh(self: &Rc<Self>) {
        let state = self.state.borrow();
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
        for index in state.disabled() {
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
        self.sort_button.set_visible(misordered > 0);

        let mut subtitle = Vec::new();
        if let Some(version) = self.game.as_ref().and_then(|g| g.version.as_deref()) {
            subtitle.push(format!("Stellaris {version}"));
        }
        subtitle.push(format!(
            "{} of {} mods enabled",
            state.enabled.len(),
            state.mods.len()
        ));
        if !state.enabled.is_empty() {
            subtitle.push(if no_achievements.is_empty() { "Achievements on" } else { "Achievements off" }.to_owned());
        }
        if state.dirty {
            subtitle.push("Unsaved changes".to_owned());
        }
        self.status.set_label(&subtitle.join(" · "));
        self.status.set_tooltip_text(Some(&if no_achievements.is_empty() {
            "No enabled mod changes the files the game checksums, so achievements stay on".to_owned()
        } else {
            format!(
                "Achievements are off because {} the files the game checksums:\n{}",
                if no_achievements.len() == 1 { "this mod changes" } else { "these mods change" },
                no_achievements.join("\n")
            )
        }));
        let (enabled, total) = (state.enabled.len(), state.mods.len());
        self.syncing.set(true);
        self.all_toggle.set_active(total > 0 && enabled == total);
        self.all_toggle
            .set_inconsistent(enabled > 0 && enabled < total);
        self.all_toggle.set_sensitive(total > 0);
        self.syncing.set(false);
        let (can_undo, can_redo) = (state.can_undo(), state.can_redo());
        drop(state);
        self.undo_action.set_enabled(can_undo);
        self.redo_action.set_enabled(can_redo);
        self.scan_if_stale();
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
        self.toasts.add_toast(toast);
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

    pub(super) fn row(
        self: &Rc<Self>,
        m: &Mod,
        index: usize,
        position: Option<usize>,
        conflicts: usize,
        issues: &[checks::Issue],
    ) -> adw::ActionRow {
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

        let row = adw::ActionRow::builder()
            .title(name)
            .subtitle(subtitle.join(" · "))
            .use_markup(false)
            .build();
        if let Some(content) = &m.content {
            row.set_tooltip_text(Some(&content.display().to_string()));
        }
        // First, so it ends up nearest the name: later prefixes go before it.
        row.add_prefix(&self.thumbnail(m));

        if conflicts > 0 {
            let badge = gtk::Label::builder()
                .label(conflicts_view::plural(conflicts, "conflict", "conflicts"))
                .tooltip_text("See the Conflicts page (Alt+2) for details")
                .valign(gtk::Align::Center)
                .css_classes(["dim-label", "caption"])
                .build();
            row.add_suffix(&badge);
        }
        if changes_checksum(m) {
            let badge = gtk::Label::builder()
                .label("No achievements")
                .tooltip_text("Changes files the game checksums, so achievements are off while it's enabled")
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
        problems.extend(issues.iter().map(|i| (i.severity, i.message.clone())));
        problems.sort_by(|a, b| b.0.cmp(&a.0));
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
            let item = gio::MenuItem::new(Some("Open Workshop Page"), None);
            item.set_action_and_target_value(Some("win.open-workshop"), Some(&index_variant));
            menu.append_item(&item);
        }

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
        row.set_activatable_widget(Some(&toggle));
        if menu.n_items() > 0 {
            row.add_suffix(
                &gtk::MenuButton::builder()
                    .icon_name("view-more-symbolic")
                    .menu_model(&menu)
                    .valign(gtk::Align::Center)
                    .tooltip_text("More")
                    .css_classes(["flat"])
                    .build(),
            );
        }

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
            row.set_subtitle(&format!(
                "{} · Always loads last",
                row.subtitle().unwrap_or_default()
            ));
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
