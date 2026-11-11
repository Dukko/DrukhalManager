//! The DLC dialog: a switch per installed DLC, written to `dlc_load.json`'s
//! `disabled_dlcs` when it closes, like Irony Mod Manager's DLC Manager.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;

use super::Window;
use crate::conflicts_view::plural;
use crate::mods::load_order;
use crate::paradox::dlc;

impl Window {
    pub(super) fn show_dlcs(self: &Rc<Self>) {
        let Some(game) = &self.game else {
            return self.toasts.add_toast(adw::Toast::new("Stellaris wasn't found, so its DLCs can't be listed"));
        };
        let dlcs = dlc::installed(&game.dir);
        if dlcs.is_empty() {
            return self.toasts.add_toast(adw::Toast::new("No DLCs are installed"));
        }
        let before: BTreeSet<String> = match load_order::DlcLoad::read(&self.paths.user_dir) {
            Ok(load) => load.disabled_dlcs.into_iter().collect(),
            Err(e) => return self.toasts.add_toast(adw::Toast::new(&format!("Could not read the DLC settings: {e}"))),
        };
        let disabled = Rc::new(RefCell::new(before.clone()));

        let page = adw::PreferencesPage::new();
        let all = adw::PreferencesGroup::builder()
            .description("DLCs that are off aren't loaded by the game. In multiplayer, every player needs the same DLCs on, except content packs such as portraits and music. Changes are saved when this closes.")
            .build();
        page.add(&all);
        let groups = [("Expansions", &["expansion"][..]), ("Story Packs", &["story_pack"]), ("Species Packs", &["species_pack"]), ("Content Packs", &["content_pack"])];
        let known: Vec<&str> = groups.iter().flat_map(|(_, c)| c.iter().copied()).collect();
        let mut switches = Vec::new();
        for (title, categories) in groups {
            // Anything unrecognized goes with the expansions.
            let members: Vec<&dlc::Dlc> = dlcs.iter().filter(|d| categories.contains(&d.category.as_str()) || (categories[0] == "expansion" && !known.contains(&d.category.as_str()))).collect();
            if members.is_empty() {
                continue;
            }
            let group = adw::PreferencesGroup::builder().title(title).build();
            for d in members {
                let row = adw::SwitchRow::builder().title(&d.name).active(!disabled.borrow().contains(&d.id)).use_markup(false).build();
                let image = gtk::Image::builder().pixel_size(40).css_classes(["mod-thumbnail"]).build();
                match &d.thumbnail {
                    Some(path) => image.set_from_file(Some(path)),
                    None => image.set_icon_name(Some("package-x-generic-symbolic")),
                }
                row.add_prefix(&image);
                let (id, disabled) = (d.id.clone(), disabled.clone());
                row.connect_active_notify(move |row| {
                    if row.is_active() {
                        disabled.borrow_mut().remove(&id);
                    } else {
                        disabled.borrow_mut().insert(id.clone());
                    }
                });
                switches.push(row.clone());
                group.add(&row);
            }
            page.add(&group);
        }
        let on = gtk::Button::builder().label("Turn All On").build();
        let switches = Rc::new(switches);
        on.connect_clicked({
            let switches = switches.clone();
            move |_| switches.iter().for_each(|s| s.set_active(true))
        });
        let header = adw::HeaderBar::new();
        header.pack_start(&on);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        let dialog = adw::Dialog::builder().title("DLC").content_width(560).content_height(680).child(&toolbar).build();

        let weak = Rc::downgrade(self);
        dialog.connect_closed(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let after = disabled.borrow().clone();
            if after == before {
                return;
            }
            let list: Vec<String> = after.iter().cloned().collect();
            match load_order::save_disabled_dlcs(&ui.paths.user_dir, &ui.paths.backup_dir, &list) {
                Ok(_) if list.is_empty() => ui.toasts.add_toast(adw::Toast::new("Every DLC is on")),
                Ok(_) => ui.toasts.add_toast(adw::Toast::new(&format!("{} off", plural(list.len(), "DLC is", "DLCs are")))),
                Err(e) => ui.toasts.add_toast(adw::Toast::builder().title(format!("Could not save the DLC settings: {e}")).timeout(0).build()),
            }
        });
        dialog.present(Some(&self.window));
    }
}
