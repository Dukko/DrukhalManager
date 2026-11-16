//! The objects enabled mods replace that a game update changed: the mods
//! still load their own copies, so the game's changes don't apply to them.
//! Each can be brought up to date with AI and saved to the merged patch.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::conflicts_view::{Analysis, MergeHooks};
use crate::diff_view::{Compare, Header, Pair};
use crate::merge::ai::{self, Checked, Versions};
use crate::merge::base_game::{Behind, Saved};
use crate::merge::patch::{Print, VANILLA};
use crate::merge::source::{self, Source};
use crate::merge_dialog;
use crate::thinking_view::{self, ThinkingView};

pub fn present(parent: &impl IsA<gtk::Widget>, analysis: Rc<Analysis>, hooks: Rc<MergeHooks>) {
    let Some((old, new)) = analysis.behind_versions.clone() else { return };
    let navigation = adw::NavigationView::new();

    let page = adw::PreferencesPage::new();
    let intro = adw::PreferencesGroup::builder()
        .description(t!("These mods still load their own copies, so the game's changes to these objects don't apply until the mods are updated."))
        .build();
    page.add(&intro);
    let mut by_mod: BTreeMap<usize, Vec<&Behind>> = BTreeMap::new();
    for b in &analysis.behind {
        by_mod.entry(b.mod_position).or_default().push(b);
    }
    for (position, objects) in by_mod {
        let group = adw::PreferencesGroup::builder().title(glib_escape(analysis.name(position))).build();
        for b in objects {
            let row = adw::ActionRow::builder().title(&b.key).subtitle(&b.category).use_markup(false).activatable(true).build();
            if b.now.is_none() {
                row.add_suffix(&gtk::Label::builder().label(t!("Removed from the game")).css_classes(["dim-label"]).build());
            }
            // Shown once the object is in the patch, including after a save
            // from the page this row opens.
            let updated = gtk::Label::builder().label(t!("Updated in the merged patch")).css_classes(["success"]).visible((hooks.is_merged)(&b.category, &b.key)).build();
            row.add_suffix(&updated);
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let (navigation, analysis, hooks, b, old, new) = (navigation.clone(), analysis.clone(), hooks.clone(), b.clone(), old.clone(), new.clone());
            row.connect_activated(move |_| {
                let updated = updated.clone();
                navigation.push(&detail(&analysis, &hooks, &navigation, &b, &old, &new, Rc::new(move || updated.set_visible(true))));
            });
            group.add(&row);
        }
        page.add(&group);
    }

    navigation.add(&wrap(&t!("Changed by the Game Update"), Some(&tf!("Objects your enabled mods replace that changed between Stellaris {old} and {new}", old = &old, new = &new)), &page));
    adw::Dialog::builder().content_width(900).content_height(720).child(&navigation).build().present(Some(parent));
}

/// Group titles are markup; mod names can hold `&` and `<`.
fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

/// A page with its own header bar.
fn wrap(title: &str, subtitle: Option<&str>, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, subtitle.unwrap_or_default())));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder().title(title).child(&toolbar).build()
}

/// Ties `value`'s lifetime to `widget`'s, so a pair keeps refreshing its
/// comparison while its widget is on screen.
fn keep_alive<T: 'static>(widget: &impl IsA<gtk::Widget>, value: Rc<T>) {
    let cell = std::cell::RefCell::new(Some(value));
    widget.connect_destroy(move |_| drop(cell.take()));
}

fn dim(text: String) -> gtk::Label {
    gtk::Label::builder().label(text).css_classes(["dim-label"]).wrap(true).xalign(0.0).margin_top(6).margin_bottom(12).margin_start(12).margin_end(12).build()
}

fn error_label() -> gtk::Label {
    gtk::Label::builder().css_classes(["error"]).wrap(true).xalign(0.0).margin_bottom(6).margin_start(12).margin_end(12).selectable(true).visible(false).build()
}

