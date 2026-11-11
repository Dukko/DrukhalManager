//! Conflict scanning, the merged patch, and the merge/fix dialogs.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::{display_name, status_page};
use super::Window;
use crate::conflicts_view::{self, Analysis, MergeHooks};
use crate::file_merge_dialog;
use crate::folder_merge_dialog;
use crate::fix_dialog;
use crate::merge::conflicts::{self, ModInput};
use crate::merge::patch::{self, Patch};
use crate::merge::snapshot;
use crate::mods::{self, archive, Mod};
use crate::settings;

impl Window {
    /// Rescans when the Conflicts page is showing a different load order.
    pub(super) fn scan_if_stale(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() != Some("conflicts") {
            return;
        }
        let revision = self.state.borrow().revision;
        if self
            .analysis
            .borrow()
            .as_ref()
            .is_none_or(|a| a.revision.get() != revision)
        {
            self.scan();
        }
    }

    /// Scans the enabled mods' files in the background.
    pub(super) fn scan(self: &Rc<Self>) {
        if self.scanning.replace(true) {
            return;
        }
        self.conflicts_page
            .set_child(Some(&conflicts_view::scanning()));
        let (revision, names, ids, load_positions, inputs, depends_on) = {
            let state = self.state.borrow();
            // The merged patch is left out: its objects are resolutions, not
            // conflicts.
            let scanned: Vec<(usize, &Mod)> = state
                .enabled
                .iter()
                .map(|&i| &state.mods[i])
                .enumerate()
                .filter(|(_, m)| m.id != patch::ID)
                .collect();
            let names: Vec<String> = scanned
                .iter()
                .map(|(_, m)| display_name(m).to_owned())
                .collect();
            let ids = scanned.iter().map(|(_, m)| m.id.clone()).collect();
            let descriptor_names: Vec<String> = scanned.iter().map(|(_, m)| m.descriptor.name.clone()).collect();
            let dependencies: Vec<Vec<String>> = scanned.iter().map(|(_, m)| m.descriptor.dependencies.clone()).collect();
            let depends_on = conflicts::depends_on(&descriptor_names, &dependencies);
            let positions = scanned.iter().map(|(p, _)| *p).collect();
            let inputs: Vec<ModInput> = scanned
                .iter()
                .map(|(_, m)| ModInput {
                    root: m.content.clone(),
                    replace_path: m.descriptor.replace_path.clone(),
                })
                .collect();
            (state.revision, names, ids, positions, inputs, depends_on)
        };

        let weak = Rc::downgrade(self);
        let game_dir = self.game.as_ref().map(|g| g.dir.clone());
        let languages = self.settings.borrow().game.languages(&self.paths.user_dir);
        let labels: Vec<String> = names.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                // Zipped mods are read from an unpacked copy; one that can't
                // be unpacked stays a zip, which the scan reports.
                let mut inputs = inputs;
                for input in &mut inputs {
                    if let Some(root) = &input.root
                        && let Ok(files) = archive::files(root)
                    {
                        input.root = Some(files);
                    }
                }
                let roots: Vec<_> = inputs.iter().map(|i| i.root.clone().filter(|r| r.is_dir())).collect();
                let report = conflicts::analyze(&inputs, Some(&languages));
                let simple = crate::merge::trivial::simple_conflicts(&report, &roots, &labels, game_dir.as_deref());
                (report, roots, simple)
            })
            .await;
            let Some(ui) = weak.upgrade() else { return };
            ui.scanning.set(false);
            match result {
                Ok((report, roots, simple)) => {
                    ui.check_stale();
                    ui.conflict_session.borrow_mut().rescanned();
                    ui.analysis.replace(Some(Rc::new(Analysis {
                        revision: Cell::new(revision),
                        names,
                        ids,
                        roots,
                        load_positions,
                        report,
                        simple,
                        depends_on,
                    })));
                    ui.render_conflicts();
                    // Shows the per-mod counts, and rescans if the load order
                    // changed while this scan ran.
                    ui.refresh();
                }
                Err(panic) => {
                    // The panic's message, to report it.
                    let detail = panic.downcast_ref::<String>().map(String::as_str).or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
                    eprintln!("Scan failed: {detail}");
                    ui.conflicts_page.set_child(Some(&status_page(
                        "Scan Failed",
                        &format!("Something went wrong while reading the mods' files. Please report this: {detail}"),
                    )));
                }
            }
        });
    }

    /// Compares the patch's fingerprints with the enabled mods' files and
    /// the base game's.
    pub(super) fn check_stale(&self) {
        let stale = {
            let state = self.state.borrow();
            let mut roots: std::collections::HashMap<String, std::path::PathBuf> = state
                .enabled
                .iter()
                .filter_map(|&i| Some((state.mods[i].id.clone(), archive::files(state.mods[i].content.as_deref()?).ok()?)))
                .collect();
            if let Some(game) = &self.game {
                roots.insert(patch::VANILLA.to_owned(), game.dir.clone());
            }
            let version = self.game.as_ref().and_then(|g| g.version.as_deref());
            self.patch
                .borrow_mut()
                .as_mut()
                .map(|p| p.recheck(&roots, version))
                .unwrap_or_default()
        };
        self.stale.replace(stale);
    }

    pub(super) fn remove_merge(self: &Rc<Self>, stale: &patch::Stale) {
        if let Err(e) = self.snapshot_patch(snapshot::BEFORE_REMOVING) {
            return self.toast_error_message(&e);
        }
        let result = self.with_patch(|p, _, _| match &stale.object {
            Some((category, key)) => p.remove(category, key),
            None => p.remove_file(&stale.path),
        });
        match result {
            Ok(()) => {
                self.check_stale();
                self.refresh();
                let what = stale
                    .object
                    .as_ref()
                    .map_or(stale.path.as_str(), |(_, k)| k.as_str());
                self.add_toast(adw::Toast::new(&format!("Removed {what} from the patch")));
            }
            Err(e) => self.toast_error_message(&e),
        }
    }

    /// Starts tracking a merge saved before fingerprints existed, taking its
    /// mods' current versions (from the last scan) as what it merged.
    pub(super) fn keep_merge(self: &Rc<Self>, stale: &patch::Stale) {
        let what = stale.object.as_ref().map_or(stale.path.clone(), |(_, k)| k.clone());
        let Some(analysis) = self.analysis.borrow().clone() else { return };
        let Some(sources) = self.patch.borrow().as_ref().and_then(|p| p.sources(stale.object.as_ref(), &stale.path).map(<[String]>::to_vec)) else { return };
        let source_mod = |m: usize| -> Option<patch::SourceMod> {
            let name = analysis.name(m);
            sources.iter().any(|s| s == name).then(|| Some((analysis.ids[m].clone(), name.to_owned(), analysis.roots[m].clone()?)))?
        };
        // Which mods define it now, according to the last scan.
        enum Found {
            Object(String, String, Vec<(patch::SourceMod, String)>),
            File(String, Vec<patch::SourceMod>),
        }
        let found = match &stale.object {
            Some((category, key)) => analysis
                .report
                .objects
                .iter()
                .find(|c| &c.category == category && &c.key == key)
                .map(|c| Found::Object(category.clone(), key.clone(), c.definitions.iter().filter_map(|d| Some((source_mod(d.mod_position)?, d.file.clone()))).collect())),
            None => analysis
                .report
                .files
                .iter()
                .find(|c| c.path.eq_ignore_ascii_case(&stale.path))
                .map(|c| Found::File(stale.path.clone(), c.mods.iter().filter_map(|&m| source_mod(m)).collect())),
        };
        let empty = match &found {
            Some(Found::Object(_, _, mods)) => mods.is_empty(),
            Some(Found::File(_, mods)) => mods.is_empty(),
            None => true,
        };
        if empty {
            return self.toast_error_message(&format!("The mods {what} was merged from no longer conflict over it. Remove it from the patch, or enable them and rescan."));
        }
        let game_dir = self.game.as_ref().map(|g| g.dir.clone());
        let (object, path) = (stale.object.clone(), stale.path.clone());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // Reading the base game's version parses whole folders.
            let prints = gio::spawn_blocking(move || match found {
                Some(Found::Object(category, key, mods)) => patch::object_prints(&category, &key, game_dir.as_deref(), &mods),
                Some(Found::File(path, mods)) => patch::file_prints(&path, game_dir.as_deref(), &mods),
                None => Vec::new(),
            })
            .await
            .unwrap_or_default();
            let Some(ui) = weak.upgrade() else { return };
            match ui.with_patch(|p, _, version| p.track(object.as_ref(), &path, prints, version)) {
                Ok(()) => {
                    ui.check_stale();
                    ui.render_conflicts();
                    ui.add_toast(adw::Toast::new(&format!("Kept {what}; changes to its mods will be noticed from now on")));
                }
                Err(e) => ui.toast_error_message(&e),
            }
        });
    }

    pub(super) fn toast_error_message(&self, message: &str) {
        self.add_toast(adw::Toast::builder().title(message).timeout(0).build());
    }

    /// Rebuilds the Conflicts page from the current analysis.
    pub(super) fn render_conflicts(self: &Rc<Self>) {
        let Some(analysis) = self.analysis.borrow().clone() else {
            return;
        };
        let hooks = self.merge_hooks(&analysis);
        let (widget, view) = conflicts_view::build(&analysis, self.rescan_button(), hooks);
        self.conflicts_page.set_child(Some(&widget));
        self.conflicts_view.replace(Some(view));
    }

    /// Opens the Conflicts page showing only the conflicts of the mod at
    /// `index` in the mod list.
    pub(super) fn show_conflicts_of(self: &Rc<Self>, index: usize) {
        let Some(id) = self.state.borrow().mods.get(index).map(|m| m.id.clone()) else { return };
        self.conflict_filter.replace(Some(id));
        self.stack.set_visible_child_name("conflicts");
        self.render_conflicts();
    }

    pub(super) fn set_ignored(self: &Rc<Self>, key: &str, ignore: bool) {
        {
            let mut settings = self.settings.borrow_mut();
            if ignore {
                settings.ignored.insert(key.to_owned());
            } else {
                settings.ignored.remove(key);
            }
            if let Err(e) = settings.save(&settings::path()) {
                self.add_toast(
                    adw::Toast::builder()
                        .title(format!("Could not save ignored conflicts: {e}"))
                        .timeout(0)
                        .build(),
                );
            }
        }
        // Updates the per-mod counts; the page updates itself.
        self.refresh();
    }

    pub(super) fn merge_hooks(self: &Rc<Self>, analysis: &Rc<Analysis>) -> Rc<MergeHooks> {
        let weak = Rc::downgrade(self);
        let is_merged = Box::new(move |category: &str, key: &str| {
            weak.upgrade().is_some_and(|ui| ui.patch.borrow().as_ref().is_some_and(|p| p.get(category, key).is_some()))
        });
        let weak = Rc::downgrade(self);
        let existing = Box::new(move |category: &str, key: &str| {
            weak.upgrade().and_then(|ui| ui.patch.borrow().as_ref().and_then(|p| p.get(category, key).cloned()))
        });
        let weak = Rc::downgrade(self);
        let save_object = Box::new(move |category: &str, key: &str, script: &str, prints: Vec<patch::Print>| {
            let ui = weak.upgrade().ok_or("The window was closed")?;
            ui.save_merge(category, key, script, prints)
        });
        let weak = Rc::downgrade(self);
        let save_file = Box::new(move |path: &str, contents: &[u8], prints: Vec<patch::Print>| {
            let ui = weak.upgrade().ok_or("The window was closed")?;
            ui.with_patch(|patch, user_dir, version| patch.save_file(user_dir, version, path, contents, prints))
        });
        let weak = Rc::downgrade(self);
        let patch_saved = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.patch_saved();
            }
        });
        let weak = Rc::downgrade(self);
        let reload_stale = Box::new(move || weak.upgrade().map(|ui| ui.stale.borrow().clone()).unwrap_or_default());
        let weak = Rc::downgrade(self);
        let is_file_merged = Box::new(move |path: &str| {
            weak.upgrade().is_some_and(|ui| ui.patch.borrow().as_ref().is_some_and(|p| p.get_file(path).is_some()))
        });
        let weak = Rc::downgrade(self);
        let files = analysis.clone();
        let open_file = Box::new(move |index: usize, mark_merged: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_file_merge(&files, index, mark_merged);
            }
        });
        let weak = Rc::downgrade(self);
        let folder = analysis.clone();
        let merge_folder = Box::new(move |indices: Vec<usize>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_folder_merge(&folder, indices);
            }
        });
        let weak = Rc::downgrade(self);
        let folder_files = analysis.clone();
        let merge_files = Box::new(move |indices: Vec<usize>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_files_merge(&folder_files, indices);
            }
        });
        let weak = Rc::downgrade(self);
        let errors = analysis.clone();
        let open_fix = Box::new(move |index: usize, mark_fixed: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_fix(&errors, index, mark_fixed);
            }
        });
        let weak = Rc::downgrade(self);
        let is_ignored = Box::new(move |key: &str| weak.upgrade().is_some_and(|ui| ui.settings.borrow().ignored.contains(key)));
        let weak = Rc::downgrade(self);
        let set_ignored = Box::new(move |key: &str, ignore: bool| {
            if let Some(ui) = weak.upgrade() {
                ui.set_ignored(key, ignore);
            }
        });
        let weak = Rc::downgrade(self);
        let rules_analysis = analysis.clone();
        let edit_rules = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.edit_ignore_rules(&rules_analysis);
            }
        });
        let weak = Rc::downgrade(self);
        let toggle_show_ignored = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.show_ignored.set(!ui.show_ignored.get());
                // After the click handler returns, since this replaces the page.
                glib::idle_add_local_once(move || ui.render_conflicts());
            }
        });
        let filter = self.conflict_filter.borrow().as_ref().and_then(|id| analysis.ids.iter().position(|i| i == id));
        let weak = Rc::downgrade(self);
        let ids = analysis.ids.clone();
        let set_filter = Box::new(move |position: Option<usize>| {
            if let Some(ui) = weak.upgrade() {
                ui.conflict_filter.replace(position.and_then(|p| ids.get(p).cloned()));
                // After the handler returns, since this replaces the page.
                glib::idle_add_local_once(move || ui.render_conflicts());
            }
        });
        let weak = Rc::downgrade(self);
        let remove_merge = Box::new(move |stale: &patch::Stale| {
            if let Some(ui) = weak.upgrade() {
                ui.remove_merge(stale);
            }
        });
        let weak = Rc::downgrade(self);
        let keep_merge = Box::new(move |stale: &patch::Stale| {
            if let Some(ui) = weak.upgrade() {
                ui.keep_merge(stale);
            }
        });
        let weak = Rc::downgrade(self);
        let rerender = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                glib::idle_add_local_once(move || ui.render_conflicts());
            }
        });
        let weak = Rc::downgrade(self);
        let provider = Box::new(move || weak.upgrade().and_then(|ui| ui.settings.borrow().ready_provider()));
        let weak = Rc::downgrade(self);
        let merge_tool = Box::new(move || weak.upgrade().map(|ui| ui.settings.borrow().merge_tool.clone()).unwrap_or_default());
        let weak = Rc::downgrade(self);
        let open_preferences = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_preferences(Some(crate::preferences::AI_PAGE));
            }
        });
        let weak = Rc::downgrade(self);
        let toast = Box::new(move |message: &str| {
            if let Some(ui) = weak.upgrade() {
                ui.add_toast(adw::Toast::new(message));
            }
        });
        Rc::new(MergeHooks {
            is_merged,
            existing,
            save_object,
            save_file,
            patch_saved,
            is_file_merged,
            open_file,
            merge_folder,
            merge_files,
            open_fix,
            is_ignored,
            set_ignored,
            rules: crate::merge::ignore_rules::Rules::parse(&self.settings.borrow().ignore_rules),
            hide_dependencies: !self.settings.borrow().show_dependency_conflicts,
            edit_rules,
            show_ignored: self.show_ignored.get(),
            toggle_show_ignored,
            filter,
            set_filter,
            stale: std::cell::RefCell::new(self.stale.borrow().clone()),
            reload_stale,
            remove_merge,
            keep_merge,
            rerender,
            game_dir: self.game.as_ref().map(|g| g.dir.clone()),
            provider,
            open_preferences,
            merge_tool,
            toast,
            session: self.conflict_session.clone(),
        })
    }

    /// Opens the AI merge of the object conflicts at `indices`, which are
    /// all in one folder.
    pub(super) fn open_folder_merge(self: &Rc<Self>, analysis: &Rc<Analysis>, indices: Vec<usize>) {
        let Some(category) = indices.first().map(|&i| analysis.report.objects[i].category.clone()) else { return };
        let items = {
            let patch = self.patch.borrow();
            let stale = self.stale.borrow();
            indices
                .iter()
                .map(|&i| {
                    let c = &analysis.report.objects[i];
                    let origins = c
                        .definitions
                        .iter()
                        .filter_map(|d| Some((analysis.ids[d.mod_position].clone(), analysis.name(d.mod_position).to_owned(), analysis.roots[d.mod_position].clone()?, d.file.clone())))
                        .collect();
                    let is_stale = stale.iter().any(|s| s.object.as_ref().is_some_and(|(cat, k)| cat == &c.category && k == &c.key));
                    let saved = patch.as_ref().and_then(|p| p.get(&c.category, &c.key)).map(|m| m.script.clone());
                    let merged = saved.is_some() && !is_stale;
                    folder_merge_dialog::Item { key: c.key.clone(), origins, merged, saved }
                })
                .collect()
        };
        let save_category = category.clone();
        let items = folder_merge_dialog::Items::Objects { category: category.clone(), items };
        self.present_merge_all(items, &category, move |ui, key, script, prints| ui.save_merge(&save_category, key, script, prints));
    }

    /// Opens the AI merge of the overridden files at `indices`, which are
    /// all in one folder.
    pub(super) fn open_files_merge(self: &Rc<Self>, analysis: &Rc<Analysis>, indices: Vec<usize>) {
        let Some(folder) = indices.first().map(|&i| conflicts_view::file_folder(&analysis.report.files[i].path).to_owned()) else { return };
        let files = {
            let patch = self.patch.borrow();
            let stale = self.stale.borrow();
            indices
                .iter()
                .map(|&i| {
                    let c = &analysis.report.files[i];
                    let origins = c.mods.iter().filter_map(|&m| Some((analysis.ids[m].clone(), analysis.name(m).to_owned(), analysis.roots[m].clone()?))).collect();
                    let is_stale = stale.iter().any(|s| s.object.is_none() && s.path.eq_ignore_ascii_case(&c.path));
                    let merged = patch.as_ref().is_some_and(|p| p.get_file(&c.path).is_some()) && !is_stale;
                    folder_merge_dialog::FileItem { path: c.path.clone(), origins, merged }
                })
                .collect()
        };
        let items = folder_merge_dialog::Items::Files { folder: folder.clone(), files };
        self.present_merge_all(items, &folder, |ui, path, contents, prints| {
            ui.with_patch(|patch, user_dir, version| patch.save_file(user_dir, version, path, contents, prints))
        });
    }

    /// Opens Merge All for `items` from `folder`, saving each result with
    /// `save` once the patch has been backed up.
    fn present_merge_all(
        self: &Rc<Self>,
        items: folder_merge_dialog::Items,
        folder: &str,
        save: impl Fn(&Rc<Self>, &str, &str, Vec<patch::Print>) -> Result<(), String> + 'static,
    ) {
        let weak = Rc::downgrade(self);
        let save = {
            let reason = snapshot::before_merge_all(folder);
            // The patch is backed up before the run's first save, so runs
            // that save nothing leave no backup behind.
            let backed_up = std::cell::Cell::new(false);
            Box::new(move |name: &str, text: &str, prints: Vec<patch::Print>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                if !backed_up.get() {
                    ui.snapshot_patch(&reason)?;
                    backed_up.set(true);
                }
                save(&ui, name, text, prints)
            })
        };
        let weak = Rc::downgrade(self);
        let open_preferences = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_preferences(Some(crate::preferences::AI_PAGE));
            }
        });
        let weak = Rc::downgrade(self);
        let on_done = Box::new(move |_saved: usize| {
            if let Some(ui) = weak.upgrade() {
                ui.patch_saved();
                ui.render_conflicts();
            }
        });
        let request = folder_merge_dialog::Request {
            items,
            game_dir: self.game.as_ref().map(|g| g.dir.clone()),
            provider: self.settings.borrow().ready_provider(),
            parallel: self.settings.borrow().merges_at_once(),
            save,
            open_preferences,
            on_done,
        };
        folder_merge_dialog::present(&self.window, request);
    }

    pub(super) fn open_fix(
        self: &Rc<Self>,
        analysis: &Rc<Analysis>,
        index: usize,
        mark_fixed: Box<dyn Fn()>,
    ) {
        let e = &analysis.report.errors[index];
        let Some(root) = analysis.roots[e.mod_position].clone() else {
            return;
        };
        let path = e.file.clone();
        let weak = Rc::downgrade(self);
        let save = {
            let path = path.clone();
            Box::new(move |contents: &str, prints: Vec<patch::Print>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                ui.with_patch(|patch, user_dir, version| {
                    patch.save_file(user_dir, version, &path, contents, prints)
                })
            })
        };
        let weak = Rc::downgrade(self);
        let open_preferences = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_preferences(Some(crate::preferences::AI_PAGE));
            }
        });
        let weak = Rc::downgrade(self);
        let on_saved = Box::new(move || {
            mark_fixed();
            if let Some(ui) = weak.upgrade() {
                ui.patch_saved();
            }
        });
        let request = fix_dialog::Request {
            path,
            mod_id: analysis.ids[e.mod_position].clone(),
            mod_name: analysis.name(e.mod_position).to_owned(),
            root,
            error: e.message.clone(),
            provider: self.settings.borrow().ready_provider(),
            save,
            open_preferences,
            on_saved,
        };
        fix_dialog::present(&self.window, request);
    }

    pub(super) fn open_file_merge(
        self: &Rc<Self>,
        analysis: &Rc<Analysis>,
        index: usize,
        mark_merged: Box<dyn Fn()>,
    ) {
        let conflict = &analysis.report.files[index];
        let origins = conflict
            .mods
            .iter()
            .filter_map(|&m| {
                Some((
                    analysis.ids[m].clone(),
                    analysis.name(m).to_owned(),
                    analysis.roots[m].clone()?,
                ))
            })
            .collect();
        let path = conflict.path.clone();
        let merged_before = self
            .patch
            .borrow()
            .as_ref()
            .is_some_and(|p| p.get_file(&path).is_some());

        let weak = Rc::downgrade(self);
        let save = {
            let path = path.clone();
            Box::new(move |contents: &str, prints: Vec<patch::Print>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                ui.with_patch(|patch, user_dir, version| {
                    patch.save_file(user_dir, version, &path, contents, prints)
                })
            })
        };
        let weak = Rc::downgrade(self);
        let open_preferences = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_preferences(Some(crate::preferences::AI_PAGE));
            }
        });
        let weak = Rc::downgrade(self);
        let on_saved = Box::new(move || {
            mark_merged();
            if let Some(ui) = weak.upgrade() {
                ui.patch_saved();
            }
        });
        let request = file_merge_dialog::Request {
            path,
            origins,
            game_dir: self.game.as_ref().map(|g| g.dir.clone()),
            provider: self.settings.borrow().ready_provider(),
            merged_before,
            save,
            open_preferences,
            on_saved,
        };
        file_merge_dialog::present(&self.window, request);
    }

    /// Runs `f` on the merged patch, opening it first if needed.
    pub(super) fn with_patch(
        &self,
        f: impl FnOnce(&mut Patch, &std::path::Path, Option<&str>) -> std::io::Result<()>,
    ) -> Result<(), String> {
        let mut patch = self.patch.borrow_mut();
        if patch.is_none() {
            *patch = Some(
                Patch::open(&self.paths.user_dir)
                    .map_err(|e| format!("Could not read the merged patch: {e}"))?,
            );
        }
        let version = self.game.as_ref().and_then(|g| g.version.as_deref());
        f(patch.as_mut().unwrap(), &self.paths.user_dir, version)
            .map_err(|e| format!("Could not save the merged patch: {e}"))
    }

    pub(super) fn save_merge(
        &self,
        category: &str,
        key: &str,
        script: &str,
        prints: Vec<patch::Print>,
    ) -> Result<(), String> {
        self.with_patch(|patch, user_dir, version| {
            patch.save(user_dir, version, category, key, script, prints)
        })
    }

    /// Makes sure the merged patch is in the load order once it has content.
    pub(super) fn patch_saved(self: &Rc<Self>) {
        self.check_stale();
        let added = {
            let mut state = self.state.borrow_mut();
            let index = match state.mods.iter().position(|m| m.id == patch::ID) {
                Some(i) => i,
                None => {
                    let descriptor = std::fs::read_to_string(self.paths.user_dir.join(patch::ID))
                        .ok()
                        .and_then(|src| mods::Descriptor::parse(&src).ok())
                        .unwrap_or_default();
                    state.mods.push(Mod {
                        id: patch::ID.to_owned(),
                        descriptor,
                        content: Some(Patch::root(&self.paths.user_dir)),
                        source: mods::Source::Local,
                        load_position: None,
                        error: None,
                        steam: None,
                        requires: Vec::new(),
                    });
                    state.mods.len() - 1
                }
            };
            let was_enabled = state.enabled.contains(&index);
            if !was_enabled {
                // Pinned to the end, like every change.
                state.set_enabled(index, true);
                // The patch isn't scanned, so the current report still holds.
                if let Some(a) = self.analysis.borrow().as_ref() {
                    a.revision.set(state.revision);
                }
            }
            (!was_enabled).then_some("added to the end of the load order")
        };
        self.refresh();
        let toast = adw::Toast::builder().title(format!("Saved to {}", patch::NAME));
        let toast = match added {
            Some(change) => toast
                .title(format!("Saved to {}, which was {change}", patch::NAME))
                .button_label("Save Load Order")
                .action_name("win.save"),
            None => toast,
        };
        self.add_toast(toast.build());
    }

    pub(super) fn rescan_button(self: &Rc<Self>) -> gtk::Button {
        let button = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("view-refresh-symbolic")
                    .label("Rescan")
                    .build(),
            )
            .tooltip_text("Scan again, e.g. after mods were updated")
            .build();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.scan();
            }
        });
        button
    }
}
