//! Preferences: finding and launching the game, AI merging, and updates.
//! The game and update options follow Irony Mod Manager's Options.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::merge::ai::{CLAUDE_MODEL, OPENROUTER_URL, Provider};
use crate::paradox::{game, steam};
use crate::settings::{self, GameOptions, Settings, UpdateOptions};
use crate::update;

pub const UPDATES_PAGE: &str = "updates";
pub const AI_PAGE: &str = "ai";

/// Edits a copy of `settings`, opening on `page` if given; `on_close`
/// receives the result when the dialog closes.
pub fn present(parent: &impl IsA<gtk::Widget>, settings: Settings, page: Option<&str>, on_close: impl Fn(Settings) + 'static) {
    let dialog = adw::PreferencesDialog::new();
    let (game_page, read_game) = game_page(&settings.game);
    let (ai_page, read_provider, read_parallel) = ai_page(&settings.provider, settings.parallel_merges);
    let (updates_page, read_updates) = updates_page(&settings.updates);
    dialog.add(&game_page);
    dialog.add(&ai_page);
    dialog.add(&updates_page);
    if let Some(page) = page {
        dialog.set_visible_page_name(page);
    }

    let (ignored, ignore_rules, key_in_keyring) = (settings.ignored.clone(), settings.ignore_rules.clone(), settings.key_in_keyring);
    let on_close = Rc::new(RefCell::new(Some(on_close)));
    dialog.connect_closed(move |_| {
        if let Some(f) = on_close.borrow_mut().take() {
            let provider = read_provider();
            let parallel_merges = read_parallel(&provider);
            f(Settings { provider, ignored: ignored.clone(), ignore_rules: ignore_rules.clone(), parallel_merges, game: read_game(), updates: read_updates(), key_in_keyring });
        }
    });
    dialog.present(Some(parent));
}

fn page(title: &str, name: &str, icon: &str) -> adw::PreferencesPage {
    adw::PreferencesPage::builder().title(title).name(name).icon_name(icon).build()
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).valign(gtk::Align::Center).css_classes(["flat"]).build()
}

/// A row showing a path that is detected unless the user picks another.
/// Returns the row and the chosen path (`None` = detected).
fn path_row(title: &str, detected: Option<PathBuf>, chosen: Option<PathBuf>, folder: bool) -> (adw::ActionRow, Rc<RefCell<Option<PathBuf>>>) {
    let row = adw::ActionRow::builder().title(title).subtitle_selectable(true).build();
    let reset = icon_button("edit-undo-symbolic", "Use the detected location");
    let choose = icon_button("document-open-symbolic", if folder { "Choose a folder" } else { "Choose a file" });
    row.add_suffix(&reset);
    row.add_suffix(&choose);
    let chosen = Rc::new(RefCell::new(chosen));

    let sync = Rc::new({
        let (row, reset, chosen) = (row.clone(), reset.clone(), chosen.clone());
        move || {
            let subtitle = match (&*chosen.borrow(), &detected) {
                (Some(p), _) => p.display().to_string(),
                (None, Some(p)) => format!("{} (detected)", p.display()),
                (None, None) => "Not found".to_owned(),
            };
            row.set_subtitle(&glib::markup_escape_text(&subtitle));
            reset.set_visible(chosen.borrow().is_some());
        }
    });
    sync();
    reset.connect_clicked({
        let (chosen, sync) = (chosen.clone(), sync.clone());
        move |_| {
            chosen.replace(None);
            sync();
        }
    });
    choose.connect_clicked({
        let (chosen, sync, title) = (chosen.clone(), sync.clone(), title.to_owned());
        move |button| {
            let file_dialog = gtk::FileDialog::builder().title(&title).modal(true).build();
            if let Some(current) = chosen.borrow().as_ref() {
                let start = if folder { current.clone() } else { current.parent().map(PathBuf::from).unwrap_or_default() };
                file_dialog.set_initial_folder(Some(&gio::File::for_path(start)));
            }
            let parent = button.root().and_downcast::<gtk::Window>();
            let (chosen, sync) = (chosen.clone(), sync.clone());
            glib::spawn_future_local(async move {
                let picked = if folder { file_dialog.select_folder_future(parent.as_ref()).await } else { file_dialog.open_future(parent.as_ref()).await };
                if let Some(path) = picked.ok().and_then(|f| f.path()) {
                    chosen.replace(Some(path));
                    sync();
                }
            });
        }
    });
    (row, chosen)
}

