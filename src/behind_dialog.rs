//! The objects enabled mods replace that a game update changed: the mods
//! still load their own copies, so the game's changes don't apply to them.
//! Each can be brought up to date with AI and saved to the merged patch, one
//! at a time or all at once. Update All sends every request first, saving
//! nothing, then shows the results one by one to save or skip.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::conflicts_view::{self, arrow, chip, diff_bar, note, Analysis, DetailHeader, MergeHooks};
use crate::diff_view::{Compare, Header, Pair};
use crate::folder_merge_dialog::{dollars, tokens};
use crate::merge::ai::{self, Checked, Estimate, Progress, Provider, Versions};
use crate::merge::base_game::{self, Behind, Saved};
use crate::merge::patch::{Print, VANILLA};
use crate::merge::snapshot;
use crate::merge::source::{self, Source};
use crate::merge::update_all;
use crate::merge::vocabulary;
use crate::merge_dialog;
use crate::thinking_view::{self, ThinkingView};

pub fn present(parent: &impl IsA<gtk::Widget>, analysis: Rc<Analysis>, hooks: Rc<MergeHooks>) {
    let Some((old, new)) = analysis.behind_versions.clone() else { return };
    if analysis.behind.is_empty() {
        return;
    }
    crate::diff_view::install_css();

    let navigation = adw::NavigationView::new();
    let dialog = adw::Dialog::builder().content_width(1150).content_height(750).child(&navigation).build();

    let page = Rc::new(BehindPage::new(analysis, hooks, old, new, navigation.clone(), dialog.clone()));
    let split_page = wrap(&t!("Changed by the Game Update"), Some(&tf!("Stellaris {old} → {new}", old = &page.old, new = &page.new)), &page.split);
    navigation.add(&split_page);

    page.setup(dialog.upcast_ref());
    dialog.present(Some(parent));
}

/// A page with its own header bar.
fn wrap(title: &str, subtitle: Option<&str>, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, subtitle.unwrap_or_default())));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder().title(title).child(&toolbar).build()
}


/// Ties `value`'s lifetime to `widget`'s, so a pair keeps refreshing its
/// comparison while its widget is on screen.
fn keep_alive<T: 'static>(widget: &impl IsA<gtk::Widget>, value: Rc<T>) {
    let cell = std::cell::RefCell::new(Some(value));
    widget.connect_destroy(move |_| drop(cell.take()));
}

/// The mod's copy of the object as it is on disk now.
fn read_mod_copy(analysis: &Analysis, b: &Behind) -> Option<String> {
    read_copy(analysis.roots.get(b.mod_position)?.as_deref(), b)
}

/// As [`read_mod_copy`], from the mod's folder. Reads a file.
fn read_copy(root: Option<&Path>, b: &Behind) -> Option<String> {
    let bytes = std::fs::read(root?.join(&b.file)).ok()?;
    source::extract(&b.category, &b.key, String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}'))
}

/// Everything one update needs, so it can run off the UI thread.
#[derive(Clone)]
struct Job {
    category: String,
    key: String,
    before: Source,
    after: Source,
    mod_copy: Source,
    game_dir: Option<PathBuf>,
    /// The game's current version.
    version: String,
}

impl Job {
    fn new(analysis: &Analysis, hooks: &MergeHooks, b: &Behind, now: &Saved, old: &str, new: &str, mod_text: &str) -> Job {
        let source = |label: String, file: &str, text: &str| Source { label, file: file.to_owned(), text: text.to_owned() };
        Job {
            category: b.category.clone(),
            key: b.key.clone(),
            before: source(tf!("Stellaris {version}", version = old), &b.was.file, &b.was.text),
            after: source(tf!("Stellaris {version}", version = new), &now.file, &now.text),
            mod_copy: source(analysis.name(b.mod_position).to_owned(), &b.file, mod_text),
            game_dir: hooks.game_dir.clone(),
            version: new.to_owned(),
        }
    }

    fn versions(&self) -> Versions<'_> {
        Versions { before: &self.before, after: &self.after, mod_copy: &self.mod_copy }
    }

    /// Asks the model, and looks for keywords found in neither game version,
    /// the game's other objects, nor the mod's copy. Blocks on the network
    /// and reads files, so it runs off the UI thread.
    fn run(&self, provider: &Provider, on: &mut dyn FnMut(Progress)) -> Result<(Checked, Vec<String>), String> {
        let check = |text: &str| merge_dialog::validate(&self.category, &self.key, text);
        let checked = ai::update_checked(provider, &self.category, &self.key, &self.versions(), &check, on)?;
        let text = match &checked.script {
            Ok(script) => script.clone(),
            Err(_) => checked.merged.script.clone(),
        };
        let snapshot = self.game_dir.as_deref().and_then(|dir| base_game::ensure(&base_game::dir(), dir, &self.version).ok());
        let known = snapshot.iter().flat_map(|s| s.texts()).chain([self.before.text.as_str(), self.after.text.as_str(), self.mod_copy.text.as_str()]);
        let warnings = vocabulary::unknown(&text, known);
        Ok((checked, warnings))
    }
}

/// Saves `text` as the updated copy of `b` to the merged patch.
fn save_update(analysis: &Analysis, hooks: &MergeHooks, b: &Behind, now: &Saved, mod_text: &str, text: &str) -> Result<(), String> {
    let script = merge_dialog::validate(&b.category, &b.key, text)?;
    // Fingerprints of both sources, so the patch flags this copy as
    // outdated if the game or the mod changes again.
    let position = b.mod_position;
    let prints = vec![Print::new(VANILLA, "Vanilla", &now.file, &now.text), Print::new(&analysis.ids[position], analysis.name(position), &b.file, mod_text)];
    (hooks.save_object)(&b.category, &b.key, &script, prints)
}

struct ItemState {
    index: usize,
    b: Behind,
    mod_text: Option<String>,
    /// The text currently in the right card editor (kept across switches).
    current_text: RefCell<String>,
    /// The initial text before any user edits (for revert button).
    initial_text: RefCell<String>,
    /// Model's notes from AI merge if run.
    notes: RefCell<String>,
    /// Unknown vocabulary warnings from AI merge.
    warnings: RefCell<Vec<String>>,
    /// Error message from AI merge or save.
    error: RefCell<Option<String>>,
    /// Whether "Game Changes" is currently toggled for this item.
    show_game_changes: Cell<bool>,
    /// Whether unchanged lines should be folded.
    fold: Cell<bool>,
    /// Whether whitespace is ignored.
    ignore_whitespace: Cell<bool>,
}

