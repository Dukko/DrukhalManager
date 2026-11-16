//! The object lookup dialog: which enabled mods define an object, from the
//! last conflict scan, whether or not another mod also defines it.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::conflicts_view::Analysis;
use crate::merge::conflicts::{self, ObjectConflict};

/// Results shown; a broad query would otherwise build thousands of rows.
const LIMIT: usize = 50;

pub fn present(parent: &impl IsA<gtk::Widget>, analysis: Rc<Analysis>) {
    let entry = gtk::SearchEntry::builder().placeholder_text(t!("Object name, event ID or localisation key")).hexpand(true).build();
    let results = adw::PreferencesGroup::new();
    let note = gtk::Label::builder().wrap(true).xalign(0.0).css_classes(["dim-label"]).build();
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
    content.append(&entry);
    content.append(&note);
    content.append(&results);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&adw::Clamp::builder().maximum_size(720).child(&content).build()).build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&t!("Look Up Objects"), &t!("Which of your enabled mods define an object, from the last conflict scan"))));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let shown: RefCell<Vec<adw::ExpanderRow>> = Default::default();
    let refresh = {
        let (results, note) = (results.clone(), note.clone());
        move |entry: &gtk::SearchEntry| {
            for row in shown.borrow_mut().drain(..) {
                results.remove(&row);
            }
            let query = entry.text();
            if query.trim().is_empty() {
                note.set_label(&t!("Type at least part of a name to search every object your enabled mods define"));
                return;
            }
            let found = analysis.report.look_up(&query, LIMIT);
            note.set_label(&if found.is_empty() {
                t!("No enabled mod defines an object by that name")
            } else if found.len() >= LIMIT {
                tf!("Only the first {count} are shown. Type more of the name to narrow them.", count = LIMIT)
            } else {
                String::new()
            });
            for object in &found {
                let row = result_row(&analysis, object);
                results.add(&row);
                shown.borrow_mut().push(row);
            }
        }
    };
    refresh(&entry);
    entry.connect_search_changed(refresh);

    let dialog = adw::Dialog::builder().content_width(720).content_height(640).child(&toolbar).build();
    dialog.set_focus(Some(&entry));
    dialog.present(Some(parent));
}

fn result_row(analysis: &Analysis, object: &ObjectConflict) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder().title(&object.key).subtitle(&object.category).use_markup(false).build();
    let winner = object.winner();
    for (i, d) in object.definitions.iter().enumerate() {
        let line = adw::ActionRow::builder().title(analysis.name(d.mod_position)).subtitle(&d.file).use_markup(false).build();
        if let Some((w, reason)) = winner.filter(|(w, _)| *w == i) {
            let badge = gtk::Label::builder().label(t!("In game")).valign(gtk::Align::Center).css_classes(["success"]).build();
            line.add_suffix(&badge);
            // The same sentence the conflict cards give for why it wins.
            line.set_tooltip_text(Some(&tf!("In game now, {name}'s version is used: {reason}.", name = analysis.name(object.definitions[w].mod_position), reason = reason.explain(conflicts::is_fios(&object.category)))));
        }
        row.add_row(&line);
    }
    row
}
