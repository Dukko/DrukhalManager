//! Saving the load order and mirroring it to the Paradox Launcher.

use std::rc::Rc;

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
                    collections.set_mods(&active, ids);
                    if let Err(e) = collections.save(&mods::collections::path()) {
                        self.toasts.add_toast(
                            adw::Toast::builder()
                                .title(format!("Could not save collections: {e}"))
                                .timeout(0)
                                .build(),
                        );
                    }
                }
                self.restore_banner.set_revealed(false);
                self.refresh();
                self.toasts.add_toast(self.sync_launcher());
                true
            }
            Err(e) => {
                self.toasts.add_toast(
                    adw::Toast::builder()
                        .title(format!("Could not save load order: {e}"))
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
            Ok(None) => adw::Toast::new("Load order saved"),
            Ok(Some(synced)) if synced.missing.is_empty() => adw::Toast::new(&format!("Load order saved and set as the “{playset_name}” playset in the Paradox Launcher")),
            Ok(Some(synced)) => adw::Toast::builder()
                .title(format!(
                    "Load order saved. The Paradox Launcher doesn't know {} yet, so its playset leaves {} out",
                    synced.missing.join(", "),
                    if synced.missing.len() == 1 { "it" } else { "them" }
                ))
                .timeout(0)
                .build(),
            Err(e) => adw::Toast::builder().title(format!("Load order saved, but the Paradox Launcher's playset wasn't updated: {e}")).timeout(0).build(),
        }
    }
}