struct RowWidgets {
    item: Rc<ItemState>,
    row: gtk::ListBoxRow,
    status_label: gtk::Label,
}

struct BehindPage {
    analysis: Rc<Analysis>,
    hooks: Rc<MergeHooks>,
    old: String,
    new: String,
    navigation: adw::NavigationView,
    split: adw::NavigationSplitView,
    list: gtk::ListBox,
    content_bin: adw::Bin,
    all_button: gtk::Button,
    search: gtk::SearchEntry,
    items: Vec<Rc<ItemState>>,
    rows: RefCell<Vec<RowWidgets>>,
    current_index: Cell<Option<usize>>,
    active_pair: RefCell<Option<Rc<Pair>>>,
    batch: RefCell<Option<Rc<Batch>>>,
}

impl BehindPage {
    fn new(analysis: Rc<Analysis>, hooks: Rc<MergeHooks>, old: String, new: String, navigation: adw::NavigationView, dialog: adw::Dialog) -> Self {
        let items: Vec<Rc<ItemState>> = analysis
            .behind
            .iter()
            .enumerate()
            .map(|(index, b)| {
                let mod_text = read_mod_copy(&analysis, b);
                let existing = (hooks.existing)(&b.category, &b.key).map(|m| m.script);
                let initial = existing.unwrap_or_else(|| b.now.as_ref().map(|n| n.text.clone()).unwrap_or_else(|| b.was.text.clone()));
                Rc::new(ItemState {
                    index,
                    b: b.clone(),
                    mod_text,
                    current_text: RefCell::new(initial.clone()),
                    initial_text: RefCell::new(initial),
                    notes: RefCell::default(),
                    warnings: RefCell::default(),
                    error: RefCell::default(),
                    show_game_changes: Cell::new(false),
                    fold: Cell::new(false),
                    ignore_whitespace: Cell::new(false),
                })
            })
            .collect();

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).css_classes(["navigation-sidebar", "conflict-list"]).build();
        let list_scroll = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();

        let all_button = gtk::Button::builder().label(t!("Update All with AI")).css_classes(["suggested-action"]).halign(gtk::Align::End).build();
        let search = gtk::SearchEntry::builder().placeholder_text(t!("Search objects")).build();
        let desc = gtk::Label::builder()
            .label(t!("These mods still load their own copies, so the game's changes to these objects don't apply until the mods are updated."))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build();

        let top = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(12).margin_start(12).margin_end(12).margin_bottom(6).build();
        top.append(&desc);
        top.append(&all_button);
        top.append(&search);

