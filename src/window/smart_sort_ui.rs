//! The Smart Sort dialog: previews the load order sorted into sections by
//! what each mod is, lets the user move a mod to another section, and
//! applies the order.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::merge::patch;
use crate::mods::smart_sort::{self, Category, Placement, Profile};
use crate::mods::{Mod, archive};

/// Rebuilds the preview; held weakly by the pickers inside it.
type Render = RefCell<Option<Box<dyn Fn()>>>;

/// What the dialog works from, read once when it opens.
struct Input {
    mods: Vec<Mod>,
    enabled: Vec<usize>,
    profiles: HashMap<usize, Profile>,
    /// Mods that belong directly below another, by index.
    below: HashMap<usize, usize>,
}

impl Window {
    pub(super) fn open_smart_sort(self: &Rc<Self>) {
        let (mods, enabled, below) = {
            let state = self.state.borrow();
            (state.mods.clone(), state.enabled.clone(), self.below_map(&state))
        };
        if enabled.is_empty() {
            return self.add_toast(adw::Toast::new(&t!("No mods are enabled")));
        }
        let overrides: Rc<RefCell<HashMap<String, Category>>> = Rc::new(RefCell::new(
            self.settings.borrow().sort_categories.iter().filter_map(|(id, key)| Some((id.clone(), Category::from_key(key)?))).collect(),
        ));
        let placed_by_hand: Rc<RefCell<BTreeMap<String, String>>> = Rc::new(RefCell::new(self.settings.borrow().placed_below.clone()));

        let cancel = gtk::Button::with_label(&t!("Cancel"));
        let apply = gtk::Button::builder().label(t!("Apply")).css_classes(["suggested-action"]).sensitive(false).build();
        let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
        header.pack_start(&cancel);
        header.pack_end(&apply);
        let groups = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(24).margin_top(12).margin_bottom(24).margin_start(12).margin_end(12).build();
        let clamp = adw::Clamp::builder().maximum_size(720).child(&groups).build();
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&clamp).build();
        let loading = adw::StatusPage::builder().title(t!("Reading Mods…")).child(&adw::Spinner::new()).build();
        let stack = gtk::Stack::new();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&scroll, Some("plan"));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let dialog = adw::Dialog::builder().title(t!("Smart Sort")).content_width(720).content_height(760).child(&toolbar).build();
        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));

        let input: Rc<RefCell<Option<Input>>> = Rc::default();
        let current: Rc<RefCell<Vec<Placement>>> = Rc::default();
        // Rebuilds the preview from the overrides.
        let render: Rc<Render> = Rc::default();
        *render.borrow_mut() = Some(Box::new({
            let (input, current, overrides, placed_by_hand, groups, apply, render) = (input.clone(), current.clone(), overrides.clone(), placed_by_hand.clone(), groups.clone(), apply.clone(), Rc::downgrade(&render));
            move || {
                let input = input.borrow();
                let Some(input) = input.as_ref() else { return };
                let by_hand = super::list::by_hand_indexes(&input.mods, &placed_by_hand.borrow());
                let placed = smart_sort::plan(&input.mods, &input.enabled, &input.profiles, &overrides.borrow(), &patch::is_patch, &input.below, &by_hand);
                while let Some(child) = groups.first_child() {
                    groups.remove(&child);
                }
                let order: Vec<usize> = placed.iter().map(|p| p.index).collect();
                let moved = order.iter().zip(&input.enabled).filter(|(a, b)| a != b).count();
                let intro = gtk::Label::builder()
                    .label(if moved == 0 {
                        t!("Your load order already follows this order.")
                    } else {
                        tnf!(
                            "Mods are grouped by what they change, the way the Stellaris community's load order guides suggest; {count} mod changes place. Sub-mods go directly below the mod they need, and every mod still loads after its dependencies. If a mod is in the wrong section, choose another for it; it's remembered.",
                            "Mods are grouped by what they change, the way the Stellaris community's load order guides suggest; {count} mods change place. Sub-mods go directly below the mod they need, and every mod still loads after its dependencies. If a mod is in the wrong section, choose another for it; it's remembered.",
                            count = moved
                        )
                    })
                    .wrap(true)
                    .xalign(0.0)
                    .css_classes(["dim-label"])
                    .build();
                groups.append(&intro);
                apply.set_sensitive(moved > 0);

                let mut position = 0;
                for category in Category::ALL {
                    let members: Vec<&Placement> = placed.iter().filter(|p| p.category == category).collect();
                    if members.is_empty() {
                        continue;
                    }
                    let group = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&tf!("{category} · {count}", category = category.title(), count = members.len())).as_str()).description(category.description()).build();
                    for p in members {
                        position += 1;
                        let m = &input.mods[p.index];
                        let was = input.enabled.iter().position(|&i| i == p.index).map_or(0, |at| at + 1);
                        let mut subtitle = p.reason.clone();
                        if was != position {
                            subtitle.push_str(&tf!(" · was {position}", position = was));
                        }
                        let row = adw::ActionRow::builder().title(m.descriptor.name.trim()).subtitle(&subtitle).use_markup(false).build();
                        row.add_prefix(&gtk::Label::builder().label(position.to_string()).width_chars(3).xalign(1.0).css_classes(["dim-label", "numeric"]).build());
                        if !patch::is_patch(&m.id) {
                            if by_hand.contains_key(&p.index) {
                                row.add_suffix(&forget_button(&m.id, &placed_by_hand, render.clone()));
                            }
                            row.add_suffix(&section_picker(p, &m.id, &overrides, &placed_by_hand, render.clone()));
                        }
                        group.add(&row);
                    }
                    groups.append(&group);
                }
                current.replace(placed);
            }
        }));

        apply.connect_clicked(glib::clone!(
            #[strong(rename_to = ui)]
            self,
            #[strong]
            current,
            #[strong]
            overrides,
            #[strong]
            placed_by_hand,
            #[weak]
            dialog,
            move |_| {
                {
                    let mut settings = ui.settings.borrow_mut();
                    settings.sort_categories = overrides.borrow().iter().map(|(id, c)| (id.clone(), c.key().to_owned())).collect();
                    settings.placed_below = placed_by_hand.borrow().clone();
                }
                ui.save_settings("the Smart Sort sections");
                let order: Vec<usize> = current.borrow().iter().map(|p| p.index).collect();
                dialog.close();
                if ui.state.borrow_mut().reorder(order) {
                    ui.refresh();
                    ui.offer_undo(&t!("Sorted the load order into sections"));
                }
            }
        ));

        // Reading every mod's folders takes a moment.
        let jobs: Vec<(usize, std::path::PathBuf)> = enabled.iter().filter_map(|&i| Some((i, mods[i].content.clone()?))).collect();
        glib::spawn_future_local(glib::clone!(
            #[strong]
            render,
            #[weak]
            stack,
            async move {
                let profiles = gio::spawn_blocking(move || jobs.into_iter().filter_map(|(i, content)| Some((i, Profile::read(&archive::files(&content).ok()?)))).collect::<HashMap<_, _>>())
                    .await
                    .unwrap_or_default();
                input.replace(Some(Input { mods, enabled, profiles, below }));
                if let Some(render) = render.borrow().as_ref() {
                    render();
                }
                stack.set_visible_child_name("plan");
            }
        ));
        dialog.present(Some(&self.window));
    }
}

