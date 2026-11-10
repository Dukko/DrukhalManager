//! Comparing the copies of a file that several mods ship. The game only
//! uses the last one; chips pick which two copies the cards show.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};

use super::object::keep_alive;
use super::{Item, Page, diff_bar, file_folder, file_name, merge_controls, note, option_toggle, plural};
use crate::diff_view::{Compare, Header, Pair};
use crate::merge::file_merge;
use crate::merge::patch;

/// Copies bigger than this aren't shown.
const MAX_SIZE: usize = 4 << 20;

/// One copy of the file.
struct Version {
    label: String,
    /// The mod's scanned position; `None` for the base game.
    position: Option<usize>,
    /// The text, or why it can't be shown.
    text: Result<String, &'static str>,
}

pub struct FileView {
    page: Weak<Page>,
    index: usize,
    item: Item,
    root: gtk::Box,
    header: adw::Bin,
    notes: gtk::Box,
    body: adw::Bin,
    versions: RefCell<Option<Vec<Version>>>,
    /// The version on the right card; the one before it is on the left.
    right: Cell<usize>,
    error: RefCell<Option<String>>,
}

pub fn view(page: &Rc<Page>, index: usize) -> (gtk::Widget, Rc<dyn std::any::Any>) {
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    let header = adw::Bin::new();
    let notes = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    let body = adw::Bin::builder().vexpand(true).build();
    root.append(&header);
    root.append(&notes);
    root.append(&body);
    let view = Rc::new(FileView { page: Rc::downgrade(page), index, item: Item::File(index), root, header, notes, body, versions: RefCell::new(None), right: Cell::new(0), error: RefCell::new(None) });
    view.render();
    view.body.set_child(Some(&adw::Spinner::builder().width_request(32).height_request(32).halign(gtk::Align::Center).valign(gtk::Align::Center).build()));
    view.load(page);
    (view.root.clone().upcast(), view)
}

