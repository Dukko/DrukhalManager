//! The file merge dialog: combines several mods' copies of one file object
//! by object, uses AI only for objects the copies really disagree on, and
//! saves the result into the merged patch at the same path.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::conflicts_view::plural;
use crate::merge::ai::{self, Provider};
use crate::merge::file_merge::{self, Plan, Status, Version};
use crate::merge::patch::{Print, VANILLA};
use crate::merge_dialog;
use crate::thinking_view::{self, ThinkingView};

pub struct Request {
    pub path: String,
    /// `(descriptor id, mod name, mod folder)` for each copy, in load order.
    pub origins: Vec<(String, String, PathBuf)>,
    pub game_dir: Option<PathBuf>,
    pub provider: Option<Provider>,
    /// Whether the patch already holds a merge of this file.
    pub merged_before: bool,
    pub save: Box<dyn Fn(&str, Vec<Print>) -> Result<(), String>>,
    pub open_preferences: Box<dyn Fn()>,
    pub on_saved: Box<dyn Fn()>,
}

pub fn present(parent: &impl IsA<gtk::Widget>, request: Request) {
    let request = Rc::new(request);
    let plan: Rc<RefCell<Option<Plan>>> = Rc::default();

    let merge_button = gtk::Button::builder().label("Merge Conflicts").css_classes(["suggested-action"]).sensitive(false).build();
    let save_button = gtk::Button::builder().label("Save to Patch").sensitive(false).build();
    let file_name = request.path.rsplit_once('/').map_or(request.path.as_str(), |(_, f)| f);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(file_name, &request.path)));
    header.pack_end(&save_button);
    header.pack_end(&merge_button);
    let banner = adw::Banner::builder().use_markup(false).build();

    let summary = adw::PreferencesGroup::builder().title("Objects").description("Reading each mod's copy…").build();
    let editor = gtk::TextView::builder().monospace(true).wrap_mode(gtk::WrapMode::None).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
    let editor_scroll = gtk::ScrolledWindow::builder().child(&editor).min_content_height(320).css_classes(["card"]).build();
    let result = adw::PreferencesGroup::builder()
        .title("Merged File")
        .description("Review and edit before saving. Merging conflicts rebuilds this from the objects above, replacing manual edits.")
        .build();
    let thinking = ThinkingView::new();
    let result_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    result_box.append(&thinking.widget);
    result_box.append(&editor_scroll);
    result.add(&result_box);

    let page = adw::PreferencesPage::new();
    page.add(&summary);
    page.add(&result);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().content_width(960).content_height(800).child(&toolbar).build();

    if request.merged_before {
        show(&banner, "This file was merged before. Saving replaces that merge with this one.", false);
    }

    // One status label per conflicting object, updated as merges finish.
    let status_labels: Rc<RefCell<Vec<(usize, gtk::Label)>>> = Rc::default();

    let path = request.path.clone();
    let origins = request.origins.clone();
    let game_dir = request.game_dir.clone();
    let prints: Rc<RefCell<Vec<Print>>> = Rc::default();
    glib::spawn_future_local(glib::clone!(
        #[strong]
        prints,
        #[weak]
        summary,
        #[weak]
        editor,
        #[weak]
        merge_button,
        #[weak]
        save_button,
        #[weak]
        banner,
        #[strong]
        plan,
        #[strong]
        status_labels,
        #[strong]
        request,
        async move {
            let planned = gio::spawn_blocking(move || {
                let versions: Vec<Version> = origins
                    .iter()
                    .map(|(_, label, root)| {
                        let bytes = std::fs::read(root.join(&path)).map_err(|e| format!("Could not read {label}'s copy: {e}"))?;
                        Ok(Version { label: label.clone(), text: String::from_utf8_lossy(&bytes).into_owned() })
                    })
                    .collect::<Result<_, String>>()?;
                let vanilla = game_dir.and_then(|dir| std::fs::read(dir.join(&path)).ok()).map(|b| String::from_utf8_lossy(&b).into_owned());
                let mut found: Vec<Print> = vanilla.iter().map(|v| Print::new(VANILLA, "Vanilla", &path, v)).collect();
                found.extend(origins.iter().zip(&versions).map(|((id, label, _), v)| Print::new(id, label, &path, &v.text)));
                file_merge::plan(&path, vanilla.as_deref(), &versions).map(|plan| (plan, found))
            })
            .await;
            let p = match planned {
                Ok(Ok((p, found))) => {
                    prints.replace(found);
                    p
                }
                Ok(Err(message)) => {
                    summary.set_description(Some("This file can't be merged object by object."));
                    show(&banner, &message, true);
                    return;
                }
                Err(_) => {
                    show(&banner, "Reading the mods' copies failed unexpectedly", true);
                    return;
                }
            };

            let count = |f: fn(&Status) -> bool| p.items.iter().filter(|i| f(&i.status)).count();
            let (same, recovered, one, conflicts) = (
                count(|s| *s == Status::Same),
                count(|s| matches!(s, Status::Recovered(_))),
                count(|s| matches!(s, Status::OneChange(_))),
                count(|s| matches!(s, Status::Conflict { .. })),
            );
            let mut parts = vec![format!("{} kept as is", plural(same, "object", "objects"))];
            if recovered > 0 {
                parts.push(format!("{recovered} recovered from overridden copies"));
            }
            if one > 0 {
                parts.push(format!("{one} taken from the only mod that changed them"));
            }
            parts.push(if conflicts == 0 { "no conflicts".to_owned() } else { format!("{} to merge", plural(conflicts, "conflict", "conflicts")) });
            summary.set_description(Some(&parts.join(" · ")));

            for (index, item) in p.items.iter().enumerate() {
                let subtitle = match &item.status {
                    Status::Same => continue,
                    Status::Recovered(from) => format!("Recovered from {}", from.join(", ")),
                    Status::OneChange(from) => format!("Only {from} changes this; using its version"),
                    Status::Conflict { sources, .. } => format!("Changed differently by {}", sources.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(", ")),
                };
                let row = adw::ActionRow::builder().title(&item.key).subtitle(subtitle).use_markup(false).build();
                if let Status::Conflict { sources, .. } = &item.status {
                    let label = gtk::Label::builder().label(format!("Using {}'s version", sources.last().unwrap().label)).css_classes(["dim-label", "caption"]).build();
                    row.add_suffix(&label);
                    // Choosing a version settles the conflict without AI.
                    let labels: Vec<&str> = sources.iter().map(|s| s.label.as_str()).collect();
                    let choice = gtk::DropDown::from_strings(&labels);
                    choice.set_selected(labels.len() as u32 - 1);
                    choice.set_valign(gtk::Align::Center);
                    choice.set_tooltip_text(Some("Whose version to keep"));
                    let texts: Vec<(String, String)> = sources.iter().map(|s| (s.label.clone(), s.text.clone())).collect();
                    choice.connect_selected_notify(glib::clone!(
                        #[strong]
                        plan,
                        #[weak]
                        editor,
                        #[weak]
                        label,
                        move |choice| {
                            let Some((name, text)) = texts.get(choice.selected() as usize) else { return };
                            if let Some(p) = plan.borrow_mut().as_mut() {
                                p.items[index].text = text.clone();
                                p.items[index].resolved = true;
                                editor.buffer().set_text(&p.render());
                            }
                            label.set_label(&format!("Using {name}'s version"));
                            label.set_css_classes(&["success", "caption"]);
                        }
                    ));
                    row.add_suffix(&choice);
                    status_labels.borrow_mut().push((index, label));
                    row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
                }
                summary.add(&row);
            }

            editor.buffer().set_text(&p.render());
            save_button.set_sensitive(true);
            if conflicts > 0 {
                merge_button.set_label(&format!("Merge {} with AI", plural(conflicts, "Conflict", "Conflicts")));
                merge_button.set_sensitive(true);
                if let Some(provider) = &request.provider {
                    merge_button.set_tooltip_text(Some(&format!("Ask {} to merge each conflicting object", provider.model_name())));
                }
            } else {
                merge_button.set_visible(false);
            }
            plan.replace(Some(p));
        }
    ));

    merge_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        thinking,
        #[strong]
        plan,
        #[strong]
        status_labels,
        #[weak]
        dialog,
        #[weak]
        editor,
        #[weak]
        save_button,
        #[weak]
        banner,
        move |button| {
            let Some(provider) = request.provider.clone() else {
                dialog.close();
                (request.open_preferences)();
                return;
            };
            banner.set_revealed(false);
            button.set_sensitive(false);
            save_button.set_sensitive(false);
            editor.set_editable(false);
            glib::spawn_future_local(glib::clone!(
                #[strong]
                thinking,
                #[strong]
                plan,
                #[strong]
                status_labels,
                #[weak]
                button,
                #[weak]
                editor,
                #[weak]
                save_button,
                #[weak]
                banner,
                async move {
                    let pending: Vec<(usize, gtk::Label)> = status_labels.borrow().clone();
                    let mut failures = 0;
                    for (index, label) in pending {
                        let job = plan.borrow().as_ref().and_then(|p| {
                            let item = &p.items[index];
                            match &item.status {
                                Status::Conflict { sources, vanilla } if !item.resolved => Some((p.category.clone(), item.key.clone(), sources.clone(), vanilla.clone())),
                                _ => None,
                            }
                        });
                        let Some((category, key, sources, vanilla)) = job else { continue };
                        label.set_label("Merging…");
                        let provider = provider.clone();
                        let (c, k) = (category.clone(), key.clone());
                        let heading = key.clone();
                        let result = thinking_view::run(&thinking, Some(&heading), move |on| ai::merge(&provider, &c, &k, vanilla.as_ref(), &sources, on)).await;
                        let merged = match result {
                            Some(Ok(m)) => merge_dialog::validate(&category, &key, &m.script),
                            Some(Err(e)) => Err(e),
                            None => Err("failed unexpectedly".to_owned()),
                        };
                        match merged {
                            Ok(text) => {
                                if let Some(p) = plan.borrow_mut().as_mut() {
                                    p.items[index].text = text;
                                    p.items[index].resolved = true;
                                    editor.buffer().set_text(&p.render());
                                }
                                label.set_label("Merged");
                                label.set_css_classes(&["success", "caption"]);
                            }
                            Err(e) => {
                                failures += 1;
                                label.set_label("Merge failed; using the last mod's version");
                                label.set_tooltip_text(Some(&e));
                                label.set_css_classes(&["error", "caption"]);
                            }
                        }
                    }
                    editor.set_editable(true);
                    save_button.set_sensitive(true);
                    let unresolved = plan.borrow().as_ref().map_or(0, |p| p.conflicts().filter(|(_, i)| !i.resolved).count());
                    button.set_sensitive(unresolved > 0);
                    button.set_label(if unresolved > 0 { "Retry Failed" } else { "Merged" });
                    if failures > 0 {
                        show(&banner, &format!("{} couldn't be merged; hover the row for the reason. They keep the last mod's version.", plural(failures, "object", "objects")), true);
                    }
                }
            ));
        }
    ));

    save_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        plan,
        #[strong]
        prints,
        #[weak]
        dialog,
        #[weak]
        editor,
        #[weak]
        banner,
        move |_| {
            let Some(category) = plan.borrow().as_ref().map(|p| p.category.clone()) else { return };
            let buffer = editor.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            if let Err(message) = file_merge::validate(&category, &text) {
                show(&banner, &message, true);
                return;
            }
            match (request.save)(&text, prints.borrow().clone()) {
                Ok(()) => {
                    dialog.close();
                    (request.on_saved)();
                }
                Err(message) => show(&banner, &message, true),
            }
        }
    ));

    dialog.present(Some(parent));
}

fn show(banner: &adw::Banner, message: &str, error: bool) {
    banner.set_title(message);
    if error {
        banner.add_css_class("error");
    } else {
        banner.remove_css_class("error");
    }
    banner.set_revealed(true);
}
