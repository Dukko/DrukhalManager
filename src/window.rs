//! The main window: a load order editor.

use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::mods::{self, Mod};
use crate::paradox::game::GameInstall;

pub struct Paths {
    pub user_dir: PathBuf,
    pub backup_dir: PathBuf,
}

struct State {
    mods: Vec<Mod>,
    /// Indices into `mods`, in load order.
    enabled: Vec<usize>,
    dirty: bool,
}

impl State {
    fn new(mods: Vec<Mod>) -> Self {
        let mut enabled: Vec<usize> = (0..mods.len()).filter(|&i| mods[i].load_position.is_some()).collect();
        enabled.sort_by_key(|&i| mods[i].load_position);
        Self { mods, enabled, dirty: false }
    }

    fn disabled(&self) -> Vec<usize> {
        let mut disabled: Vec<usize> = (0..self.mods.len()).filter(|i| !self.enabled.contains(i)).collect();
        disabled.sort_by_cached_key(|&i| self.mods[i].descriptor.name.to_lowercase());
        disabled
    }

    /// Enabling appends to the end of the load order.
    fn set_enabled(&mut self, index: usize, on: bool) {
        match (on, self.enabled.iter().position(|&i| i == index)) {
            (true, None) => self.enabled.push(index),
            (false, Some(p)) => {
                self.enabled.remove(p);
            }
            _ => return,
        }
        self.dirty = true;
    }

    /// Moves the mod at load position `from` in front of the mod currently at
    /// `before` (`before == len` moves it to the end).
    fn move_enabled(&mut self, from: usize, mut before: usize) {
        if from >= self.enabled.len() {
            return;
        }
        if before > from {
            before -= 1;
        }
        if before == from {
            return;
        }
        let item = self.enabled.remove(from);
        self.enabled.insert(before.min(self.enabled.len()), item);
        self.dirty = true;
    }

    fn enabled_ids(&self) -> Vec<String> {
        self.enabled.iter().map(|&i| self.mods[i].id.clone()).collect()
    }
}

struct Window {
    window: adw::ApplicationWindow,
    paths: Paths,
    game: Option<GameInstall>,
    state: RefCell<State>,
    title: adw::WindowTitle,
    enabled_list: gtk::ListBox,
    disabled_list: gtk::ListBox,
    toasts: adw::ToastOverlay,
    save_action: gio::SimpleAction,
}

pub fn build(app: &adw::Application, paths: Paths, game: Option<GameInstall>, found: io::Result<Vec<Mod>>) {
    let title = adw::WindowTitle::new("DrukhalManager", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("DrukhalManager")
        .default_width(960)
        .default_height(720)
        .content(&toasts)
        .build();

    let found = match found {
        Ok(found) => found,
        Err(e) => {
            toolbar.set_content(Some(&status_page("Could Not Read Mods", &format!("{}: {e}", paths.user_dir.display()))));
            window.present();
            return;
        }
    };

    let launch = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().icon_name("media-playback-start-symbolic").label("Launch").build())
        .action_name("win.launch")
        .css_classes(["suggested-action"])
        .build();
    let save = gtk::Button::builder().label("Save").action_name("win.save").tooltip_text("Save load order (Ctrl+S)").build();
    header.pack_end(&launch);
    header.pack_end(&save);
    if game.is_none() {
        launch.set_tooltip_text(Some("Stellaris was not found in any Steam library"));
    }

    let enabled_list = boxed_list("No mods enabled");
    let disabled_list = boxed_list("Every mod is enabled");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&heading("Load Order"));
    content.append(&enabled_list);
    content.append(&heading("Disabled"));
    content.append(&disabled_list);
    let clamp = adw::Clamp::builder().maximum_size(900).child(&content).margin_top(12).margin_bottom(24).margin_start(12).margin_end(12).build();
    toolbar.set_content(Some(&gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build()));

    let save_action = gio::SimpleAction::new("save", None);
    let launch_action = gio::SimpleAction::new("launch", None);
    launch_action.set_enabled(game.is_some());
    window.add_action(&save_action);
    window.add_action(&launch_action);
    app.set_accels_for_action("win.save", &["<Control>s"]);

    let ui = Rc::new(Window {
        window: window.clone(),
        paths,
        game,
        state: RefCell::new(State::new(found)),
        title,
        enabled_list,
        disabled_list,
        toasts,
        save_action: save_action.clone(),
    });

    let weak = Rc::downgrade(&ui);
    save_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.save();
        }
    });
    let weak = Rc::downgrade(&ui);
    launch_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.launch();
        }
    });
    ui.refresh();
    // The window owns the only strong reference through this handler; every
    // other closure holds a weak one. Destroying the window drops it.
    let owner = ui.clone();
    window.connect_close_request(move |_| {
        if owner.state.borrow().dirty {
            owner.confirm_close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });

    window.present();
}