        let sidebar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["conflict-sidebar"])
            .overflow(gtk::Overflow::Hidden)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .build();
        sidebar.append(&top);
        sidebar.append(&list_scroll);

        let content_bin = adw::Bin::builder().hexpand(true).vexpand(true).build();
        let content_scroll = gtk::ScrolledWindow::builder().child(&content_bin).hscrollbar_policy(gtk::PolicyType::Automatic).vscrollbar_policy(gtk::PolicyType::Automatic).build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(&adw::NavigationPage::builder().title(t!("Game Update")).child(&sidebar).build())
            .content(&adw::NavigationPage::builder().title(t!("Object")).child(&content_scroll).build())
            .min_sidebar_width(280.0)
            .max_sidebar_width(400.0)
            .sidebar_width_fraction(0.26)
            .css_classes(["conflict-split"])
            .build();

        let _ = dialog;
        BehindPage {
            analysis,
            hooks,
            old,
            new,
            navigation,
            split,
            list,
            content_bin,
            all_button,
            search,
            items,
            rows: RefCell::default(),
            current_index: Cell::new(None),
            active_pair: RefCell::default(),
            batch: RefCell::default(),
        }
    }

    fn setup(self: &Rc<Self>, dialog: &adw::Dialog) {
        let mut by_mod: BTreeMap<usize, Vec<Rc<ItemState>>> = BTreeMap::new();
        for item in &self.items {
            by_mod.entry(item.b.mod_position).or_default().push(item.clone());
        }

        let mut row_widgets = Vec::new();
        for (position, items) in by_mod {
            let heading_box = gtk::Box::builder().spacing(6).margin_start(6).margin_top(8).margin_bottom(4).build();
            let mod_name = self.analysis.name(position);
            heading_box.append(&gtk::Label::builder().label(mod_name).xalign(0.0).hexpand(true).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).css_classes(["dim-label"]).build());
            heading_box.append(&gtk::Label::builder().label(items.len().to_string()).css_classes(["dim-label", "numeric"]).build());
            let heading_row = gtk::ListBoxRow::builder().child(&heading_box).selectable(false).activatable(false).css_classes(["folder"]).build();
            self.list.append(&heading_row);

            for item in items {
                let row_box = gtk::Box::builder().spacing(8).build();
                let text_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).build();
                let title_label = gtk::Label::builder().label(&item.b.key).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
                let subtitle_label = gtk::Label::builder().label(&item.b.category).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["dim-label", "caption"]).build();
                text_box.append(&title_label);
                text_box.append(&subtitle_label);

                let status_label = gtk::Label::builder().xalign(1.0).build();
                if item.b.now.is_none() {
                    status_label.set_label(&t!("Removed from the game"));
                    status_label.set_css_classes(&["dim-label", "caption"]);
                } else if (self.hooks.is_merged)(&item.b.category, &item.b.key) {
                    status_label.set_label(&t!("Updated in the merged patch"));
                    status_label.set_css_classes(&["success", "caption"]);
                }

                row_box.append(&text_box);
                row_box.append(&status_label);
                let row = gtk::ListBoxRow::builder().child(&row_box).css_classes(["conflict"]).build();
                self.list.append(&row);
                row_widgets.push(RowWidgets { item, row, status_label });
            }
        }
        self.rows.replace(row_widgets);

        // Search filtering.
        let rows_ref = Rc::downgrade(self);
        self.list.set_filter_func(move |row| {
            let Some(page) = rows_ref.upgrade() else { return true };
            if row.has_css_class("folder") {
                return true;
            }
            let query = page.get_search_query();
            if query.is_empty() {
                return true;
            }
            let rows = page.rows.borrow();
            let Some(rw) = rows.iter().find(|rw| &rw.row == row) else { return true };
            let mod_name = page.analysis.name(rw.item.b.mod_position).to_lowercase();
            let key = rw.item.b.key.to_lowercase();
            let category = rw.item.b.category.to_lowercase();
            key.contains(&query) || category.contains(&query) || mod_name.contains(&query)
        });

        let weak = Rc::downgrade(self);
        self.list.connect_row_selected(move |_, row| {
            let Some(page) = weak.upgrade() else { return };
            let Some(row) = row else { return };
            if row.has_css_class("folder") {
                return;
            }
            let rows = page.rows.borrow();
            if let Some(rw) = rows.iter().find(|rw| &rw.row == row) {
                page.select_item(rw.item.index);
            }
        });

        let weak_for_search = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(page) = weak_for_search.upgrade() {
                page.list.invalidate_filter();
            }
        });

        let weak_refresh = Rc::downgrade(self);
        let refresh: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(page) = weak_refresh.upgrade() {
                page.refresh_status();
            }
        });

        let batch = Rc::new(Batch {
            analysis: self.analysis.clone(),
            hooks: self.hooks.clone(),
            navigation: self.navigation.clone(),
            dialog: dialog.clone(),
            old: self.old.clone(),
            new: self.new.clone(),
            refresh: refresh.clone(),
            cancelled: Rc::default(),
        });
        dialog.connect_closed({
            let batch = batch.clone();
            move |_| batch.cancelled.set(true)
        });
        self.all_button.connect_clicked({
            let batch = batch.clone();
            move |_| batch.ask()
        });
        self.batch.replace(Some(batch));

        self.refresh_status();

        // Select the first item initially.
        if !self.items.is_empty()
            && let Some(first_row) = self.rows.borrow().first()
        {
            self.list.select_row(Some(&first_row.row));
        }
    }

    fn get_search_query(&self) -> String {
        self.search.text().trim().to_lowercase()
    }

    fn refresh_status(&self) {
        let rows = self.rows.borrow();
        for rw in rows.iter() {
            if rw.item.b.now.is_none() {
                rw.status_label.set_label(&t!("Removed from the game"));
                rw.status_label.set_css_classes(&["dim-label", "caption"]);
                rw.status_label.set_visible(true);
            } else if (self.hooks.is_merged)(&rw.item.b.category, &rw.item.b.key) {
                rw.status_label.set_label(&t!("Updated in the merged patch"));
                rw.status_label.set_css_classes(&["success", "caption"]);
                rw.status_label.set_visible(true);
            } else {
                rw.status_label.set_visible(false);
            }
        }
        let unmerged = update_all::included(&self.analysis.behind, |c, k| (self.hooks.is_merged)(c, k)).len();
        self.all_button.set_visible(unmerged >= 2);
    }

    fn select_item(self: &Rc<Self>, index: usize) {
        if self.current_index.get() == Some(index) {
            return;
        }
        self.save_active_editor();
        self.current_index.set(Some(index));
        let item = self.items[index].clone();
        let detail_widget = self.render_detail(item);
        self.content_bin.set_child(Some(&detail_widget));
        if self.split.is_collapsed() {
            self.split.set_show_content(true);
        }
    }

    fn save_active_editor(&self) {
        if let (Some(idx), Some(pair)) = (self.current_index.get(), self.active_pair.borrow().as_ref())
            && idx < self.items.len()
            && !self.items[idx].show_game_changes.get()
        {
            *self.items[idx].current_text.borrow_mut() = pair.right.text();
        }
    }

    fn render_detail(self: &Rc<Self>, item: Rc<ItemState>) -> gtk::Widget {
        let b = &item.b;
        let mod_name = self.analysis.name(b.mod_position);
        let subtitle = tf!("Object · {category} · {mod_name} replaces this, and the base game update", category = &b.category, mod_name = mod_name);
        let header = self.detail_header(&b.key, &subtitle);

        let chips = &header.chips;
        chips.insert(&chip(mod_name, Some(b.mod_position), "current"), -1);
        chips.insert(&arrow(), -1);
        chips.insert(&chip(&tf!("Stellaris {version}", version = &self.new), None, "done"), -1);
        if (self.hooks.is_merged)(&b.category, &b.key) {
            chips.insert(&arrow(), -1);
            chips.insert(&chip(&t!("Updated in merged patch"), None, "done"), -1);
        }

        let notes_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        let thinking = ThinkingView::new();

        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
        root.append(&header.widget);
        root.append(&notes_box);
        root.append(&thinking.widget);

        if b.now.is_none() {
            notes_box.append(&note(&t!("This object was removed from Stellaris in this update."), Some("warning")));
            let pair = Rc::new(Pair::new(false));
            pair.show(&b.was.text, "", &Compare::Each, false);
            pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = &self.old), subtitle: b.was.file.clone(), color: None, badge: None });
            pair.right.set_header(&Header { title: tf!("Stellaris {version}", version = &self.new), subtitle: t!("Removed from the game"), color: None, badge: None });
            pair.widget.set_vexpand(true);
            root.append(&pair.widget);
            keep_alive(&root, pair);
            return root.upcast();
        }

        let Some(ref mod_text) = item.mod_text else {
            notes_box.append(&note(&t!("The mod's copy couldn't be read, so it can't be updated here."), Some("warning")));
            let pair = Rc::new(Pair::new(false));
            pair.show(&b.was.text, b.now.as_ref().map_or("", |n| n.text.as_str()), &Compare::Each, false);
            pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = &self.old), subtitle: b.was.file.clone(), color: None, badge: None });
            pair.right.set_header(&Header { title: tf!("Stellaris {version}", version = &self.new), subtitle: b.now.as_ref().map_or_else(String::new, |n| n.file.clone()), color: None, badge: None });
            pair.widget.set_vexpand(true);
            root.append(&pair.widget);
            keep_alive(&root, pair);
            return root.upcast();
        };

        let now = b.now.as_ref().unwrap();

        // Notes.
        notes_box.append(&note(&tf!("{mod_name} still uses its own copy, from {file}.", mod_name = mod_name, file = &b.file), None));
        if (self.hooks.is_merged)(&b.category, &b.key) {
            notes_box.append(&note(&t!("Updated in the merged patch. Saving again replaces it."), Some("success")));
        }
        let notes_text = item.notes.borrow().clone();
        if !notes_text.is_empty() {
            notes_box.append(&note(&tf!("Notes from the model: {notes}", notes = &notes_text), None));
        }
        let warnings = item.warnings.borrow().clone();
        if !warnings.is_empty() {
            notes_box.append(&note(&tf!("Not found in Stellaris {version} or the mod's copy: {keys}. Check these before saving.", version = &self.new, keys = warnings.join(", ")), Some("warning")));
        }
        let error_msg = item.error.borrow().clone();
        if let Some(err) = error_msg {
            notes_box.append(&note(&err, Some("error")));
        }

        let pair = Pair::new(true);
        self.active_pair.replace(Some(pair.clone()));

        // Diff bar.
        let bar = diff_bar(&pair);

        let take_btn = gtk::Button::builder().label(t!("Take Change")).tooltip_text(t!("Copy the highlighted difference from the mod's version into the editable result")).sensitive(false).css_classes(["flat"]).build();
        {
            let take_weak = take_btn.downgrade();
            let item_weak = Rc::downgrade(&item);
            pair.connect_change(move |_, current, _| {
                if let (Some(btn), Some(item)) = (take_weak.upgrade(), item_weak.upgrade()) {
                    btn.set_sensitive(current.is_some() && !item.show_game_changes.get());
                }
            });
            let pair_weak = Rc::downgrade(&pair);
            take_btn.connect_clicked(move |_| {
                if let Some(pair) = pair_weak.upgrade() {
                    pair.put_in_current();
                }
            });
        }
        bar.extra.append(&take_btn);

        let merge_tool_bar_btn = gtk::Button::builder().label(t!("Merge Tool…")).tooltip_text(t!("Open in your external merge tool to resolve differences manually")).css_classes(["flat"]).build();
        {
            let (page, item_ref, pair_ref) = (self.clone(), item.clone(), pair.clone());
            merge_tool_bar_btn.connect_clicked(move |btn| {
                page.open_merge_tool(btn, &item_ref, &pair_ref);
            });
        }
        bar.extra.append(&merge_tool_bar_btn);

        let game_changes_toggle = gtk::ToggleButton::builder()
            .label(t!("Game Changes"))
            .tooltip_text(tf!("Toggle between comparing the mod's copy with the update and comparing what changed in the base game between Stellaris {old} and {new}", old = &self.old, new = &self.new))
            .active(item.show_game_changes.get())
            .css_classes(["flat"])
            .build();
        {
            let (page, item_ref, pair_ref) = (self.clone(), item.clone(), pair.clone());
            let take_weak = take_btn.downgrade();
            game_changes_toggle.connect_toggled(move |btn| {
                let active = btn.is_active();
                item_ref.show_game_changes.set(active);
                if let Some(t) = take_weak.upgrade() {
                    t.set_sensitive(!active && pair_ref.current().is_some());
                }
                page.refresh_pair_display(&item_ref, &pair_ref);
            });
        }
        bar.extra.append(&game_changes_toggle);

        let fold_toggle = gtk::ToggleButton::builder().label(t!("Changed Lines Only")).tooltip_text(t!("Fold runs of unchanged lines")).active(item.fold.get()).css_classes(["flat"]).build();
        {
            let (page, item_ref, pair_ref) = (self.clone(), item.clone(), pair.clone());
            fold_toggle.connect_toggled(move |btn| {
                let active = btn.is_active();
                item_ref.fold.set(active);
                pair_ref.set_fold(active);
                page.refresh_pair_display(&item_ref, &pair_ref);
            });
        }
        bar.extra.append(&fold_toggle);

        let whitespace_toggle = gtk::ToggleButton::builder().label(t!("Ignore Spacing")).tooltip_text(t!("Ignore differences in whitespace")).active(item.ignore_whitespace.get()).css_classes(["flat"]).build();
        {
            let (page, item_ref, pair_ref) = (self.clone(), item.clone(), pair.clone());
            whitespace_toggle.connect_toggled(move |btn| {
                let active = btn.is_active();
                item_ref.ignore_whitespace.set(active);
                page.refresh_pair_display(&item_ref, &pair_ref);
            });
        }
        bar.extra.append(&whitespace_toggle);

        // Header actions.
        let report_btn = gtk::Button::builder().label(t!("Report for Mod Author")).icon_name("edit-copy-symbolic").tooltip_text(t!("Copy a formatted report for the mod author to clipboard")).css_classes(["flat"]).build();
        {
            let (mod_name_str, category_str, key_str, mod_file_str, base_file_str, old_ver, new_ver, mod_t, now_t, hooks) = (
                mod_name.to_owned(),
                b.category.clone(),
                b.key.clone(),
                b.file.clone(),
                now.file.clone(),
                self.old.clone(),
                self.new.clone(),
                mod_text.clone(),
                now.text.clone(),
                self.hooks.clone(),
            );
            report_btn.connect_clicked(move |btn| {
                let text = format!(
                    "Hi! It looks like {mod_name_str} has an out-of-date override for Stellaris {new_ver}:\n\n\
                     • Object: `{key_str}`\n\
                     • Category: `{category_str}`\n\
                     • Mod file: `{mod_file_str}`\n\
                     • Base game file: `{base_file_str}`\n\n\
                     The mod's copy appears to be based on Stellaris {old_ver} and is missing changes introduced in Stellaris {new_ver}.\n\n\
                     ```stellaris\n\
                     # Current copy in {mod_name_str}\n\
                     {mod_t}\n\
                     ```\n\n\
                     ```stellaris\n\
                     # Base game version in Stellaris {new_ver}\n\
                     {now_t}\n\
                     ```\n"
                );
                btn.clipboard().set_text(&text);
                (hooks.toast)(&t!("Copied report for mod author to clipboard"));
            });
        }
        header.actions.append(&report_btn);

        let merge_tool_hdr_btn = gtk::Button::builder().label(t!("Merge Tool…")).tooltip_text(t!("Open in your external merge tool to resolve differences manually")).build();
        {
            let (page, item_ref, pair_ref) = (self.clone(), item.clone(), pair.clone());
            merge_tool_hdr_btn.connect_clicked(move |btn| {
                page.open_merge_tool(btn, &item_ref, &pair_ref);
            });
        }
        header.actions.append(&merge_tool_hdr_btn);

        let ai_btn = gtk::Button::builder().child(&adw::ButtonContent::builder().icon_name(conflicts_view::AI_ICON).label(t!("Update with AI")).build()).css_classes(["suggested-action"]).build();
        {
            let (page, item_ref, pair_ref, thinking_ref) = (self.clone(), item.clone(), pair.clone(), thinking.clone());
            ai_btn.connect_clicked(move |btn| {
                page.update_with_ai(btn, &item_ref, &pair_ref, &thinking_ref);
            });
        }
        header.actions.append(&ai_btn);

        let save_btn = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).tooltip_text(t!("Save the updated copy to the merged patch")).build();
        {
            let (page, item_ref, pair_ref) = (self.clone(), item.clone(), pair.clone());
            save_btn.connect_clicked(move |_| {
                page.save_item(&item_ref, &pair_ref);
            });
        }
        header.actions.append(&save_btn);

        // Edit hookup for pair.
        {
            let item_weak = Rc::downgrade(&item);
            pair.connect_edit(move |text| {
                if let Some(it) = item_weak.upgrade()
                    && !it.show_game_changes.get()
                {
                    *it.current_text.borrow_mut() = text.to_owned();
                }
            });
        }

        pair.set_fold(item.fold.get());
        self.refresh_pair_display(&item, &pair);

        pair.widget.set_vexpand(true);
        root.append(&bar.widget);
        root.append(&pair.widget);
        keep_alive(&root, pair);

        root.upcast()
    }

    fn detail_header(&self, title: &str, subtitle: &str) -> DetailHeader {
        let back = gtk::Button::builder().icon_name("go-previous-symbolic").tooltip_text(t!("Back to the list")).valign(gtk::Align::Center).css_classes(["flat"]).build();
        self.split.bind_property("collapsed", &back, "visible").sync_create().build();
        let split_weak = self.split.downgrade();
        back.connect_clicked(move |_| {
            if let Some(split) = split_weak.upgrade() {
                split.set_show_content(false);
            }
        });
        let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
        titles.append(&gtk::Label::builder().label(title).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).css_classes(["conflict-title"]).build());
        titles.append(&gtk::Label::builder().label(subtitle).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).tooltip_text(subtitle).css_classes(["dim-label"]).build());
        let actions = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
        let top = gtk::Box::builder().spacing(12).build();
        top.append(&back);
        top.append(&titles);
        top.append(&actions);
        let chips = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(4).row_spacing(4).max_children_per_line(64).homogeneous(false).build();
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        widget.append(&top);
        widget.append(&chips);
        DetailHeader { widget, chips, actions }
    }

    fn refresh_pair_display(&self, item: &Rc<ItemState>, pair: &Rc<Pair>) {
        let mod_name = self.analysis.name(item.b.mod_position);
        let mod_text = item.mod_text.as_deref().unwrap_or_default();
        let now = item.b.now.as_ref().unwrap();

        if item.show_game_changes.get() {
            pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = &self.old), subtitle: item.b.was.file.clone(), color: None, badge: Some((t!("Base Game"), None)) });
            pair.left.set_actions(&[]);
            pair.right.set_header(&Header { title: tf!("Stellaris {version}", version = &self.new), subtitle: now.file.clone(), color: None, badge: Some((t!("Base Game"), None)) });
            pair.right.set_actions(&[]);
            pair.right.code.set_editable(false);
            pair.show(&item.b.was.text, &now.text, &Compare::Each, item.ignore_whitespace.get());
        } else {
            let use_mod = gtk::Button::builder().label(t!("Use Mod's Version")).tooltip_text(t!("Reset editor to the mod's version")).css_classes(["flat"]).build();
            {
                let (weak_pair, mod_t) = (Rc::downgrade(pair), mod_text.to_owned());
                use_mod.connect_clicked(move |_| {
                    if let Some(p) = weak_pair.upgrade() {
                        p.right.replace_text(&mod_t);
                    }
                });
            }
            pair.left.set_header(&Header { title: mod_name.to_owned(), subtitle: item.b.file.clone(), color: Some(item.b.mod_position), badge: Some((t!("Mod"), None)) });
            pair.left.set_actions(&[use_mod.upcast()]);

            let is_merged = (self.hooks.is_merged)(&item.b.category, &item.b.key);
            let right_title = if is_merged { t!("Updated in merged patch") } else { t!("Updated Copy") };
            let right_badge = (t!("Editable"), Some("accent"));
            pair.right.set_header(&Header { title: right_title, subtitle: now.file.clone(), color: None, badge: Some(right_badge) });

            let revert = gtk::Button::builder().icon_name("document-revert-symbolic").tooltip_text(t!("Revert edits to initial version")).css_classes(["flat"]).build();
            {
                let (weak_pair, item_ref) = (Rc::downgrade(pair), item.clone());
                revert.connect_clicked(move |_| {
                    if let Some(p) = weak_pair.upgrade() {
                        let init = item_ref.initial_text.borrow().clone();
                        p.right.replace_text(&init);
                    }
                });
            }
            let use_game = gtk::Button::builder().label(t!("Use Game's Version")).tooltip_text(t!("Reset editor to the new base game version")).css_classes(["flat"]).build();
            {
                let (weak_pair, now_t) = (Rc::downgrade(pair), now.text.clone());
                use_game.connect_clicked(move |_| {
                    if let Some(p) = weak_pair.upgrade() {
                        p.right.replace_text(&now_t);
                    }
                });
            }
            pair.right.set_actions(&[revert.upcast(), use_game.upcast()]);
            pair.right.code.set_editable(true);
            pair.show(mod_text, &item.current_text.borrow(), &Compare::Each, item.ignore_whitespace.get());
        }
    }

    fn open_merge_tool(&self, button: &gtk::Button, item: &Rc<ItemState>, pair: &Rc<Pair>) {
        let command = (self.hooks.merge_tool)();
        if command.is_empty() {
            (self.hooks.toast)(&t!("Choose a merge tool in Preferences first"));
            return (self.hooks.open_merge_tool_settings)();
        }
        let tool = conflicts_view::merge_tool_name(&command);
        let left_label = self.analysis.name(item.b.mod_position).to_owned();
        let right_label = tf!("Stellaris {version}", version = &self.new);
        let left = conflicts_view::ToolSide { label: left_label, text: item.mod_text.clone().unwrap_or_default() };
        let right = conflicts_view::ToolSide { label: right_label, text: pair.right.text() };
        let base = Some(item.b.was.text.clone());
        let (hooks, pair_weak) = (self.hooks.clone(), Rc::downgrade(pair));
        conflicts_view::run_merge_tool(button, command, left, right, base, move |result| {
            match result {
                Ok(Some(merged)) => {
                    if let Some(p) = pair_weak.upgrade() {
                        p.right.replace_text(&merged);
                        (hooks.toast)(&t!("The merge tool's result is in the editor; review and save when ready"));
                    }
                }
                Ok(None) => (hooks.toast)(&tf!("{tool} was closed without saving, so nothing changed", tool = tool)),
                Err(message) => (hooks.toast)(&message),
            }
        });
    }

    fn update_with_ai(self: &Rc<Self>, button: &gtk::Button, item: &Rc<ItemState>, pair: &Rc<Pair>, thinking: &Rc<ThinkingView>) {
        let Some(provider) = (self.hooks.provider)() else {
            (self.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
            (self.hooks.open_preferences)();
            return;
        };
        let Some(ref now) = item.b.now else { return };
        let Some(ref mod_text) = item.mod_text else { return };
        let job = Job::new(&self.analysis, &self.hooks, &item.b, now, &self.old, &self.new, mod_text);
        button.set_sensitive(false);
        item.error.replace(None);

        let (page, item_ref, pair_ref, thinking_ref, button_ref) = (self.clone(), item.clone(), pair.clone(), thinking.clone(), button.clone());
        glib::spawn_future_local(async move {
            let result = thinking_view::run(&thinking_ref, None, move |on| job.run(&provider, on)).await;
            button_ref.set_sensitive(true);
            match result {
                Some(Ok((checked, warnings))) => {
                    let script = match checked.script {
                        Ok(s) => s,
                        Err(_) => checked.merged.script,
                    };
                    *item_ref.notes.borrow_mut() = checked.merged.notes;
                    *item_ref.warnings.borrow_mut() = warnings;
                    pair_ref.right.replace_text(&script);
                    page.re_render_current();
                }
                Some(Err(message)) => {
                    item_ref.error.replace(Some(message));
                    page.re_render_current();
                }
                None => {
                    item_ref.error.replace(Some(t!("The merge failed unexpectedly")));
                    page.re_render_current();
                }
            }
        });
    }

    fn save_item(self: &Rc<Self>, item: &Rc<ItemState>, pair: &Rc<Pair>) {
        let Some(ref now) = item.b.now else { return };
        let Some(ref mod_text) = item.mod_text else { return };
        let text = pair.right.text();
        match save_update(&self.analysis, &self.hooks, &item.b, now, mod_text, &text) {
            Ok(()) => {
                *item.initial_text.borrow_mut() = text.clone();
                *item.current_text.borrow_mut() = text;
                self.hooks.saved();
                self.refresh_status();
                (self.hooks.toast)(&tf!("Saved the updated {key} to the merged patch", key = &item.b.key));
                self.re_render_current();
            }
            Err(message) => {
                item.error.replace(Some(message));
                self.re_render_current();
            }
        }
    }

    fn re_render_current(self: &Rc<Self>) {
        if let Some(idx) = self.current_index.get() {
            let item = self.items[idx].clone();
            let detail_widget = self.render_detail(item);
            self.content_bin.set_child(Some(&detail_widget));
        }
    }
}

