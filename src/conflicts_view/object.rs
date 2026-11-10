//! Merging one object conflict two versions at a time: the first mod's
//! version against the next one, then the result so far against the one
//! after, until one editable result is left to save.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};

use super::{Item, Page, chip, arrow, diff_bar, note, object_key, option_toggle, plural};
use crate::diff_view::{Card, Compare, Header, Pair};
use crate::merge::ai;
use crate::merge::conflicts::{self, ObjectConflict};
use crate::merge::source::Source;
use crate::merge::trivial;
use crate::merge_dialog::{self, Loaded};
use crate::thinking_view::{self, ThinkingView};

/// How far one object's merge has got.
pub struct Steps {
    loaded: Loaded,
    /// The scanned position of the mod of each of `loaded.sources`.
    positions: Vec<usize>,
    /// The version kept so far, on the left card.
    result: String,
    /// The mods whose changes `result` holds.
    from: Vec<String>,
    /// Whether differences were copied into `result` by hand.
    edited: bool,
    /// Index into `loaded.sources` of the version on the right card. Every
    /// version was gone through once it's past the last.
    next: usize,
    /// The model's notes on its last merge.
    notes: String,
    /// The right card's version as edited by hand, if it was.
    right_edit: Option<String>,
    /// The text in the final editor.
    final_text: Option<String>,
    /// Whether the saved merge was opened as it is.
    from_saved: bool,
    history: Vec<Snapshot>,
}

#[derive(Clone)]
struct Snapshot {
    result: String,
    from: Vec<String>,
    edited: bool,
    next: usize,
    notes: String,
    right_edit: Option<String>,
    final_text: Option<String>,
    from_saved: bool,
}

impl Steps {
    fn new(loaded: Loaded, positions: Vec<usize>, saved: Option<String>) -> Self {
        let first = loaded.sources.first();
        let result = first.map(|s| s.text.clone()).unwrap_or_default();
        let from = first.map(|s| vec![s.label.clone()]).unwrap_or_default();
        let n = loaded.sources.len();
        let mut steps = Steps { loaded, positions, result, from, edited: false, next: 1, notes: String::new(), right_edit: None, final_text: None, from_saved: false, history: Vec::new() };
        if let Some(text) = saved {
            steps.next = n.max(1);
            steps.final_text = Some(text);
            steps.from_saved = true;
        }
        steps
    }

    /// Number of pairs to go through.
    fn total(&self) -> usize {
        self.loaded.sources.len().saturating_sub(1)
    }

    fn done(&self) -> bool {
        self.next >= self.loaded.sources.len()
    }

    /// `(step, steps)` once a merge is under way, for the sidebar. A step
    /// past the last means the result is ready to save.
    pub fn progress(&self) -> Option<(usize, usize)> {
        if self.from_saved || self.total() == 0 || (self.history.is_empty() && !self.done()) {
            return None;
        }
        Some((self.next.min(self.total() + 1), self.total()))
    }

    fn snapshot(&mut self) {
        self.history.push(Snapshot {
            result: self.result.clone(),
            from: self.from.clone(),
            edited: self.edited,
            next: self.next,
            notes: self.notes.clone(),
            right_edit: self.right_edit.clone(),
            final_text: self.final_text.clone(),
            from_saved: self.from_saved,
        });
    }

    /// Whether the left card still shows the first mod's version as it is.
    /// Which of the versions is the one the game uses, if it was found.
    fn winner(&self, c: &ObjectConflict) -> Option<usize> {
        let (w, _) = c.winner()?;
        let w = &c.definitions[w];
        (0..self.loaded.sources.len()).find(|&i| self.positions[i] == w.mod_position && self.loaded.sources[i].file == w.file)
    }

    fn left_is_first(&self) -> bool {
        self.next == 1 && !self.edited
    }

    /// The left card's version, as the AI should see it.
    fn left_source(&self) -> Source {
        let first = &self.loaded.sources[0];
        if self.left_is_first() {
            return first.clone();
        }
        Source { label: format!("Result so far ({})", self.from.join(" + ")), file: first.file.clone(), text: self.result.clone() }
    }

    fn right(&self) -> &Source {
        &self.loaded.sources[self.next]
    }

    /// The right card's version, with any edits.
    fn right_source(&self) -> Source {
        let mut source = self.right().clone();
        if let Some(text) = &self.right_edit {
            source.text = text.clone();
        }
        source
    }

