//! The Smart Sort dialog: previews the load order sorted into sections by
//! what each mod is, lets the user move a mod to another section, and
//! applies the order.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;
use crate::conflicts_view::plural;
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
}

impl Window {
    pub(super) fn open_smart_sort(self: &Rc<Self>) {
        let (mods, enabled) = {
            let state = self.state.borrow();
            (state.mods.clone(), state.enabled.clone())
        };
        if enabled.is_empty() {
            return self.toasts.add_toast(adw::Toast::new("No mods are enabled"));
        }
        let overrides: Rc<RefCell<HashMap<String, Category>>> = Rc::new(RefCell::new(
            self.settings.borrow().sort_categories.iter().filter_map(|(id, key)| Some((id.clone(), Category::from_key(key)?))).collect(),
        ));

        let cancel = gtk::Button::with_label("Cancel");
        let apply = gtk::Button::builder().label("Apply").css_classes(["suggested-action"]).sensitive(false).build();
        let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
        header.pack_start(&cancel);
        header.pack_end(&apply);
        let groups = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(24).margin_top(12).margin_bottom(24).margin_start(12).margin_end(12).build();
        let clamp = adw::Clamp::builder().maximum_size(720).child(&groups).build();
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&clamp).build();
        let loading = adw::StatusPage::builder().title("Reading Mods…").child(&adw::Spinner::new()).build();
        let stack = gtk::Stack::new();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&scroll, Some("plan"));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let dialog = adw::Dialog::builder().title("Smart Sort").content_width(720).content_height(760).child(&toolbar).build();
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
            let (input, current, overrides, groups, apply, render) = (input.clone(), current.clone(), overrides.clone(), groups.clone(), apply.clone(), Rc::downgrade(&render));
            move || {
                let input = input.borrow();
                let Some(input) = input.as_ref() else { return };
                let placed = smart_sort::plan(&input.mods, &input.enabled, &input.profiles, &overrides.borrow(), patch::ID);
                while let Some(child) = groups.first_child() {
                    groups.remove(&child);
                }
                let order: Vec<usize> = placed.iter().map(|p| p.index).collect();
                let moved = order.iter().zip(&input.enabled).filter(|(a, b)| a != b).count();
                let intro = gtk::Label::builder()
                    .label(if moved == 0 {
                        "Your load order already follows this order.".to_owned()
                    } else {
                        format!(
                            "Mods are grouped by what they change, the way the Stellaris community's load order guides suggest; {} change place. Sub-mods go directly below the mod they need, and every mod still loads after its dependencies. If a mod is in the wrong section, choose another for it; it's remembered.",
                            plural(moved, "mod", "mods")
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
                    let group = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&format!("{} · {}", category.title(), members.len())).as_str()).description(category.description()).build();
                    for p in members {
                        position += 1;
                        let m = &input.mods[p.index];
                        let was = input.enabled.iter().position(|&i| i == p.index).map_or(0, |at| at + 1);
                        let mut subtitle = p.reason.clone();
                        if was != position {
                            subtitle.push_str(&format!(" · was {was}"));
                        }
                        let row = adw::ActionRow::builder().title(m.descriptor.name.trim()).subtitle(&subtitle).use_markup(false).build();
                        row.add_prefix(&gtk::Label::builder().label(position.to_string()).width_chars(3).xalign(1.0).css_classes(["dim-label", "numeric"]).build());
                        if m.id != patch::ID {
                            row.add_suffix(&section_picker(p, &m.id, &overrides, render.clone()));
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
            #[weak]
            dialog,
            move |_| {
                {
                    let mut settings = ui.settings.borrow_mut();
                    settings.sort_categories = overrides.borrow().iter().map(|(id, c)| (id.clone(), c.key().to_owned())).collect();
                }
                ui.save_settings("the Smart Sort sections");
                let order: Vec<usize> = current.borrow().iter().map(|p| p.index).collect();
                dialog.close();
                if ui.state.borrow_mut().reorder(order) {
                    ui.refresh();
                    ui.offer_undo("Sorted the load order into sections");
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
                input.replace(Some(Input { mods, enabled, profiles }));
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
fn section_picker(p: &Placement, id: &str, overrides: &Rc<RefCell<HashMap<String, Category>>>, render: std::rc::Weak<Render>) -> gtk::DropDown {
    let mut labels = vec!["Automatic"];
    labels.extend(Category::ALL.iter().map(|c| c.title()));
    let picker = gtk::DropDown::from_strings(&labels);
    picker.set_valign(gtk::Align::Center);
    picker.set_tooltip_text(Some("Which section this mod goes in"));
    let chosen = overrides.borrow().get(id).copied();
    picker.set_selected(chosen.and_then(|c| Category::ALL.iter().position(|&x| x == c)).map_or(0, |at| at as u32 + 1));
    if chosen.is_none() {
        // Shows the section it's in, while still following the rules.
        picker.set_tooltip_text(Some(&format!("Automatic: {}", p.category.title())));
    }
    let (overrides, id) = (overrides.clone(), id.to_owned());
    picker.connect_selected_notify(move |picker| {
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
