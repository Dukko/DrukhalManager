//! The list of mods whose every file a later mod replaces, with buttons to
//! turn them off.

use std::rc::Rc;

use adw::prelude::*;

use super::{Window, display_name};

/// The most replacing mods named in a row before "and N more".
const NAMED: usize = 3;

impl Window {
    pub(super) fn show_unused(self: &Rc<Self>) {
        let Some(analysis) = self.analysis.borrow().clone() else { return };
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(t!("Mods That Change Nothing"))
            .description(t!("When two mods ship the same file, the game uses the copy from the one that loads last. Every file of these mods is replaced by mods loading after them, so turning them off changes nothing in game."))
            .build();
        let mut all: Vec<(String, String)> = Vec::new();
        {
            let state = self.state.borrow();
            if analysis.revision.get() != state.revision {
                return;
            }
            for u in &analysis.unused {
                let Some(id) = analysis.ids.get(u.mod_position) else { continue };
                let Some(index) = state.mods.iter().position(|m| &m.id == id) else { continue };
                if !state.enabled.contains(&index) {
                    continue;
                }
                let name = display_name(&state.mods[index]).to_owned();
                let named: Vec<&str> = u.by.iter().take(NAMED).filter_map(|&(m, _)| analysis.names.get(m).map(String::as_str)).collect();
                let more = u.by.len().saturating_sub(NAMED);
                let replacers = if more == 0 {
                    named.join(", ")
                } else {
                    tnf!("{mods} and {count} more", "{mods} and {count} more", count = more, mods = named.join(", "))
                };
                let row = adw::ActionRow::builder()
                    .title(&name)
                    .use_markup(false)
                    .subtitle(tf!("Every file it ships is replaced by {mods}", mods = replacers))
                    .subtitle_lines(0)
                    .build();
                let button = gtk::Button::builder().label(t!("Disable")).valign(gtk::Align::Center).css_classes(["flat"]).tooltip_text(tf!("Turn off {name}", name = &name)).build();
                let (weak, id_for_click, name_for_click) = (Rc::downgrade(self), id.clone(), name.clone());
                button.connect_clicked(move |button| {
                    if let Some(dialog) = button.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
                        dialog.close();
                    }
                    let Some(ui) = weak.upgrade() else { return };
                    {
                        let mut state = ui.state.borrow_mut();
                        let Some(index) = state.mods.iter().position(|m| m.id == id_for_click) else { return };
                        state.set_enabled(index, false);
                    }
                    ui.refresh();
                    ui.offer_undo(&tf!("Turned off {name}", name = &name_for_click));
                });
                row.add_suffix(&button);
                group.add(&row);
                all.push((id.clone(), name));
            }
        }
        if all.is_empty() {
            return;
        }
        page.add(&group);

        let header = adw::HeaderBar::new();
        let disable_all = gtk::Button::builder().label(t!("Disable All")).css_classes(["destructive-action"]).build();
        let weak = Rc::downgrade(self);
        disable_all.connect_clicked(move |button| {
            if let Some(dialog) = button.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
                dialog.close();
            }
            let Some(ui) = weak.upgrade() else { return };
            let count = {
                let mut state = ui.state.borrow_mut();
                let indices: Vec<usize> = all.iter().filter_map(|(id, _)| state.mods.iter().position(|m| &m.id == id)).collect();
                if !state.set_enabled_many(&indices, false) {
                    return;
                }
                indices.len()
            };
            ui.refresh();
            ui.offer_undo(&tnf!("Turned off {count} mod", "Turned off {count} mods", count = count));
        });
        header.pack_start(&disable_all);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        adw::Dialog::builder().title(t!("Mods That Change Nothing")).content_width(560).content_height(520).child(&toolbar).build().present(Some(&self.window));
    }
}