impl Window {
    fn refresh(self: &Rc<Self>) {
        let state = self.state.borrow();
        self.enabled_list.remove_all();
        self.disabled_list.remove_all();
        for (position, &index) in state.enabled.iter().enumerate() {
            self.enabled_list.append(&self.row(&state.mods[index], index, Some(position)));
        }
        for index in state.disabled() {
            self.disabled_list.append(&self.row(&state.mods[index], index, None));
        }

        let mut subtitle = Vec::new();
        if let Some(version) = self.game.as_ref().and_then(|g| g.version.as_deref()) {
            subtitle.push(format!("Stellaris {version}"));
        }
        subtitle.push(format!("{} of {} mods enabled", state.enabled.len(), state.mods.len()));
        if state.dirty {
            subtitle.push("Unsaved changes".to_owned());
        }
        self.title.set_subtitle(&subtitle.join(" · "));
        self.save_action.set_enabled(state.dirty);
    }

    /// Applies a state change after the current signal handler returns, since
    /// refreshing destroys the widget that emitted it.
    fn update(self: &Rc<Self>, change: impl FnOnce(&mut State) + 'static) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                change(&mut ui.state.borrow_mut());
                ui.refresh();
            }
        });
    }

    fn row(self: &Rc<Self>, m: &Mod, index: usize, position: Option<usize>) -> adw::ActionRow {
        let d = &m.descriptor;
        let name = if d.name.is_empty() { m.id.as_str() } else { d.name.as_str() };
        let source = match m.source {
            mods::Source::Workshop => "Workshop",
            mods::Source::Local => "Local",
        };
        let mut subtitle = vec![source.to_owned()];
        if !d.game_version().is_empty() {
            subtitle.push(format!("Stellaris {}", d.game_version()));
        }
        if !d.tags.is_empty() {
            subtitle.push(d.tags.join(", "));
        }

        let row = adw::ActionRow::builder().title(name).subtitle(subtitle.join(" · ")).use_markup(false).build();
        if let Some(content) = &m.content {
            row.set_tooltip_text(Some(&content.display().to_string()));
        }

        if let Some(error) = &m.error {
            let icon = gtk::Image::builder().icon_name("dialog-warning-symbolic").tooltip_text(error).css_classes(["warning"]).build();
            row.add_suffix(&icon);
        }

        let toggle = gtk::Switch::builder().active(position.is_some()).valign(gtk::Align::Center).build();
        let weak = Rc::downgrade(self);
        toggle.connect_active_notify(move |toggle| {
            let on = toggle.is_active();
            if let Some(ui) = weak.upgrade() {
                ui.update(move |state| state.set_enabled(index, on));
            }
        });
        row.add_suffix(&toggle);
        row.set_activatable_widget(Some(&toggle));

        if let Some(position) = position {
            let handle = gtk::Image::builder().icon_name("list-drag-handle-symbolic").css_classes(["dim-label"]).build();
            let number = gtk::Label::builder().label((position + 1).to_string()).width_chars(3).xalign(1.0).css_classes(["dim-label", "numeric"]).build();
            row.add_prefix(&handle);
            row.add_prefix(&number);
            self.make_reorderable(&row, position);
        }
        row
    }

    fn make_reorderable(self: &Rc<Self>, row: &adw::ActionRow, position: usize) {
        let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&(position as u32).to_value())));
        source.connect_drag_begin(glib::clone!(
            #[weak]
            row,
            move |source, _| {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            }
        ));
        row.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = Rc::downgrade(self);
        target.connect_drop(glib::clone!(
            #[weak]
            row,
            #[upgrade_or]
            false,
            move |_, value, _, y| {
                let (Ok(from), Some(ui)) = (value.get::<u32>(), weak.upgrade()) else {
                    return false;
                };
                // Dropping on the lower half of a row places the mod after it.
                let before = position + usize::from(y > f64::from(row.height()) / 2.0);
                ui.update(move |state| state.move_enabled(from as usize, before));
                true
            }
        ));
        row.add_controller(target);
    }

    fn save(self: &Rc<Self>) -> bool {
        let ids = self.state.borrow().enabled_ids();
        match mods::load_order::save(&self.paths.user_dir, &self.paths.backup_dir, &ids) {
            Ok(_) => {
                self.state.borrow_mut().dirty = false;
                self.refresh();
                self.toasts.add_toast(adw::Toast::new("Load order saved"));
                true
            }
            Err(e) => {
                self.toasts.add_toast(adw::Toast::builder().title(format!("Could not save load order: {e}")).timeout(0).build());
                false
            }
        }
    }

    fn launch(self: &Rc<Self>) {
        let Some(game) = &self.game else { return };
        if self.state.borrow().dirty && !self.save() {
            return;
        }
        match game.launch() {
            Ok(()) => self.toasts.add_toast(adw::Toast::new("Starting Stellaris…")),
            Err(e) => self.toasts.add_toast(adw::Toast::builder().title(format!("Could not start Stellaris: {e}")).timeout(0).build()),
        }
    }

    fn confirm_close(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(Some("Save Changes?"), Some("The load order has unsaved changes. Unsaved changes will be lost if you close."));
        dialog.add_responses(&[("cancel", "_Cancel"), ("discard", "_Discard"), ("save", "_Save")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            let close = match response {
                "save" => ui.save(),
                "discard" => true,
                _ => false,
            };
            if close {
                ui.state.borrow_mut().dirty = false;
                ui.window.close();
            }
        });
        dialog.present(Some(&self.window));
    }
}

