//! The Conflicts page: a read-only view of a conflict report.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

use crate::merge::conflicts::Report;
use crate::merge::file_merge;

/// Rows shown per folder; the rest are summarised so huge folders (e.g.
/// localisation) stay responsive.
const ROWS_PER_GROUP: usize = 300;

/// A finished scan of a particular load order.
pub struct Analysis {
    /// The load order revision this describes. Bumped in place for changes
    /// that don't affect the scan, like enabling the merged patch.
    pub revision: Cell<u64>,
    /// Mod names by scanned position, as of the scan.
    pub names: Vec<String>,
    /// Each scanned mod's folder.
    pub roots: Vec<Option<PathBuf>>,
    /// Each scanned mod's position in the full load order (the merged patch
    /// itself is not scanned, so these can differ).
    pub load_positions: Vec<usize>,
    pub report: Report,
}

/// What the page needs from the window to offer merging.
pub struct MergeHooks {
    /// Whether the patch already holds a merge of `(category, key)`.
    pub is_merged: Box<dyn Fn(&str, &str) -> bool>,
    /// Opens the merge dialog for an object conflict; the callback marks the
    /// row as merged once it's saved.
    pub open: Box<dyn Fn(usize, Box<dyn Fn()>)>,
    /// Whether the patch already holds a merged copy of a file.
    pub is_file_merged: Box<dyn Fn(&str) -> bool>,
    /// Opens the file merge dialog for a file override.
    pub open_file: Box<dyn Fn(usize, Box<dyn Fn()>)>,
}

impl Analysis {
    pub fn name(&self, position: usize) -> &str {
        &self.names[position]
    }
}

pub fn scanning() -> gtk::Widget {
    let page = adw::StatusPage::builder().title("Scanning Mods…").description("Reading every enabled mod's files").build();
    page.set_paintable(Some(&adw::SpinnerPaintable::new(Some(&page))));
    page.upcast()
}

pub fn build(analysis: &Rc<Analysis>, rescan: gtk::Button, hooks: Rc<MergeHooks>) -> gtk::Widget {
    let report = &analysis.report;
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);

    let involved: BTreeSet<usize> = report.files.iter().flat_map(|c| c.mods.iter().copied()).chain(report.objects.iter().flat_map(|c| c.definitions.iter().map(|d| d.mod_position))).collect();
    let summary = if report.files.is_empty() && report.objects.is_empty() {
        "No conflicts between enabled mods".to_owned()
    } else {
        format!(
            "{} · {} · {} of {} mods involved",
            plural(report.files.len(), "file override", "file overrides"),
            plural(report.objects.len(), "object conflict", "object conflicts"),
            involved.len(),
            analysis.names.len()
        )
    };
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    top.append(&gtk::Label::builder().label(summary).xalign(0.0).hexpand(true).wrap(true).css_classes(["dim-label"]).build());
    rescan.set_valign(gtk::Align::Center);
    top.append(&rescan);
    page.append(&top);

    if report.files.is_empty() && report.objects.is_empty() && report.errors.is_empty() && report.skipped.is_empty() {
        let empty = adw::StatusPage::builder().icon_name("emblem-ok-symbolic").title("No Conflicts").description("None of the enabled mods change the same files or objects").vexpand(true).build();
        page.append(&empty);
        return page.upcast();
    }

    if !report.files.is_empty() {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, c) in report.files.iter().enumerate() {
            let folder = c.path.rsplit_once('/').map_or("", |(dir, _)| dir);
            groups.entry(folder.to_owned()).or_default().push(i);
        }
        let make: Rc<dyn Fn(usize) -> adw::ActionRow> = {
            let analysis = analysis.clone();
            let hooks = hooks.clone();
            Rc::new(move |i| file_row(&analysis, i, &hooks))
        };
        section(&page, "Overridden Files", "Only the copy from the mod loaded last is used. Merge a script or localisation file to keep every mod's objects.", groups, make);
    }
    if !report.objects.is_empty() {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, c) in report.objects.iter().enumerate() {
            groups.entry(c.category.clone()).or_default().push(i);
        }
        let make: Rc<dyn Fn(usize) -> adw::ActionRow> = {
            let analysis = analysis.clone();
            Rc::new(move |i| object_row(&analysis, i, &hooks))
        };
        section(&page, "Conflicting Objects", "Defined by several mods; which one the game keeps depends on the folder. Merge one to keep every mod's changes.", groups, make);
    }

    if !report.errors.is_empty() || !report.skipped.is_empty() {
        page.append(&heading("Not Scanned", "These files or mods were left out of the report"));
        let list = boxed_list();
        for (position, reason) in &report.skipped {
            list.append(&row(analysis.name(*position), reason));
        }
        for e in &report.errors {
            let r = row(&e.file, &format!("{} · {}", analysis.name(e.mod_position), e.message));
            r.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
            list.append(&r);
        }
        page.append(&list);
    }
    page.upcast()
}