/// The page of the list and what Update All needs from it.
struct Batch {
    analysis: Rc<Analysis>,
    hooks: Rc<MergeHooks>,
    navigation: adw::NavigationView,
    dialog: adw::Dialog,
    old: String,
    new: String,
    /// Refreshes the list's labels and the Update All button.
    refresh: Rc<dyn Fn()>,
    /// Set by Stop and by closing the dialog.
    cancelled: Rc<Cell<bool>>,
}

/// One object queued for Update All.
struct Queued {
    /// Index into the analysis' `behind`.
    index: usize,
    now: Saved,
    mod_text: String,
    job: Job,
}

/// A finished request: the result and the keywords it uses that nothing has.
type Done = (Checked, Vec<String>);

/// Finished requests, by position in the queue.
type Finished = Vec<(usize, Result<Done, String>)>;

impl Batch {
    /// Reads the mods' copies, then asks the player to confirm with the
    /// number of requests and what they may cost.
    fn ask(self: &Rc<Self>) {
        let Some(provider) = (self.hooks.provider)() else {
            (self.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
            (self.hooks.open_preferences)();
            return;
        };
        let chosen = update_all::included(&self.analysis.behind, |c, k| (self.hooks.is_merged)(c, k));
        if chosen.is_empty() {
            return;
        }
        let wanted: Vec<(usize, Option<PathBuf>, Behind)> = chosen.iter().map(|&i| (i, self.analysis.roots.get(self.analysis.behind[i].mod_position).cloned().flatten(), self.analysis.behind[i].clone())).collect();
        let batch = self.clone();
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking(move || wanted.into_iter().map(|(i, root, b)| (i, read_copy(root.as_deref(), &b))).collect::<Vec<_>>()).await.unwrap_or_default();
            let mut items = Vec::new();
            let mut unreadable = Vec::new();
            for (index, text) in read {
                let b = &batch.analysis.behind[index];
                match (text, &b.now) {
                    (Some(mod_text), Some(now)) => {
                        let job = Job::new(&batch.analysis, &batch.hooks, b, now, &batch.old, &batch.new, &mod_text);
                        items.push(Queued { index, now: now.clone(), mod_text, job });
                    }
                    _ => unreadable.push((index, t!("The mod's copy couldn't be read, so it can't be updated here."))),
                }
            }
            if items.is_empty() {
                // Nothing to send; show why.
                let run = Rc::new(Run::new(batch.clone(), items, unreadable));
                return run.finish(0);
            }
            batch.confirm(provider, items, unreadable);
        });
    }

    /// Shows how many requests will be sent and the estimated cost.
    fn confirm(self: &Rc<Self>, provider: Provider, items: Vec<Queued>, unreadable: Vec<(usize, String)>) {
        let total = items.iter().fold(Estimate::default(), |mut sum, q| {
            sum += ai::estimate_update(&q.job.category, &q.job.key, &q.job.versions());
            sum
        });
        let parallel = self.hooks.merges_at_once.clamp(1, items.len());
        let count = items.len();
        let alert = adw::AlertDialog::new(Some(&tnf!("Update {count} Object with AI?", "Update {count} Objects with AI?", count = count)), None);
        let intro = tf!("Each object is sent to {model} on its own, up to {parallel} at a time. Nothing is saved until you have looked at each result.", model = provider.model_name(), parallel = parallel);
        let calls = tnf!("{count} AI call", "{count} AI calls", count = count);
        let summary = tf!("{calls}{without_ai} · about {input} tokens in, {low}–{high} out", calls = &calls, without_ai = "", input = tokens(total.input), low = tokens(total.output_low), high = tokens(total.output_high));
        let body = {
            let (intro, summary) = (intro.clone(), summary.clone());
            move |price_line: &str| format!("{intro}\n\n{summary}\n{price_line}")
        };
        alert.set_body(&body(&t!("Looking up the model's price…")));
        alert.add_responses(&[("cancel", &t!("Cancel")), ("update", &t!("Update All"))]);
        alert.set_response_appearance("update", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let items = Rc::new(RefCell::new(Some(items)));
        let batch = self.clone();
        let (run_provider, unreadable) = (provider.clone(), RefCell::new(Some(unreadable)));
        alert.connect_response(None, move |_, response| {
            if response != "update" {
                return;
            }
            if let (Some(items), Some(unreadable)) = (items.borrow_mut().take(), unreadable.borrow_mut().take()) {
                batch.cancelled.set(false);
                Rc::new(Run::new(batch.clone(), items, unreadable)).start(run_provider.clone());
            }
        });
        alert.present(Some(&self.dialog));

        let alert = alert.downgrade();
        glib::spawn_future_local(async move {
            let found = gio::spawn_blocking(move || ai::price(&provider)).await.ok().flatten();
            let line = match found {
                Some(p) => tf!(
                    "Estimated cost: {low} to {high}, depending on how much the model thinks. Prices: ${input:.3} per million tokens in, ${output:.3} out.",
                    low = dollars(p.cost(total.input, total.output_low)),
                    high = dollars(p.cost(total.input, total.output_high)),
                    input = p.input,
                    output = p.output
                ),
                None => t!("The provider doesn't publish this model's price, so the cost can't be estimated."),
            };
            if let Some(alert) = alert.upgrade() {
                alert.set_body(&body(&line));
            }
        });
    }
}