fn game_page(options: &GameOptions) -> (adw::PreferencesPage, impl Fn() -> GameOptions + use<>) {
    let flatpak = crate::sandbox::is_flatpak();
    let home = glib::home_dir();
    let detected_exe = game::find(&steam::library_folders(&home)).map(|g| g.exe);

    let folders = adw::PreferencesGroup::builder()
        .title("Locations")
        .description("Changes take effect the next time DrukhalManager starts.")
        .build();
    let (exe_row, exe) = path_row("Game Executable", detected_exe, options.executable.clone(), false);
    let (data_row, data_dir) = path_row("Data Directory", Some(steam::stellaris_user_dir()), options.data_dir.clone(), true);
    data_row.set_tooltip_text(Some("Where Stellaris keeps mod descriptors, saves and the load order"));
    folders.add(&exe_row);
    folders.add(&data_row);

    let launching = adw::PreferencesGroup::builder().title("Launching").build();
    let args_row = adw::EntryRow::builder().title("Game Launch Options").text(&options.launch_args).build();
    let refresh_row = adw::SwitchRow::builder()
        .title("Refresh Descriptors Before Playing")
        .subtitle("Point mod descriptors at their mods' folders again if they moved")
        .active(options.refresh_descriptors)
        .build();
    let close_row = adw::SwitchRow::builder().title("Close After Launching Game").active(options.close_after_launch).build();
    launching.add(&args_row);
    launching.add(&refresh_row);
    launching.add(&close_row);
    if flatpak {
        // The sandbox can't run the game itself; Steam starts it.
        exe_row.set_sensitive(false);
        exe_row.set_subtitle("Steam starts the game in the Flatpak");
        args_row.set_sensitive(false);
        launching.set_description(Some("In the Flatpak, Steam starts the game, so set launch options in Steam: right-click Stellaris, choose Properties, then Launch Options."));
    } else {
        args_row.set_tooltip_text(Some("Added after the launcher's own arguments, e.g. -debug_mode"));
    }

    let page = page("Game", "game", "input-gaming-symbolic");
    page.add(&folders);
    page.add(&launching);
    let read = move || GameOptions {
        executable: exe.borrow().clone(),
        data_dir: data_dir.borrow().clone(),
        launch_args: args_row.text().trim().to_owned(),
        refresh_descriptors: refresh_row.is_active(),
        close_after_launch: close_row.is_active(),
    };
    (page, read)
}

