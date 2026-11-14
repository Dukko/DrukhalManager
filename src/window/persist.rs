//! Saving the load order and mirroring it to the Paradox Launcher.

use std::rc::Rc;

use adw::prelude::*;

use super::Window;
use super::display_name;
use crate::mods;
use crate::paradox::launcher_db;

impl Window {
    pub(super) fn save(self: &Rc<Self>) -> bool {
        let ids = self.state.borrow().enabled_ids();
        match mods::load_order::save(&self.paths.user_dir, &self.paths.backup_dir, &ids) {
            Ok(_) => {
                self.state.borrow_mut().dirty = false;
                {
                    let mut collections = self.collections.borrow_mut();
                    let active = collections.active.clone();
                    if let Some(c) = collections.get_mut(&active) {
                        c.update(ids, &self.state.borrow().mods);
                    }
                    if let Err(e) = collections.save(&mods::collections::path()) {
                        self.add_toast(
                            adw::Toast::builder()
                                .title(tf!("Could not save collections: {error}", error = e))
                                .timeout(0)
                                .build(),
                        );
                    }
                }
                self.restore_banner.set_revealed(false);
                self.refresh();
                self.add_toast(self.sync_launcher());
                true
            }
            Err(e) => {
                self.add_toast(
                    adw::Toast::builder()
                        .title(tf!("Could not save load order: {error}", error = e))
                        .timeout(0)
                        .build(),
                );
                false
            }
        }
    }

    /// Mirrors the saved load order into the Paradox Launcher's playsets and
    /// returns a toast describing the outcome.
    pub(super) fn sync_launcher(&self) -> adw::Toast {
        let playset: Vec<launcher_db::PlaysetMod> = {
            let state = self.state.borrow();
            state
                .enabled
                .iter()
                .map(|&i| {
                    let m = &state.mods[i];
                    launcher_db::PlaysetMod {
                        registry_id: m.id.clone(),
                        steam_id: m.descriptor.remote_file_id,
                        name: display_name(m).to_owned(),
                        dir: m.content.clone(),
                        tags: m.descriptor.tags.clone(),
                        supported_version: m.descriptor.game_version().to_owned(),
                    }
                })
                .collect()
        };
        let playset_name = mods::collections::playset_name(&self.collections.borrow().active);
        match launcher_db::sync(&self.paths.user_dir, &self.paths.backup_dir, &playset_name, &playset) {
            Ok(None) => adw::Toast::new(&t!("Load order saved")),
            Ok(Some(synced)) if synced.missing.is_empty() => adw::Toast::new(&tf!("Load order saved and set as the “{playset}” playset in the Paradox Launcher", playset = playset_name)),
            Ok(Some(synced)) => {
                // Names can be long; they're listed behind Show.
                let toast = adw::Toast::builder()
                    .title(tnf!("Load order saved. The Paradox Launcher doesn't know {count} mod yet", "Load order saved. The Paradox Launcher doesn't know {count} mods yet", count = synced.missing.len()))
                    .button_label(t!("Show"))
                    .timeout(0)
                    .build();
                let window = self.window.downgrade();
                toast.connect_button_clicked(move |_| {
                    let mods = synced.missing.iter().map(|m| format!("• {m}")).collect::<Vec<_>>().join("\n");
                    let body = tnf!(
                        "Its “{playset}” playset leaves this mod out until the Launcher has seen it: open the Launcher once, then save again.\n\n{mods}",
                        "Its “{playset}” playset leaves these mods out until the Launcher has seen them: open the Launcher once, then save again.\n\n{mods}",
                        count = synced.missing.len(),
                        playset = &playset_name,
                        mods = mods
                    );
                    let dialog = adw::AlertDialog::new(Some(&t!("Not in the Paradox Launcher Yet")), Some(&body));
                    dialog.add_response("close", &t!("_Close"));
                    dialog.present(window.upgrade().as_ref());
                });
                toast
            }
            Err(e) => adw::Toast::builder().title(tf!("Load order saved, but the Paradox Launcher's playset wasn't updated: {error}", error = e)).timeout(0).build(),
        }
    }
}