    /// Records the right card's text, or drops the edit when it's back to
    /// the mod's version.
    fn edit_right(&mut self, text: &str) {
        let lines = |s: &str| s.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
        self.right_edit = (lines(text) != lines(&self.right().text)).then(|| text.to_owned());
    }

    /// Keeps the left card's version and moves on.
    fn keep_left(&mut self) {
        self.snapshot();
        self.next += 1;
        self.notes.clear();
        self.right_edit = None;
    }

    /// Takes the right card's version and moves on.
    fn use_right(&mut self) {
        self.snapshot();
        let right = self.right_source();
        self.result = right.text;
        self.from = vec![right.label];
        self.edited = self.right_edit.take().is_some();
        self.next += 1;
        self.notes.clear();
    }

    /// Takes the model's merge of the two cards and moves on.
    fn merged(&mut self, script: String, notes: String) {
        self.snapshot();
        let label = self.right().label.clone();
        self.result = script;
        if !self.from.contains(&label) {
            self.from.push(label);
        }
        self.edited = false;
        self.next += 1;
        self.notes = notes;
        self.right_edit = None;
    }

    /// Replaces the left card's version with one edited by hand.
    fn take(&mut self, text: String) {
        self.snapshot();
        let label = self.right().label.clone();
        self.result = text;
        if !self.from.contains(&label) {
            self.from.push(label);
        }
        self.edited = true;
    }

    fn take_final(&mut self, text: String) {
        self.snapshot();
        self.final_text = Some(text);
    }

    fn undo(&mut self) {
        if let Some(s) = self.history.pop() {
            (self.result, self.from, self.edited, self.next, self.notes, self.right_edit, self.final_text, self.from_saved) = (s.result, s.from, s.edited, s.next, s.notes, s.right_edit, s.final_text, s.from_saved);
        }
    }

    fn start_over(&mut self) {
        let first = &self.loaded.sources[0];
        self.result = first.text.clone();
        self.from = vec![first.label.clone()];
        (self.edited, self.next, self.from_saved) = (false, 1, false);
        self.notes.clear();
        self.right_edit = None;
        self.final_text = None;
        self.history.clear();
    }

    fn final_text(&self) -> String {
        self.final_text.clone().unwrap_or_else(|| self.result.clone())
    }
}

/// What the final step shows: the editor, or the result compared with one
/// of the versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Final {
    Edit,
    Base,
    Source(usize),
}

pub struct ObjectView {
    page: Weak<Page>,
    item: Item,
    /// The session key of the object.
    key: String,
    category: String,
    name: String,
    root: gtk::Box,
    header: adw::Bin,
    notes: gtk::Box,
    stack: gtk::Stack,
    thinking: Rc<ThinkingView>,
    busy: Cell<bool>,
    error: RefCell<Option<String>>,
    showing: Cell<Final>,
    pages: Cell<u32>,
}

pub fn view(page: &Rc<Page>, index: usize) -> (gtk::Widget, Rc<dyn std::any::Any>) {
    let c = &page.analysis.report.objects[index];
    let stack = gtk::Stack::builder().transition_duration(280).vexpand(true).hexpand(true).build();
    // Drops the cards that slid out.
    stack.connect_transition_running_notify(|stack| {
        if !stack.is_transition_running() {
            drop_hidden(stack);
        }
    });
    let thinking = ThinkingView::new();
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    let header = adw::Bin::new();
    let notes = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    root.append(&header);
    root.append(&notes);
    root.append(&thinking.widget);
    root.append(&stack);

    let view = Rc::new(ObjectView {
        page: Rc::downgrade(page),
        item: Item::Object(index),
        key: object_key(&c.category, &c.key),
        category: c.category.clone(),
        name: c.key.clone(),
        root,
        header,
        notes,
        stack,
        thinking,
        busy: Cell::new(false),
        error: RefCell::new(None),
        showing: Cell::new(Final::Edit),
        pages: Cell::new(0),
    });

    if page.hooks.session.borrow().steps.contains_key(&view.key) {
        view.render(gtk::StackTransitionType::None);
    } else {
        let header = page.detail_header(&c.key, &format!("Object · {}", c.category));
        view.header.set_child(Some(&header.widget));
        let spinner = adw::Spinner::builder().width_request(32).height_request(32).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        view.push(&spinner.upcast(), gtk::StackTransitionType::None);
        view.load(page, index);
    }
    (view.root.clone().upcast(), view)
}