/// The result next to the mod's copy, to edit, with the notes, the keyword
/// warning and the model's remaining problem, if any. The caller adds the
/// buttons.
struct Review {
    body: gtk::Box,
    pair: Rc<Pair>,
    error: gtk::Label,
    buttons: gtk::Box,
}

fn review_parts(analysis: &Analysis, b: &Behind, mod_text: &str, version: &str, checked: Checked, warnings: &[String]) -> Review {
    let (script, problem) = match checked.script {
        Ok(script) => (script, None),
        // Kept even when still rejected, to fix by hand.
        Err(problem) => (checked.merged.script, Some(problem)),
    };
    let pair = Pair::new(true);
    pair.show(mod_text, &script, &Compare::Each, false);
    pair.left.set_header(&Header { title: analysis.name(b.mod_position).to_owned(), subtitle: b.file.clone(), color: None, badge: None });
    pair.right.set_header(&Header { title: t!("Updated Copy"), subtitle: String::new(), color: None, badge: None });
    pair.widget.set_vexpand(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&pair.widget);
    if !checked.merged.notes.trim().is_empty() {
        body.append(&note(&checked.merged.notes, None));
    }
    if !warnings.is_empty() {
        let warning = note(&tf!("Not found in Stellaris {version} or the mod's copy: {keys}. Check these before saving.", version = version, keys = warnings.join(", ")), Some("warning"));
        body.append(&warning);
    }
    let error = gtk::Label::builder().css_classes(["error"]).wrap(true).xalign(0.0).margin_bottom(6).margin_start(12).margin_end(12).selectable(true).visible(false).build();
    if let Some(problem) = problem {
        error.set_label(&tf!("The model's result still has a problem after a correction: {problem}. Fix it by hand, or undo and try again.", problem = problem));
        error.set_visible(true);
    }
    body.append(&error);
    let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).margin_start(12).margin_end(12).margin_bottom(12).build();
    body.append(&buttons);
    Review { body, pair, error, buttons }
}