fn show_error(label: &gtk::Label, message: &str) {
    label.set_label(message);
    label.set_visible(true);
}

/// The mod's copy of the object as it is on disk now.
fn read_mod_copy(analysis: &Analysis, b: &Behind) -> Option<String> {
    let root = analysis.roots.get(b.mod_position)?.as_ref()?;
    let bytes = std::fs::read(root.join(&b.file)).ok()?;
    source::extract(&b.category, &b.key, String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}'))
}

/// What the page needs once the player asks for an update.
struct Update {
    analysis: Rc<Analysis>,
    hooks: Rc<MergeHooks>,
    navigation: adw::NavigationView,
    b: Behind,
    old: String,
    new: String,
    now: Saved,
    mod_text: String,
    /// Holds the comparison, and the review once there is a result.
    stack: gtk::Stack,
    on_saved: Rc<dyn Fn()>,
}

fn detail(analysis: &Rc<Analysis>, hooks: &Rc<MergeHooks>, navigation: &adw::NavigationView, b: &Behind, old: &str, new: &str, on_saved: Rc<dyn Fn()>) -> adw::NavigationPage {
    let pair = Pair::new(false);
    pair.show(&b.was.text, b.now.as_ref().map_or("", |n| n.text.as_str()), &Compare::Each, false);
    pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = old), subtitle: b.was.file.clone(), color: None, badge: None });
    pair.right.set_header(&Header {
        title: tf!("Stellaris {version}", version = new),
        subtitle: b.now.as_ref().map_or_else(|| t!("Removed from the game"), |n| n.file.clone()),
        color: None,
        badge: None,
    });
    pair.widget.set_vexpand(true);

    let note = dim(tf!("{name} still uses its own copy, from {file}.", name = analysis.name(b.mod_position), file = &b.file));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&pair.widget);
    body.append(&note);
    keep_alive(&body, pair);

    let stack = gtk::Stack::new();
    stack.add_named(&body, Some("compare"));

    // Objects the update removed have nothing to bring up to date with.
    if let Some(now) = &b.now {
        match read_mod_copy(analysis, b) {
            None => body.append(&dim(t!("The mod's copy couldn't be read, so it can't be updated here."))),
            Some(mod_text) => {
                let update = Rc::new(Update {
                    analysis: analysis.clone(),
                    hooks: hooks.clone(),
                    navigation: navigation.clone(),
                    b: b.clone(),
                    old: old.to_owned(),
                    new: new.to_owned(),
                    now: now.clone(),
                    mod_text,
                    stack: stack.clone(),
                    on_saved,
                });
                let error = error_label();
                let thinking = ThinkingView::new();
                let button = gtk::Button::builder().label(t!("Update with AI")).css_classes(["suggested-action"]).halign(gtk::Align::End).margin_start(12).margin_end(12).margin_bottom(12).build();
                body.append(&error);
                body.append(&button);
                let thinking_box = gtk::Box::builder().margin_start(12).margin_end(12).margin_bottom(12).build();
                thinking_box.append(&thinking.widget);
                thinking.widget.set_hexpand(true);
                body.append(&thinking_box);
                let (button_ref, error) = (button.clone(), error);
                button.connect_clicked(move |_| update.start(&button_ref, &error, &thinking));
            }
        }
    }
    wrap(&b.key, None, &stack)
}

