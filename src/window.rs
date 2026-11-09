//! The main window: a load order editor.

mod collections_ui;
mod conflicts_ui;
mod launch;
mod list;
mod persist;
mod repair_ui;
mod shortcuts;
mod state;
mod thumbnails;
mod updates;

use std::cell::{Cell, RefCell};
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use state::State;

use crate::conflicts_view::Analysis;
use crate::merge::patch::{self, Patch};
use crate::mods::Mod;
use crate::mods::collections::Collections;
use crate::paradox::game::GameInstall;
use crate::preferences;
use crate::secrets;
use crate::settings::{self, Settings};

pub struct Paths {
    pub user_dir: PathBuf,
    pub backup_dir: PathBuf,
}

struct Window {
    window: adw::ApplicationWindow,
    thumbnails: RefCell<thumbnails::Cache>,
    paths: Paths,
    game: Option<GameInstall>,
    state: RefCell<State>,
    status: gtk::Label,
    launch_menu: gio::Menu,
    /// Checked when every mod is enabled, mixed when some are.
    all_toggle: gtk::CheckButton,
    /// Set while `refresh` updates `all_toggle`, so it isn't taken as a click.
    syncing: Cell<bool>,
    /// Shown when some mods load before mods they depend on.
    sort_button: gtk::Button,
    search: gtk::SearchEntry,
    /// Offers to repair descriptors with stale paths or no mod behind them.
    repair_banner: adw::Banner,
    stack: adw::ViewStack,
    conflicts_page: adw::Bin,
    analysis: RefCell<Option<Rc<Analysis>>>,
    scanning: Cell<bool>,
    /// Whether the Conflicts page shows ignored conflicts (dimmed).
    show_ignored: Cell<bool>,
    /// Merges and fixes in the patch whose sources changed since.
    stale: RefCell<Vec<patch::Stale>>,
    settings: RefCell<Settings>,
    /// `None` if the patch's manifest couldn't be read.
    patch: RefCell<Option<Patch>>,
    enabled_list: gtk::ListBox,
    disabled_list: gtk::ListBox,
    toasts: adw::ToastOverlay,
    collections: RefCell<Collections>,
    collection_button: gtk::MenuButton,
    /// Offers to restore the active collection when the game's load order
    /// was changed elsewhere.
    restore_banner: adw::Banner,
    /// Disabled when there is nothing to undo or redo; kept in sync by
    /// `refresh`.
    undo_action: gio::SimpleAction,
    redo_action: gio::SimpleAction,
    /// Flatpak's update monitor, once an update was looked for.
    update_monitor: RefCell<Option<Rc<crate::update::Monitor>>>,
    /// The toast showing an update being installed.
    update_toast: RefCell<Option<adw::Toast>>,
    /// Set once the user was told about an update, so it's said only once.
    update_announced: Cell<bool>,
}

