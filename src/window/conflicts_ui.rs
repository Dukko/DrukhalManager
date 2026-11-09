//! Conflict scanning, the merged patch, and the merge/fix dialogs.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::{display_name, status_page};
use super::Window;
use crate::conflicts_view::{self, Analysis, MergeHooks};
use crate::file_merge_dialog;
use crate::fix_dialog;
use crate::merge::conflicts::{self, ModInput};
use crate::merge::patch::{self, Patch};
use crate::merge_dialog;
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
        let (revision, names, ids, load_positions, inputs) = {
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
            let names = scanned
                .iter()
                .map(|(_, m)| display_name(m).to_owned())
                .collect();
            let ids = scanned.iter().map(|(_, m)| m.id.clone()).collect();
            let positions = scanned.iter().map(|(p, _)| *p).collect();
            let inputs: Vec<ModInput> = scanned
                .iter()
                .map(|(_, m)| ModInput {
                    root: m.content.clone(),
                    replace_path: m.descriptor.replace_path.clone(),
                })
                .collect();
            (state.revision, names, ids, positions, inputs)
        };

        let weak = Rc::downgrade(self);
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
                let roots = inputs.iter().map(|i| i.root.clone().filter(|r| r.is_dir())).collect();
                (conflicts::analyze(&inputs), roots)
            })
            .await;
            let Some(ui) = weak.upgrade() else { return };
            ui.scanning.set(false);
            match result {
                Ok((report, roots)) => {
                    ui.check_stale();
                    ui.analysis.replace(Some(Rc::new(Analysis {
                        revision: Cell::new(revision),
                        names,
                        ids,
                        roots,
                        load_positions,
                        report,
                    })));
                    ui.render_conflicts();
                    // Shows the per-mod counts, and rescans if the load order
                    // changed while this scan ran.
                    ui.refresh();
                }
                Err(_) => {
                    ui.conflicts_page.set_child(Some(&status_page(
                        "Scan Failed",
                        "Something went wrong while reading the mods' files",
                    )));
                }
            }
        });
    }

    /// Compares the patch's fingerprints with the enabled mods' files.
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
            self.patch
                .borrow()
                .as_ref()
                .map(|p| p.stale(&roots))
                .unwrap_or_default()
        };
        self.stale.replace(stale);
    }

    pub(super) fn remove_merge(self: &Rc<Self>, stale: &patch::Stale) {
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
                self.toasts
                    .add_toast(adw::Toast::new(&format!("Removed {what} from the patch")));
            }
            Err(e) => self.toast_error_message(&e),
        }
    }

    pub(super) fn toast_error_message(&self, message: &str) {
        self.toasts
            .add_toast(adw::Toast::builder().title(message).timeout(0).build());
    }

    /// Rebuilds the Conflicts page from the current analysis.
    pub(super) fn render_conflicts(self: &Rc<Self>) {
        let Some(analysis) = self.analysis.borrow().clone() else {
            return;
        };
        let hooks = self.merge_hooks(&analysis);
        self.conflicts_page.set_child(Some(&conflicts_view::build(
            &analysis,
            self.rescan_button(),
            hooks,
        )));
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
                self.toasts.add_toast(
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
            weak.upgrade().is_some_and(|ui| {
                ui.patch
                    .borrow()
                    .as_ref()
                    .is_some_and(|p| p.get(category, key).is_some())
            })
        });
        let weak = Rc::downgrade(self);
        let objects = analysis.clone();
        let open = Box::new(move |index: usize, mark_merged: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_merge(&objects, index, mark_merged);
            }
        });
        let weak = Rc::downgrade(self);
        let is_file_merged = Box::new(move |path: &str| {
            weak.upgrade().is_some_and(|ui| {
                ui.patch
                    .borrow()
                    .as_ref()
                    .is_some_and(|p| p.get_file(path).is_some())
            })
        });
        let weak = Rc::downgrade(self);
        let files = analysis.clone();
        let open_file = Box::new(move |index: usize, mark_merged: Box<dyn Fn()>| {
            if let Some(ui) = weak.upgrade() {
                ui.open_file_merge(&files, index, mark_merged);
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
        let is_ignored = Box::new(move |key: &str| {
            weak.upgrade()
                .is_some_and(|ui| ui.settings.borrow().ignored.contains(key))
        });
        let weak = Rc::downgrade(self);
        let set_ignored = Box::new(move |key: &str, ignore: bool| {
            if let Some(ui) = weak.upgrade() {
                ui.set_ignored(key, ignore);
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
        let weak = Rc::downgrade(self);
        let remove_merge = Box::new(move |stale: &patch::Stale| {
            if let Some(ui) = weak.upgrade() {
                ui.remove_merge(stale);
            }
        });
        Rc::new(MergeHooks {
            is_merged,
            open,
            is_file_merged,
            open_file,
            open_fix,
            is_ignored,
            set_ignored,
            show_ignored: self.show_ignored.get(),
            toggle_show_ignored,
            stale: self.stale.borrow().clone(),
            remove_merge,
        })
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

    pub(super) fn open_merge(
        self: &Rc<Self>,
        analysis: &Rc<Analysis>,
        index: usize,
        mark_merged: Box<dyn Fn()>,
    ) {
        let conflict = &analysis.report.objects[index];
        let origins = conflict
            .definitions
            .iter()
            .filter_map(|d| {
                let root = analysis.roots[d.mod_position].clone()?;
                Some(merge_dialog::Origin {
                    mod_id: analysis.ids[d.mod_position].clone(),
                    label: analysis.name(d.mod_position).to_owned(),
                    root,
                    file: d.file.clone(),
                })
            })
            .collect();
        let (category, key) = (conflict.category.clone(), conflict.key.clone());
        let existing = self
            .patch
            .borrow()
            .as_ref()
            .and_then(|p| p.get(&category, &key).cloned());

        let weak = Rc::downgrade(self);
        let save = {
            let (category, key) = (category.clone(), key.clone());
            Box::new(move |script: &str, prints: Vec<patch::Print>| {
                let ui = weak.upgrade().ok_or("The window was closed")?;
                ui.save_merge(&category, &key, script, prints)
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

        let request = merge_dialog::Request {
            category,
            key,
            origins,
            game_dir: self.game.as_ref().map(|g| g.dir.clone()),
            provider: self.settings.borrow().ready_provider(),
            existing,
            save,
            open_preferences,
            on_saved,
        };
        merge_dialog::present(&self.window, request);
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
        self.toasts.add_toast(toast.build());
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
