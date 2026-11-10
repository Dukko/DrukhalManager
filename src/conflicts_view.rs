//! The Conflicts page: a read-only view of a conflict report.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

use crate::merge::conflicts::Report;
use crate::merge::file_merge;
use crate::merge::patch::Stale;

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
    /// Descriptor ids by scanned position.
    pub ids: Vec<String>,
    /// Each scanned mod's folder.
    pub roots: Vec<Option<PathBuf>>,
    /// Each scanned mod's position in the full load order (the merged patch
    /// itself is not scanned, so these can differ).
    pub load_positions: Vec<usize>,
    pub report: Report,
    /// Object conflicts that need no AI to merge, by index, with why.
    pub simple: std::collections::HashMap<usize, String>,
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
    /// Opens the AI merge of several object conflicts in one folder.
    pub merge_folder: Box<dyn Fn(Vec<usize>)>,
    /// Opens the fix dialog for a file that failed to parse.
    pub open_fix: Box<dyn Fn(usize, Box<dyn Fn()>)>,
    pub is_ignored: Box<dyn Fn(&str) -> bool>,
    pub set_ignored: Box<dyn Fn(&str, bool)>,
    /// Whether ignored conflicts are shown (dimmed) instead of hidden.
    pub show_ignored: bool,
    pub toggle_show_ignored: Box<dyn Fn()>,
    /// The scanned position of the mod whose conflicts alone are shown.
    pub filter: Option<usize>,
    pub set_filter: Box<dyn Fn(Option<usize>)>,
    /// Merges and fixes whose sources changed since they were saved.
    pub stale: Vec<Stale>,
    /// Removes an out-of-date merge or fix from the patch.
    pub remove_merge: Box<dyn Fn(&Stale)>,
    /// Starts tracking an untracked merge from its mods' current files.
    pub keep_merge: Box<dyn Fn(&Stale)>,
}

impl MergeHooks {
    fn stale_object(&self, category: &str, key: &str) -> Option<&[String]> {
        self.stale.iter().find(|s| !s.untracked && s.object.as_ref().is_some_and(|(c, k)| c == category && k == key)).map(|s| s.reasons.as_slice())
    }

    fn stale_file(&self, path: &str) -> Option<&[String]> {
        self.stale.iter().find(|s| !s.untracked && s.object.is_none() && s.path.eq_ignore_ascii_case(path)).map(|s| s.reasons.as_slice())
    }
}

// Keys for ignored conflicts, stored in the settings.
pub fn file_key(path: &str) -> String {
    format!("file:{}", path.to_lowercase())
}
pub fn object_key(category: &str, key: &str) -> String {
    format!("object:{category}:{key}")
}
pub fn files_folder_key(folder: &str) -> String {
    format!("files-in:{}", folder.to_lowercase())
}
pub fn objects_folder_key(category: &str) -> String {
    format!("objects-in:{category}")
}

