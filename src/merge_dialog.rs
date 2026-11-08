//! The merge dialog: shows every version of one conflicting object, asks the
//! AI provider for a merged version, lets the user edit it, and saves it to
//! the merged patch.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::merge::ai::{self, Provider};
use crate::merge::patch::{Merge, Print, VANILLA};
use crate::merge::source::{self, Source};
use crate::paradox::script;
use crate::thinking_view::{self, ThinkingView};

/// Where one mod's version of the object lives.
pub struct Origin {
    /// Descriptor id, for fingerprinting.
    pub mod_id: String,
    pub label: String,
    pub root: PathBuf,
    pub file: String,
}

pub struct Request {
    pub category: String,
    pub key: String,
    /// In load order.
    pub origins: Vec<Origin>,
    pub game_dir: Option<PathBuf>,
    pub provider: Option<Provider>,
    pub existing: Option<Merge>,
    /// Writes the approved script to the patch.
    /// Writes the approved script, with fingerprints of its sources.
    pub save: Box<dyn Fn(&str, Vec<Print>) -> Result<(), String>>,
    pub open_preferences: Box<dyn Fn()>,
    pub on_saved: Box<dyn Fn()>,
}

struct Loaded {
    vanilla: Option<Source>,
    sources: Vec<Source>,
    missing: Vec<String>,
    /// Fingerprints of the versions found, to notice later changes.
    prints: Vec<Print>,
}

pub fn present(parent: &impl IsA<gtk::Widget>, request: Request) {
    let request = Rc::new(request);
    let loaded: Rc<RefCell<Option<Loaded>>> = Rc::default();

    let merge_button = gtk::Button::builder().label("Merge").css_classes(["suggested-action"]).sensitive(false).build();
    if let Some(p) = &request.provider {
        merge_button.set_tooltip_text(Some(&format!("Ask {} to merge these versions", p.model_name())));
    }
    let save_button = gtk::Button::builder().label("Save to Patch").sensitive(false).build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&request.key, &request.category)));
    header.pack_end(&save_button);
    header.pack_end(&merge_button);

    let versions = adw::PreferencesGroup::builder().title("Versions").description("Loading…").build();
    let editor = gtk::TextView::builder().monospace(true).wrap_mode(gtk::WrapMode::None).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
    let editor_scroll = gtk::ScrolledWindow::builder().child(&editor).min_content_height(260).css_classes(["card"]).build();
    let notes = gtk::Label::builder().xalign(0.0).wrap(true).selectable(true).css_classes(["dim-label"]).visible(false).build();
    let status = adw::Banner::new("");
    let result = adw::PreferencesGroup::builder().title("Merged Result").description("Merge to get a combined version, then review and edit it before saving").build();
    let result_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let thinking = ThinkingView::new();
    result_box.append(&thinking.widget);
    result_box.append(&editor_scroll);
    result_box.append(&notes);
    result.add(&result_box);

    let page = adw::PreferencesPage::new();
    page.add(&versions);
    page.add(&result);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&status);
    toolbar.set_content(Some(&page));

    let dialog = adw::Dialog::builder().content_width(960).content_height(760).child(&toolbar).build();

    if let Some(existing) = &request.existing {
        editor.buffer().set_text(&existing.script);
        notes.set_label(&format!("Saved merge of: {}", existing.sources.join(", ")));
        notes.set_visible(true);
        merge_button.set_label("Merge Again");
    }
    let buffer = editor.buffer();
    buffer.connect_changed(glib::clone!(
        #[weak]
        save_button,
        move |buffer| save_button.set_sensitive(buffer.char_count() > 0)
    ));
    save_button.set_sensitive(buffer.char_count() > 0);

    // Read every version off the main thread; vanilla lookups parse whole
    // game folders.
    let (category, key, game_dir) = (request.category.clone(), request.key.clone(), request.game_dir.clone());
    let origins: Vec<(String, String, PathBuf, String)> = request.origins.iter().map(|o| (o.mod_id.clone(), o.label.clone(), o.root.clone(), o.file.clone())).collect();
    glib::spawn_future_local(glib::clone!(
        #[weak]
        versions,
        #[weak]
        merge_button,
        #[weak]
        buffer,
        #[weak]
        notes,
        #[strong]
        loaded,
        async move {
            let result = gio::spawn_blocking(move || load(&category, &key, game_dir.as_deref(), origins)).await;
            let Ok(result) = result else {
                versions.set_description(Some("Could not read the mods' files"));
                return;
            };
            versions.set_description(Some(&match (&result.vanilla, result.missing.is_empty()) {
                (Some(_), true) => "The base game's version and each mod's version, in load order".to_owned(),
                (None, true) => "Each mod's version, in load order. The base game doesn't define this.".to_owned(),
                (_, false) => format!("Could not find the definition in: {}", result.missing.join(", ")),
            }));
            for s in result.vanilla.iter().chain(&result.sources) {
                versions.add(&version_row(s, &buffer, &notes));
            }
            merge_button.set_sensitive(result.sources.len() >= 2 || (result.vanilla.is_some() && !result.sources.is_empty()));
            loaded.replace(Some(result));
        }
    ));

    merge_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        thinking,
        #[strong]
        loaded,
        #[weak]
        dialog,
        #[weak]
        buffer,
        #[weak]
        notes,
        #[weak]
        status,
        move |button| {
            let Some(provider) = request.provider.clone() else {
                show_error(&status, "Set up an AI provider in Preferences first");
                dialog.close();
                (request.open_preferences)();
                return;
            };
            let Some((vanilla, sources)) = loaded.borrow().as_ref().map(|l| (l.vanilla.clone(), l.sources.clone())) else { return };
            status.set_revealed(false);
            button.set_sensitive(false);
            button.set_child(Some(&adw::Spinner::new()));
            let (category, key) = (request.category.clone(), request.key.clone());
            glib::spawn_future_local(glib::clone!(
                #[strong]
                thinking,
                #[weak]
                button,
                #[weak]
                buffer,
                #[weak]
                notes,
                #[weak]
                status,
                async move {
                    let result = thinking_view::run(&thinking, None, move |on| ai::merge(&provider, &category, &key, vanilla.as_ref(), &sources, on)).await;
                    button.set_label("Merge Again");
                    button.set_sensitive(true);
                    match result {
                        Some(Ok(merged)) => {
                            buffer.set_text(&merged.script);
                            notes.set_label(&merged.notes);
                            notes.set_visible(!merged.notes.is_empty());
                        }
                        Some(Err(message)) => show_error(&status, &message),
                        None => show_error(&status, "The merge failed unexpectedly"),
                    }
                }
            ));
        }
    ));

    save_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        loaded,
        #[weak]
        dialog,
        #[weak]
        buffer,
        #[weak]
        status,
        move |_| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let text = match validate(&request.category, &request.key, &text) {
                Ok(text) => text,
                Err(message) => {
                    show_error(&status, &message);
                    return;
                }
            };
            let prints = loaded.borrow().as_ref().map(|l| l.prints.clone()).unwrap_or_default();
            match (request.save)(&text, prints) {
                Ok(()) => {
                    dialog.close();
                    (request.on_saved)();
                }
                Err(message) => show_error(&status, &message),
            }
        }
    ));

    dialog.present(Some(parent));
}