/// The badge on the version the game uses today.
fn in_game() -> (String, Option<&'static str>) {
    ("In game".into(), Some("success"))
}

/// Removes every page but the visible one.
fn drop_hidden(stack: &gtk::Stack) {
    let visible = stack.visible_child();
    let mut child = stack.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        if Some(&c) != visible.as_ref() {
            stack.remove(&c);
        }
    }
}

impl ObjectView {
    /// Reads every version off the main thread; finding the base game's
    /// version parses whole game folders.
    fn load(self: &Rc<Self>, page: &Rc<Page>, index: usize) {
        let analysis = &page.analysis;
        let c = &analysis.report.objects[index];
        let origins: Vec<(usize, String, String, PathBuf, String)> = c
            .definitions
            .iter()
            .filter_map(|d| Some((d.mod_position, analysis.ids[d.mod_position].clone(), analysis.name(d.mod_position).to_owned(), analysis.roots[d.mod_position].clone()?, d.file.clone())))
            .collect();
        let (category, key, game_dir) = (c.category.clone(), c.key.clone(), page.hooks.game_dir.clone());
        let existing = (page.hooks.existing)(&category, &key);
        let stale = page.hooks.stale_object(&category, &key).is_some();
        let view = self.clone();
        glib::spawn_future_local(async move {
            let for_load = origins.iter().map(|(_, id, name, root, file)| (id.clone(), name.clone(), root.clone(), file.clone())).collect();
            let loaded = gio::spawn_blocking(move || merge_dialog::load(&category, &key, game_dir.as_deref(), for_load)).await;
            let Some(page) = view.page.upgrade() else { return };
            let Ok(loaded) = loaded else {
                view.error.replace(Some("Could not read the mods' files".into()));
                view.header.set_child(Some(&page.detail_header(&view.name, &view.category).widget));
                view.show_error_only();
                return;
            };
            // Versions that were found keep their order, so match them up.
            let mut positions = Vec::new();
            let mut found = loaded.sources.iter().peekable();
            for (position, _, name, _, file) in &origins {
                if found.peek().is_some_and(|s| &s.label == name && &s.file == file) {
                    positions.push(*position);
                    found.next();
                }
            }
            let saved = existing.filter(|_| !stale).map(|m| m.script);
            page.hooks.session.borrow_mut().steps.entry(view.key.clone()).or_insert_with(|| Steps::new(loaded, positions, saved));
            view.render(gtk::StackTransitionType::Crossfade);
        });
    }

    fn with_steps<R>(&self, f: impl FnOnce(&mut Steps) -> R) -> Option<R> {
        let page = self.page.upgrade()?;
        let mut session = page.hooks.session.borrow_mut();
        session.steps.get_mut(&self.key).map(f)
    }

    /// Shows `widget` in place of the current cards.
    fn push(&self, widget: &gtk::Widget, transition: gtk::StackTransitionType) {
        self.pages.set(self.pages.get() + 1);
        self.stack.add_named(widget, Some(&format!("step-{}", self.pages.get())));
        self.stack.set_transition_type(transition);
        self.stack.set_visible_child(widget);
        if transition == gtk::StackTransitionType::None || !self.stack.is_mapped() {
            drop_hidden(&self.stack);
        }
    }

    fn show_error_only(&self) {
        let message = self.error.borrow().clone().unwrap_or_default();
        let status = adw::StatusPage::builder().icon_name("dialog-warning-symbolic").title("Nothing to Compare").description(message).build();
        self.push(&status.upcast(), gtk::StackTransitionType::None);
    }