pub fn build(
    app: &adw::Application,
    paths: Paths,
    game: Option<GameInstall>,
    found: io::Result<Vec<Mod>>,
) {
    thumbnails::add_css();
    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
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
            toolbar.set_content(Some(&status_page(
                "Could Not Read Mods",
                &format!("{}: {e}", paths.user_dir.display()),
            )));
            window.present();
            return;
        }
    };

    // The menu offers to continue the last save; see `update_continue`.
    let launch_menu = gio::Menu::new();
    let launch = adw::SplitButton::builder()
        .menu_model(&launch_menu)
        .dropdown_tooltip("More Ways to Launch")
        .child(
            &adw::ButtonContent::builder()
                .icon_name("media-playback-start-symbolic")
                .label("Launch")
                .build(),
        )
        .action_name("win.launch")
        .css_classes(["suggested-action"])
        .build();
    let save = gtk::Button::builder()
        .label("Save")
        .action_name("win.save")
        .tooltip_text("Save the load order for the game and the Paradox Launcher (Ctrl+S)")
        .build();
    let menu = gio::Menu::new();
    menu.append(Some("_Undo"), Some("win.undo"));
    menu.append(Some("_Redo"), Some("win.redo"));
    menu.append(Some("Last Game's _Errors…"), Some("win.errors"));
    menu.append(Some("_Multiplayer Check…"), Some("win.multiplayer"));
    menu.append(Some("_Preferences"), Some("win.preferences"));
    menu.append(Some("_Keyboard Shortcuts"), Some("win.shortcuts"));
    menu.append(Some("_About DrukhalManager"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Main Menu")
        .primary(true)
        .build();
    header.pack_end(&menu_button);
    header.pack_end(&launch);
    header.pack_end(&save);
    let collection_button = gtk::MenuButton::builder()
        .tooltip_text("Collections: switch between saved load orders")
        .build();
    header.pack_start(&collection_button);
    if game.is_none() {
        launch.set_tooltip_text(Some("Stellaris was not found in any Steam library"));
    }

    let enabled_list = boxed_list("No mods enabled");
    let disabled_list = boxed_list("Every mod is enabled");
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let all_toggle = gtk::CheckButton::builder()
        .label("Enable All")
        .tooltip_text("Enable or disable every mod")
        .halign(gtk::Align::End)
        .build();
    let sort_button = gtk::Button::builder()
        .label("Sort by Dependencies")
        .tooltip_text("Move mods after the mods they depend on")
        .css_classes(["pill", "suggested-action"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status.set_hexpand(true);
    top.append(&status);
    top.append(&sort_button);
    top.append(&all_toggle);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search mods by name or tag (Ctrl+F)")
        .build();
    // Banner titles are plain text: mod names can contain markup characters.
    let repair_banner = adw::Banner::builder()
        .button_label("Repair…")
        .use_markup(false)
        .build();
    toolbar.add_top_bar(&repair_banner);
    let restore_banner = adw::Banner::builder()
        .button_label("Restore")
        .use_markup(false)
        .build();
    toolbar.add_top_bar(&restore_banner);
    content.append(&top);
    content.append(&search);
    for list in [&enabled_list, &disabled_list] {
        let search = search.clone();
        list.set_filter_func(move |row| {
            let query = search.text().to_lowercase();
            if query.is_empty() {
                return true;
            }
            row.downcast_ref::<adw::ActionRow>().is_none_or(|r| {
                format!("{} {}", r.title(), r.subtitle().unwrap_or_default())
                    .to_lowercase()
                    .contains(&query)
            })
        });
    }
    search.connect_search_changed(glib::clone!(
        #[weak]
        enabled_list,
        #[weak]
        disabled_list,
        move |_| {
            enabled_list.invalidate_filter();
            disabled_list.invalidate_filter();
        }
    ));
    content.append(&heading("Load Order"));
    content.append(&enabled_list);
    content.append(&heading("Disabled"));
    content.append(&disabled_list);
    stack.add_titled_with_icon(
        &scrolled_page(&content),
        Some("load-order"),
        "Load Order",
        "view-list-symbolic",
    );
    let conflicts_page = adw::Bin::new();
    stack.add_titled_with_icon(
        &scrolled_page(&conflicts_page),
        Some("conflicts"),
        "Conflicts",
        "dialog-warning-symbolic",
    );
    toolbar.set_content(Some(&stack));

    let save_action = gio::SimpleAction::new("save", None);
    let launch_action = gio::SimpleAction::new("launch", None);
    launch_action.set_enabled(game.is_some());
    window.add_action(&save_action);
    window.add_action(&launch_action);
    let continue_action = gio::SimpleAction::new("launch-continue", None);
    continue_action.set_enabled(false);
    window.add_action(&continue_action);
    app.set_accels_for_action("win.save", &["<Control>s"]);
    let preferences_action = gio::SimpleAction::new("preferences", None);
    window.add_action(&preferences_action);
    app.set_accels_for_action("win.preferences", &["<Control>comma"]);
    let page_action = gio::SimpleAction::new("page", Some(glib::VariantTy::STRING));
    page_action.connect_activate(glib::clone!(
        #[weak]
        stack,
        move |_, page| {
            if let Some(page) = page.and_then(|p| p.str()) {
                stack.set_visible_child_name(page);
            }
        }
    ));
    window.add_action(&page_action);
    app.set_accels_for_action("win.page::load-order", &["<Alt>1"]);
    app.set_accels_for_action("win.page::conflicts", &["<Alt>2"]);
    let undo_action = gio::SimpleAction::new("undo", None);
    let redo_action = gio::SimpleAction::new("redo", None);
    window.add_action(&undo_action);
    window.add_action(&redo_action);
    app.set_accels_for_action("win.undo", &["<Control>z"]);
    app.set_accels_for_action("win.redo", &["<Control><Shift>z", "<Control>y"]);

    let ui = Rc::new(Window {
        window: window.clone(),
        paths,
        game,
        collections: RefCell::new(Collections::starting_with(Vec::new())),
        collection_button,
        restore_banner,
        state: RefCell::new(State::new(found)),
        status,
        launch_menu,
        all_toggle: all_toggle.clone(),
        syncing: Cell::new(false),
        sort_button: sort_button.clone(),
        search: search.clone(),
        repair_banner: repair_banner.clone(),
        stack: stack.clone(),
        conflicts_page,
        analysis: RefCell::new(None),
        scanning: Cell::new(false),
        show_ignored: Cell::new(false),
        stale: RefCell::new(Vec::new()),
        settings: RefCell::new(Settings::load(&settings::path())),
        patch: RefCell::new(None),
        enabled_list,
        disabled_list,
        toasts,
        undo_action: undo_action.clone(),
        redo_action: redo_action.clone(),
        update_monitor: RefCell::new(None),
        update_toast: RefCell::new(None),
        update_announced: Cell::new(false),
        thumbnails: RefCell::default(),
    });
    ui.load_api_key();
    ui.add_update_actions();
    ui.add_shortcuts_action(app);

    let weak = Rc::downgrade(&ui);
    undo_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.undo();
        }
    });
    let weak = Rc::downgrade(&ui);
    redo_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.redo();
        }
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
            ui.launch(false);
        }
    });
    let weak = Rc::downgrade(&ui);
    continue_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.launch(true);
        }
    });
    // The game writes its last save as it plays; check again on coming
    // back to the window.
    ui.update_continue();
    let weak = Rc::downgrade(&ui);
    ui.window.connect_is_active_notify(move |w| {
        if let (true, Some(ui)) = (w.is_active(), weak.upgrade()) {
            ui.update_continue();
        }
    });
    match Patch::open(&ui.paths.user_dir) {
        Ok(p) => {
            ui.patch.replace(Some(p));
        }
        Err(e) => ui.toasts.add_toast(
            adw::Toast::builder()
                .title(format!("Could not read the merged patch: {e}"))
                .timeout(0)
                .build(),
        ),
    }
    let weak = Rc::downgrade(&ui);
    repair_banner.connect_button_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.confirm_repair();
        }
    });
    ui.update_repair_banner();
    ui.setup_collections();
    let weak = Rc::downgrade(&ui);
    sort_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.sort_by_dependencies();
        }
    });
    for (name, open) in [
        ("open-folder", Window::open_folder as fn(&Window, usize)),
        ("open-workshop", Window::open_workshop),
    ] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
        let weak = Rc::downgrade(&ui);
        action.connect_activate(move |_, index| {
            if let (Some(ui), Some(index)) = (weak.upgrade(), index.and_then(|i| i.get::<u32>())) {
                open(&ui, index as usize);
            }
        });
        window.add_action(&action);
    }
    for (name, to_top) in [("move-top", true), ("move-bottom", false)] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
        let weak = Rc::downgrade(&ui);
        action.connect_activate(move |_, index| {
            let (Some(ui), Some(index)) = (weak.upgrade(), index.and_then(|i| i.get::<u32>()))
            else {
                return;
            };
            ui.update(move |state| {
                if let Some(from) = state.enabled.iter().position(|&i| i == index as usize) {
                    let to = if to_top { 0 } else { state.enabled.len() };
                    state.move_to(from, to);
                }
            });
        });
        window.add_action(&action);
    }
    let errors = gio::SimpleAction::new("errors", None);
    let weak = Rc::downgrade(&ui);
    errors.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.show_errors();
        }
    });
    window.add_action(&errors);
    let multiplayer = gio::SimpleAction::new("multiplayer", None);
    let weak = Rc::downgrade(&ui);
    multiplayer.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.show_multiplayer();
        }
    });
    window.add_action(&multiplayer);
    let find = gio::SimpleAction::new("find", None);
    let weak = Rc::downgrade(&ui);
    find.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.stack.set_visible_child_name("load-order");
            ui.search.grab_focus();
        }
    });
    window.add_action(&find);
    app.set_accels_for_action("win.find", &["<Control>f"]);
    let weak = Rc::downgrade(&ui);
    all_toggle.connect_toggled(move |_| {
        if let Some(ui) = weak.upgrade()
            && !ui.syncing.get()
        {
            ui.toggle_all();
        }
    });
    ui.check_stale();
    let weak = Rc::downgrade(&ui);
    preferences_action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.open_preferences(None);
        }
    });
    let weak = Rc::downgrade(&ui);
    stack.connect_visible_child_name_notify(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.scan_if_stale();
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
    if ui.game.is_none() && crate::sandbox::is_flatpak() {
        ui.explain_sandbox_access();
    }
    ui.check_for_updates_at_startup();
}

