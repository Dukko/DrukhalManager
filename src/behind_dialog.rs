//! The objects enabled mods replace that a game update changed: the mods
//! still load their own copies, so the game's changes don't apply to them.
//! Each can be brought up to date with AI and saved to the merged patch, one
//! at a time or all at once. Update All sends every request first, saving
//! nothing, then shows the results one by one to save or skip.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::conflicts_view::{self, arrow, chip, diff_bar, note, Analysis, Item, MergeHooks, Page};
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

#[derive(Clone, Default)]
pub struct BehindState {
    pub current_text: String,
    pub initial_text: String,
    pub notes: String,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub show_game_changes: bool,
    /// Inline scripts updated with this object.
    pub scripts: Vec<ai::MergedScript>,
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
    /// The mod's folder and descriptor id, for replaced inline scripts.
    mod_root: PathBuf,
    mod_id: String,
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
            mod_root: analysis.roots[b.mod_position].clone().unwrap_or_default(),
            mod_id: analysis.ids[b.mod_position].clone(),
            version: new.to_owned(),
        }
    }

    fn versions(&self) -> Versions<'_> {
        Versions { before: &self.before, after: &self.after, mod_copy: &self.mod_copy }
    }

    fn run(&self, provider: &Provider, on: &mut dyn FnMut(Progress)) -> Result<(Checked, Vec<String>), String> {
        let check = |text: &str| merge_dialog::validate(&self.category, &self.key, text);
        let mut checked = ai::update_checked(provider, &self.category, &self.key, &self.versions(), &check, on)?;
        checked.scripts = self.update_replaced_scripts(provider, on)?;
        let text = match &checked.script {
            Ok(script) => script.clone(),
            Err(_) => checked.merged.script.clone(),
        };
        let snapshot = self.game_dir.as_deref().and_then(|dir| base_game::ensure(&base_game::dir(), dir, &self.version).ok());
        let known = snapshot.iter().flat_map(|s| s.texts()).chain([self.before.text.as_str(), self.after.text.as_str(), self.mod_copy.text.as_str()]);
        let warnings = vocabulary::unknown(&text, known);
        Ok((checked, warnings))
    }

    /// Updates each mod script that replaced a vanilla inline script the game
    /// changed, using the old and new vanilla script as the base.
    fn update_replaced_scripts(
        &self,
        provider: &Provider,
        on: &mut dyn FnMut(Progress),
    ) -> Result<Vec<ai::MergedScript>, String> {
        let Some(game_dir) = self.game_dir.as_deref() else {
            return Ok(Vec::new());
        };
        let pairs = crate::merge::inline_scripts::replaced_call_pairs(
            &self.before.text,
            &self.mod_copy.text,
        );
        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let Some(old_snapshot) = base_game::previous(&base_game::dir(), &self.version) else {
            return Ok(Vec::new());
        };
        let mut updated = Vec::new();
        for (old_name, mod_name) in pairs {
            let Some(old) = old_snapshot.script(&old_name) else {
                continue;
            };
            let Some(new) = crate::merge::inline_scripts::read_inline_script(
                &[("Vanilla", game_dir)],
                &old_name,
            ) else {
                continue;
            };
            if old.text.trim() == new.text.trim() {
                continue;
            }
            let Some(mod_script) = crate::merge::inline_scripts::read_inline_script(
                &[(&self.mod_copy.label, &self.mod_root)],
                &mod_name,
            ) else {
                continue;
            };
            let Some(path) = crate::merge::inline_scripts::file_path(&mod_name) else {
                return Err(tf!(
                    "The replaced inline script `{name}` has an unsafe path",
                    name = &mod_name
                ));
            };
            let before = Source {
                label: old_snapshot.version.clone(),
                file: old.file.clone(),
                text: old.text.clone(),
            };
            let after = Source {
                label: self.version.clone(),
                file: new.path.clone(),
                text: new.text.clone(),
            };
            let mod_copy = Source {
                label: self.mod_copy.label.clone(),
                file: mod_script.path.clone(),
                text: mod_script.text.clone(),
            };
            let check = |text: &str| {
                let value = crate::merge::validate::validate_file(
                    &path,
                    text,
                    Some(&before),
                    std::slice::from_ref(&mod_copy),
                    None,
                );
                if value.is_ok() {
                    Ok(value.normalized)
                } else {
                    Err(value
                        .error_summary()
                        .unwrap_or_else(|| "Invalid inline script".into()))
                }
            };
            let checked = ai::update_file_checked(
                provider,
                "common/inline_scripts",
                &mod_name,
                &Versions {
                    before: &before,
                    after: &after,
                    mod_copy: &mod_copy,
                },
                &check,
                on,
            )?;
            let script = checked.script.map_err(|problem| {
                tf!(
                    "The replaced inline script `{name}` was rejected: {problem}",
                    name = &mod_name,
                    problem = problem
                )
            })?;
            updated.push(ai::MergedScript {
                name: mod_name,
                script,
                prints: vec![
                    Print::new(VANILLA, "Vanilla", &new.path, &new.text),
                    Print::new(&self.mod_id, &self.mod_copy.label, &mod_script.path, &mod_script.text),
                ],
            });
        }
        Ok(updated)
    }
}