    /// Rebuilds the header, notes and cards from the object's steps.
    fn render(self: &Rc<Self>, transition: gtk::StackTransitionType) {
        let Some(page) = self.page.upgrade() else { return };
        let hooks = page.hooks.clone();
        let session = hooks.session.borrow();
        let Some(steps) = session.steps.get(&self.key) else { return };
        let analysis = &page.analysis;
        let sources = &steps.loaded.sources;
        let vanilla = steps.loaded.vanilla.as_ref();
        let busy = self.busy.get();

        // Header.
        let c = match self.item {
            Item::Object(i) => &analysis.report.objects[i],
            _ => unreachable!(),
        };
        let n_mods = c.definitions.iter().map(|d| d.mod_position).collect::<std::collections::BTreeSet<_>>().len();
        let mut subtitle = format!("Object · {} · {}", self.category, plural(n_mods, "mod defines this", "mods define this"));
        if vanilla.is_some() {
            subtitle.push_str(", and the base game");
        }
        let header = page.detail_header(&self.name, &subtitle);
        let done = steps.done();
        for (i, s) in sources.iter().enumerate() {
            let class = if done || (i < steps.next && steps.next > 1) {
                "done"
            } else if i <= steps.next {
                "current"
            } else {
                "later"
            };
            if i > 0 {
                header.chips.insert(&arrow(), -1);
            }
            header.chips.insert(&chip(&s.label, Some(steps.positions[i]), class), -1);
        }
        header.chips.set_visible(sources.len() > 1);
        header.actions.append(&page.ignore_button(self.item));
        if !steps.history.is_empty() {
            let undo = gtk::Button::builder().icon_name("edit-undo-symbolic").tooltip_text("Undo Last Step").sensitive(!busy).css_classes(["flat"]).build();
            let weak = Rc::downgrade(self);
            undo.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.with_steps(Steps::undo);
                    view.render(gtk::StackTransitionType::SlideRight);
                }
            });
            header.actions.append(&undo);
        }
        if !steps.history.is_empty() || steps.from_saved {
            let over = gtk::Button::builder().label("Start Over").tooltip_text("Go through the versions again from the first").sensitive(!busy).css_classes(["flat"]).build();
            let weak = Rc::downgrade(self);
            over.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.with_steps(Steps::start_over);
                    view.showing.set(Final::Edit);
                    view.render(gtk::StackTransitionType::SlideRight);
                }
            });
            header.actions.append(&over);
        }
        if !done && sources.len() > 1 {
            let merge = gtk::Button::builder().label("Merge with AI").css_classes(["suggested-action"]).sensitive(!busy).build();
            merge.set_tooltip_text(Some(&match (hooks.provider)() {
                Some(p) => format!("Ask {} to combine these two versions", p.model_name()),
                None => "Set up an AI provider in Preferences first".to_owned(),
            }));
            if busy {
                merge.set_child(Some(&adw::Spinner::new()));
            }
            let weak = Rc::downgrade(self);
            merge.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.merge_with_ai();
                }
            });
            header.actions.append(&merge);
        }
        self.header.set_child(Some(&header.widget));

        // Notes.
        while let Some(child) = self.notes.first_child() {
            self.notes.remove(&child);
        }
        if let Some((w, reason)) = c.winner().filter(|_| !(hooks.is_merged)(&self.category, &self.name)) {
            let name = analysis.name(c.definitions[w].mod_position);
            self.notes.append(&note(&format!("In game now, {name}'s version is used: {}.", reason.explain(conflicts::is_fios(&self.category))), None));
        }
        if !steps.loaded.missing.is_empty() {
            self.notes.append(&note(&format!("Couldn't find the definition in: {}", steps.loaded.missing.join(", ")), Some("warning")));
        }
        if let Some(reasons) = hooks.stale_object(&self.category, &self.name) {
            self.notes.append(&note(&format!("The saved merge is out of date: {}. Saving a new one replaces it.", reasons.join("; ")), Some("warning")));
        }
        if steps.from_saved {
            let sources = (hooks.existing)(&self.category, &self.name).map(|m| m.sources.join(", ")).unwrap_or_default();
            self.notes.append(&note(&format!("This is the saved merge of {sources}. Edit and save it again, or start over to go through the versions again."), None));
        }
        let suggestion = (!done && sources.len() > 1).then(|| trivial::resolve(vanilla, &[steps.left_source(), steps.right_source()])).flatten();
        if let Some(s) = &suggestion {
            let which = if s.index == 0 { if steps.left_is_first() { sources[0].label.as_str() } else { "the result so far" } } else { steps.right().label.as_str() };
            self.notes.append(&note(&format!("{}, so {which} keeps every change. No AI needed.", s.reason), Some("accent")));
        }
        if !steps.notes.is_empty() {
            self.notes.append(&note(&format!("Notes from the model: {}", steps.notes), None));
        }
        if let Some(error) = &*self.error.borrow() {
            self.notes.append(&note(error, Some("error")));
        }

        let body = if sources.is_empty() {
            let status = adw::StatusPage::builder().icon_name("dialog-warning-symbolic").title("Nothing to Compare").description("The definition wasn't found in any mod's file. Rescan, in case the mods changed.").build();
            status.upcast()
        } else if done {
            self.final_body(&page, steps)
        } else {
            self.pair_body(&page, steps, suggestion.map(|s| s.index))
        };
        drop(session);
        self.push(&body, transition);
        page.update_status(self.item);
    }

    /// Two versions side by side, with buttons to keep either one.
    fn pair_body(self: &Rc<Self>, page: &Rc<Page>, steps: &Steps, suggested: Option<usize>) -> gtk::Widget {
        let analysis = &page.analysis;
        let busy = self.busy.get();
        let pair = Pair::new(true);
        let (left, right) = (steps.left_source(), steps.right_source());
        let right_edited = steps.right_edit.is_some();
        let right_position = steps.positions[steps.next];
        let winner = match self.item {
            Item::Object(i) => steps.winner(&analysis.report.objects[i]),
            _ => None,
        };
        let right_wins = winner == Some(steps.next);

        let left_button = gtk::Button::builder().sensitive(!busy).build();
        let left_header = if steps.left_is_first() {
            left_button.set_label("Use This Version");
            left_button.set_tooltip_text(Some(&format!("Keep {}'s version and leave out {}'s changes", left.label, right.label)));
            Header { title: left.label.clone(), subtitle: analysis.origin(steps.positions[0], &left.file), color: Some(steps.positions[0]), badge: (winner == Some(0)).then(in_game) }
        } else {
            left_button.set_label("Keep This");
            left_button.set_tooltip_text(Some(&format!("Keep the result so far and leave out {}'s changes", right.label)));
            // Merged only once it holds more than one mod's changes.
            let badge = match (steps.edited, steps.from.len()) {
                (true, _) => ("Edited", Some("accent")),
                (false, 1) => ("Kept", None),
                _ => ("Merged", Some("success")),
            };
            Header { title: "Result so far".into(), subtitle: steps.from.join(" + "), color: None, badge: Some((badge.0.into(), badge.1)) }
        };
        let right_button = gtk::Button::builder().label("Use This Version").tooltip_text(format!("Use {}'s version in place of the one on the left", right.label)).sensitive(!busy).build();
        match suggested {
            Some(0) => left_button.add_css_class("suggested-action"),
            Some(_) => right_button.add_css_class("suggested-action"),
            None => {}
        }
        pair.left.set_header(&left_header);
        pair.left.set_actions(&[left_button.clone().upcast()]);
        let (right_title, right_origin) = (right.label.clone(), analysis.origin(right_position, &right.file));
        let right_header = move |edited: bool| Header {
            title: right_title.clone(),
            subtitle: right_origin.clone(),
            color: Some(right_position),
            badge: if edited { Some(("Edited".into(), Some("accent"))) } else { right_wins.then(in_game) },
        };
        pair.right.set_header(&right_header(right_edited));
        let revert = gtk::Button::builder().icon_name("document-revert-symbolic").tooltip_text("Undo your edits to this version").sensitive(right_edited && !busy).css_classes(["flat"]).build();
        {
            let weak = Rc::downgrade(self);
            revert.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.with_steps(|s| s.right_edit = None);
                    view.render(gtk::StackTransitionType::None);
                }
            });
        }
        pair.right.set_actions(&[revert.clone().upcast(), right_button.clone().upcast()]);
        pair.right.code.set_editable(!busy);
        {
            // Edits are kept with the step, so they survive re-rendering.
            let (weak, card, revert) = (Rc::downgrade(self), Rc::downgrade(&pair.right), revert.downgrade());
            pair.connect_edit(move |text| {
                let Some(view) = weak.upgrade() else { return };
                let edited = view.with_steps(|s| {
                    s.edit_right(text);
                    s.right_edit.is_some()
                });
                let edited = edited.unwrap_or(false);
                if let Some(card) = card.upgrade() {
                    card.set_header(&right_header(edited));
                }
                if let Some(revert) = revert.upgrade() {
                    revert.set_sensitive(edited);
                }
            });
        }
        for (button, keep_left) in [(&left_button, true), (&right_button, false)] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.error.replace(None);
                    view.with_steps(|s| if keep_left { s.keep_left() } else { s.use_right() });
                    view.render(gtk::StackTransitionType::SlideLeft);
                }
            });
        }

        let bar = diff_bar(&pair);
        let take = gtk::Button::builder().label("Take Change").tooltip_text("Copy the right card's side of the highlighted difference into the left card").sensitive(false).css_classes(["flat"]).build();
        {
            let take_ref = take.downgrade();
            pair.connect_change(move |_, current, _| {
                if let Some(t) = take_ref.upgrade() {
                    t.set_sensitive(current.is_some() && !busy);
                }
            });
            let (weak, pair_ref) = (Rc::downgrade(self), Rc::downgrade(&pair));
            take.connect_clicked(move |_| {
                let (Some(view), Some(pair)) = (weak.upgrade(), pair_ref.upgrade()) else { return };
                if let Some(text) = pair.take_current() {
                    view.with_steps(|s| s.take(text));
                    view.render(gtk::StackTransitionType::None);
                }
            });
        }
        bar.extra.append(&take);
        self.view_options(page, &bar.extra, steps.loaded.vanilla.is_some());

        let session = page.hooks.session.borrow();
        let compare = match (&steps.loaded.vanilla, session.compare_base) {
            (Some(v), true) => Compare::Base(v.text.clone()),
            _ => Compare::Each,
        };
        pair.show(&left.text, &steps.right_source().text, &compare, session.ignore_whitespace);
        drop(session);
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        body.append(&bar.widget);
        body.append(&pair.widget);
        // The pair lives as long as its widgets.
        keep_alive(&body, pair);
        body.upcast()
    }

    /// Toggles for comparing with the base game and ignoring spacing.
    fn view_options(self: &Rc<Self>, page: &Rc<Page>, into: &gtk::Box, has_base: bool) {
        let rerender = {
            let weak = Rc::downgrade(self);
            move || {
                if let Some(view) = weak.upgrade() {
                    view.render(gtk::StackTransitionType::None);
                }
            }
        };
        if has_base {
            into.append(&option_toggle(&page.hooks, "Base Game", "Mark what each version changed from the base game, instead of the differences between the two", |s| s.compare_base, |s, v| s.compare_base = v, rerender.clone()));
        }
        into.append(&option_toggle(&page.hooks, "Ignore Spacing", "Don't count lines that only differ in spacing or indentation", |s| s.ignore_whitespace, |s, v| s.ignore_whitespace = v, rerender));
    }

    /// The final step: the result in an editor, or compared with a version.
    fn final_body(self: &Rc<Self>, page: &Rc<Page>, steps: &Steps) -> gtk::Widget {
        let hooks = &page.hooks;
        let analysis = &page.analysis;
        let text = steps.final_text();
        let saved = (hooks.is_merged)(&self.category, &self.name) && hooks.stale_object(&self.category, &self.name).is_none() && steps.from_saved;

        // What to show: the editor, or the result next to another version.
        let mut choices = vec![(Final::Edit, "Edit the Result".to_owned())];
        if steps.loaded.vanilla.is_some() {
            choices.push((Final::Base, "Compare with the Base Game".to_owned()));
        }
        for (i, s) in steps.loaded.sources.iter().enumerate() {
            choices.push((Final::Source(i), format!("Compare with {}", s.label)));
        }
        let labels: Vec<&str> = choices.iter().map(|(_, l)| l.as_str()).collect();
        let dropdown = gtk::DropDown::from_strings(&labels);
        let showing = choices.iter().position(|(c, _)| *c == self.showing.get()).unwrap_or(0);
        dropdown.set_selected(showing as u32);
        {
            let weak = Rc::downgrade(self);
            let choices: Vec<Final> = choices.iter().map(|(c, _)| *c).collect();
            dropdown.connect_selected_notify(move |d| {
                if let Some(view) = weak.upgrade() {
                    view.showing.set(choices[d.selected() as usize]);
                    // After the dropdown's popover closes, since this
                    // replaces the dropdown.
                    glib::idle_add_local_once(move || view.render(gtk::StackTransitionType::Crossfade));
                }
            });
        }
        let toolbar = gtk::Box::builder().spacing(6).build();
        toolbar.append(&dropdown);

        let result_header = Header {
            title: "Merged Result".into(),
            subtitle: if steps.from_saved { "Saved merge".into() } else { steps.from.join(" + ") },
            color: None,
            badge: Some(if saved { ("Saved".into(), Some("success")) } else { ("Not saved yet".into(), None) }),
        };
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        body.append(&toolbar);
        match choices[showing].0 {
            Final::Edit => {
                let card = Card::new(true);
                card.set_header(&result_header);
                let save = gtk::Button::builder().label("Save to Patch").css_classes(["suggested-action"]).tooltip_text(format!("Save to {}", crate::merge::patch::NAME)).build();
                card.set_actions(&[save.clone().upcast()]);
                card.set_text(&text);
                {
                    let weak = Rc::downgrade(self);
                    card.code.buffer().connect_changed(move |buffer| {
                        if let Some(view) = weak.upgrade() {
                            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
                            view.with_steps(|s| s.final_text = Some(text));
                        }
                    });
                }
                let (weak, card_ref) = (Rc::downgrade(self), Rc::downgrade(&card));
                save.connect_clicked(move |_| {
                    if let (Some(view), Some(card)) = (weak.upgrade(), card_ref.upgrade()) {
                        view.save(&card.text());
                    }
                });
                body.append(&card.widget);
                keep_alive(&body, card);
            }
            other => {
                let theirs = match other {
                    Final::Base => steps.loaded.vanilla.clone().unwrap(),
                    Final::Source(i) => steps.loaded.sources[i].clone(),
                    Final::Edit => unreachable!(),
                };
                let pair = Pair::new(false);
                pair.left.set_header(&result_header);
                pair.right.set_header(&match other {
                    Final::Source(i) => Header {
                        title: theirs.label.clone(),
                        subtitle: analysis.origin(steps.positions[i], &theirs.file),
                        color: Some(steps.positions[i]),
                        badge: match self.item {
                            Item::Object(o) => (steps.winner(&analysis.report.objects[o]) == Some(i)).then(in_game),
                            _ => None,
                        },
                    },
                    _ => Header { title: "Base Game".into(), subtitle: theirs.file.clone(), color: None, badge: None },
                });
                let bar = diff_bar(&pair);
                let take = gtk::Button::builder().label("Take Change").tooltip_text("Copy the right card's side of the highlighted difference into the result").sensitive(false).css_classes(["flat"]).build();
                let take_ref = take.downgrade();
                pair.connect_change(move |_, current, _| {
                    if let Some(t) = take_ref.upgrade() {
                        t.set_sensitive(current.is_some());
                    }
                });
                let (weak, pair_ref) = (Rc::downgrade(self), Rc::downgrade(&pair));
                take.connect_clicked(move |_| {
                    let (Some(view), Some(pair)) = (weak.upgrade(), pair_ref.upgrade()) else { return };
                    if let Some(text) = pair.take_current() {
                        view.with_steps(|s| s.take_final(text));
                        view.render(gtk::StackTransitionType::None);
                    }
                });
                bar.extra.append(&take);
                let whitespace = page.hooks.session.borrow().ignore_whitespace;
                pair.show(&text, &theirs.text, &crate::diff_view::Compare::Each, whitespace);
                toolbar.append(&bar.widget);
                bar.widget.set_hexpand(true);
                body.append(&pair.widget);
                keep_alive(&body, pair);
            }
        }
        body.upcast()
    }

    fn merge_with_ai(self: &Rc<Self>) {
        let Some(page) = self.page.upgrade() else { return };
        let hooks = page.hooks.clone();
        let Some(provider) = (hooks.provider)() else {
            (hooks.toast)("Set up an AI provider in Preferences first");
            (hooks.open_preferences)();
            return;
        };
        let Some((vanilla, sources)) = self.with_steps(|s| (s.loaded.vanilla.clone(), vec![s.left_source(), s.right_source()])) else { return };
        self.busy.set(true);
        self.error.replace(None);
        self.render(gtk::StackTransitionType::None);
        let (category, key, view) = (self.category.clone(), self.name.clone(), self.clone());
        glib::spawn_future_local(async move {
            let result = thinking_view::run(&view.thinking, None, move |on| ai::merge(&provider, &category, &key, vanilla.as_ref(), &sources, on)).await;
            view.busy.set(false);
            match result {
                Some(Ok(merged)) => {
                    view.with_steps(|s| s.merged(merged.script, merged.notes));
                    view.render(gtk::StackTransitionType::SlideLeft);
                }
                Some(Err(message)) => {
                    view.error.replace(Some(message));
                    view.render(gtk::StackTransitionType::None);
                }
                None => {
                    view.error.replace(Some("The merge failed unexpectedly".into()));
                    view.render(gtk::StackTransitionType::None);
                }
            }
        });
    }

    fn save(self: &Rc<Self>, text: &str) {
        let Some(page) = self.page.upgrade() else { return };
        let script = match merge_dialog::validate(&self.category, &self.name, text) {
            Ok(script) => script,
            Err(message) => {
                self.error.replace(Some(message));
                self.render(gtk::StackTransitionType::None);
                return;
            }
        };
        let prints = self.with_steps(|s| s.loaded.prints.clone()).unwrap_or_default();
        match (page.hooks.save_object)(&self.category, &self.name, &script, prints) {
            Ok(()) => {
                page.hooks.session.borrow_mut().steps.remove(&self.key);
                page.hooks.saved();
                page.update_status(self.item);
                page.advance(self.item);
            }
            Err(message) => {
                self.error.replace(Some(message));
                self.render(gtk::StackTransitionType::None);
            }
        }
    }
}

