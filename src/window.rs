//! The main window: a load order editor.

mod backups_ui;
mod collections_ui;
mod conflicts_ui;
mod ignore_rules_ui;
mod game_update_ui;
mod launch;
mod list;
mod persist;
mod reload;
mod repair_ui;
mod requirements_ui;
mod shortcuts;
mod smart_sort_ui;
mod dlc_ui;
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
use crate::mods::{Mod, filter};
use crate::mods::collections::Collections;
use crate::paradox::game::GameInstall;
use crate::preferences;
use crate::secrets;
use crate::settings::{self, Settings};

/// Filled by `row`, read by the lists' filter.
type SearchFacts = Rc<RefCell<std::collections::HashMap<usize, filter::Facts>>>;

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
    /// What the search can match each listed mod on, by mod index.
    search_facts: SearchFacts,
    /// Workshop required items, by Workshop id.
    requirements: RefCell<crate::mods::requirements::Cache>,
    /// Set while requirements are being fetched.
    fetching_requirements: Cell<bool>,
    /// No requirements are fetched before this: between batches, and for a
    /// while after Steam refused or couldn't be reached.
    requirements_not_before: Cell<Option<std::time::Instant>>,
    /// Offers to repair descriptors with stale paths or no mod behind them.
    repair_banner: adw::Banner,
    stack: adw::ViewStack,
    conflicts_page: adw::Bin,
    /// The Conflicts page's state, kept while the page shows.
    conflicts_view: RefCell<Option<Rc<dyn std::any::Any>>>,
    /// What the Conflicts page remembers between rebuilds.
    conflict_session: Rc<RefCell<crate::conflicts_view::Session>>,
    analysis: RefCell<Option<Rc<Analysis>>>,
    scanning: Cell<bool>,
    /// Whether the Conflicts page shows ignored conflicts (dimmed).
    show_ignored: Cell<bool>,
    /// The mod (descriptor id) the Conflicts page is filtered to.
    conflict_filter: RefCell<Option<String>>,
    /// Merges and fixes in the patch whose sources changed since.
    stale: RefCell<Vec<patch::Stale>>,
    settings: RefCell<Settings>,
    /// `None` if the patch's manifest couldn't be read.
    patch: RefCell<Option<Patch>>,
    enabled_list: gtk::ListBox,
    disabled_list: gtk::ListBox,
    /// The active collection's mods that aren't installed, and its heading;
    /// hidden when there are none.
    missing_list: gtk::ListBox,
    missing_heading: gtk::Label,
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
    crate::sandbox::load_host_stylesheet();
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
    menu.append(Some("Re_load Mods"), Some("win.reload"));
    menu.append(Some("Last Game's _Errors…"), Some("win.errors"));
    menu.append(Some("_DLC…"), Some("win.dlc"));
    menu.append(Some("_Multiplayer Check…"), Some("win.multiplayer"));
    menu.append(Some("Mods _Not Updated for This Game…"), Some("win.outdated-mods"));
    menu.append(Some("Merged Patch _Backups…"), Some("win.patch-backups"));
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
    let missing_list = boxed_list("");
    let missing_heading = heading("Not Installed");
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
    let sort_menu = gio::Menu::new();
    sort_menu.append(Some("Sort A–Z, Dependencies First"), Some("win.sort-alphabetically"));
    let sort_alpha_button = adw::SplitButton::builder()
        .child(&adw::ButtonContent::builder().icon_name("view-sort-ascending-symbolic").label("Smart Sort…").build())
        .tooltip_text("Sort into sections by what each mod changes, keeping sub-mods below their mods and every mod after its dependencies")
        .dropdown_tooltip("More ways to sort")
        .menu_model(&sort_menu)
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status.set_hexpand(true);
    top.append(&status);
    top.append(&sort_button);
    top.append(&sort_alpha_button);
    top.append(&all_toggle);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search mods (Ctrl+F), or try source:local or enabled:no")
        .hexpand(true)
        .build();
    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    search_row.append(&search);
    search_row.append(&filter_button(&search));
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
    content.append(&search_row);
    let search_facts = SearchFacts::default();
    let query: Rc<RefCell<filter::Query>> = Rc::default();
    for list in [&enabled_list, &disabled_list] {
        let (query, facts) = (query.clone(), search_facts.clone());
        list.set_filter_func(move |row| {
            let query = query.borrow();
            query.is_empty()
                || row
                    .widget_name()
                    .strip_prefix("mod-")
                    .and_then(|i| facts.borrow().get(&i.parse().ok()?).map(|f| query.matches(f)))
                    .unwrap_or(true)
        });
    }
    search.connect_search_changed(glib::clone!(
        #[weak]
        enabled_list,
        #[weak]
        disabled_list,
        move |search| {
            query.replace(filter::Query::parse(&search.text()));
            enabled_list.invalidate_filter();
            disabled_list.invalidate_filter();
        }
    ));
    content.append(&heading("Load Order"));
    content.append(&enabled_list);
    content.append(&missing_heading);
    content.append(&missing_list);
    let disabled_order = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&["Name", "Recently Updated"]))
        .tooltip_text("Order of the disabled mods")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let disabled_heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let label = heading("Disabled");
    label.set_hexpand(true);
    disabled_heading.append(&label);
    disabled_heading.append(&gtk::Label::builder().label("Sort by").css_classes(["dim-label"]).build());
    disabled_heading.append(&disabled_order);
    content.append(&disabled_heading);
    content.append(&disabled_list);
    stack.add_titled_with_icon(
        &scrolled_page(&content),
        Some("load-order"),
        "Load Order",
        "view-list-symbolic",
    );
    let conflicts_page = adw::Bin::new();
    conflicts_page.set_vexpand(true);
    stack.add_titled_with_icon(
        &conflicts_page,
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
        search_facts,
        requirements: RefCell::new(crate::mods::requirements::Cache::load(&crate::mods::requirements::Cache::path())),
        fetching_requirements: Cell::new(false),
        requirements_not_before: Cell::new(None),
        repair_banner: repair_banner.clone(),
        stack: stack.clone(),
        conflicts_page,
        conflicts_view: RefCell::new(None),
        conflict_session: Rc::default(),
        analysis: RefCell::new(None),
        scanning: Cell::new(false),
        show_ignored: Cell::new(false),
        conflict_filter: RefCell::new(None),
        stale: RefCell::new(Vec::new()),
        settings: RefCell::new(Settings::load(&settings::path())),
        patch: RefCell::new(None),
        enabled_list,
        disabled_list,
        missing_list,
        missing_heading,
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
    ui.add_reload_action(app);

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
    disabled_order.set_selected(match ui.settings.borrow().disabled_order {
        settings::DisabledOrder::Name => 0,
        settings::DisabledOrder::RecentlyUpdated => 1,
    });
    let weak = Rc::downgrade(&ui);
    disabled_order.connect_selected_notify(move |picker| {
        let Some(ui) = weak.upgrade() else { return };
        let order = if picker.selected() == 1 { settings::DisabledOrder::RecentlyUpdated } else { settings::DisabledOrder::Name };
        if ui.settings.borrow().disabled_order == order {
            return;
        }
        ui.settings.borrow_mut().disabled_order = order;
        ui.save_settings("the Disabled list's order");
        ui.refresh();
    });
    let weak = Rc::downgrade(&ui);
    sort_alpha_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.open_smart_sort();
        }
    });
    let sort_alphabetically = gio::SimpleAction::new("sort-alphabetically", None);
    let weak = Rc::downgrade(&ui);
    sort_alphabetically.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.sort_alphabetically();
        }
    });
    window.add_action(&sort_alphabetically);
    for (name, open) in [
        ("open-folder", Window::open_folder as fn(&Window, usize)),
        ("open-workshop", Window::open_workshop),
        ("open-in-steam", Window::open_in_steam),
        ("unsubscribe", Window::unsubscribe_in_steam),
        ("copy-workshop-link", Window::copy_workshop_link),
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
    let rename_mod = gio::SimpleAction::new("rename-mod", Some(glib::VariantTy::UINT32));
    let weak = Rc::downgrade(&ui);
    rename_mod.connect_activate(move |_, index| {
        if let (Some(ui), Some(index)) = (weak.upgrade(), index.and_then(|i| i.get::<u32>())) {
            ui.rename_mod(index as usize);
        }
    });
    window.add_action(&rename_mod);
    let get_requirements = gio::SimpleAction::new("get-requirements", Some(glib::VariantTy::UINT32));
    let weak = Rc::downgrade(&ui);
    get_requirements.connect_activate(move |_, index| {
        if let (Some(ui), Some(index)) = (weak.upgrade(), index.and_then(|i| i.get::<u32>())) {
            ui.get_requirements(index as usize);
        }
    });
    window.add_action(&get_requirements);
    for (name, ignore) in [("ignore-requirement", true), ("unignore-requirement", false)] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT64));
        let weak = Rc::downgrade(&ui);
        action.connect_activate(move |_, id| {
            if let (Some(ui), Some(id)) = (weak.upgrade(), id.and_then(|i| i.get::<u64>())) {
                ui.ignore_requirement(id, ignore);
            }
        });
        window.add_action(&action);
    }
    let show_conflicts = gio::SimpleAction::new("show-conflicts", Some(glib::VariantTy::UINT32));
    let weak = Rc::downgrade(&ui);
    show_conflicts.connect_activate(move |_, index| {
        if let (Some(ui), Some(index)) = (weak.upgrade(), index.and_then(|i| i.get::<u32>())) {
            ui.show_conflicts_of(index as usize);
        }
    });
    window.add_action(&show_conflicts);
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
    let outdated_mods = gio::SimpleAction::new("outdated-mods", None);
    let weak = Rc::downgrade(&ui);
    outdated_mods.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.show_outdated_mods(false);
        }
    });
    window.add_action(&outdated_mods);
    let multiplayer = gio::SimpleAction::new("multiplayer", None);
    let weak = Rc::downgrade(&ui);
    multiplayer.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.show_multiplayer();
        }
    });
    window.add_action(&multiplayer);
    let dlc = gio::SimpleAction::new("dlc", None);
    let weak = Rc::downgrade(&ui);
    dlc.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.show_dlcs();
        }
    });
    window.add_action(&dlc);
    let backups = gio::SimpleAction::new("patch-backups", None);
    let weak = Rc::downgrade(&ui);
    backups.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.show_patch_backups();
        }
    });
    window.add_action(&backups);
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
    // Once the window is up, so the dialog has a parent to show over.
    let weak = Rc::downgrade(&ui);
    glib::idle_add_local_once(move || {
        if let Some(ui) = weak.upgrade() {
            ui.check_game_update();
        }
    });
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
                let languages_changed = ui.settings.borrow().game.languages(&ui.paths.user_dir) != settings.game.languages(&ui.paths.user_dir);
                let extra_changed = ui.settings.borrow().game.extra_mod_dir != settings.game.extra_mod_dir;
                let mut settings = settings;
                if let Some(key) = settings.new_steam_key.take() {
                    ui.store_steam_key(key);
                }
                ui.settings.replace(settings);
                if extra_changed {
                    // Lists the new folder's mods, and drops the old one's.
                    ui.reload(|_| {});
                }
                if languages_changed && ui.analysis.borrow().is_some() {
                    ui.scan();
                }
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