impl FileView {
    fn load(self: &Rc<Self>, page: &Rc<Page>) {
        let analysis = &page.analysis;
        let c = &analysis.report.files[self.index];
        let path = c.path.clone();
        let mods: Vec<(String, Option<usize>, Option<PathBuf>)> = c.mods.iter().map(|&m| (analysis.name(m).to_owned(), Some(m), analysis.roots[m].clone())).collect();
        let game_dir = page.hooks.game_dir.clone();
        let view = self.clone();
        glib::spawn_future_local(async move {
            let versions = gio::spawn_blocking(move || {
                let read = |dir: &std::path::Path| -> Result<String, &'static str> {
                    let bytes = std::fs::read(dir.join(&path)).map_err(|_| "Could not read this copy")?;
                    if bytes.len() > MAX_SIZE {
                        return Err("Too big to show");
                    }
                    if bytes[..bytes.len().min(8192)].contains(&0) {
                        return Err("Not a text file");
                    }
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                };
                let mut versions = Vec::new();
                if let Some(dir) = &game_dir
                    && dir.join(&path).is_file()
                {
                    versions.push(Version { label: "Base Game".into(), position: None, text: read(dir) });
                }
                for (label, position, root) in mods {
                    let text = root.as_deref().map_or(Err("The mod's files weren't found"), read);
                    versions.push(Version { label, position, text });
                }
                versions
            })
            .await
            .unwrap_or_default();
            view.right.set(versions.len().saturating_sub(1));
            view.versions.replace(Some(versions));
            view.render();
        });
    }

    fn render(self: &Rc<Self>) {
        let Some(page) = self.page.upgrade() else { return };
        let hooks = &page.hooks;
        let analysis = &page.analysis;
        let c = &analysis.report.files[self.index];
        let winner = *c.mods.last().unwrap();
        let folder = file_folder(&c.path);
        let subtitle = format!("Overridden file · {} · {} · {} wins", if folder.is_empty() { "top level" } else { folder }, plural(c.mods.len(), "mod ships it", "mods ship it"), analysis.name(winner));
        let header = page.detail_header(file_name(&c.path), &subtitle);
        header.actions.append(&page.ignore_button(self.item));
        let merged = (hooks.is_file_merged)(&c.path);
        let stale = hooks.stale_file(&c.path);
        if file_merge::mergeable(&c.path) {
            let (_, button) = merge_controls(merged, stale.as_deref());
            button.set_css_classes(&["suggested-action"]);
            button.set_tooltip_text(Some("Combine the copies object by object, using AI only where they disagree"));
            let weak = Rc::downgrade(self);
            let index = self.index;
            button.connect_clicked(move |_| {
                let Some(view) = weak.upgrade() else { return };
                let Some(page) = view.page.upgrade() else { return };
                let weak = Rc::downgrade(&view);
                (page.hooks.open_file)(
                    index,
                    Box::new(move || {
                        if let Some(view) = weak.upgrade()
                            && let Some(page) = view.page.upgrade()
                        {
                            page.hooks.stale.replace((page.hooks.reload_stale)());
                            page.update_status(view.item);
                            view.render();
                        }
                    }),
                );
            });
            header.actions.append(&button);
        }

        while let Some(child) = self.notes.first_child() {
            self.notes.remove(&child);
        }
        self.notes.append(&note(&format!("The game only uses the copy from {}, the mod loaded last. The others' changes to this file are lost.", analysis.name(winner)), None));
        match (&stale, merged) {
            (Some(reasons), _) => self.notes.append(&note(&format!("The merged patch's copy is out of date: {}.", reasons.join("; ")), Some("warning"))),
            (None, true) => self.notes.append(&note("The merged patch holds its own copy, which is used instead of all of these.", Some("success"))),
            _ => {}
        }
        if let Some(error) = &*self.error.borrow() {
            self.notes.append(&note(error, Some("error")));
        }

        let versions = self.versions.borrow();
        if let Some(versions) = &*versions
            && versions.len() >= 2
        {
            let right = self.right.get().clamp(1, versions.len() - 1);
            let left = right - 1;
            for (i, v) in versions.iter().enumerate() {
                if i > 0 {
                    header.chips.insert(&super::arrow(), -1);
                }
                let class = if i == left || i == right { "current" } else { "later" };
                let button = gtk::Button::builder().child(&super::chip(&v.label, v.position, class)).css_classes(["flat", "chip-button"]).tooltip_text(format!("Compare {} with the copy before it", v.label)).build();
                let weak = Rc::downgrade(self);
                button.connect_clicked(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.right.set(i.max(1));
                        view.render();
                    }
                });
                header.chips.insert(&button, -1);
            }
            self.body.set_child(Some(&self.cards(&page, &versions[left], &versions[right], versions.first().filter(|v| v.position.is_none()))));
        } else if versions.is_some() {
            self.body.set_child(Some(&adw::StatusPage::builder().icon_name("dialog-warning-symbolic").title("Nothing to Compare").description("The mods' copies of this file weren't found. Rescan, in case the mods changed.").build()));
        }
        self.header.set_child(Some(&header.widget));
    }

    fn cards(self: &Rc<Self>, page: &Rc<Page>, left: &Version, right: &Version, base: Option<&Version>) -> gtk::Widget {
        let (Ok(left_text), Ok(right_text)) = (&left.text, &right.text) else {
            let reason = left.text.as_ref().err().or(right.text.as_ref().err()).copied().unwrap_or_default();
            let lines: Vec<String> = [left, right].iter().map(|v| format!("{}: {}", v.label, v.text.as_ref().err().copied().unwrap_or("text"))).collect();
            return adw::StatusPage::builder().icon_name("text-x-generic-symbolic").title(reason).description(format!("These copies can't be shown side by side.\n{}", lines.join("\n"))).build().upcast();
        };
        let analysis = &page.analysis;
        let c = &analysis.report.files[self.index];
        let winner = *c.mods.last().unwrap();
        let pair = Pair::new(false);
        for (card, v) in [(&pair.left, left), (&pair.right, right)] {
            let header = match v.position {
                None => Header { title: v.label.clone(), subtitle: "The game's own copy".into(), color: None, badge: None },
                Some(p) => Header {
                    title: v.label.clone(),
                    subtitle: analysis.origin(p, &c.path),
                    color: Some(p),
                    badge: Some(if p == winner { ("Used in game".into(), Some("success")) } else { ("Overridden".into(), None) }),
                },
            };
            card.set_header(&header);
            if let Some(p) = v.position.filter(|&p| p != winner) {
                let use_it = gtk::Button::builder().label("Use This Version").tooltip_text(format!("Save this copy to {}, so it's used instead of {}'s", patch::NAME, analysis.name(winner))).build();
                let (weak, text) = (Rc::downgrade(self), v.text.clone().unwrap_or_default());
                use_it.connect_clicked(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.use_version(p, &text);
                    }
                });
                card.set_actions(&[use_it.upcast()]);
            }
        }
        let bar = diff_bar(&pair);
        let has_base = base.is_some_and(|b| b.text.is_ok()) && left.position.is_some();
        let rerender = {
            let weak = Rc::downgrade(self);
            move || {
                if let Some(view) = weak.upgrade() {
                    view.render();
                }
            }
        };
        if has_base {
            bar.extra.append(&option_toggle(&page.hooks, "Base Game", "Mark what each copy changed from the base game, instead of the differences between the two", |s| s.compare_base, |s, v| s.compare_base = v, rerender.clone()));
        }
        bar.extra.append(&option_toggle(&page.hooks, "Ignore Spacing", "Don't count lines that only differ in spacing or indentation", |s| s.ignore_whitespace, |s, v| s.ignore_whitespace = v, rerender));
        let session = page.hooks.session.borrow();
        let compare = match base.and_then(|b| b.text.as_ref().ok()) {
            Some(b) if has_base && session.compare_base => Compare::Base(b.clone()),
            _ => Compare::Each,
        };
        pair.show(left_text, right_text, &compare, session.ignore_whitespace);
        drop(session);
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        body.append(&bar.widget);
        body.append(&pair.widget);
        keep_alive(&body, pair);
        body.upcast()
    }

    /// Saves one mod's copy to the patch, so it's used instead of the last
    /// mod's.
    fn use_version(self: &Rc<Self>, position: usize, text: &str) {
        let Some(page) = self.page.upgrade() else { return };
        let analysis = &page.analysis;
        let c = &analysis.report.files[self.index];
        let mods: Vec<patch::SourceMod> = c.mods.iter().filter_map(|&m| Some((analysis.ids[m].clone(), analysis.name(m).to_owned(), analysis.roots[m].clone()?))).collect();
        let prints = patch::file_prints(&c.path, page.hooks.game_dir.as_deref(), &mods);
        match (page.hooks.save_file)(&c.path, text, prints) {
            Ok(()) => {
                self.error.replace(None);
                page.hooks.saved();
                (page.hooks.toast)(&format!("Using {}'s copy of {}", analysis.name(position), file_name(&c.path)));
                page.update_status(self.item);
                self.render();
            }
            Err(message) => {
                self.error.replace(Some(message));
                self.render();
            }
        }
    }
}
