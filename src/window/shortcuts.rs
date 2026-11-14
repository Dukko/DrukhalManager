//! The Keyboard Shortcuts dialog (`?` or `Ctrl+?`, and the main menu).

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;

/// Sections of (title, detailed action). Each item reads its keys from the
/// accelerators registered for the action, so the list can't go stale; an
/// action without one shows no keys. The titles are made when the dialog
/// opens, so they follow the language.
fn sections() -> Vec<(String, Vec<(String, &'static str)>)> {
    vec![
        (
            t!("General"),
            vec![(t!("Preferences"), "win.preferences"), (t!("Keyboard Shortcuts"), "win.shortcuts")],
        ),
        (
            t!("Load Order"),
            vec![
                (t!("Save Load Order"), "win.save"),
                (t!("Search Mods"), "win.find"),
                (t!("Reload Mods"), "win.reload"),
                (t!("Undo"), "win.undo"),
                (t!("Redo"), "win.redo"),
            ],
        ),
        (
            t!("Pages"),
            vec![(t!("Load Order"), "win.page::load-order"), (t!("Conflicts"), "win.page::conflicts")],
        ),
    ]
}

impl Window {
    pub(super) fn add_shortcuts_action(self: &Rc<Self>, app: &adw::Application) {
        let action = gio::SimpleAction::new("shortcuts", None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.show_shortcuts();
            }
        });
        self.window.add_action(&action);
        app.set_accels_for_action("win.shortcuts", &["<Control>question"]);
        for (action, accel, _) in crate::conflicts_view::shortcuts() {
            app.set_accels_for_action(&format!("conflicts.{action}"), &[accel]);
        }

        // A plain `?` can't be an accelerator: those run before the focused
        // widget, so it couldn't be typed into the search. Handle it only
        // when the focus isn't in a text field.
        let keys = gtk::EventControllerKey::new();
        let window = self.window.downgrade();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let plain = !modifiers.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::SUPER_MASK);
            let Some(window) = window.upgrade() else { return glib::Propagation::Proceed };
            let typing = gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|w| w.is::<gtk::Text>() || w.is::<gtk::TextView>() || w.is::<gtk::Editable>());
            if key == gtk::gdk::Key::question && plain && !typing {
                let _ = WidgetExt::activate_action(&window, "win.shortcuts", None);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
    }

    fn show_shortcuts(&self) {
        let dialog = adw::ShortcutsDialog::new();
        for (title, items) in sections() {
            let section = adw::ShortcutsSection::new(Some(&title));
            for (item, action) in items {
                // Plain `?` is handled by hand (see above), so it isn't
                // among the accelerators `from_action` reads.
                let shortcut = if action == "win.shortcuts" { adw::ShortcutsItem::new(&item, "question <Control>question") } else { adw::ShortcutsItem::from_action(&item, action) };
                section.add(shortcut);
            }
            dialog.add(section);
        }
        let conflicts = adw::ShortcutsSection::new(Some(&t!("Conflicts")));
        for (action, _, title) in crate::conflicts_view::shortcuts() {
            conflicts.add(adw::ShortcutsItem::from_action(&title, &format!("conflicts.{action}")));
        }
        // These belong to the comparison, not to an action.
        conflicts.add(adw::ShortcutsItem::new(&t!("Next Difference"), "<Alt>Down"));
        conflicts.add(adw::ShortcutsItem::new(&t!("Previous Difference"), "<Alt>Up"));
        dialog.add(conflicts);
        dialog.present(Some(&self.window));
    }
}
