//! The list of mods that load above a mod replacing most of their files,
//! with a button to move each one below it.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;

use super::state::State;
use super::{Window, display_name};

/// Where the mod with `id` is: its index in `state.mods` and its load position.
fn find(state: &State, id: &str) -> Option<(usize, usize)> {
    let index = state.mods.iter().position(|m| m.id == id)?;
    let position = state.enabled.iter().position(|&i| i == index)?;
    Some((index, position))
}

impl Window {
    /// For each mod the files say belongs below another, the index of that
    /// mod, both indexes into `state.mods`. Relations don't depend on the
    /// load order, so a scan of an older order still counts.
    pub(super) fn below_map(&self, state: &State) -> HashMap<usize, usize> {
        let analysis = self.analysis.borrow();
        let Some(analysis) = analysis.as_ref() else { return HashMap::new() };
        let index = |position: usize| {
            let id = analysis.ids.get(position)?;
            state.mods.iter().position(|m| &m.id == id)
        };
        // The best relation for each patch: most files replaced, then the
        // target that loads first.
        let mut best: HashMap<usize, (usize, usize)> = HashMap::new();
        for r in &analysis.order {
            let better = best.get(&r.patch).is_none_or(|&(replaced, target)| (r.replaced, std::cmp::Reverse(r.target)) > (replaced, std::cmp::Reverse(target)));
            if better {
                best.insert(r.patch, (r.replaced, r.target));
            }
        }
        best.into_iter().filter_map(|(patch, (_, target))| Some((index(patch)?, index(target)?))).collect()
    }

    pub(super) fn show_misplaced(self: &Rc<Self>) {
        let Some(analysis) = self.analysis.borrow().clone() else { return };
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(t!("Changes Thrown Away by Load Order"))
            .description(t!("When two mods ship the same file, the game uses the copy from the one that loads last. These mods load above a mod that ships most of their files in other versions, so their changes don't show in game."))
            .build();
        {
            let state = self.state.borrow();
            if analysis.revision.get() != state.revision {
                return;
            }
            for r in analysis.order.iter().filter(|r| r.misplaced()) {
                let (Some(patch_id), Some(target_id)) = (analysis.ids.get(r.patch), analysis.ids.get(r.target)) else { continue };
                let (Some((patch, _)), Some((target, _))) = (find(&state, patch_id), find(&state, target_id)) else { continue };
                let (patch_name, target_name) = (display_name(&state.mods[patch]).to_owned(), display_name(&state.mods[target]).to_owned());
                let row = adw::ActionRow::builder()
                    .title(tf!("{patch} loads above {target}", patch = &patch_name, target = &target_name))
                    .use_markup(false)
                    .subtitle(tnf!("{target} replaces {count} of its {total} file", "{target} replaces {count} of its {total} files", count = r.replaced, target = &target_name, total = r.of))
                    .build();
                let button = gtk::Button::builder()
                    .label(t!("Move Below"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .tooltip_text(tf!("Move {patch} directly below {target}", patch = &patch_name, target = &target_name))
                    .build();
                let (weak, patch_id, target_id) = (Rc::downgrade(self), patch_id.clone(), target_id.clone());
                button.connect_clicked(move |button| {
                    if let Some(dialog) = button.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
                        dialog.close();
                    }
                    let Some(ui) = weak.upgrade() else { return };
                    {
                        // The order may have changed since the list was built.
                        let mut state = ui.state.borrow_mut();
                        let (Some((_, from)), Some((_, to))) = (find(&state, &patch_id), find(&state, &target_id)) else { return };
                        state.move_enabled(from, to + 1);
                    }
                    ui.refresh();
                    ui.offer_undo(&tf!("Moved {patch} below {target}", patch = &patch_name, target = &target_name));
                });
                row.add_suffix(&button);
                group.add(&row);
            }
        }
        page.add(&group);

        let header = adw::HeaderBar::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        adw::Dialog::builder().title(t!("Load Order")).content_width(560).content_height(520).child(&toolbar).build().present(Some(&self.window));
    }
}