/// Saves `text` as the updated copy of `b` to the merged patch.
fn save_update(
    analysis: &Analysis,
    hooks: &MergeHooks,
    b: &Behind,
    now: &Saved,
    mod_text: &str,
    text: &str,
    scripts: &[ai::MergedScript],
) -> Result<(), String> {
    let script = merge_dialog::validate(&b.category, &b.key, text)?;
    for merged in scripts {
        let path = crate::merge::inline_scripts::file_path(&merged.name)
            .ok_or_else(|| tf!("The updated inline script `{name}` has an unsafe path", name = &merged.name))?;
        (hooks.save_file)(&path, merged.script.as_bytes(), merged.prints.clone())?;
    }
    let position = b.mod_position;
    let prints = vec![Print::new(VANILLA, "Vanilla", &now.file, &now.text), Print::new(&analysis.ids[position], analysis.name(position), &b.file, mod_text)];
    (hooks.save_object)(&b.category, &b.key, &script, prints)?;
    if !scripts.is_empty() {
        (hooks.set_scripts)(
            &b.category,
            &b.key,
            scripts.iter().map(|s| s.name.clone()).collect(),
        )?;
    }
    Ok(())
}

/// The view for one game-update conflict embedded directly in the Conflicts page.
pub struct BehindView {
    page: Weak<Page>,
    index: usize,
    key: String,
    root: gtk::Box,
    header: adw::Bin,
    notes: gtk::Box,
    thinking: Rc<ThinkingView>,
    pair: Rc<Pair>,
    busy: Cell<bool>,
}

pub fn view(page: &Rc<Page>, index: usize) -> (gtk::Widget, Rc<dyn Any>) {
    let b = &page.analysis.behind[index];
    let key = format!("behind:{}:{}", b.category, b.key);
    let thinking = ThinkingView::new();
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    let header = adw::Bin::new();
    let notes = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    root.append(&header);
    root.append(&notes);
    root.append(&thinking.widget);

    let pair = Pair::new(true);
    root.append(&pair.widget);

    let view = Rc::new(BehindView {
        page: Rc::downgrade(page),
        index,
        key,
        root: root.clone(),
        header,
        notes,
        thinking,
        pair,
        busy: Cell::new(false),
    });

    view.init(page);
    (root.upcast(), view)
}

