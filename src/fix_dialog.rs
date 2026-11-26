//! The fix dialog: repairs a mod file with brace mistakes, optionally asks
//! the AI provider to restore the intended nesting of the damaged objects,
//! and saves the fixed copy into the merged patch at the same path.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::diff_view::{Compare, Header, Pair};
use crate::merge::ai::{self, FixOutcome, Provider};
use crate::merge::{file_merge, lint};
use crate::merge::patch::Print;
use crate::merge_dialog;
use crate::thinking_view::{self, ThinkingView};
use crate::paradox::script;

#[allow(clippy::type_complexity)]
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
    damage: Vec<lint::Damage>,
}

/// An object's key, its status label and its row.
/// The open review panels, by object key.
type Panels = Rc<RefCell<Vec<(String, gtk::Box)>>>;

type Row = (String, gtk::Label, adw::ActionRow);

pub fn present(parent: &impl IsA<gtk::Widget>, request: Request) {
    let request = Rc::new(request);
    let state: Rc<RefCell<Option<State>>> = Rc::default();

    let ai_button = gtk::Button::builder().label(t!("Fix with AI")).css_classes(["suggested-action"]).visible(false).build();
    let stop_button = gtk::Button::builder().label(t!("Stop")).visible(false).build();
    let save_button = gtk::Button::builder()
        .label(t!("Save to Patch"))
        .tooltip_text(t!("Save to the merged patch"))
        .sensitive(false)
        .build();
    let file_name = request.path.rsplit_once('/').map_or(request.path.as_str(), |(_, f)| f);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&tf!("Fix {file}", file = file_name), &request.mod_name)));
    header.pack_end(&save_button);
    header.pack_end(&ai_button);
    header.pack_end(&stop_button);
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
    crate::diff_view::attach_clean_up_menu(&editor);
    let editor_scroll = gtk::ScrolledWindow::builder().child(&editor).min_content_height(320).css_classes(["card"]).build();
    let result = adw::PreferencesGroup::builder().title(t!("Fixed File")).description(t!("Review and edit before saving. It replaces the mod's copy through the merged patch.")).build();
    let review = adw::PreferencesGroup::builder()
        .title(t!("Review AI Fixes"))
        .description(t!("Nothing changes in the file below until you apply a fix."))
        .visible(false)
        .build();
    let pending: Panels = Rc::default();
    let thinking = ThinkingView::new();
    let result_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    result_box.append(&thinking.widget);
    result_box.append(&crate::resize_grip::resizable(&editor_scroll));
    result.add(&result_box);

    let page = adw::PreferencesPage::new();
    page.add(&problem);
    page.add(&repairs);
    page.add(&review);
    page.add(&result);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().content_width(960).content_height(800).child(&toolbar).build();
    crate::diff_view::bind_button_shortcuts(&dialog, &[("<Alt>m", &ai_button), ("<Control>s", &save_button)]);

    let labels: Rc<RefCell<Vec<Row>>> = Rc::default();
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
                state.replace(Some(State { category, damage: Vec::new() }));
                return;
            };

            let desc = if repair.damaged.is_empty() {
                t!("Braces were fixed unambiguously so the file parses cleanly. Review and save to the merged patch.")
            } else {
                tnf!(
                    "Braces were fixed so the file parses and every object is separate again. {count} object may still be nested wrongly inside.",
                    "Braces were fixed so the file parses and every object is separate again. {count} objects may still be nested wrongly inside.",
                    count = repair.damaged.len()
                )
            };
            repairs.set_description(Some(&desc));
            for fix in &repair.fixes {
                repairs.add(&adw::ActionRow::builder().title(fix).use_markup(false).build());
            }
            for damage in &repair.damaged {
                let row = adw::ActionRow::builder().title(&damage.key).subtitle(t!("Braces changed; check its nesting")).use_markup(false).build();
                row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
                let label = gtk::Label::builder().label(t!("Not reviewed")).css_classes(["dim-label", "caption"]).build();
                row.add_suffix(&label);
                labels.borrow_mut().push((damage.key.clone(), label, row.clone()));
                repairs.add(&row);
            }
            editor.buffer().set_text(&repair.text);
            ai_button.set_label(&tnf!("Fix {count} Object with AI", "Fix {count} Objects with AI", count = repair.damaged.len()));
            ai_button.set_visible(!repair.damaged.is_empty());
            state.replace(Some(State { category, damage: repair.damaged }));
        }
    ));

    let cancelled = Rc::new(Cell::new(false));
    stop_button.connect_clicked(glib::clone!(
        #[strong]
        cancelled,
        #[strong]
        thinking,
        move |btn| {
            cancelled.set(true);
            btn.set_sensitive(false);
            thinking.stop();
        }
    ));
    dialog.connect_closed(glib::clone!(
        #[strong]
        cancelled,
        #[strong]
        thinking,
        move |_| {
            cancelled.set(true);
            thinking.stop();
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
        #[strong]
        cancelled,
        #[strong]
        pending,
        #[weak]
        dialog,
        #[weak]
        editor,
        #[weak]
        save_button,
        #[weak]
        stop_button,
        #[weak]
        banner,
        #[weak]
        review,
        move |button| {
            let Some(provider) = request.provider.clone() else {
                dialog.close();
                (request.open_preferences)();
                return;
            };
            let Some((category, damage)) = state.borrow().as_ref().map(|s| (s.category.clone(), s.damage.clone())) else { return };
            banner.set_revealed(false);
            button.set_visible(false);
            stop_button.set_visible(true);
            stop_button.set_sensitive(true);
            cancelled.set(false);
            save_button.set_sensitive(false);
            editor.set_editable(false);
            let mod_name = request.mod_name.clone();
            glib::spawn_future_local(glib::clone!(
                #[strong]
                thinking,
                #[strong]
                labels,
                #[strong]
                cancelled,
                #[strong]
                pending,
                #[weak]
                review,
                #[weak]
                button,
                #[weak]
                editor,
                #[weak]
                save_button,
                #[weak]
                stop_button,
                #[weak]
                banner,
                async move {
                    let mut failures = 0;
                    let rows = labels.borrow().clone();
                    for (key, label, row) in rows {
                        if cancelled.get() || thinking.is_stopped() {
                            break;
                        }
                        if [t!("Fixed"), t!("Unchanged"), t!("Kept original"), t!("Review below")].contains(&label.text().to_string()) {
                            continue;
                        }
                        let Some(damage) = damage.iter().find(|d| d.key == key).cloned() else { continue };
                        let buffer = editor.buffer();
                        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
                        let Some(range) = lint::object_span(&category, &key, &text) else {
                            label.set_label(&t!("Not found in the file"));
                            failures += 1;
                            continue;
                        };
                        label.set_label(&t!("Fixing…"));
                        let object = text[range].to_owned();
                        let (provider, c, m, d, o) = (provider.clone(), category.clone(), mod_name.clone(), damage, object.clone());
                        let heading = key.clone();
                        let result = thinking_view::run_cancellable(&thinking, Some(&heading), move |cancel, on| {
                            let validate = |candidate: &str| merge_dialog::validate(&c, &d.key, candidate);
                            ai::fix_checked(&provider, &c, &d, &o, &m, &validate, Some(cancel), on)
                        })
                        .await;
                        if cancelled.get() || thinking.is_stopped() {
                            label.set_label(&t!("Stopped"));
                            label.set_css_classes(&["dim-label", "caption"]);
                            break;
                        }
                        match result.unwrap_or_else(|| Err(t!("failed unexpectedly"))) {
                            Ok(FixOutcome::Unchanged { notes }) => {
                                label.set_label(&t!("Unchanged"));
                                label.set_css_classes(&["success", "caption"]);
                                row.set_subtitle(&if notes.is_empty() { t!("The AI found nothing to change") } else { notes });
                            }
                            Ok(outcome) => {
                                label.set_label(&t!("Review below"));
                                label.set_css_classes(&["warning", "caption"]);
                                row.set_subtitle(&outcome_notes(&outcome));
                                let proposal = Proposal { key: key.clone(), category: category.clone(), original: object, outcome };
                                add_review(&review, &pending, &editor, &label, &button, proposal);
                            }
                            Err(e) => {
                                failures += 1;
                                label.set_label(&t!("Not fixed; check it by hand"));
                                label.set_css_classes(&["error", "caption"]);
                                row.set_subtitle(&e);
                            }
                        }
                    }
                    editor.set_editable(true);
                    save_button.set_sensitive(true);
                    stop_button.set_visible(false);
                    button.set_visible(true);
                    let stopped = cancelled.get() || thinking.is_stopped();
                    if stopped {
                        button.set_sensitive(true);
                        button.set_label(&t!("Retry"));
                        show(&banner, &t!("Fix stopped"));
                    } else {
                        button.set_sensitive(failures > 0);
                        button.set_label(&if failures > 0 {
                            t!("Retry")
                        } else if !pending.borrow().is_empty() {
                            t!("Review below")
                        } else {
                            t!("Done")
                        });
                        if failures > 0 {
                            show(&banner, &tnf!("{count} object couldn't be fixed by AI; see the row for the reason", "{count} objects couldn't be fixed by AI; see the rows for the reasons", count = failures));
                        }
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

struct Proposal {
    key: String,
    category: String,
    /// The object as it is in the editor.
    original: String,
    outcome: FixOutcome,
}

fn outcome_notes(outcome: &FixOutcome) -> String {
    match outcome {
        FixOutcome::Unchanged { notes } | FixOutcome::Options { notes, .. } | FixOutcome::Fixed { notes, .. } => notes.clone(),
    }
}

/// What each choice in a review puts in the object, `None` for keeping the
/// original, with a title and an explanation.
fn choices(proposal: &Proposal) -> Vec<(String, String, Option<String>)> {
    let mut choices = vec![(t!("Keep original"), t!("Leave the object as the mechanical repair made it"), None)];
    match &proposal.outcome {
        FixOutcome::Fixed { script, notes, removed, shrank } => {
            let mut about = notes.clone();
            if !removed.is_empty() {
                about = format!("{about}\n{}", tf!("Removes: {lines}", lines = removed.join(" | ")));
            }
            if *shrank {
                about = format!("{about}\n{}", t!("The result is shorter and the AI didn't say what it dropped"));
            }
            choices.push((t!("Apply the fix"), about.trim().to_owned(), Some(script.clone())));
        }
        FixOutcome::Options { options, .. } => {
            for (n, option) in options.iter().enumerate() {
                choices.push((tf!("Option {number}", number = n + 1), option.notes.clone(), Some(option.script.clone())));
            }
        }
        FixOutcome::Unchanged { .. } => {}
    }
    choices
}

/// Shows what the AI proposes for one object as a before/after diff, and
/// puts the chosen version into the editor only when the user applies it.
fn add_review(review: &adw::PreferencesGroup, pending: &Panels, editor: &gtk::TextView, label: &gtk::Label, retry: &gtk::Button, proposal: Proposal) {
    let choices = choices(&proposal);
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let heading = gtk::Label::builder().label(&proposal.key).xalign(0.0).css_classes(["heading"]).build();
    let notes = gtk::Label::builder().label(outcome_notes(&proposal.outcome)).xalign(0.0).wrap(true).selectable(true).build();
    panel.append(&heading);
    panel.append(&notes);

    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let pair = Pair::new(false);
    pair.left.set_header(&Header { title: t!("Before"), subtitle: t!("The object now"), ..Header::default() });
    pair.right.set_header(&Header { title: t!("After"), ..Header::default() });
    for card in [&pair.left, &pair.right] {
        card.scroll.set_min_content_height(240);
    }
    let apply = gtk::Button::builder().css_classes(["suggested-action"]).halign(gtk::Align::End).build();
    let selected: Rc<RefCell<Option<String>>> = Rc::default();
    let mut first: Option<gtk::CheckButton> = None;
    for (n, (title, about, script)) in choices.iter().enumerate() {
        let row = adw::ActionRow::builder().title(title).subtitle(about).use_markup(false).build();
        let radio = gtk::CheckButton::new();
        match &first {
            Some(group) => radio.set_group(Some(group)),
            None => first = Some(radio.clone()),
        }
        row.add_prefix(&radio);
        row.set_activatable_widget(Some(&radio));
        list.append(&row);
        let preselected = if matches!(&proposal.outcome, FixOutcome::Fixed { shrank: true, .. }) { n == 0 } else { n == 1 };
        let (pair, apply, selected) = (pair.clone(), apply.clone(), selected.clone());
        let (original, script, title) = (proposal.original.clone(), script.clone(), title.clone());
        let show = move || {
            pair.show(&original, script.as_deref().unwrap_or(&original), &Compare::Each, false);
            apply.set_label(&if script.is_some() { t!("Apply") } else { title.clone() });
            selected.replace(script.clone());
        };
        radio.connect_toggled(move |radio| {
            if radio.is_active() {
                show();
            }
        });
        radio.set_active(preselected);
    }
    panel.append(&list);
    pair.widget.set_vexpand(false);
    panel.append(&pair.widget);
    panel.append(&apply);
    let message = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["error"]).visible(false).build();
    panel.append(&message);
    let replaced = pending.borrow().iter().position(|(key, _)| *key == proposal.key);
    if let Some(at) = replaced {
        let (_, old) = pending.borrow_mut().remove(at);
        review.remove(&old);
    }
    review.add(&panel);
    review.set_visible(true);
    pending.borrow_mut().push((proposal.key.clone(), panel.clone()));

    apply.connect_clicked(glib::clone!(
        #[weak]
        review,
        #[weak]
        panel,
        #[weak]
        editor,
        #[weak]
        label,
        #[weak]
        retry,
        #[weak]
        message,
        #[strong]
        pending,
        #[strong]
        selected,
        move |_| {
            match selected.borrow().as_deref() {
                None => {
                    label.set_label(&t!("Kept original"));
                    label.set_css_classes(&["dim-label", "caption"]);
                }
                Some(script) => {
                    let buffer = editor.buffer();
                    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
                    let current = lint::object_span(&proposal.category, &proposal.key, &text).map(|range| &text[range]);
                    if current != Some(proposal.original.as_str()) {
                        label.set_label(&t!("Not fixed; check it by hand"));
                        label.set_css_classes(&["error", "caption"]);
                        message.set_label(&t!("The object changed after this fix was made. Use Retry so the AI sees the current text."));
                        message.set_visible(true);
                        retry.set_label(&t!("Retry"));
                        retry.set_sensitive(true);
                        retry.set_visible(true);
                        return;
                    }
                    let Some(new_text) = lint::replace_object(&proposal.category, &proposal.key, &text, script) else {
                        label.set_label(&t!("Not found in the file"));
                        label.set_css_classes(&["error", "caption"]);
                        return;
                    };
                    buffer.set_text(&new_text);
                    label.set_label(&t!("Fixed"));
                    label.set_css_classes(&["success", "caption"]);
                }
            }
            review.remove(&panel);
            pending.borrow_mut().retain(|(_, p)| *p != panel);
            review.set_visible(!pending.borrow().is_empty());
        }
    ));
}

fn show(banner: &adw::Banner, message: &str) {
    banner.set_title(message);
    banner.set_revealed(true);
}