/// Ties `value`'s lifetime to `widget`'s.
pub(super) fn keep_alive<T: 'static>(widget: &impl IsA<gtk::Widget>, value: Rc<T>) {
    let cell = RefCell::new(Some(value));
    widget.connect_destroy(move |_| drop(cell.take()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(label: &str, text: &str) -> Source {
        Source { label: label.into(), file: format!("common/buildings/{label}.txt"), text: text.into() }
    }

    fn steps(texts: &[&str]) -> Steps {
        let sources: Vec<Source> = texts.iter().enumerate().map(|(i, t)| source(&format!("M{i}"), t)).collect();
        let loaded = Loaded { vanilla: None, sources, missing: Vec::new(), prints: Vec::new() };
        Steps::new(loaded, (0..texts.len()).collect(), None)
    }

    #[test]
    fn steps_through_each_version() {
        let mut s = steps(&["a", "b", "c"]);
        assert_eq!((s.total(), s.next, s.done(), s.progress()), (2, 1, false, None));
        assert!(s.left_is_first());
        s.use_right();
        assert_eq!((s.result.as_str(), s.next, s.progress()), ("b", 2, Some((2, 2))));
        assert_eq!(s.left_source().label, "Result so far (M1)");
        s.merged("bc".into(), "combined".into());
        assert!(s.done());
        assert_eq!(s.progress(), Some((3, 2)), "past the last step: ready to save");
        assert_eq!(s.final_text(), "bc");
        assert_eq!(s.from, ["M1", "M2"]);
        s.undo();
        assert_eq!((s.result.as_str(), s.next, s.notes.as_str()), ("b", 2, ""));
        s.start_over();
        assert_eq!((s.result.as_str(), s.next, s.history.len()), ("a", 1, 0));
    }

    #[test]
    fn taking_a_change_edits_the_left_version() {
        let mut s = steps(&["a", "b"]);
        s.take("a2".into());
        assert!(!s.left_is_first());
        assert_eq!((s.result.as_str(), s.next, s.from.clone()), ("a2", 1, vec!["M0".to_owned(), "M1".to_owned()]));
        s.keep_left();
        assert!(s.done());
        assert_eq!(s.final_text(), "a2");
    }

    #[test]
    fn edits_to_the_right_version_are_used_and_undone() {
        let mut s = steps(&["a", "b\nc", "d"]);
        s.edit_right("b\nc\n");
        assert_eq!(s.right_edit, None, "the same text, give or take a newline, isn't an edit");
        s.edit_right("b\nC");
        assert_eq!(s.right_source().text, "b\nC");
        assert_eq!(s.right_source().label, "M1");
        s.use_right();
        assert_eq!((s.result.as_str(), s.edited, s.right_edit.clone()), ("b\nC", true, None));
        // The next pair starts from the mod's own version again.
        assert_eq!(s.right_source().text, "d");
        s.undo();
        assert_eq!((s.next, s.right_edit.as_deref()), (1, Some("b\nC")), "undo brings the edit back");
        s.keep_left();
        assert_eq!((s.result.as_str(), s.right_edit.clone()), ("a", None));
    }

    #[test]
    fn a_saved_merge_opens_at_the_end() {
        let sources = vec![source("A", "a"), source("B", "b")];
        let loaded = Loaded { vanilla: None, sources, missing: Vec::new(), prints: Vec::new() };
        let s = Steps::new(loaded, vec![0, 1], Some("saved".into()));
        assert!(s.done());
        assert_eq!((s.final_text(), s.progress()), ("saved".to_owned(), None));
    }
}
