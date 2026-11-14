//! Listing the merged patch's snapshots and restoring one.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::Window;
use crate::merge::patch::Patch;
use crate::merge::snapshot::{self, Snapshot};

impl Window {
    /// Takes a snapshot of the patch before a change, reporting a failure.
    /// Returns whether the change may go ahead.
    pub(super) fn snapshot_patch(&self, reason: &str) -> Result<(), String> {
        snapshot::take(&self.paths.user_dir, &self.patch_target(), &self.paths.backup_dir, reason)
            .map(|_| ())
            .map_err(|e| tf!("Could not back up the merged patch first, so nothing was changed: {error}", error = e))
    }

    pub(super) fn show_patch_backups(self: &Rc<Self>) {
        let target = self.patch_target();
        let snapshots = snapshot::list(&self.paths.backup_dir, &target);
        let header = adw::HeaderBar::new();
        // Each collection's patch has its own backups.
        header.set_title_widget(Some(&adw::WindowTitle::new(&t!("Merged Patch Backups"), &target.name)));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        let dialog = adw::Dialog::builder().title(t!("Merged Patch Backups")).content_width(620).content_height(560).child(&toolbar).build();

        if snapshots.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("document-open-recent-symbolic")
                .title(t!("No Backups Yet"))
                .description(t!("The merged patch is backed up before each Merge All, and before merges are removed from it."))
                .build();
            toolbar.set_content(Some(&empty));
            return dialog.present(Some(&self.window));
        }

        let group = adw::PreferencesGroup::builder()
            .description(tf!(
                "The merged patch is backed up before each Merge All, and before merges are removed from it. Restoring one replaces the patch's merges with the ones it holds; the patch as it is now is backed up first. The last {kept} are kept.",
                kept = snapshot::KEPT
            ))
            .build();
        for s in snapshots {
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(&s.reason).as_str()).subtitle(subtitle(&s)).build();
            let restore = gtk::Button::builder().label(t!("Restore…")).valign(gtk::Align::Center).css_classes(["flat"]).build();
            let (weak, dialog_ref) = (Rc::downgrade(self), dialog.downgrade());
            restore.connect_clicked(move |_| {
                if let (Some(ui), Some(dialog)) = (weak.upgrade(), dialog_ref.upgrade()) {
                    ui.confirm_restore(&dialog, s.clone());
                }
            });
            row.add_suffix(&restore);
            group.add(&row);
        }
        let page = adw::PreferencesPage::new();
        page.add(&group);
        toolbar.set_content(Some(&page));
        dialog.present(Some(&self.window));
    }

    fn confirm_restore(self: &Rc<Self>, parent: &adw::Dialog, snapshot: Snapshot) {
        let alert = adw::AlertDialog::new(
            Some(&t!("Restore This Backup?")),
            Some(&tf!(
                "The merged patch goes back to {contents} from {when}. Its current merges are backed up first, so you can undo this from this list.",
                contents = contents(&snapshot),
                when = when(&snapshot)
            )),
        );
        alert.add_responses(&[("cancel", &t!("_Cancel")), ("restore", &t!("_Restore"))]);
        alert.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
        alert.set_close_response("cancel");
        let (weak, parent_ref) = (Rc::downgrade(self), parent.downgrade());
        alert.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            if response != "restore" {
                return;
            }
            if let Some(parent) = parent_ref.upgrade() {
                parent.close();
            }
            ui.restore_patch(&snapshot);
        });
        alert.present(Some(parent));
    }

    fn restore_patch(self: &Rc<Self>, snapshot: &Snapshot) {
        // Dropped first: it would otherwise write its old records back.
        self.patch.replace(None);
        let target = self.patch_target();
        let result = snapshot::restore(&self.paths.user_dir, &target, &self.paths.backup_dir, snapshot);
        match Patch::open(&self.paths.user_dir, &target) {
            Ok(p) => {
                self.patch.replace(Some(p));
            }
            Err(e) => return self.toast_error_message(&tf!("Could not read the merged patch: {error}", error = e)),
        }
        if let Err(e) = result {
            return self.toast_error_message(&tf!("Could not restore the backup: {error}", error = e));
        }
        self.patch_saved();
        self.render_conflicts();
        self.add_toast(adw::Toast::new(&tf!("Restored the merged patch from {when}", when = when(snapshot))));
    }
}

fn when(s: &Snapshot) -> String {
    let secs = s.taken.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    glib::DateTime::from_unix_local(i64::try_from(secs).unwrap_or_default())
        .ok()
        .and_then(|d| d.format("%x %H:%M").ok())
        .map_or_else(|| t!("an unknown time"), |s| s.to_string())
}

fn contents(s: &Snapshot) -> String {
    match (s.merges, s.files) {
        (0, 0) => t!("no merges"),
        (m, 0) => tnf!("{count} merged object", "{count} merged objects", count = m),
        (0, f) => tnf!("{count} merged file", "{count} merged files", count = f),
        (m, f) => tf!(
            "{objects} and {files}",
            objects = tnf!("{count} merged object", "{count} merged objects", count = m),
            files = tnf!("{count} file", "{count} files", count = f)
        ),
    }
}

fn subtitle(s: &Snapshot) -> String {
    tf!("{when} · {contents}", when = when(s), contents = contents(s))
}