fn file_folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Whether a conflict is ignored, directly or through its folder.
pub fn file_ignored(is_ignored: &dyn Fn(&str) -> bool, c: &crate::merge::conflicts::FileConflict) -> bool {
    is_ignored(&file_key(&c.path)) || is_ignored(&files_folder_key(file_folder(&c.path)))
}
pub fn object_ignored(is_ignored: &dyn Fn(&str) -> bool, c: &crate::merge::conflicts::ObjectConflict) -> bool {
    is_ignored(&object_key(&c.category, &c.key)) || is_ignored(&objects_folder_key(&c.category))
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

    let ignored = |k: &str| (hooks.is_ignored)(k);
    let files: Vec<usize> = (0..report.files.len()).filter(|&i| hooks.show_ignored || !file_ignored(&ignored, &report.files[i])).collect();
    let objects: Vec<usize> = (0..report.objects.len()).filter(|&i| hooks.show_ignored || !object_ignored(&ignored, &report.objects[i])).collect();
    let n_ignored = report.files.iter().filter(|c| file_ignored(&ignored, c)).count() + report.objects.iter().filter(|c| object_ignored(&ignored, c)).count();
    let (shown_files, shown_objects) = if hooks.show_ignored {
        (files.iter().filter(|&&i| !file_ignored(&ignored, &report.files[i])).count(), objects.iter().filter(|&&i| !object_ignored(&ignored, &report.objects[i])).count())
    } else {
        (files.len(), objects.len())
    };

    let involved: BTreeSet<usize> = files
        .iter()
        .flat_map(|&i| report.files[i].mods.iter().copied())
        .chain(objects.iter().flat_map(|&i| report.objects[i].definitions.iter().map(|d| d.mod_position)))
        .collect();

    // Only one mod's conflicts, if filtered.
    let filter = hooks.filter;
    let keep_file = |i: &usize| filter.is_none_or(|p| report.files[*i].mods.contains(&p));
    let keep_object = |i: &usize| filter.is_none_or(|p| report.objects[*i].definitions.iter().any(|d| d.mod_position == p));
    let (all_files, all_objects) = (shown_files, shown_objects);
    let files: Vec<usize> = files.into_iter().filter(keep_file).collect();
    let objects: Vec<usize> = objects.into_iter().filter(keep_object).collect();
    let (shown_files, shown_objects) = if filter.is_some() {
        (files.iter().filter(|&&i| !file_ignored(&ignored, &report.files[i])).count(), objects.iter().filter(|&&i| !object_ignored(&ignored, &report.objects[i])).count())
    } else {
        (shown_files, shown_objects)
    };
    let errors: Vec<usize> = (0..report.errors.len()).filter(|&i| filter.is_none_or(|p| report.errors[i].mod_position == p)).collect();
    let skipped: Vec<&(usize, String)> = report.skipped.iter().filter(|(p, _)| filter.is_none_or(|f| *p == f)).collect();

    let mut summary = if let Some(p) = filter {
        if shown_files == 0 && shown_objects == 0 {
            format!("{} has no conflicts", analysis.name(p))
        } else {
            format!(
                "{} of {} involve {}",
                plural(shown_files + shown_objects, "conflict", "conflicts"),
                all_files + all_objects,
                analysis.name(p)
            )
        }
    } else if shown_files == 0 && shown_objects == 0 {
        "No conflicts between enabled mods".to_owned()
    } else {
        format!(
            "{} · {} · {} of {} mods involved",
            plural(shown_files, "file override", "file overrides"),
            plural(shown_objects, "object conflict", "object conflicts"),
            involved.len(),
            analysis.names.len()
        )
    };
    if n_ignored > 0 && filter.is_none() {
        summary.push_str(&format!(" · {n_ignored} ignored"));
    }
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    top.append(&gtk::Label::builder().label(summary).xalign(0.0).hexpand(true).wrap(true).css_classes(["dim-label"]).build());
    // Mods with conflicts, in load order, plus the filtered one if it has none.
    let choices: Vec<usize> = (0..analysis.names.len()).filter(|p| involved.contains(p) || filter == Some(*p)).collect();
    if choices.len() > 1 || filter.is_some() {
        let mut labels = vec!["All Mods"];
        labels.extend(choices.iter().map(|&p| analysis.name(p)));
        let dropdown = gtk::DropDown::from_strings(&labels);
        dropdown.set_enable_search(true);
        dropdown.set_valign(gtk::Align::Center);
        dropdown.set_tooltip_text(Some("Show only the conflicts of one mod"));
        dropdown.set_selected(filter.and_then(|f| choices.iter().position(|&p| p == f)).map_or(0, |i| i as u32 + 1));
        // Search by the shown text.
        dropdown.set_expression(Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")));
        let hooks = hooks.clone();
        dropdown.connect_selected_notify(move |d| {
            let position = (d.selected() as usize).checked_sub(1).and_then(|i| choices.get(i).copied());
            (hooks.set_filter)(position);
        });
        top.append(&dropdown);
    }
    if n_ignored > 0 {
        let toggle = gtk::Button::builder().label(if hooks.show_ignored { "Hide Ignored" } else { "Show Ignored" }).valign(gtk::Align::Center).css_classes(["flat"]).build();
        let hooks = hooks.clone();
        toggle.connect_clicked(move |_| (hooks.toggle_show_ignored)());
        top.append(&toggle);
    }
    rescan.set_valign(gtk::Align::Center);
    top.append(&rescan);
    page.append(&top);

    let (untracked, outdated): (Vec<&Stale>, Vec<&Stale>) = hooks.stale.iter().partition(|s| s.untracked);
    if !outdated.is_empty() {
        page.append(&heading("Outdated Merges", "Mods changed since these were merged or fixed, so the patch may be undoing their updates. Re-merge them below, or remove them from the patch."));
        page.append(&stale_list(&hooks, &outdated));
    }
    if !untracked.is_empty() {
        page.append(&heading("Merges to Check", "These were merged by an older version of DrukhalManager, which didn't record what the mods looked like, so their updates can't be noticed. If one still looks right, keep it to track changes from now on; otherwise re-merge it below or remove it."));
        page.append(&stale_list(&hooks, &untracked));
    }

    if filter.is_some() && files.is_empty() && objects.is_empty() && errors.is_empty() && skipped.is_empty() {
        return page.upcast();
    }
    if files.is_empty() && objects.is_empty() && report.errors.is_empty() && report.skipped.is_empty() {
        let empty = adw::StatusPage::builder().icon_name("emblem-ok-symbolic").title("No Conflicts").description("None of the enabled mods change the same files or objects").vexpand(true).build();
        page.append(&empty);
        return page.upcast();
    }

    if !files.is_empty() {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in &files {
            groups.entry(file_folder(&report.files[i].path).to_owned()).or_default().push(i);
        }
        let make: Rc<dyn Fn(usize) -> adw::ActionRow> = {
            let analysis = analysis.clone();
            let hooks = hooks.clone();
            Rc::new(move |i| file_row(&analysis, i, &hooks))
        };
        section(&page, "Overridden Files", "Only the copy from the mod loaded last is used. Merge a script or localisation file to keep every mod's objects.", groups, make, files_folder_key, &hooks, None);
    }
    if !objects.is_empty() {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in &objects {
            groups.entry(report.objects[i].category.clone()).or_default().push(i);
        }
        let make: Rc<dyn Fn(usize) -> adw::ActionRow> = {
            let analysis = analysis.clone();
            let hooks = hooks.clone();
            Rc::new(move |i| object_row(&analysis, i, &hooks))
        };
        let merge_all: Rc<dyn Fn(&[usize])> = {
            let (analysis, hooks) = (analysis.clone(), hooks.clone());
            Rc::new(move |items: &[usize]| {
                let ignored = |k: &str| (hooks.is_ignored)(k);
                (hooks.merge_folder)(items.iter().copied().filter(|&i| !object_ignored(&ignored, &analysis.report.objects[i])).collect());
            })
        };
        section(&page, "Conflicting Objects", "Defined by several mods; which one the game keeps depends on the folder. Merge one to keep every mod's changes, or merge a whole folder with AI.", groups, make, objects_folder_key, &hooks, Some(merge_all));
    }

    if !errors.is_empty() || !skipped.is_empty() {
        page.append(&heading("Problems", "Files the game will misread, and mods that couldn't be scanned"));
        let list = boxed_list();
        for (position, reason) in skipped {
            list.append(&row(analysis.name(*position), reason));
        }
        for index in errors {
            let e = &report.errors[index];
            let r = row(&e.file, &format!("{} · {}", analysis.name(e.mod_position), e.message));
            r.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
            // Parse errors read "line N: …"; others are read failures.
            if e.message.starts_with("line ") && analysis.roots[e.mod_position].is_some() {
                let (fixed, button) = merge_controls((hooks.is_file_merged)(&e.file), hooks.stale_file(&e.file));
                fixed.set_tooltip_text(Some("Fixed through the merged patch"));
                button.set_label(match (hooks.stale_file(&e.file), fixed.is_visible()) {
                    (Some(_), _) => "Fix Again…",
                    (None, true) => "Edit Fix…",
                    (None, false) => "Fix…",
                });
                let mark = merged_marker(&fixed, &button);
                let hooks = hooks.clone();
                button.connect_clicked(move |_| (hooks.open_fix)(index, mark()));
                r.add_suffix(&fixed);
                r.add_suffix(&button);
            }
            list.append(&r);
        }
        page.append(&list);
    }
    page.upcast()
}

/// A heading plus one collapsed row per folder. Each folder's rows are only
/// built when it is first expanded.
#[allow(clippy::too_many_arguments)]
fn section(page: &gtk::Box, title: &str, description: &str, groups: BTreeMap<String, Vec<usize>>, make: Rc<dyn Fn(usize) -> adw::ActionRow>, folder_key: fn(&str) -> String, hooks: &Rc<MergeHooks>, merge_all: Option<Rc<dyn Fn(&[usize])>>) {
    page.append(&heading(title, description));
    let list = boxed_list();
    for (folder, items) in groups {
        let expander = adw::ExpanderRow::builder()
            .title(if folder.is_empty() { "(top level)" } else { folder.as_str() })
            .subtitle(plural(items.len(), "conflict", "conflicts"))
            .use_markup(false)
            .build();
        if let Some(merge_all) = &merge_all {
            let button = gtk::Button::builder()
                .label("Merge All…")
                .tooltip_text("Merge every conflict in this folder with AI, after an estimate of the cost")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let (merge_all, items) = (merge_all.clone(), items.clone());
            button.connect_clicked(move |_| merge_all(&items));
            expander.add_suffix(&button);
        }
        expander.add_suffix(&ignore_button(hooks, folder_key(&folder), expander.upcast_ref(), "Ignore every conflict in this folder"));
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
    r.add_suffix(&ignore_button(hooks, file_key(&c.path), r.upcast_ref(), "Ignore this conflict"));
    if file_merge::mergeable(&c.path) {
        let is_merged = (hooks.is_file_merged)(&c.path);
        let (merged, button) = merge_controls(is_merged, hooks.stale_file(&c.path));
        let mark_merged = merged_marker(&merged, &button);
        let hooks = hooks.clone();
        button.connect_clicked(move |_| (hooks.open_file)(index, mark_merged()));
        r.add_suffix(&merged);
        r.add_suffix(&button);
    }
    r
}

/// The merged-state icon and the merge button for a row. An out-of-date
/// merge shows a warning with the reasons instead of a check mark.
/// Rows for merges that need attention, each with a way to deal with it.
fn stale_list(hooks: &Rc<MergeHooks>, stale: &[&Stale]) -> gtk::ListBox {
    let list = boxed_list();
    for s in stale {
        let file_name = s.path.rsplit_once('/').map_or(s.path.as_str(), |(_, f)| f);
        let title = s.object.as_ref().map_or(file_name, |(_, key)| key.as_str());
        let subtitle = match (&s.object, s.untracked) {
            // The section heading already says why.
            (Some((category, _)), true) => category.clone(),
            (None, true) => s.path.clone(),
            (Some((category, _)), false) => format!("{category} · {}", s.reasons.join("; ")),
            (None, false) => format!("{} · {}", s.path, s.reasons.join("; ")),
        };
        let r = row(title, &subtitle);
        let icon = if s.untracked { "dialog-question-symbolic" } else { "dialog-warning-symbolic" };
        r.add_prefix(&gtk::Image::builder().icon_name(icon).css_classes(["warning"]).build());
        if s.untracked {
            let keep = gtk::Button::builder()
                .label("Keep As Is")
                .tooltip_text("Track changes to its mods from their current versions")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let (hooks, stale) = (hooks.clone(), (*s).clone());
            keep.connect_clicked(move |_| (hooks.keep_merge)(&stale));
            r.add_suffix(&keep);
        }
        let remove = gtk::Button::builder().label("Remove from Patch").valign(gtk::Align::Center).css_classes(["flat"]).build();
        let (hooks, stale, row_ref) = (hooks.clone(), (*s).clone(), r.downgrade());
        remove.connect_clicked(move |_| {
            (hooks.remove_merge)(&stale);
            if let Some(r) = row_ref.upgrade() {
                r.set_visible(false);
            }
        });
        r.add_suffix(&remove);
        list.append(&r);
    }
    list
}

fn merge_controls(is_merged: bool, stale: Option<&[String]>) -> (gtk::Image, gtk::Button) {
    let merged = gtk::Image::builder().icon_name("emblem-ok-symbolic").tooltip_text("Merged into the patch").css_classes(["success"]).visible(is_merged).build();
    let label = match (stale, is_merged) {
        (Some(reasons), _) => {
            merged.set_icon_name(Some("dialog-warning-symbolic"));
            merged.set_css_classes(&["warning"]);
            merged.set_tooltip_text(Some(&format!("Out of date: {}", reasons.join("; "))));
            merged.set_visible(true);
            "Re-merge…"
        }
        (None, true) => "Edit Merge…",
        (None, false) => "Merge…",
    };
    let button = gtk::Button::builder().label(label).valign(gtk::Align::Center).css_classes(["flat"]).build();
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
    if let Some(reason) = analysis.simple.get(&index) {
        r.add_prefix(
            &gtk::Image::builder()
                .icon_name("media-flash-symbolic")
                .tooltip_text(format!("Simple merge, no AI needed: {reason}"))
                .css_classes(["accent"])
                .build(),
        );
    }
    r.add_suffix(&ignore_button(hooks, object_key(&c.category, &c.key), r.upcast_ref(), "Ignore this conflict"));

    let (merged, button) = merge_controls((hooks.is_merged)(&c.category, &c.key), hooks.stale_object(&c.category, &c.key));
    let mark_merged = merged_marker(&merged, &button);
    let hooks = hooks.clone();
    button.connect_clicked(move |_| (hooks.open)(index, mark_merged()));
    r.add_suffix(&merged);
    r.add_suffix(&button);
    r
}

/// A button that ignores (or stops ignoring) `key`. Ignoring hides `row`
/// right away unless ignored conflicts are being shown, where it's dimmed.
fn ignore_button(hooks: &Rc<MergeHooks>, key: String, row: &gtk::Widget, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder().valign(gtk::Align::Center).css_classes(["flat"]).build();
    let sync = |button: &gtk::Button, row: &gtk::Widget, ignored: bool, tooltip: &str| {
        button.set_icon_name(if ignored { "view-reveal-symbolic" } else { "view-conceal-symbolic" });
        button.set_tooltip_text(Some(if ignored { "Stop ignoring" } else { tooltip }));
        if ignored {
            row.add_css_class("dim-label");
        } else {
            row.remove_css_class("dim-label");
        }
    };
    sync(&button, row, (hooks.is_ignored)(&key), tooltip);
    let (hooks, row, tooltip) = (hooks.clone(), row.downgrade(), tooltip.to_owned());
    button.connect_clicked(move |button| {
        let Some(row) = row.upgrade() else { return };
        let ignore = !(hooks.is_ignored)(&key);
        (hooks.set_ignored)(&key, ignore);
        if ignore && !hooks.show_ignored {
            row.set_visible(false);
        }
        sync(button, &row, ignore, &tooltip);
    });
    button
}

/// A callback factory that flips a row to its merged state.
fn merged_marker(merged: &gtk::Image, button: &gtk::Button) -> Rc<dyn Fn() -> Box<dyn Fn()>> {
    let (merged, button) = (merged.downgrade(), button.downgrade());
    Rc::new(move || {
        let (merged, button) = (merged.clone(), button.clone());
        Box::new(move || {
            if let (Some(merged), Some(button)) = (merged.upgrade(), button.upgrade()) {
                merged.set_icon_name(Some("emblem-ok-symbolic"));
                merged.set_css_classes(&["success"]);
                merged.set_tooltip_text(Some("Merged into the patch"));
                merged.set_visible(true);
                if button.label().is_some_and(|l| l.contains("Fix")) {
                    button.set_label("Edit Fix…");
                } else {
                    button.set_label("Edit Merge…");
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::conflicts::{Definition, FileConflict, ObjectConflict};

    #[test]
    fn ignores_directly_or_by_folder() {
        let ignored: std::collections::BTreeSet<String> = [file_key("common/Buildings/A.txt"), objects_folder_key("common/portrait_sets")].into();
        let is = |k: &str| ignored.contains(k);
        let file = |p: &str| FileConflict { path: p.into(), mods: vec![0, 1] };
        let object = |c: &str| ObjectConflict { category: c.into(), key: "x".into(), definitions: vec![Definition { mod_position: 0, file: "f".into() }] };
        assert!(file_ignored(&is, &file("common/buildings/a.txt")), "paths match case-insensitively");
        assert!(!file_ignored(&is, &file("common/buildings/b.txt")));
        assert!(object_ignored(&is, &object("common/portrait_sets")));
        assert!(!object_ignored(&is, &object("common/buildings")));
        let folder: std::collections::BTreeSet<String> = [files_folder_key("gfx/models")].into();
        assert!(file_ignored(&|k| folder.contains(k), &file("gfx/models/ship.mesh")));
    }
}