impl BehindView {
    fn init(self: &Rc<Self>, page: &Rc<Page>) {
        let b = &page.analysis.behind[self.index];
        {
            let mut session = page.hooks.session.borrow_mut();
            if !session.behind.contains_key(&self.key) {
                let existing = (page.hooks.existing)(&b.category, &b.key).map(|m| m.script);
                let initial = existing.unwrap_or_else(|| b.now.as_ref().map(|n| n.text.clone()).unwrap_or_else(|| b.was.text.clone()));
                session.behind.insert(
                    self.key.clone(),
                    BehindState {
                        current_text: initial.clone(),
                        initial_text: initial,
                        notes: String::new(),
                        warnings: Vec::new(),
                        error: None,
                        show_game_changes: false,
                        scripts: Vec::new(),
                    },
                );
            }
        }

        // Connect editor changes.
        let weak_view = Rc::downgrade(self);
        self.pair.connect_edit(move |text| {
            let Some(view) = weak_view.upgrade() else { return };
            let Some(page) = view.page.upgrade() else { return };
            let mut session = page.hooks.session.borrow_mut();
            if let Some(state) = session.behind.get_mut(&view.key)
                && !state.show_game_changes
            {
                state.current_text = text.to_owned();
            }
        });

        self.render();
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut BehindState) -> R) -> Option<R> {
        let page = self.page.upgrade()?;
        let mut session = page.hooks.session.borrow_mut();
        session.behind.get_mut(&self.key).map(f)
    }

    fn render(self: &Rc<Self>) {
        let Some(page) = self.page.upgrade() else { return };
        let b = &page.analysis.behind[self.index];
        let mod_name = page.analysis.name(b.mod_position);
        let Some((old_ver, new_ver)) = page.analysis.behind_versions.as_ref() else { return };

        let state = page.hooks.session.borrow().behind.get(&self.key).cloned().unwrap_or_default();
        let is_merged = (page.hooks.is_merged)(&b.category, &b.key);
        let busy = self.busy.get();

        // Header.
        let subtitle = tf!("Object · {category} · {mod_name} replaces this, and the base game update", category = &b.category, mod_name = mod_name);
        let header = page.detail_header(&b.key, &subtitle);

        let chips = &header.chips;
        chips.insert(&chip(mod_name, Some(b.mod_position), "current"), -1);
        chips.insert(&arrow(), -1);
        chips.insert(&chip(&tf!("Stellaris {version}", version = new_ver), None, "done"), -1);
        if is_merged {
            chips.insert(&arrow(), -1);
            chips.insert(&chip(&t!("Updated in merged patch"), None, "done"), -1);
        }

        // Top actions.
        let mod_text_opt = read_mod_copy(&page.analysis, b);
        if let (Some(now), Some(mod_text)) = (&b.now, &mod_text_opt) {
            let report_btn = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .tooltip_text(t!("Copy a formatted report for the mod author to clipboard"))
                .css_classes(["flat"])
                .build();
            {
                let (mod_name_str, category_str, key_str, mod_file_str, base_file_str, old_v, new_v, mod_t, now_t, hooks) = (
                    mod_name.to_owned(),
                    b.category.clone(),
                    b.key.clone(),
                    b.file.clone(),
                    now.file.clone(),
                    old_ver.clone(),
                    new_ver.clone(),
                    mod_text.clone(),
                    now.text.clone(),
                    page.hooks.clone(),
                );
                report_btn.connect_clicked(move |btn| {
                    let text = format!(
                        "Hi! It looks like {mod_name_str} has an out-of-date override for Stellaris {new_v}:\n\n\
                         • Object: `{key_str}`\n\
                         • Category: `{category_str}`\n\
                         • Mod file: `{mod_file_str}`\n\
                         • Base game file: `{base_file_str}`\n\n\
                         The mod's copy appears to be based on Stellaris {old_v} and is missing changes introduced in Stellaris {new_v}.\n\n\
                         ```stellaris\n\
                         # Current copy in {mod_name_str}\n\
                         {mod_t}\n\
                         ```\n\n\
                         ```stellaris\n\
                         # Base game version in Stellaris {new_v}\n\
                         {now_t}\n\
                         ```\n"
                    );
                    btn.clipboard().set_text(&text);
                    (hooks.toast)(&t!("Copied report for mod author to clipboard"));
                });
            }
            header.actions.append(&report_btn);

            let merge_tool_btn = gtk::Button::builder().label(t!("Merge Tool…")).tooltip_text(t!("Open in your external merge tool to resolve differences manually")).build();
            {
                let view = self.clone();
                merge_tool_btn.connect_clicked(move |btn| {
                    view.open_merge_tool(btn);
                });
            }
            header.actions.append(&merge_tool_btn);

            let ai_btn = gtk::Button::builder()
                .child(&adw::ButtonContent::builder().icon_name(conflicts_view::AI_ICON).label(t!("Update with AI")).build())
                .css_classes(["suggested-action"])
                .sensitive(!busy)
                .build();
            {
                let view = self.clone();
                ai_btn.connect_clicked(move |btn| {
                    view.update_with_ai(btn);
                });
            }
            page.bind_key("merge-ai", &ai_btn);
            header.actions.append(&ai_btn);

            let save_btn = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).tooltip_text(t!("Save the updated copy to the merged patch")).sensitive(!busy).build();
            {
                let view = self.clone();
                save_btn.connect_clicked(move |_| {
                    view.save();
                });
            }
            header.actions.append(&save_btn);
            header.actions.append(&page.ignore_button(conflicts_view::Item::Behind(self.index)));
        }

        self.header.set_child(Some(&header.widget));

        // Notes.
        while let Some(child) = self.notes.first_child() {
            self.notes.remove(&child);
        }
        if b.now.is_none() {
            self.notes.append(&note(&t!("This object was removed from Stellaris in this update."), Some("warning")));
        } else if mod_text_opt.is_none() {
            self.notes.append(&note(&t!("The mod's copy couldn't be read, so it can't be updated here."), Some("warning")));
        } else {
            self.notes.append(&note(&tf!("{mod_name} still uses its own copy, from {file}.", mod_name = mod_name, file = &b.file), None));
            if is_merged {
                self.notes.append(&note(&t!("Updated in the merged patch. Saving again replaces it."), Some("success")));
            }
            if !state.notes.is_empty() {
                self.notes.append(&note(&tf!("Notes from the model: {notes}", notes = &state.notes), None));
            }
            if !state.warnings.is_empty() {
                self.notes.append(&note(&tf!("Not found in Stellaris {version} or the mod's copy: {keys}. Check these before saving.", version = new_ver, keys = state.warnings.join(", ")), Some("warning")));
            }
            if let Some(err) = &state.error {
                self.notes.append(&note(err, Some("error")));
            }
        }

        // Diff controls & cards.
        let session = page.hooks.session.borrow();
        self.pair.set_fold(session.fold_unchanged);
        let ignore_ws = session.ignore_whitespace;
        drop(session);

        if b.now.is_none() {
            self.pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = old_ver), subtitle: b.was.file.clone(), color: None, badge: None });
            self.pair.left.set_actions(&[]);
            self.pair.right.set_header(&Header { title: tf!("Stellaris {version}", version = new_ver), subtitle: t!("Removed from the game"), color: None, badge: None });
            self.pair.right.set_actions(&[]);
            self.pair.right.code.set_editable(false);
            self.pair.show(&b.was.text, "", &Compare::Each, false);
            return;
        }

        let Some(now) = &b.now else { return };
        let Some(mod_text) = mod_text_opt else {
            self.pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = old_ver), subtitle: b.was.file.clone(), color: None, badge: None });
            self.pair.left.set_actions(&[]);
            self.pair.right.set_header(&Header { title: tf!("Stellaris {version}", version = new_ver), subtitle: now.file.clone(), color: None, badge: None });
            self.pair.right.set_actions(&[]);
            self.pair.right.code.set_editable(false);
            self.pair.show(&b.was.text, &now.text, &Compare::Each, false);
            return;
        };

        if state.show_game_changes {
            self.pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = old_ver), subtitle: b.was.file.clone(), color: None, badge: Some((t!("Base Game"), None)) });
            self.pair.left.set_actions(&[]);
            self.pair.right.set_header(&Header { title: tf!("Stellaris {version}", version = new_ver), subtitle: now.file.clone(), color: None, badge: Some((t!("Base Game"), None)) });
            self.pair.right.set_actions(&[]);
            self.pair.right.code.set_editable(false);
            self.pair.show(&b.was.text, &now.text, &Compare::Each, ignore_ws);
        } else {
            let use_mod = gtk::Button::builder().label(t!("Use Mod's Version")).tooltip_text(t!("Reset editor to the mod's version")).sensitive(!busy).css_classes(["flat"]).build();
            {
                let (weak_pair, mod_t) = (Rc::downgrade(&self.pair), mod_text.clone());
                use_mod.connect_clicked(move |_| {
                    if let Some(p) = weak_pair.upgrade() {
                        p.right.replace_text(&mod_t);
                    }
                });
            }
            self.pair.left.set_header(&Header { title: mod_name.to_owned(), subtitle: b.file.clone(), color: Some(b.mod_position), badge: Some((t!("Mod"), None)) });
            self.pair.left.set_actions(&[use_mod.upcast()]);

            let right_title = if is_merged { t!("Updated in merged patch") } else { t!("Updated Copy") };
            self.pair.right.set_header(&Header { title: right_title, subtitle: now.file.clone(), color: None, badge: None });

            let revert = gtk::Button::builder().icon_name("document-revert-symbolic").tooltip_text(t!("Revert edits to initial version")).sensitive(!busy).css_classes(["flat"]).build();
            {
                let (weak_pair, init_text) = (Rc::downgrade(&self.pair), state.initial_text.clone());
                revert.connect_clicked(move |_| {
                    if let Some(p) = weak_pair.upgrade() {
                        p.right.replace_text(&init_text);
                    }
                });
            }
            let use_game = gtk::Button::builder().label(t!("Use Game's Version")).tooltip_text(t!("Reset editor to the new base game version")).sensitive(!busy).css_classes(["flat"]).build();
            {
                let (weak_pair, now_t) = (Rc::downgrade(&self.pair), now.text.clone());
                use_game.connect_clicked(move |_| {
                    if let Some(p) = weak_pair.upgrade() {
                        p.right.replace_text(&now_t);
                    }
                });
            }
            self.pair.right.set_actions(&[revert.upcast(), use_game.upcast()]);
            self.pair.right.code.set_editable(!busy);
            self.pair.show(&mod_text, &state.current_text, &Compare::Each, ignore_ws);
        }

        // Set up the diff bar if not attached.
        if self.root.first_child().as_ref() == Some(self.header.upcast_ref())
            && self.root.observe_children().n_items() == 4
        {
            let bar = diff_bar(&self.pair);
            let take_btn = gtk::Button::builder().label(t!("Take Change")).tooltip_text(t!("Copy the highlighted difference from the mod's version into the editable result")).sensitive(false).css_classes(["flat"]).build();
            {
                let take_weak = take_btn.downgrade();
                let view_weak = Rc::downgrade(self);
                self.pair.connect_change(move |_, current, _| {
                    if let (Some(btn), Some(view)) = (take_weak.upgrade(), view_weak.upgrade()) {
                        let is_game = view.with_state(|s| s.show_game_changes).unwrap_or(false);
                        btn.set_sensitive(current.is_some() && !is_game && !view.busy.get());
                    }
                });
                let pair_weak = Rc::downgrade(&self.pair);
                take_btn.connect_clicked(move |_| {
                    if let Some(pair) = pair_weak.upgrade() {
                        pair.put_in_current();
                    }
                });
            }
            bar.extra.append(&take_btn);

            let game_changes_toggle = gtk::ToggleButton::builder()
                .label(t!("Game Changes"))
                .tooltip_text(tf!("Toggle between comparing the mod's copy with the update and comparing what changed in the base game between Stellaris {old} and {new}", old = old_ver, new = new_ver))
                .active(state.show_game_changes)
                .css_classes(["flat"])
                .build();
            {
                let view = self.clone();
                game_changes_toggle.connect_toggled(move |btn| {
                    view.with_state(|s| s.show_game_changes = btn.is_active());
                    view.render();
                });
            }
            bar.extra.append(&game_changes_toggle);

            let now_text = b.now.as_ref().map(|n| n.text.as_str()).unwrap_or(&b.was.text);
            let mut left_roots: Vec<(&str, &Path)> = Vec::new();
            if let Some(Some(root)) = page.analysis.roots.get(b.mod_position) {
                left_roots.push((page.analysis.name(b.mod_position), root.as_path()));
            }
            let mut right_roots: Vec<(&str, &Path)> = Vec::new();
            if let Some(game_dir) = page.hooks.game_dir.as_deref() {
                left_roots.push(("Vanilla", game_dir));
                right_roots.push(("Vanilla", game_dir));
            }
            let inline_comparisons = crate::merge::inline_scripts::find_comparisons(&mod_text, &left_roots, now_text, &right_roots);
            if !inline_comparisons.is_empty() {
                let label = tnf!("Inline Script ({count})", "Inline Scripts ({count})", count = inline_comparisons.len());
                let btn = gtk::Button::builder()
                    .label(label)
                    .tooltip_text(t!("Compare the inline script files that differ or were replaced between the mod and the base game"))
                    .css_classes(["flat"])
                    .build();
                let page_weak = Rc::downgrade(&page);
                btn.connect_clicked(move |b| {
                    let Some(page) = page_weak.upgrade() else { return };
                    let session = page.hooks.session.borrow();
                    conflicts_view::show_inline_scripts(b, inline_comparisons.clone(), session.fold_unchanged, session.ignore_whitespace);
                });
                bar.extra.append(&btn);
            }

            let fold_toggle = gtk::ToggleButton::builder().label(t!("Changed Lines Only")).tooltip_text(t!("Fold runs of unchanged lines")).active(page.hooks.session.borrow().fold_unchanged).css_classes(["flat"]).build();
            {
                let (pair_weak, page_weak) = (Rc::downgrade(&self.pair), Rc::downgrade(&page));
                fold_toggle.connect_toggled(move |btn| {
                    let active = btn.is_active();
                    if let Some(page) = page_weak.upgrade() {
                        page.hooks.session.borrow_mut().fold_unchanged = active;
                    }
                    if let Some(pair) = pair_weak.upgrade() {
                        pair.set_fold(active);
                    }
                });
            }
            bar.extra.append(&fold_toggle);

            let whitespace_toggle = gtk::ToggleButton::builder().label(t!("Ignore Spacing")).tooltip_text(t!("Ignore differences in whitespace")).active(page.hooks.session.borrow().ignore_whitespace).css_classes(["flat"]).build();
            {
                let view = self.clone();
                whitespace_toggle.connect_toggled(move |btn| {
                    if let Some(page) = view.page.upgrade() {
                        page.hooks.session.borrow_mut().ignore_whitespace = btn.is_active();
                    }
                    view.render();
                });
            }
            bar.extra.append(&whitespace_toggle);

            self.root.insert_child_after(&bar.widget, Some(&self.thinking.widget));
        }
    }

    fn open_merge_tool(self: &Rc<Self>, button: &gtk::Button) {
        let Some(page) = self.page.upgrade() else { return };
        let command = (page.hooks.merge_tool)();
        if command.is_empty() {
            (page.hooks.toast)(&t!("Choose a merge tool in Preferences first"));
            return (page.hooks.open_merge_tool_settings)();
        }
        let b = &page.analysis.behind[self.index];
        let Some((_, new_ver)) = page.analysis.behind_versions.as_ref() else { return };
        let tool = conflicts_view::merge_tool_name(&command);
        let left_label = page.analysis.name(b.mod_position).to_owned();
        let right_label = tf!("Stellaris {version}", version = new_ver);
        let mod_text = read_mod_copy(&page.analysis, b).unwrap_or_default();
        let left = conflicts_view::ToolSide { label: left_label, text: mod_text };
        let right = conflicts_view::ToolSide { label: right_label, text: self.pair.right.text() };
        let base = Some(b.was.text.clone());
        let (hooks, pair_weak) = (page.hooks.clone(), Rc::downgrade(&self.pair));
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

    fn update_with_ai(self: &Rc<Self>, button: &gtk::Button) {
        let Some(page) = self.page.upgrade() else { return };
        let Some(provider) = (page.hooks.provider)() else {
            (page.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
            (page.hooks.open_preferences)();
            return;
        };
        let b = &page.analysis.behind[self.index];
        let Some((old_ver, new_ver)) = page.analysis.behind_versions.as_ref() else { return };
        let Some(ref now) = b.now else { return };
        let Some(mod_text) = read_mod_copy(&page.analysis, b) else { return };

        let job = Job::new(&page.analysis, &page.hooks, b, now, old_ver, new_ver, &mod_text);
        button.set_sensitive(false);
        self.busy.set(true);
        self.with_state(|s| s.error = None);

        let (view, thinking, button_ref) = (self.clone(), self.thinking.clone(), button.clone());
        glib::spawn_future_local(async move {
            let result = thinking_view::run(&thinking, None, move |on| job.run(&provider, on)).await;
            view.busy.set(false);
            button_ref.set_sensitive(true);
            match result {
                Some(Ok((checked, warnings))) => {
                    let script = match checked.script {
                        Ok(s) => s,
                        Err(_) => checked.merged.script,
                    };
                    let mut notes = checked.merged.notes;
                    if !checked.scripts.is_empty() {
                        notes.push_str(&tnf!(
                            " · {count} inline script updated",
                            " · {count} inline scripts updated",
                            count = checked.scripts.len()
                        ));
                    }
                    view.with_state(|s| {
                        s.notes = notes;
                        s.warnings = warnings;
                        s.current_text = script.clone();
                        s.scripts = checked.scripts.clone();
                    });
                    view.pair.right.replace_text(&script);
                    view.render();
                }
                Some(Err(message)) => {
                    view.with_state(|s| s.error = Some(message));
                    view.render();
                }
                None => {
                    view.with_state(|s| s.error = Some(t!("The merge failed unexpectedly")));
                    view.render();
                }
            }
        });
    }

    fn save(self: &Rc<Self>) {
        let Some(page) = self.page.upgrade() else { return };
        let b = &page.analysis.behind[self.index];
        let Some(ref now) = b.now else { return };
        let Some(mod_text) = read_mod_copy(&page.analysis, b) else { return };
        let text = self.pair.right.text();
        let scripts = self
            .with_state(|s| s.scripts.clone())
            .unwrap_or_default();
        match save_update(&page.analysis, &page.hooks, b, now, &mod_text, &text, &scripts) {
            Ok(()) => {
                self.with_state(|s| {
                    s.initial_text = text.clone();
                    s.current_text = text;
                    s.error = None;
                });
                page.hooks.saved();
                page.update_status(Item::Behind(self.index));
                (page.hooks.toast)(&tf!("Saved the updated {key} to the merged patch", key = &b.key));
                page.advance(Item::Behind(self.index));
            }
            Err(message) => {
                self.with_state(|s| s.error = Some(message));
                self.render();
            }
        }
    }
}

/// Runs "Update All with AI" for every game-update conflict from the group header action.
pub fn run_update_all(page: &Rc<Page>) {
    let Some(provider) = (page.hooks.provider)() else {
        (page.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
        (page.hooks.open_preferences)();
        return;
    };
    let Some((old_ver, new_ver)) = page.analysis.behind_versions.clone() else { return };
    let chosen = update_all::included(&page.analysis.behind, |c, k| (page.hooks.is_merged)(c, k));
    if chosen.is_empty() {
        return;
    }
    let wanted: Vec<(usize, Option<PathBuf>, Behind)> = chosen.iter().map(|&i| (i, page.analysis.roots.get(page.analysis.behind[i].mod_position).cloned().flatten(), page.analysis.behind[i].clone())).collect();
    let page = page.clone();
    glib::spawn_future_local(async move {
        let read = gio::spawn_blocking(move || wanted.into_iter().map(|(i, root, b)| (i, read_copy(root.as_deref(), &b))).collect::<Vec<_>>()).await.unwrap_or_default();
        let mut items = Vec::new();
        let mut unreadable = Vec::new();
        for (index, text) in read {
            let b = &page.analysis.behind[index];
            match (text, &b.now) {
                (Some(mod_text), Some(now)) => {
                    let job = Job::new(&page.analysis, &page.hooks, b, now, &old_ver, &new_ver, &mod_text);
                    items.push(Queued { index, now: now.clone(), mod_text, job });
                }
                _ => unreadable.push((index, t!("The mod's copy couldn't be read, so it can't be updated here."))),
            }
        }
        if items.is_empty() {
            return;
        }
        confirm_batch(&page, provider, items, unreadable, old_ver, new_ver);
    });
}

struct Queued {
    index: usize,
    now: Saved,
    mod_text: String,
    job: Job,
}

type Done = (Checked, Vec<String>);
type Finished = Vec<(usize, Result<Done, String>)>;

fn confirm_batch(page: &Rc<Page>, provider: Provider, items: Vec<Queued>, unreadable: Vec<(usize, String)>, old: String, new: String) {
    let total = items.iter().fold(Estimate::default(), |mut sum, q| {
        sum += ai::estimate_update(&q.job.category, &q.job.key, &q.job.versions());
        sum
    });
    let parallel = page.hooks.merges_at_once.clamp(1, items.len());
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
    let (run_page, run_provider, unreadable) = (page.clone(), provider.clone(), RefCell::new(Some(unreadable)));
    alert.connect_response(None, move |_, response| {
        if response != "update" {
            return;
        }
        if let (Some(items), Some(unreadable)) = (items.borrow_mut().take(), unreadable.borrow_mut().take()) {
            RunDialog::start(&run_page, run_provider.clone(), items, unreadable, old.clone(), new.clone());
        }
    });
    let parent = page.split.root().and_downcast::<gtk::Window>();
    alert.present(parent.as_ref());

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

struct RunDialog {
    page: Rc<Page>,
    dialog: adw::Dialog,
    items: Vec<Queued>,
    unreadable: Vec<(usize, String)>,
    stack: gtk::Stack,
    title: adw::WindowTitle,
    ready: RefCell<VecDeque<(usize, Done)>>,
    failed: RefCell<Vec<(usize, String)>>,
    reviewed: Cell<usize>,
    to_review: Cell<usize>,
    saved: Cell<usize>,
    skipped: Cell<usize>,
    not_started: Cell<usize>,
    backed_up: Cell<bool>,
    cancelled: Rc<Cell<bool>>,
    new: String,
}

impl RunDialog {
    fn start(page: &Rc<Page>, provider: Provider, items: Vec<Queued>, unreadable: Vec<(usize, String)>, _old: String, new: String) {
        let stack = gtk::Stack::new();
        let title = adw::WindowTitle::new(&t!("Update All with AI"), "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));

        let dialog = adw::Dialog::builder().content_width(900).content_height(720).child(&toolbar).build();

        let cancelled = Rc::new(Cell::new(false));
        dialog.connect_closed({
            let cancelled = cancelled.clone();
            move |_| cancelled.set(true)
        });

        let run = Rc::new(RunDialog {
            page: page.clone(),
            dialog: dialog.clone(),
            items,
            failed: RefCell::new(unreadable.clone()),
            unreadable,
            stack,
            title,
            ready: RefCell::default(),
            reviewed: Cell::new(0),
            to_review: Cell::new(0),
            saved: Cell::new(0),
            skipped: Cell::new(0),
            not_started: Cell::new(0),
            backed_up: Cell::new(false),
            cancelled,
            new,
        });

        let total = run.items.len();
        let progress = gtk::ProgressBar::builder().show_text(true).text(tf!("{done} of {total} done", done = 0, total = total)).build();
        let stop = gtk::Button::builder().label(t!("Stop")).halign(gtk::Align::Center).build();
        let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Center).margin_start(24).margin_end(24).build();
        column.append(&progress);
        column.append(&stop);
        run.stack.add_named(&column, Some("progress"));

        let cancelled = run.cancelled.clone();
        stop.connect_clicked(move |stop| {
            cancelled.set(true);
            stop.set_sensitive(false);
            stop.set_label(&t!("Stopping…"));
        });

        let parent = page.split.root().and_downcast::<gtk::Window>();
        dialog.present(parent.as_ref());

        let jobs: Rc<Vec<Job>> = Rc::new(run.items.iter().map(|q| q.job.clone()).collect());
        let queue: Rc<RefCell<VecDeque<usize>>> = Rc::new(RefCell::new((0..total).collect()));
        let results: Rc<RefCell<Finished>> = Rc::default();
        let finished = Rc::new(Cell::new(0));
        let run_clone = run.clone();
        glib::spawn_future_local(async move {
            let workers: Vec<_> = (0..run_clone.page.hooks.merges_at_once.clamp(1, total))
                .map(|_| {
                    let (provider, jobs, queue, results, finished, cancelled, progress) = (provider.clone(), jobs.clone(), queue.clone(), results.clone(), finished.clone(), run_clone.cancelled.clone(), progress.clone());
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
            run_clone.failed.borrow_mut().extend(sorted.failed.into_iter().map(|(position, message)| (run_clone.items[position].index, message)));
            run_clone.ready.replace(sorted.ready.into_iter().collect());
            run_clone.finish(sorted.not_started.len());
        });
    }

    fn finish(self: &Rc<Self>, not_started: usize) {
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
        let b = &self.page.analysis.behind[q.index];
        self.title.set_title(&b.key);
        self.title.set_subtitle(&tf!("{n} of {total}", n = self.reviewed.get(), total = self.to_review.get()));

        let Review {
            body,
            pair,
            error,
            buttons,
            scripts,
        } = review_parts(&self.page.analysis, b, &q.mod_text, &self.new, checked, &warnings);
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
                run.save(position, &pair.right.text(), &error, &scripts);
            }
        });
        if let Some(old) = self.stack.child_by_name("review") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&body, Some("review"));
        self.stack.set_visible_child_name("review");
    }

    fn save(
        self: &Rc<Self>,
        position: usize,
        text: &str,
        error: &gtk::Label,
        scripts: &[ai::MergedScript],
    ) {
        let hooks = &self.page.hooks;
        if !self.backed_up.get() {
            if let Err(message) = (hooks.backup)(snapshot::BEFORE_UPDATING_ALL) {
                error.set_label(&message);
                error.set_visible(true);
                return;
            }
            self.backed_up.set(true);
        }
        let q = &self.items[position];
        match save_update(
            &self.page.analysis,
            hooks,
            &self.page.analysis.behind[q.index],
            &q.now,
            &q.mod_text,
            text,
            scripts,
        ) {
            Ok(()) => {
                hooks.saved();
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
        self.page.hooks.saved();
        self.page.fill();
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
                let b = &self.page.analysis.behind[index];
                let row = adw::ActionRow::builder().title(&b.key).subtitle(&message).subtitle_lines(3).use_markup(false).build();
                group.add(&row);
            }
            page.add(&group);
        }
        let dialog = self.dialog.clone();
        done.connect_clicked(move |_| {
            dialog.close();
        });
        if let Some(old) = self.stack.child_by_name("summary") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&page, Some("summary"));
        self.stack.set_visible_child_name("summary");
    }
}