fn ai_page(current: &Provider, parallel: Option<u32>) -> (adw::PreferencesPage, impl Fn() -> Provider + use<>, impl Fn(&Provider) -> Option<u32> + use<>) {
    // Remember both providers' fields while the user switches between them.
    let (mut anthropic_key, mut openai) = (String::new(), settings::openrouter());
    match current {
        Provider::Anthropic { api_key } => anthropic_key = api_key.clone(),
        p @ Provider::OpenAiCompatible { .. } => openai = p.clone(),
    }
    let Provider::OpenAiCompatible { base_url, model, api_key: openai_key } = openai else { unreachable!() };

    let provider = adw::ComboRow::builder()
        .title("Provider")
        .model(&gtk::StringList::new(&["Claude (Anthropic API)", "OpenAI-compatible"]))
        .selected(u32::from(matches!(current, Provider::OpenAiCompatible { .. })))
        .build();

    let anthropic = adw::PreferencesGroup::builder()
        .title("Claude")
        .description(format!("Merges use {CLAUDE_MODEL}. The key is kept in your system keyring. Leave it empty to use the ANTHROPIC_API_KEY environment variable."))
        .build();
    let anthropic_key_row = adw::PasswordEntryRow::builder().title("API Key").text(&anthropic_key).build();
    anthropic.add(&anthropic_key_row);

    let compatible = adw::PreferencesGroup::builder()
        .title("OpenAI-Compatible Endpoint")
        .description("OpenRouter, or a local server such as Ollama (http://localhost:11434/v1) or LM Studio (http://localhost:1234/v1). Use a model that's good at code; small local models often produce broken merges.")
        .build();
    let url_row = adw::EntryRow::builder().title("Base URL").text(&base_url).build();
    let model_row = adw::EntryRow::builder().title("Model ID, as the provider names it").text(&model).build();
    let key_row = adw::PasswordEntryRow::builder().title("API Key (optional for local servers)").text(&openai_key).build();
    compatible.add(&url_row);
    compatible.add(&model_row);
    compatible.add(&key_row);

    let choice = adw::PreferencesGroup::builder().title("AI Merging").description("Merging sends the conflicting definitions to the provider you choose.").build();
    choice.add(&provider);

    let sync = {
        let (anthropic, compatible) = (anthropic.clone(), compatible.clone());
        move |row: &adw::ComboRow| {
            anthropic.set_visible(row.selected() == 0);
            compatible.set_visible(row.selected() == 1);
        }
    };
    sync(&provider);
    provider.connect_selected_notify(sync);

    let merge_all = adw::PreferencesGroup::builder().title("Merge All").build();
    let parallel_row = adw::SpinRow::with_range(1.0, f64::from(settings::MAX_PARALLEL), 1.0);
    parallel_row.set_title("Merges at Once");
    parallel_row.set_subtitle("How many objects Merge All sends to the provider at the same time. Lower it if the provider rate-limits you.");
    parallel_row.set_value(f64::from(parallel.unwrap_or_else(|| settings::default_parallel(current))));
    merge_all.add(&parallel_row);

    let page = page("AI Merging", AI_PAGE, "applications-science-symbolic");
    page.add(&choice);
    page.add(&anthropic);
    page.add(&compatible);
    page.add(&merge_all);
    let read = move || {
        if provider.selected() == 0 {
            Provider::Anthropic { api_key: anthropic_key_row.text().trim().to_owned() }
        } else {
            let url = url_row.text().trim().to_owned();
            Provider::OpenAiCompatible {
                base_url: if url.is_empty() { OPENROUTER_URL.to_owned() } else { url },
                model: model_row.text().trim().to_owned(),
                api_key: key_row.text().trim().to_owned(),
            }
        }
    };
    // Only remembered when it differs from the provider's default.
    let read_parallel = move |provider: &Provider| Some(parallel_row.value() as u32).filter(|&n| n != settings::default_parallel(provider));
    (page, read, read_parallel)
}