fn boxed_list(placeholder: &str) -> gtk::ListBox {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.set_placeholder(Some(&gtk::Label::builder().label(placeholder).margin_top(12).margin_bottom(12).css_classes(["dim-label"]).build()));
    list
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).css_classes(["heading"]).build()
}

fn status_page(title: &str, description: &str) -> gtk::Widget {
    adw::StatusPage::builder().icon_name("applications-games-symbolic").title(title).description(description).build().upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(enabled: &[usize]) -> State {
        let mods = (0..4)
            .map(|i| Mod {
                id: format!("mod/{i}.mod"),
                descriptor: mods::Descriptor { name: format!("{i}"), ..Default::default() },
                content: None,
                source: mods::Source::Local,
                load_position: enabled.iter().position(|&e| e == i),
                error: None,
            })
            .collect();
        State::new(mods)
    }

    #[test]
    fn moves_within_load_order() {
        let mut s = state(&[0, 1, 2]);
        s.move_enabled(0, 2);
        assert_eq!(s.enabled, [1, 0, 2]);
        s.move_enabled(2, 0);
        assert_eq!(s.enabled, [2, 1, 0]);
        s.move_enabled(0, 3);
        assert_eq!(s.enabled, [1, 0, 2]);
        assert!(s.dirty);

        let mut s = state(&[0, 1]);
        s.move_enabled(0, 1);
        assert_eq!(s.enabled, [0, 1]);
        assert!(!s.dirty, "dropping a mod onto its own slot is a no-op");
    }

    #[test]
    fn toggling_appends_and_removes() {
        let mut s = state(&[2, 0]);
        assert_eq!(s.disabled(), [1, 3]);
        s.set_enabled(3, true);
        s.set_enabled(2, false);
        assert_eq!(s.enabled, [0, 3]);
        assert_eq!(s.enabled_ids(), ["mod/0.mod", "mod/3.mod"]);
    }
}