struct Review {
    body: gtk::Box,
    pair: Rc<Pair>,
    error: gtk::Label,
    buttons: gtk::Box,
    scripts: Vec<ai::MergedScript>,
}

fn review_parts(analysis: &Analysis, b: &Behind, mod_text: &str, version: &str, checked: Checked, warnings: &[String]) -> Review {
    let scripts = checked.scripts.clone();
    let (script, problem) = match checked.script {
        Ok(script) => (script, None),
        Err(problem) => (checked.merged.script, Some(problem)),
    };
    let pair = Pair::new(true);
    pair.show(mod_text, &script, &Compare::Each, false);
    pair.left.set_header(&Header { title: analysis.name(b.mod_position).to_owned(), subtitle: b.file.clone(), color: None, badge: None });
    pair.right.set_header(&Header { title: t!("Updated Copy"), subtitle: String::new(), color: None, badge: None });
    pair.widget.set_vexpand(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&pair.widget);
    let mut notes = checked.merged.notes.clone();
    if !scripts.is_empty() {
        notes.push_str(&tnf!(
            " · {count} inline script updated",
            " · {count} inline scripts updated",
            count = scripts.len()
        ));
    }
    if !notes.trim().is_empty() {
        body.append(&note(&notes, None));
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
    Review {
        body,
        pair,
        error,
        buttons,
        scripts,
    }
}