fn updates_page(options: &UpdateOptions) -> (adw::PreferencesPage, impl Fn() -> UpdateOptions + use<>) {
    let flatpak = crate::sandbox::is_flatpak();

    let version = adw::PreferencesGroup::new();
    let version_row = adw::ActionRow::builder()
        .title("DrukhalManager")
        .subtitle(format!("Version {}{}", update::CURRENT, if flatpak { " (Flatpak)" } else { "" }))
        .subtitle_selectable(true)
        .build();
    version_row.add_prefix(&gtk::Image::builder().icon_name("io.github.dukko.DrukhalManager").pixel_size(32).build());
    version.add(&version_row);

    let group = adw::PreferencesGroup::builder().title("Updates").build();
    let startup_row = adw::SwitchRow::builder()
        .title("Check for Updates at Startup")
        .subtitle(if flatpak {
            "Flatpak checks DrukhalManager's repository and offers to install new versions"
        } else {
            "Asks GitHub for the latest release. Your package manager installs it."
        })
        .active(options.check_at_startup)
        .build();
    let check_row = adw::ActionRow::builder().title("Check for Updates").build();
    let spinner = adw::Spinner::builder().visible(false).build();
    let check = gtk::Button::builder().label("Check").valign(gtk::Align::Center).build();
    check_row.add_suffix(&spinner);
    check_row.add_suffix(&check);
    check_row.set_activatable_widget(Some(&check));
    group.add(&startup_row);
    group.add(&check_row);

    // Filled in when a newer release is found.
    let release = adw::PreferencesGroup::builder().visible(false).build();
    let notes = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let notes_card = gtk::Box::builder().css_classes(["card"]).margin_bottom(12).build();
    notes_card.append(&notes);
    let buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::End).build();
    let ignore = gtk::ToggleButton::builder().label("Ignore This Version").build();
    let open_page = gtk::Button::builder().label("Open Release Page").build();
    let install = gtk::Button::builder().label("Install Update").action_name("win.install-update").css_classes(["suggested-action"]).visible(flatpak).build();
    buttons.append(&ignore);
    buttons.append(&open_page);
    buttons.append(&install);
    let notes_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    notes_box.append(&notes_card);
    notes_box.append(&buttons);
    release.add(&notes_box);

    let skipped = Rc::new(RefCell::new(options.skipped_version.clone()));
    let found: Rc<RefCell<Option<update::Release>>> = Rc::default();
    ignore.connect_toggled({
        let (skipped, found, previous) = (skipped.clone(), found.clone(), options.skipped_version.clone());
        move |b| {
            let version = found.borrow().as_ref().map(|r| r.version.clone());
            skipped.replace(if b.is_active() { version } else { previous.clone().filter(|p| Some(p) != version.as_ref()) });
        }
    });
    open_page.connect_clicked({
        let found = found.clone();
        move |b| {
            let url = found.borrow().as_ref().map_or_else(|| update::RELEASES_URL.to_owned(), |r| r.url.clone());
            gtk::UriLauncher::new(&url).launch(b.root().and_downcast_ref::<gtk::Window>(), None::<&gio::Cancellable>, |_| {});
        }
    });
    check.connect_clicked({
        let (check_row, spinner, release, notes, ignore, found, skipped) =
            (check_row.clone(), spinner.clone(), release.clone(), notes.clone(), ignore.clone(), found.clone(), skipped.clone());
        move |button| {
            button.set_sensitive(false);
            spinner.set_visible(true);
            check_row.set_subtitle("");
            let (button, check_row, spinner, release, notes, ignore, found, skipped) =
                (button.clone(), check_row.clone(), spinner.clone(), release.clone(), notes.clone(), ignore.clone(), found.clone(), skipped.clone());
            glib::spawn_future_local(async move {
                let result = gio::spawn_blocking(update::latest).await.unwrap_or_else(|_| Err("The check failed".to_owned()));
                button.set_sensitive(true);
                spinner.set_visible(false);
                match result {
                    Ok(latest) if update::is_newer(&latest.version, update::CURRENT) => {
                        check_row.set_subtitle(&format!("Version {} is available", latest.version));
                        release.set_title(&format!("What's New in {}", latest.version));
                        notes.set_label(&if latest.notes.is_empty() { "No release notes.".to_owned() } else { latest.notes.replace("**", "") });
                        ignore.set_active(skipped.borrow().as_deref() == Some(latest.version.as_str()));
                        release.set_visible(true);
                        found.replace(Some(latest));
                    }
                    Ok(_) => {
                        check_row.set_subtitle("DrukhalManager is up to date");
                        release.set_visible(false);
                    }
                    Err(e) => check_row.set_subtitle(&glib::markup_escape_text(&e)),
                }
            });
        }
    });

    let page = page("Updates", UPDATES_PAGE, "software-update-available-symbolic");
    page.add(&version);
    page.add(&group);
    page.add(&release);
    let read = move || UpdateOptions { check_at_startup: startup_row.is_active(), skipped_version: skipped.borrow().clone() };
    (page, read)
}