/// A "Filters" button whose popover sets the search's `key:value` terms.
fn filter_button(search: &gtk::SearchEntry) -> gtk::MenuButton {
    // Each key's title, and its values with their labels.
    type Choice = (&'static str, &'static str, &'static [(&'static str, &'static str)]);
    let choices: [Choice; 4] = [
        ("enabled", "Enabled", &[("yes", "Yes"), ("no", "No")]),
        ("source", "Source", &[("workshop", "Workshop"), ("local", "Local")]),
        ("achievements", "Achievements", &[("yes", "Keeps Them On"), ("no", "Turns Them Off")]),
        ("conflicts", "Conflicts", &[("yes", "Has Conflicts"), ("no", "None")]),
    ];
    debug_assert!(choices.iter().map(|c| c.0).eq(filter::KEYS));
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    let mut pickers = Vec::new();
    // Set while the pickers are shown what the search says.
    let syncing = Rc::new(Cell::new(false));
    for (row, (key, title, values)) in choices.into_iter().enumerate() {
        let mut labels = vec!["Any"];
        labels.extend(values.iter().map(|v| v.1));
        let picker = gtk::DropDown::from_strings(&labels);
        grid.attach(&gtk::Label::builder().label(title).xalign(0.0).build(), 0, row as i32, 1, 1);
        grid.attach(&picker, 1, row as i32, 1, 1);
        let (search, syncing) = (search.clone(), syncing.clone());
        picker.connect_selected_notify(move |picker| {
            if syncing.get() {
                return;
            }
            let value = (picker.selected() as usize).checked_sub(1).and_then(|i| values.get(i)).map(|v| v.0);
            let text = filter::with_value(&search.text(), key, value);
            if text != search.text() {
                search.set_text(&text);
            }
        });
        pickers.push((key, values, picker));
    }
    let hint = gtk::Label::builder()
        .label("Or type them: version:3.12, -- to leave out (source:--local), || for either")
        .wrap(true)
        .max_width_chars(32)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    grid.attach(&hint, 0, choices.len() as i32, 2, 1);
    let popover = gtk::Popover::builder().child(&grid).build();
    // Shows what the search already says when opened.
    popover.connect_show(glib::clone!(
        #[weak]
        search,
        move |_| {
            let text = search.text();
            syncing.set(true);
            for (key, values, picker) in &pickers {
                let at = filter::value_of(&text, key).and_then(|v| values.iter().position(|x| x.0 == v)).map_or(0, |i| i as u32 + 1);
                picker.set_selected(at);
            }
            syncing.set(false);
        }
    ));
    gtk::MenuButton::builder()
        .label("Filters")
        .tooltip_text("Filter the mods by state")
        .popover(&popover)
        .build()
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
