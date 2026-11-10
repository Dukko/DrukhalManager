//! The Keyboard Shortcuts dialog (`?` or `Ctrl+?`, and the main menu).

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Window;

/// Sections of (title, detailed action). Each item reads its keys from the
/// accelerators registered for the action, so the list can't go stale; an
/// action without one shows no keys.
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    ("General", &[("Preferences", "win.preferences"), ("Keyboard Shortcuts", "win.shortcuts")]),
    ("Load Order", &[("Save Load Order", "win.save"), ("Search Mods", "win.find"), ("Reload Mods", "win.reload"), ("Undo", "win.undo"), ("Redo", "win.redo")]),
    ("Pages", &[("Load Order", "win.page::load-order"), ("Conflicts", "win.page::conflicts")]),
];

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
        for (title, items) in SECTIONS {
            let section = adw::ShortcutsSection::new(Some(title));
            for (item, action) in *items {
                // Plain `?` is handled by hand (see above), so it isn't
                // among the accelerators `from_action` reads.
                let shortcut = if *action == "win.shortcuts" { adw::ShortcutsItem::new(item, "question <Control>question") } else { adw::ShortcutsItem::from_action(item, action) };
                section.add(shortcut);
            }
            dialog.add(section);
        }
        dialog.present(Some(&self.window));
    }
}