impl Window {
    fn open_preferences(self: &Rc<Self>, page: Option<&str>) {
        let weak = Rc::downgrade(self);
        preferences::present(
            &self.window,
            self.settings.borrow().clone(),
            page,
            move |settings| {
                let Some(ui) = weak.upgrade() else { return };
                let key_changed = ui.settings.borrow().provider != settings.provider;
                if *ui.settings.borrow() == settings {
                    return;
                }
                ui.settings.replace(settings);
                if key_changed {
                    // Saved once the keyring has (or couldn't take) the key.
                    ui.store_api_key();
                } else {
                    ui.save_settings("preferences");
                }
            },
        );
    }

    fn save_settings(&self, what: &str) {
        if let Err(e) = self.settings.borrow().save(&settings::path()) {
            self.toasts.add_toast(
                adw::Toast::builder()
                    .title(format!("Could not save {what}: {e}"))
                    .timeout(0)
                    .build(),
            );
        }
    }

    /// Fills in the API key from the keyring (moving one still in the
    /// settings file there), off the main thread since it may prompt.
    fn load_api_key(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let path = settings::path();
            let Ok(loaded) = gio::spawn_blocking(move || settings::Settings::load(&path.clone()).with_keyring(&path)).await else { return };
            let Some(ui) = weak.upgrade() else { return };
            let mut current = ui.settings.borrow_mut();
            // Unless Preferences changed the provider meanwhile.
            if secrets::kind(&current.provider) == secrets::kind(&loaded.provider) && settings::api_key_mut(&mut current.provider).is_empty() {
                current.provider = loaded.provider;
                current.key_in_keyring = loaded.key_in_keyring;
            }
        });
    }

    /// Puts the provider's key in the keyring and saves the settings without
    /// it, or, with no keyring, keeps it in the (private) settings file.
    fn store_api_key(self: &Rc<Self>) {
        let mut provider = self.settings.borrow().provider.clone();
        let kind = secrets::kind(&provider);
        let key = std::mem::take(settings::api_key_mut(&mut provider));
        // Until the keyring answers, other saves leave the key out.
        self.settings.borrow_mut().key_in_keyring = true;
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let stored = gio::spawn_blocking(move || secrets::set(kind, &key)).await.unwrap_or_else(|_| Err("The keyring call failed".into()));
            let Some(ui) = weak.upgrade() else { return };
            ui.settings.borrow_mut().key_in_keyring = stored.is_ok();
            ui.save_settings("preferences");
            if let Err(e) = stored {
                ui.toasts.add_toast(
                    adw::Toast::builder()
                        .title(format!("{e}. The API key is saved in settings.json, readable only by you."))
                        .timeout(0)
                        .build(),
                );
            }
        });
    }
}

fn boxed_list(placeholder: &str) -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.set_placeholder(Some(
        &gtk::Label::builder()
            .label(placeholder)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(["dim-label"])
            .build(),
    ));
    list
}

fn display_name(m: &Mod) -> &str {
    if m.descriptor.name.is_empty() {
        &m.id
    } else {
        &m.descriptor.name
    }
}

fn scrolled_page(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(900)
        .child(child)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build()
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .build()
}

fn status_page(title: &str, description: &str) -> gtk::Widget {
    adw::StatusPage::builder()
        .icon_name("applications-games-symbolic")
        .title(title)
        .description(description)
        .build()
        .upcast()
}