/// A drop-down to put the mod with `id` in another section, or back in the
/// one Smart Sort finds for it.
fn section_picker(p: &Placement, id: &str, overrides: &Rc<RefCell<HashMap<String, Category>>>, placed_by_hand: &Rc<RefCell<BTreeMap<String, String>>>, render: std::rc::Weak<Render>) -> gtk::DropDown {
    let mut labels = vec![t!("Automatic")];
    labels.extend(Category::ALL.iter().map(|c| c.title().to_owned()));
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let picker = gtk::DropDown::from_strings(&labels);
    picker.set_valign(gtk::Align::Center);
    picker.set_tooltip_text(Some(&t!("Which section this mod goes in")));
    let chosen = overrides.borrow().get(id).copied();
    picker.set_selected(chosen.and_then(|c| Category::ALL.iter().position(|&x| x == c)).map_or(0, |at| at as u32 + 1));
    if chosen.is_none() {
        // Shows the section it's in, while still following the rules.
        picker.set_tooltip_text(Some(&tf!("Automatic: {category}", category = p.category.title())));
    }
    let (overrides, placed_by_hand, id) = (overrides.clone(), placed_by_hand.clone(), id.to_owned());
    picker.connect_selected_notify(move |picker| {
        placed_by_hand.borrow_mut().remove(&id);
        match picker.selected() {
            0 => overrides.borrow_mut().remove(&id),
            n => overrides.borrow_mut().insert(id.clone(), Category::ALL[n as usize - 1]),
        };
        // Rebuilt once this signal is done with the drop-down.
        let render = render.clone();
        glib::idle_add_local_once(move || {
            if let Some(render) = render.upgrade()
                && let Some(render) = render.borrow().as_ref()
            {
                render();
            }
        });
    });
    picker
}

/// A button to make Smart Sort forget where the user placed the mod with
/// `id`.
fn forget_button(id: &str, placed_by_hand: &Rc<RefCell<BTreeMap<String, String>>>, render: std::rc::Weak<Render>) -> gtk::Button {
    let button = gtk::Button::builder().icon_name("edit-clear-symbolic").valign(gtk::Align::Center).css_classes(["flat"]).tooltip_text(t!("Forget where you placed it, so Smart Sort decides")).build();
    let (placed_by_hand, id) = (placed_by_hand.clone(), id.to_owned());
    button.connect_clicked(move |_| {
        placed_by_hand.borrow_mut().remove(&id);
        // Rebuilt once this signal is done with the button.
        let render = render.clone();
        glib::idle_add_local_once(move || {
            if let Some(render) = render.upgrade()
                && let Some(render) = render.borrow().as_ref()
            {
                render();
            }
        });
    });
    button
}