impl Update {
    fn start(self: &Rc<Self>, button: &gtk::Button, error: &gtk::Label, thinking: &Rc<ThinkingView>) {
        let Some(provider) = (self.hooks.provider)() else {
            (self.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
            (self.hooks.open_preferences)();
            return;
        };
        let source = |label: String, file: &str, text: &str| Source { label, file: file.to_owned(), text: text.to_owned() };
        let before = source(tf!("Stellaris {version}", version = &self.old), &self.b.was.file, &self.b.was.text);
        let after = source(tf!("Stellaris {version}", version = &self.new), &self.now.file, &self.now.text);
        let mod_copy = source(self.analysis.name(self.b.mod_position).to_owned(), &self.b.file, &self.mod_text);
        let (category, key) = (self.b.category.clone(), self.b.key.clone());
        button.set_sensitive(false);
        error.set_visible(false);
        let (update, button, error, thinking) = (self.clone(), button.clone(), error.clone(), thinking.clone());
        glib::spawn_future_local(async move {
            let result = thinking_view::run(&thinking, None, move |on| {
                let check = |text: &str| merge_dialog::validate(&category, &key, text);
                ai::update_checked(&provider, &category, &key, &Versions { before: &before, after: &after, mod_copy: &mod_copy }, &check, on)
            })
            .await;
            button.set_sensitive(true);
            match result {
                Some(Ok(checked)) => update.review(checked),
                Some(Err(message)) => show_error(&error, &message),
                None => show_error(&error, &t!("The merge failed unexpectedly")),
            }
        });
    }

    /// Shows the result next to the mod's copy, to edit and save.
    fn review(self: &Rc<Self>, checked: Checked) {
        let (script, problem) = match checked.script {
            Ok(script) => (script, None),
            // Kept even when still rejected, to fix by hand.
            Err(problem) => (checked.merged.script, Some(problem)),
        };
        let pair = Pair::new(true);
        pair.show(&self.mod_text, &script, &Compare::Each, false);
        pair.left.set_header(&Header { title: self.analysis.name(self.b.mod_position).to_owned(), subtitle: self.b.file.clone(), color: None, badge: None });
        pair.right.set_header(&Header { title: t!("Updated Copy"), subtitle: String::new(), color: None, badge: None });
        pair.widget.set_vexpand(true);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&pair.widget);
        if !checked.merged.notes.trim().is_empty() {
            body.append(&dim(checked.merged.notes.clone()));
        }
        let error = error_label();
        if let Some(problem) = problem {
            show_error(&error, &tf!("The model's result still has a problem after a correction: {problem}. Fix it by hand, or undo and try again.", problem = problem));
        }
        body.append(&error);
        let discard = gtk::Button::builder().label(t!("Discard")).build();
        let save = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).build();
        let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).margin_start(12).margin_end(12).margin_bottom(12).build();
        buttons.append(&discard);
        buttons.append(&save);
        body.append(&buttons);

        let stack = self.stack.clone();
        discard.connect_clicked(move |_| stack.set_visible_child_name("compare"));
        let (update, weak, error) = (self.clone(), Rc::downgrade(&pair), error.clone());
        save.connect_clicked(move |_| {
            if let Some(pair) = weak.upgrade() {
                update.save(&pair.right.text(), &error);
            }
        });
        keep_alive(&body, pair);

        if let Some(old) = self.stack.child_by_name("review") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&body, Some("review"));
        self.stack.set_visible_child_name("review");
    }

    fn save(&self, text: &str, error: &gtk::Label) {
        let (category, key) = (&self.b.category, &self.b.key);
        let script = match merge_dialog::validate(category, key, text) {
            Ok(script) => script,
            Err(message) => return show_error(error, &message),
        };
        // Fingerprints of both sources, so the patch flags this copy as
        // outdated if the game or the mod changes again.
        let position = self.b.mod_position;
        let prints = vec![
            Print::new(VANILLA, "Vanilla", &self.now.file, &self.now.text),
            Print::new(&self.analysis.ids[position], self.analysis.name(position), &self.b.file, &self.mod_text),
        ];
        match (self.hooks.save_object)(category, key, &script, prints) {
            Ok(()) => {
                self.hooks.saved();
                (self.on_saved)();
                (self.hooks.toast)(&tf!("Saved the updated {key} to the merged patch", key = key));
                self.navigation.pop();
            }
            Err(message) => show_error(error, &message),
        }
    }
}