/// One Update All: the requests, then the review of their results.
struct Run {
    batch: Rc<Batch>,
    items: Vec<Queued>,
    /// Objects that couldn't be sent, by index into `behind`, with why.
    unreadable: Vec<(usize, String)>,
    /// The pages of the run: progress, one result at a time, the summary.
    stack: gtk::Stack,
    title: adw::WindowTitle,
    page: adw::NavigationPage,
    /// Results still to review, as positions in `items`.
    ready: RefCell<VecDeque<(usize, Done)>>,
    /// Objects that failed, by index into `behind`, with why.
    failed: RefCell<Vec<(usize, String)>>,
    reviewed: Cell<usize>,
    to_review: Cell<usize>,
    saved: Cell<usize>,
    skipped: Cell<usize>,
    not_started: Cell<usize>,
    /// Whether the merged patch has been backed up for this run.
    backed_up: Cell<bool>,
}

impl Run {
    fn new(batch: Rc<Batch>, items: Vec<Queued>, unreadable: Vec<(usize, String)>) -> Run {
        let stack = gtk::Stack::new();
        let title = adw::WindowTitle::new(&t!("Update All with AI"), "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder().title(t!("Update All with AI")).child(&toolbar).build();
        Run {
            batch,
            items,
            failed: RefCell::new(unreadable.clone()),
            unreadable,
            stack,
            title,
            page,
            ready: RefCell::default(),
            reviewed: Cell::new(0),
            to_review: Cell::new(0),
            saved: Cell::new(0),
            skipped: Cell::new(0),
            not_started: Cell::new(0),
            backed_up: Cell::new(false),
        }
    }

    fn start(self: Rc<Self>, provider: Provider) {
        let total = self.items.len();
        let progress = gtk::ProgressBar::builder().show_text(true).text(tf!("{done} of {total} done", done = 0, total = total)).build();
        let stop = gtk::Button::builder().label(t!("Stop")).halign(gtk::Align::Center).build();
        let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Center).margin_start(24).margin_end(24).build();
        column.append(&progress);
        column.append(&stop);
        self.stack.add_named(&column, Some("progress"));
        self.page.set_can_pop(false);
        self.batch.navigation.push(&self.page);

        let cancelled = self.batch.cancelled.clone();
        stop.connect_clicked(move |stop| {
            cancelled.set(true);
            stop.set_sensitive(false);
            stop.set_label(&t!("Stopping…"));
        });

        let jobs: Rc<Vec<Job>> = Rc::new(self.items.iter().map(|q| q.job.clone()).collect());
        let queue: Rc<RefCell<VecDeque<usize>>> = Rc::new(RefCell::new((0..total).collect()));
        let results: Rc<RefCell<Finished>> = Rc::default();
        let finished = Rc::new(Cell::new(0));
        glib::spawn_future_local(async move {
            let workers: Vec<_> = (0..self.batch.hooks.merges_at_once.clamp(1, total))
                .map(|_| {
                    let (provider, jobs, queue, results, finished, cancelled, progress) = (provider.clone(), jobs.clone(), queue.clone(), results.clone(), finished.clone(), self.batch.cancelled.clone(), progress.clone());
                    glib::spawn_future_local(async move {
                        loop {
                            if cancelled.get() {
                                break;
                            }
                            let Some(position) = queue.borrow_mut().pop_front() else { break };
                            let (job, provider) = (jobs[position].clone(), provider.clone());
                            let thinking = ThinkingView::new();
                            let result = thinking_view::run(&thinking, None, move |on| job.run(&provider, on)).await.unwrap_or_else(|| Err(t!("The merge failed unexpectedly")));
                            let result = result.and_then(|(checked, warnings)| match checked.script {
                                Ok(_) => Ok((checked, warnings)),
                                Err(e) => Err(tf!("The model's result was rejected, even after a correction: {error}", error = e)),
                            });
                            results.borrow_mut().push((position, result));
                            finished.set(finished.get() + 1);
                            progress.set_fraction(finished.get() as f64 / total as f64);
                            progress.set_text(Some(&tf!("{done} of {total} done", done = finished.get(), total = total)));
                        }
                    })
                })
                .collect();
            for worker in workers {
                let _ = worker.await;
            }
            let sorted = update_all::sort(total, results.take());
            self.failed.borrow_mut().extend(sorted.failed.into_iter().map(|(position, message)| (self.items[position].index, message)));
            self.ready.replace(sorted.ready.into_iter().collect());
            self.finish(sorted.not_started.len());
        });
    }