fn load(category: &str, key: &str, game_dir: Option<&std::path::Path>, origins: Vec<(String, String, PathBuf, String)>) -> Loaded {
    let vanilla = game_dir.and_then(|dir| source::vanilla(dir, category, key));
    let mut prints: Vec<Print> = vanilla.iter().map(|v| Print::new(VANILLA, "Vanilla", &v.file, &v.text)).collect();
    let mut sources = Vec::new();
    let mut missing = Vec::new();
    for (mod_id, label, root, file) in origins {
        let text = std::fs::read(root.join(&file)).ok().and_then(|bytes| source::extract(category, key, &String::from_utf8_lossy(&bytes)));
        match text {
            Some(text) => {
                prints.push(Print::new(&mod_id, &label, &file, &text));
                sources.push(Source { label, file, text });
            }
            None => missing.push(label),
        }
    }
    Loaded { vanilla, sources, missing, prints }
}

/// Checks the edited script before it goes into the patch: it must parse,
/// and it must still define the object being merged. Returns the text to
/// save; for localisation, comments and blank lines are dropped.
pub fn validate(category: &str, key: &str, text: &str) -> Result<String, String> {
    if category == "localisation" {
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        let name = key.split_once(':').map_or(key, |(_, k)| k);
        return match lines[..] {
            [line] if line.split_once(':').is_some_and(|(k, _)| k == name) => Ok(line.to_owned()),
            _ => Err(format!("A localisation merge must be a single line starting with `{name}:`")),
        };
    }
    script::parse(text).map_err(|e| format!("The merged script has a syntax error at {e}"))?;
    if source::extract(category, key, text).is_none() {
        return Err(format!("The merged script must define `{key}`"));
    }
    Ok(text.trim().to_owned())
}

/// One version of the object, expandable to read it, with a button that
/// takes it as the result without asking the AI.
fn version_row(s: &Source, buffer: &gtk::TextBuffer, notes: &gtk::Label) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder().title(&s.label).subtitle(&s.file).use_markup(false).build();
    let use_it = gtk::Button::builder().label("Use This Version").valign(gtk::Align::Center).css_classes(["flat"]).tooltip_text("Keep this version as it is, without merging").build();
    use_it.connect_clicked(glib::clone!(
        #[weak]
        buffer,
        #[weak]
        notes,
        #[strong(rename_to = text)]
        s.text,
        #[strong(rename_to = label)]
        s.label,
        move |_| {
            buffer.set_text(&text);
            notes.set_label(&format!("Using {label}'s version as it is."));
            notes.set_visible(true);
        }
    ));
    row.add_suffix(&use_it);
    let text = gtk::Label::builder()
        .label(&s.text)
        .xalign(0.0)
        .selectable(true)
        .css_classes(["monospace"])
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    let scroll = gtk::ScrolledWindow::builder().child(&text).max_content_height(360).propagate_natural_height(true).build();
    row.add_row(&scroll);
    row
}

fn show_error(banner: &adw::Banner, message: &str) {
    banner.set_title(&glib::markup_escape_text(message));
    banner.set_revealed(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_merged_scripts() {
        assert!(validate("common/buildings", "building_lab", "@c = 1\nbuilding_lab = { cost = @c }").is_ok());
        assert!(validate("common/buildings", "building_lab", "building_lab = {").unwrap_err().contains("syntax error"));
        assert!(validate("common/buildings", "building_lab", "building_other = {}").unwrap_err().contains("must define"));
        assert!(validate("events", "ns.1", "country_event = { id = ns.1 }").is_ok());
        assert_eq!(validate("localisation", "l_english:lab", "# merged: kept B\n lab:0 \"Lab\" \n").unwrap(), "lab:0 \"Lab\"");
        assert!(validate("localisation", "l_english:lab", "lab:0 \"A\"\nlab:0 \"B\"").is_err());
        assert!(validate("localisation", "l_english:lab", "other:0 \"x\"").is_err());
    }
}