/// A heading plus one collapsed row per folder. Each folder's rows are only
/// built when it is first expanded.
fn section(page: &gtk::Box, title: &str, description: &str, groups: BTreeMap<String, Vec<usize>>, make: Rc<dyn Fn(usize) -> adw::ActionRow>) {
    page.append(&heading(title, description));
    let list = boxed_list();
    for (folder, items) in groups {
        let expander = adw::ExpanderRow::builder()
            .title(if folder.is_empty() { "(top level)" } else { folder.as_str() })
            .subtitle(plural(items.len(), "conflict", "conflicts"))
            .use_markup(false)
            .build();
        let filled = Cell::new(false);
        let make = make.clone();
        expander.connect_expanded_notify(move |expander| {
            if !expander.is_expanded() || filled.replace(true) {
                return;
            }
            for &i in items.iter().take(ROWS_PER_GROUP) {
                expander.add_row(&make(i));
            }
            if items.len() > ROWS_PER_GROUP {
                let more = row(&format!("…and {} more", items.len() - ROWS_PER_GROUP), "");
                more.add_css_class("dim-label");
                expander.add_row(&more);
            }
        });
        list.append(&expander);
    }
    page.append(&list);
}

fn file_row(analysis: &Analysis, index: usize, hooks: &Rc<MergeHooks>) -> adw::ActionRow {
    let c = &analysis.report.files[index];
    let (winner, losers) = c.mods.split_last().unwrap();
    let file = c.path.rsplit_once('/').map_or(c.path.as_str(), |(_, f)| f);
    let overridden: Vec<&str> = losers.iter().map(|&m| analysis.name(m)).collect();
    let r = row(file, &format!("Used: {} · Overridden: {}", analysis.name(*winner), overridden.join(", ")));
    r.set_tooltip_text(Some(&c.path));
    if file_merge::mergeable(&c.path) {
        let is_merged = (hooks.is_file_merged)(&c.path);
        let (merged, button) = merge_controls(is_merged);
        let mark_merged = merged_marker(&merged, &button);
        let hooks = hooks.clone();
        button.connect_clicked(move |_| (hooks.open_file)(index, mark_merged()));
        r.add_suffix(&merged);
        r.add_suffix(&button);
    }
    r
}

fn merge_controls(is_merged: bool) -> (gtk::Image, gtk::Button) {
    let merged = gtk::Image::builder().icon_name("emblem-ok-symbolic").tooltip_text("Merged into the patch").css_classes(["success"]).visible(is_merged).build();
    let button = gtk::Button::builder().label(if is_merged { "Edit Merge…" } else { "Merge…" }).valign(gtk::Align::Center).css_classes(["flat"]).build();
    (merged, button)
}

fn object_row(analysis: &Analysis, index: usize, hooks: &Rc<MergeHooks>) -> adw::ActionRow {
    let c = &analysis.report.objects[index];
    let mut mods: Vec<usize> = c.definitions.iter().map(|d| d.mod_position).collect();
    mods.dedup();
    let names: Vec<&str> = mods.iter().map(|&m| analysis.name(m)).collect();
    let r = row(&c.key, &names.join(" · "));
    let files: Vec<String> = c.definitions.iter().map(|d| format!("{}: {}", analysis.name(d.mod_position), d.file)).collect();
    r.set_tooltip_text(Some(&files.join("\n")));

    let (merged, button) = merge_controls((hooks.is_merged)(&c.category, &c.key));
    let mark_merged = merged_marker(&merged, &button);
    let hooks = hooks.clone();
    button.connect_clicked(move |_| (hooks.open)(index, mark_merged()));
    r.add_suffix(&merged);
    r.add_suffix(&button);
    r
}

/// A callback factory that flips a row to its merged state.
fn merged_marker(merged: &gtk::Image, button: &gtk::Button) -> Rc<dyn Fn() -> Box<dyn Fn()>> {
    let (merged, button) = (merged.downgrade(), button.downgrade());
    Rc::new(move || {
        let (merged, button) = (merged.clone(), button.clone());
        Box::new(move || {
            if let (Some(merged), Some(button)) = (merged.upgrade(), button.upgrade()) {
                merged.set_visible(true);
                button.set_label("Edit Merge…");
            }
        })
    })
}

fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder().title(title).subtitle(subtitle).use_markup(false).subtitle_lines(2).build()
}

fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build()
}

fn heading(title: &str, description: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.set_margin_top(12);
    b.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["heading"]).build());
    b.append(&gtk::Label::builder().label(description).xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build());
    b
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
