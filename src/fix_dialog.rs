//! The fix dialog: repairs a mod file with brace mistakes, optionally asks
//! the AI provider to restore the intended nesting of the damaged objects,
//! and saves the fixed copy into the merged patch at the same path.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::merge::ai::{self, Provider};
use crate::merge::{file_merge, lint};
use crate::merge::patch::Print;
use crate::merge_dialog;
use crate::thinking_view::{self, ThinkingView};
use crate::paradox::script;

pub struct Request {
    pub path: String,
    pub mod_id: String,
    pub mod_name: String,
    pub root: PathBuf,
    /// The parse error the scan reported.
    pub error: String,
    pub provider: Option<Provider>,
    pub save: Box<dyn Fn(&str, Vec<Print>) -> Result<(), String>>,
    pub open_preferences: Box<dyn Fn()>,
    pub on_saved: Box<dyn Fn()>,
}

struct State {
    category: String,
    fixes: Vec<String>,
}

pub fn present(parent: &impl IsA<gtk::Widget>, request: Request) {
    let request = Rc::new(request);
    let state: Rc<RefCell<Option<State>>> = Rc::default();

    let ai_button = gtk::Button::builder().label(t!("Fix with AI")).css_classes(["suggested-action"]).visible(false).build();
    let save_button = gtk::Button::builder().label(t!("Save to Patch")).sensitive(false).build();
    let file_name = request.path.rsplit_once('/').map_or(request.path.as_str(), |(_, f)| f);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&tf!("Fix {file}", file = file_name), &request.mod_name)));
    header.pack_end(&save_button);
    header.pack_end(&ai_button);
    let banner = adw::Banner::builder().use_markup(false).build();

    let problem = adw::PreferencesGroup::builder()
        .title(t!("Problem"))
        .description(tf!(
            "{mod_name} · {error}. The game keeps reading after a brace mistake, but everything after it ends up nested inside the broken object and stops working.",
            mod_name = &request.mod_name, error = &request.error
        ))
        .build();
    let repairs = adw::PreferencesGroup::builder().title(t!("Repair")).description(t!("Repairing…")).build();
    let editor = gtk::TextView::builder().monospace(true).wrap_mode(gtk::WrapMode::None).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
    let editor_scroll = gtk::ScrolledWindow::builder().child(&editor).min_content_height(320).css_classes(["card"]).build();
    let result = adw::PreferencesGroup::builder().title(t!("Fixed File")).description(t!("Review and edit before saving. It replaces the mod's copy through the merged patch.")).build();
    let thinking = ThinkingView::new();
    let result_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    result_box.append(&thinking.widget);
    result_box.append(&crate::resize_grip::resizable(&editor_scroll));
    result.add(&result_box);

    let page = adw::PreferencesPage::new();
    page.add(&problem);
    page.add(&repairs);
    page.add(&result);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().content_width(960).content_height(800).child(&toolbar).build();

    let labels: Rc<RefCell<Vec<(String, gtk::Label)>>> = Rc::default();
    let print: Rc<RefCell<Option<Print>>> = Rc::default();
    let (path, root) = (request.path.clone(), request.root.clone());
    glib::spawn_future_local(glib::clone!(
        #[strong]
        print,
        #[strong]
        request,
        #[weak]
        repairs,
        #[weak]
        editor,
        #[weak]
        ai_button,
        #[weak]
        save_button,
        #[weak]
        banner,
        #[strong]
        state,
        #[strong]
        labels,
        async move {
            let loaded = gio::spawn_blocking(move || {
                let src = String::from_utf8_lossy(&std::fs::read(root.join(&path)).map_err(|e| e.to_string())?).into_owned();
                let category = file_merge::category(&path);
                let repair = lint::repair(&category, &src);
                Ok::<_, String>((category, src, repair))
            })
            .await;
            let Ok(Ok((category, src, repair))) = loaded else {
                show(&banner, &t!("Could not read the mod's file"));
                return;
            };
            // If the mod later changes this file, the fix may be out of date.
            print.replace(Some(Print::new(&request.mod_id, &request.mod_name, &request.path, &src)));
            save_button.set_sensitive(true);
            let Some(repair) = repair else {
                repairs.set_description(Some(&t!("This can't be repaired automatically. Fix it by hand below; the file is checked before saving.")));
                editor.buffer().set_text(&src);
                state.replace(Some(State { category, fixes: Vec::new() }));
                return;
            };

            repairs.set_description(Some(&tnf!(
                "Braces were fixed so the file parses and every object is separate again. {count} object may still be nested wrongly inside.",
                "Braces were fixed so the file parses and every object is separate again. {count} objects may still be nested wrongly inside.",
                count = repair.damaged.len()
            )));
            for fix in &repair.fixes {
                repairs.add(&adw::ActionRow::builder().title(fix).use_markup(false).build());
            }
            for key in &repair.damaged {
                let row = adw::ActionRow::builder().title(key).subtitle(t!("Braces changed; check its nesting")).use_markup(false).build();
                row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
                let label = gtk::Label::builder().label(t!("Not reviewed")).css_classes(["dim-label", "caption"]).build();
                row.add_suffix(&label);
                labels.borrow_mut().push((key.clone(), label));
                repairs.add(&row);
            }
            editor.buffer().set_text(&repair.text);
            ai_button.set_label(&tnf!("Fix {count} Object with AI", "Fix {count} Objects with AI", count = repair.damaged.len()));
            ai_button.set_visible(!repair.damaged.is_empty());
            state.replace(Some(State { category, fixes: repair.fixes }));
        }
    ));

    ai_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        thinking,
        #[strong]
        state,
        #[strong]
        labels,
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
            let Some((category, fixes)) = state.borrow().as_ref().map(|s| (s.category.clone(), s.fixes.clone())) else { return };
            banner.set_revealed(false);
            button.set_sensitive(false);
            save_button.set_sensitive(false);
            editor.set_editable(false);
            let mod_name = request.mod_name.clone();
            glib::spawn_future_local(glib::clone!(
                #[strong]
                thinking,
                #[strong]
                labels,
                #[weak]
                button,
                #[weak]
                editor,
                #[weak]
                save_button,
                #[weak]
                banner,
                async move {
                    let mut failures = 0;
                    for (key, label) in labels.borrow().clone() {
                        let buffer = editor.buffer();
                        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
                        let Some(range) = lint::object_span(&category, &key, &text) else {
                            label.set_label(&t!("Not found in the file"));
                            failures += 1;
                            continue;
                        };
                        label.set_label(&t!("Fixing…"));
                        let (provider, c, k, object, fixes, m) = (provider.clone(), category.clone(), key.clone(), text[range].to_owned(), fixes.clone(), mod_name.clone());
                        let heading = key.clone();
                        let result = thinking_view::run(&thinking, Some(&heading), move |on| ai::fix(&provider, &c, &k, &object, &fixes, &m, on)).await;
                        let fixed = match result {
                            Some(Ok(f)) => merge_dialog::validate(&category, &key, &f.script).map(|script| (script, f.notes)),
                            Some(Err(e)) => Err(e),
                            None => Err(t!("failed unexpectedly")),
                        };
                        match fixed.and_then(|(script, notes)| lint::replace_object(&category, &key, &text, &script).map(|t| (t, notes)).ok_or_else(|| t!("could not place the fix"))) {
                            Ok((new_text, notes)) => {
                                buffer.set_text(&new_text);
                                label.set_label(&t!("Fixed"));
                                label.set_tooltip_text(Some(&notes));
                                label.set_css_classes(&["success", "caption"]);
                            }
                            Err(e) => {
                                failures += 1;
                                label.set_label(&t!("Not fixed; check it by hand"));
                                label.set_tooltip_text(Some(&e));
                                label.set_css_classes(&["error", "caption"]);
                            }
                        }
                    }
                    editor.set_editable(true);
                    save_button.set_sensitive(true);
                    button.set_sensitive(failures > 0);
                    button.set_label(&if failures > 0 { t!("Retry") } else { t!("Fixed") });
                    if failures > 0 {
                        show(&banner, &tnf!("{count} object couldn't be fixed by AI; hover the row for the reason", "{count} objects couldn't be fixed by AI; hover the row for the reason", count = failures));
                    }
                }
            ));
        }
    ));

    save_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        print,
        #[weak]
        dialog,
        #[weak]
        editor,
        #[weak]
        banner,
        move |_| {
            let buffer = editor.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            if let Err(e) = script::parse(&text) {
                show(&banner, &tf!("The file still has a syntax error at {error}", error = e));
                return;
            }
            match (request.save)(&text, print.borrow().iter().cloned().collect()) {
                Ok(()) => {
                    dialog.close();
                    (request.on_saved)();
                }
                Err(message) => show(&banner, &message),
            }
        }
    ));

    dialog.present(Some(parent));
}

fn show(banner: &adw::Banner, message: &str) {
    banner.set_title(message);
    banner.set_revealed(true);
}
