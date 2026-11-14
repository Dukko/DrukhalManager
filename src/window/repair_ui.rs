//! Repairing stale or orphaned mod descriptors.

use std::rc::Rc;

use adw::prelude::*;

use super::Window;
use crate::paradox::steam::ItemState;
use crate::mods;

impl Window {
    pub(super) fn update_repair_banner(&self) {
        let findings = mods::repair::find(&self.paths.user_dir, &self.state.borrow().mods);
        let mut parts = Vec::new();
        if !findings.stale.is_empty() {
            parts.push(tnf!(
                "{count} mod descriptor points to a folder that no longer exists",
                "{count} mod descriptors point to folders that no longer exist",
                count = findings.stale.len()
            ));
        }
        if !findings.orphans.is_empty() {
            let state = self.state.borrow();
            let unsubscribed = findings
                .orphans
                .iter()
                .filter(|o| state.mods.iter().any(|m| m.id == o.id && m.steam == Some(ItemState::Unsubscribed)))
                .count();
            parts.push(match unsubscribed {
                0 => tnf!(
                    "{count} descriptor belongs to a mod whose files are gone",
                    "{count} descriptors belong to mods whose files are gone",
                    count = findings.orphans.len()
                ),
                n if n == findings.orphans.len() => tnf!(
                    "{count} mod you unsubscribed from in Steam is still listed",
                    "{count} mods you unsubscribed from in Steam are still listed",
                    count = n
                ),
                n => tnf!(
                    "{count} descriptor belongs to a mod whose files are gone, {unsubscribed} of them unsubscribed in Steam",
                    "{count} descriptors belong to mods whose files are gone, {unsubscribed} of them unsubscribed in Steam",
                    count = findings.orphans.len(),
                    unsubscribed = n
                ),
            });
        }
        self.repair_banner.set_title(&parts.join(&t!("; ")));
        self.repair_banner.set_revealed(!findings.is_empty());
    }

    pub(super) fn confirm_repair(self: &Rc<Self>) {
        let findings = mods::repair::find(&self.paths.user_dir, &self.state.borrow().mods);
        if findings.is_empty() {
            self.update_repair_banner();
            return;
        }
        let mut body = Vec::new();
        if !findings.stale.is_empty() {
            body.push(tnf!(
                "{count} descriptor will point to where the mod actually is, so the game can find it even when Steam doesn't say.",
                "{count} descriptors will point to where the mods actually are, so the game can find them even when Steam doesn't say.",
                count = findings.stale.len()
            ));
        }
        if !findings.orphans.is_empty() {
            let names: Vec<&str> = findings
                .orphans
                .iter()
                .take(8)
                .map(|o| o.name.as_str())
                .collect();
            let more = findings.orphans.len().saturating_sub(names.len());
            let more_text = if more > 0 {
                tnf!(" and {count} more", " and {count} more", count = more)
            } else {
                String::new()
            };
            body.push(tf!(
                "These descriptors have no mod files behind them and can be removed: {names}{more}.",
                names = names.join(", "),
                more = more_text
            ));
        }
        body.push(t!("Changed or removed descriptors are backed up first."));

        let dialog =
            adw::AlertDialog::new(Some(&t!("Repair Mod Descriptors?")), Some(&body.join("\n\n")));
        dialog.add_response("cancel", &t!("_Cancel"));
        match (findings.stale.is_empty(), findings.orphans.is_empty()) {
            (false, false) => {
                dialog.add_response("fix", &t!("_Fix Paths Only"));
                dialog.add_response("all", &t!("Fix and _Remove"));
                dialog.set_response_appearance("all", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("all"));
            }
            (false, true) => {
                dialog.add_response("fix", &t!("_Fix Paths"));
                dialog.set_response_appearance("fix", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("fix"));
            }
            (true, _) => {
                dialog.add_response("all", &t!("_Remove"));
                dialog.set_response_appearance("all", adw::ResponseAppearance::Destructive);
            }
        }
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            let remove: &[mods::repair::Orphan] = match response {
                "all" => &findings.orphans,
                "fix" => &[],
                _ => return,
            };
            ui.repair(&findings.stale, remove);
        });
        dialog.present(Some(&self.window));
    }

    pub(super) fn repair(
        self: &Rc<Self>,
        stale: &[mods::repair::StalePath],
        remove: &[mods::repair::Orphan],
    ) {
        match mods::repair::apply(&self.paths.user_dir, &self.paths.backup_dir, stale, remove) {
            Ok(backup) => {
                {
                    let mut state = self.state.borrow_mut();
                    for s in stale {
                        if let Some(m) = state.mods.iter_mut().find(|m| m.id == s.id) {
                            m.descriptor.path = Some(s.new.to_string_lossy().into_owned());
                        }
                    }
                    let ids: Vec<String> = remove.iter().map(|o| o.id.clone()).collect();
                    if !ids.is_empty() {
                        state.remove_mods(&ids);
                    }
                }
                self.refresh();
                self.update_repair_banner();
                let mut done = Vec::new();
                if !stale.is_empty() {
                    done.push(tnf!("fixed {count} path", "fixed {count} paths", count = stale.len()));
                }
                if !remove.is_empty() {
                    done.push(tnf!("removed {count} descriptor", "removed {count} descriptors", count = remove.len()));
                }
                let toast = adw::Toast::builder()
                    .title(tf!(
                        "Repaired descriptors: {done}. Backup in {backup}",
                        done = done.join(&t!(", ")),
                        backup = backup.display()
                    ))
                    .timeout(8)
                    .build();
                self.add_toast(toast);
            }
            Err(e) => self.add_toast(
                adw::Toast::builder()
                    .title(tf!("Could not repair descriptors: {error}", error = e))
                    .timeout(0)
                    .build(),
            ),
        }
    }
}