    fn finish(self: &Rc<Self>, not_started: usize) {
        if self.stack.child_by_name("progress").is_none() {
            self.batch.navigation.push(&self.page);
        }
        self.page.set_can_pop(true);
        self.not_started.set(not_started);
        self.to_review.set(self.ready.borrow().len());
        self.next();
    }

    fn next(self: &Rc<Self>) {
        let Some((position, (checked, warnings))) = self.ready.borrow_mut().pop_front() else {
            return self.summary();
        };
        self.reviewed.set(self.reviewed.get() + 1);
        let q = &self.items[position];
        let b = &self.batch.analysis.behind[q.index];
        self.title.set_title(&b.key);
        self.title.set_subtitle(&tf!("{n} of {total}", n = self.reviewed.get(), total = self.to_review.get()));
        let Review { body, pair, error, buttons } = review_parts(&self.batch.analysis, b, &q.mod_text, &self.batch.new, checked, &warnings);
        let skip = gtk::Button::builder().label(t!("Skip")).build();
        let save = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).build();
        buttons.append(&skip);
        buttons.append(&save);
        let run = self.clone();
        skip.connect_clicked(move |_| {
            run.skipped.set(run.skipped.get() + 1);
            run.next();
        });
        let (run, weak) = (self.clone(), Rc::downgrade(&pair));
        save.connect_clicked(move |_| {
            if let Some(pair) = weak.upgrade() {
                run.save(position, &pair.right.text(), &error);
            }
        });
        keep_alive(&body, pair);
        if let Some(old) = self.stack.child_by_name("review") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&body, Some("review"));
        self.stack.set_visible_child_name("review");
    }

    fn save(self: &Rc<Self>, position: usize, text: &str, error: &gtk::Label) {
        let hooks = &self.batch.hooks;
        if !self.backed_up.get() {
            if let Err(message) = (hooks.backup)(snapshot::BEFORE_UPDATING_ALL) {
                error.set_label(&message);
                error.set_visible(true);
                return;
            }
            self.backed_up.set(true);
        }
        let q = &self.items[position];
        match save_update(&self.batch.analysis, hooks, &self.batch.analysis.behind[q.index], &q.now, &q.mod_text, text) {
            Ok(()) => {
                hooks.saved();
                (self.batch.refresh)();
                self.saved.set(self.saved.get() + 1);
                self.next();
            }
            Err(message) => {
                error.set_label(&message);
                error.set_visible(true);
            }
        }
    }

    fn summary(self: &Rc<Self>) {
        (self.batch.refresh)();
        self.title.set_title(&t!("Update All with AI"));
        self.title.set_subtitle("");
        let failed = self.failed.borrow().clone();
        let total = self.items.len() + self.unreadable.len();
        let mut line = tf!("Updated {saved} of {total}", saved = self.saved.get(), total = total);
        if self.skipped.get() > 0 {
            line.push_str(&tf!(" · {count} skipped", count = self.skipped.get()));
        }
        if !failed.is_empty() {
            line.push_str(&tf!(" · {count} failed", count = failed.len()));
        }
        if self.not_started.get() > 0 {
            line.push_str(&tf!(" · {count} not started", count = self.not_started.get()));
        }
        let page = adw::PreferencesPage::new();
        let overview = adw::PreferencesGroup::builder().description(line).build();
        let done = gtk::Button::builder().label(t!("Done")).css_classes(["suggested-action"]).halign(gtk::Align::End).build();
        overview.add(&done);
        page.add(&overview);
        if !failed.is_empty() {
            let group = adw::PreferencesGroup::builder().title(t!("Not Updated")).description(t!("These objects could not be updated in the batch.")).build();
            for (index, message) in failed {
                let b = &self.batch.analysis.behind[index];
                let row = adw::ActionRow::builder().title(&b.key).subtitle(&message).subtitle_lines(3).use_markup(false).build();
                group.add(&row);
            }
            page.add(&group);
        }
        let navigation = self.batch.navigation.clone();
        done.connect_clicked(move |_| {
            navigation.pop();
        });
        if let Some(old) = self.stack.child_by_name("summary") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&page, Some("summary"));
        self.stack.set_visible_child_name("summary");
    }
}
