//! Preferences: finding and launching the game, AI merging, and updates.
//! The game and update options follow Irony Mod Manager's Options.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::merge::ai::{CLAUDE_MODEL, OPENROUTER_URL, Provider};
use crate::merge::collection;
use crate::merge::conflicts::LANGUAGES;
use crate::paradox::{game, steam};
use crate::settings::{self, GameOptions, Settings, UpdateOptions};
use crate::update;

pub const UPDATES_PAGE: &str = "updates";
pub const AI_PAGE: &str = "ai";
pub const MERGING_PAGE: &str = "merging";

/// Edits a copy of `settings`, opening on `page` if given; `on_close`
/// receives the result when the dialog closes.
pub fn present(parent: &impl IsA<gtk::Widget>, settings: Settings, page: Option<&str>, on_close: impl Fn(Settings) + 'static) {
    let dialog = adw::PreferencesDialog::new();
    let (game_page, merging_page, read_game) = game_page(&settings.game);
    let (ai_page, read_provider, read_parallel) = ai_page(&settings.provider, settings.parallel_merges);
    let (updates_page, read_updates) = updates_page(&settings.updates);
    let (tool_group, read_tool) = merge_tool_group(&settings.merge_tool);
    merging_page.add(&tool_group);
    let (steam_group, read_steam_key) = steam_key_group();
    let steam_page = self::page("Steam", "steam", "network-server-symbolic");
    steam_page.add(&steam_group);
    // Several light pages rather than a few long ones: the dialog animates
    // in slowly when the open page holds many rows.
    dialog.add(&game_page);
    dialog.add(&merging_page);
    dialog.add(&ai_page);
    dialog.add(&steam_page);
    dialog.add(&updates_page);
    if let Some(page) = page {
        dialog.set_visible_page_name(page);
    }

    // Settings these pages don't show are kept as they are.
    let base = settings.clone();
    let on_close = Rc::new(RefCell::new(Some(on_close)));
    dialog.connect_closed(move |_| {
        if let Some(f) = on_close.borrow_mut().take() {
            let provider = read_provider();
            let parallel_merges = read_parallel(&provider);
            f(Settings { provider, parallel_merges, game: read_game(), updates: read_updates(), merge_tool: read_tool(), new_steam_key: read_steam_key(), ..base.clone() });
        }
    });
    dialog.present(Some(parent));
}

/// The Steam Web API key, which lets DrukhalManager read mods' Workshop
/// "Required items". Loaded from the keyring in the background; the
/// reader returns it only if it was changed.
fn steam_key_group() -> (adw::PreferencesGroup, impl Fn() -> Option<String> + use<>) {
    let group = adw::PreferencesGroup::builder()
        .title("Steam Workshop")
        .description("With your own Steam Web API key, the mod list also checks the Workshop's “Required items” of enabled mods, which many mods list only there. Get a key at <a href=\"https://steamcommunity.com/dev/apikey\">steamcommunity.com/dev/apikey</a>; it's kept in your keyring.")
        .build();
    let row = adw::PasswordEntryRow::builder().title("Steam Web API Key").build();
    group.add(&row);
    let loaded: Rc<RefCell<Option<String>>> = Rc::default();
    glib::spawn_future_local(glib::clone!(
        #[weak]
        row,
        #[strong]
        loaded,
        async move {
            let key = gio::spawn_blocking(|| crate::secrets::get(crate::secrets::STEAM)).await.ok().and_then(Result::ok).flatten().unwrap_or_default();
            // Not if the user typed something meanwhile.
            if row.text().is_empty() {
                row.set_text(&key);
            }
            loaded.replace(Some(key));
        }
    ));
    let read = move || {
        let text = row.text().trim().to_owned();
        let before = loaded.borrow().clone();
        (before.as_deref() != Some(text.as_str()) && !(before.is_none() && text.is_empty())).then_some(text)
    };
    (group, read)
}

/// The external merge tool's command, with presets for common tools.
fn merge_tool_group(command: &str) -> (adw::PreferencesGroup, impl Fn() -> String + use<>) {
    use crate::merge::external;
    let mut description = "Merge Tool…, on the Conflicts page and when merging a file, opens two versions in this program, and uses what you save. {left}, {right}, {base} and {output} stand for the files.".to_owned();
    if crate::sandbox::is_flatpak() {
        description.push_str(&format!(" The Flatpak needs permission to start it, given once in a terminal with: {}", external::FLATPAK_OVERRIDE));
    }
    let group = adw::PreferencesGroup::builder().title("External Merge Tool").description(glib::markup_escape_text(&description).as_str()).build();
    let initial = if command.is_empty() { external::detect().unwrap_or_default() } else { command };
    let row = adw::EntryRow::builder().title("Command").text(initial).build();
    let presets = gio::Menu::new();
    for (i, (name, _)) in external::PRESETS.iter().enumerate() {
        let item = gio::MenuItem::new(Some(name), None);
        item.set_action_and_target_value(Some("tool.preset"), Some(&(i as u32).to_variant()));
        presets.append_item(&item);
    }
    let button = gtk::MenuButton::builder().icon_name("view-more-symbolic").tooltip_text("Use a common tool").menu_model(&presets).valign(gtk::Align::Center).css_classes(["flat"]).build();
    row.add_suffix(&button);
    let actions = gio::SimpleActionGroup::new();
    let preset = gio::SimpleAction::new("preset", Some(glib::VariantTy::UINT32));
    preset.connect_activate(glib::clone!(
        #[weak]
        row,
        move |_, i| {
            if let Some((_, command)) = i.and_then(|v| v.get::<u32>()).and_then(|i| external::PRESETS.get(i as usize)) {
                row.set_text(command);
            }
        }
    ));
    actions.add_action(&preset);
    row.insert_action_group("tool", Some(&actions));
    group.add(&row);
    (group, move || row.text().trim().to_owned())
}

fn page(title: &str, name: &str, icon: &str) -> adw::PreferencesPage {
    adw::PreferencesPage::builder().title(title).name(name).icon_name(icon).build()
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).valign(gtk::Align::Center).css_classes(["flat"]).build()
}

/// A row showing a path that is detected unless the user picks another.
/// Returns the row and the chosen path (`None` = detected).
fn path_row(title: &str, detected: Option<PathBuf>, chosen: Option<PathBuf>, folder: bool, unset: &'static str) -> (adw::ActionRow, Rc<RefCell<Option<PathBuf>>>) {
    let row = adw::ActionRow::builder().title(title).subtitle_selectable(true).build();
    let reset = icon_button("edit-undo-symbolic", if detected.is_some() { "Use the detected location" } else { "Don't use a folder" });
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
                (None, None) => unset.to_owned(),
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

/// The Game page, and the Merging page with the scan and merge options
/// (kept apart so neither is slow to animate in).
fn game_page(options: &GameOptions) -> (adw::PreferencesPage, adw::PreferencesPage, impl Fn() -> GameOptions + use<>) {
    let flatpak = crate::sandbox::is_flatpak();
    let home = glib::home_dir();
    let detected_exe = game::find(&steam::library_folders(&home)).map(|g| g.exe);

    let folders = adw::PreferencesGroup::builder()
        .title("Locations")
        .description("A new game executable or data directory takes effect the next time DrukhalManager starts.")
        .build();
    let (exe_row, exe) = path_row("Game Executable", detected_exe, options.executable.clone(), false, "Not found");
    let (data_row, data_dir) = path_row("Data Directory", Some(steam::stellaris_user_dir()), options.data_dir.clone(), true, "Not found");
    data_row.set_tooltip_text(Some("Where Stellaris keeps mod descriptors, saves and the load order"));
    let (extra_row, extra_dir) = path_row("Extra Mod Folder", None, options.extra_mod_dir.clone(), true, "None");
    extra_row.set_tooltip_text(Some("Mods in this folder's subfolders are listed and loaded like the rest, e.g. local mods on another drive or in a git checkout"));
    let merge_extra_row = adw::SwitchRow::builder()
        .title("Write Merged Mods There")
        .subtitle("Merge into One Mod writes into the extra mod folder instead of the game's")
        .active(options.merge_to_extra)
        .build();
    folders.add(&exe_row);
    folders.add(&data_row);
    folders.add(&extra_row);
    folders.add(&merge_extra_row);
    // Only meaningful with a folder chosen.
    let sync_merge_row = {
        let (row, dir) = (merge_extra_row.clone(), extra_dir.clone());
        move || row.set_sensitive(dir.borrow().is_some())
    };
    sync_merge_row();
    extra_row.connect_subtitle_notify(move |_| sync_merge_row());

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

    // Which localisation the conflict scan reads, like Irony's Allowed
    // Languages.
    let user_dir = options.data_dir.clone().unwrap_or_else(steam::stellaris_user_dir);
    let game_language = settings::game_language(&user_dir);
    let chosen = {
        let mut o = options.clone();
        o.skip_localisation = false;
        o.languages(&user_dir)
    };
    let scan = adw::PreferencesGroup::builder()
        .title("Conflict Scan")
        .description("Localisation in languages you don't play in adds conflicts you don't need to solve. Changes apply on the next scan.")
        .build();
    let localisation = adw::ExpanderRow::builder()
        .title("Localisation")
        .show_enable_switch(true)
        .enable_expansion(!options.skip_localisation)
        .build();
    let checks: Vec<(&'static str, gtk::CheckButton)> = LANGUAGES
        .iter()
        .map(|(id, name)| {
            let check = gtk::CheckButton::builder().active(chosen.contains(*id)).valign(gtk::Align::Center).build();
            let row = adw::ActionRow::builder().title(*name).activatable_widget(&check).build();
            if *id == game_language {
                row.set_subtitle("The game's language");
            }
            row.add_prefix(&check);
            localisation.add_row(&row);
            (*id, check)
        })
        .collect();
    let checks = Rc::new(checks);
    let summarize = {
        let (localisation, checks) = (localisation.clone(), checks.clone());
        move || {
            let names: Vec<&str> = LANGUAGES.iter().zip(checks.iter()).filter(|(_, (_, c))| c.is_active()).map(|((_, name), _)| *name).collect();
            localisation.set_subtitle(&match (localisation.enables_expansion(), names.len()) {
                (false, _) | (true, 0) => "Not scanned".to_owned(),
                (true, n) if n == LANGUAGES.len() => "Every language".to_owned(),
                (true, n) if n <= 2 => names.join(", "),
                (true, n) => format!("{n} languages"),
            });
        }
    };
    summarize();
    for (_, check) in checks.iter() {
        let summarize = summarize.clone();
        check.connect_toggled(move |_| summarize());
    }
    localisation.connect_enable_expansion_notify(move |_| summarize());
    scan.add(&localisation);

    let merging = adw::PreferencesGroup::builder()
        .title("Merge into One Mod")
        .description("{Name} stands for the collection's name. Applies to the next merge.")
        .build();
    let template = options.merged_name.clone().unwrap_or_else(|| collection::DEFAULT_TEMPLATE.to_owned());
    let name_row = adw::EntryRow::builder().title("Merged Mod Name").text(&template).build();
    let reset_name = icon_button("edit-undo-symbolic", "Use the default name");
    name_row.add_suffix(&reset_name);
    let check_name = |row: &adw::EntryRow, reset: &gtk::Button| {
        let text = row.text();
        // Without {Name}, the default is used instead.
        if collection::is_template(&text) {
            row.remove_css_class("error");
            row.set_tooltip_text(None);
        } else {
            row.add_css_class("error");
            row.set_tooltip_text(Some("Needs {Name}, or every collection's merge gets the same name"));
        }
        reset.set_visible(text.trim() != collection::DEFAULT_TEMPLATE);
    };
    check_name(&name_row, &reset_name);
    name_row.connect_changed(glib::clone!(
        #[weak]
        reset_name,
        move |row| check_name(row, &reset_name)
    ));
    reset_name.connect_clicked(glib::clone!(
        #[weak]
        name_row,
        move |_| name_row.set_text(collection::DEFAULT_TEMPLATE)
    ));
    merging.add(&name_row);

    let page = page("Game", "game", "input-gaming-symbolic");
    page.add(&folders);
    page.add(&launching);
    let merging_page = self::page("Merging", MERGING_PAGE, "view-dual-symbolic");
    merging_page.add(&scan);
    merging_page.add(&merging);
    // Chosen in the merge dialog, not here.
    let merge_to_zip = options.merge_to_zip;
    let read = move || GameOptions {
        executable: exe.borrow().clone(),
        data_dir: data_dir.borrow().clone(),
        launch_args: args_row.text().trim().to_owned(),
        refresh_descriptors: refresh_row.is_active(),
        close_after_launch: close_row.is_active(),
        scan_languages: {
            let picked: Vec<String> = checks.iter().filter(|(_, c)| c.is_active()).map(|(id, _)| id.to_string()).collect();
            // Just the game's language keeps following the game.
            (picked != [game_language.as_str()]).then_some(picked)
        },
        skip_localisation: !localisation.enables_expansion(),
        merged_name: {
            let text = name_row.text().trim().to_owned();
            (collection::is_template(&text) && text != collection::DEFAULT_TEMPLATE).then_some(text)
        },
        merge_to_zip,
        extra_mod_dir: extra_dir.borrow().clone(),
        merge_to_extra: merge_extra_row.is_active(),
    };
    (page, merging_page, read)
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
    let prerelease_row = adw::SwitchRow::builder()
        .title("Include Test Versions")
        .subtitle("Also offers betas and release candidates, which may have bugs")
        .active(options.prereleases)
        .build();
    let check_row = adw::ActionRow::builder().title("Check for Updates").build();
    let spinner = adw::Spinner::builder().visible(false).build();
    let check = gtk::Button::builder().label("Check").valign(gtk::Align::Center).build();
    check_row.add_suffix(&spinner);
    check_row.add_suffix(&check);
    check_row.set_activatable_widget(Some(&check));
    group.add(&startup_row);
    group.add(&prerelease_row);
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
        let (check_row, spinner, release, notes, ignore, found, skipped, prerelease_row, install) =
            (check_row.clone(), spinner.clone(), release.clone(), notes.clone(), ignore.clone(), found.clone(), skipped.clone(), prerelease_row.clone(), install.clone());
        move |button| {
            button.set_sensitive(false);
            spinner.set_visible(true);
            check_row.set_subtitle("");
            let prereleases = prerelease_row.is_active();
            let (button, check_row, spinner, release, notes, ignore, found, skipped, install) =
                (button.clone(), check_row.clone(), spinner.clone(), release.clone(), notes.clone(), ignore.clone(), found.clone(), skipped.clone(), install.clone());
            glib::spawn_future_local(async move {
                let result = gio::spawn_blocking(move || update::latest(prereleases)).await.unwrap_or_else(|_| Err("The check failed".to_owned()));
                button.set_sensitive(true);
                spinner.set_visible(false);
                match result {
                    Ok(latest) if update::is_newer(&latest.version, update::CURRENT) => {
                        check_row.set_subtitle(&format!("{} {} is available", if latest.prerelease { "Test version" } else { "Version" }, latest.version));
                        release.set_title(&format!("What's New in {}", latest.version));
                        let mut text = if latest.notes.is_empty() { "No release notes.".to_owned() } else { latest.notes.replace("**", "") };
                        // The repository Flatpak updates from only has
                        // stable releases.
                        if latest.prerelease && flatpak {
                            text.push_str("\n\nTest versions aren't in DrukhalManager's Flatpak repository. To try this one, download DrukhalManager.flatpak from its release page and run “flatpak install --reinstall DrukhalManager.flatpak”. The next stable release replaces it as usual.");
                        }
                        notes.set_label(&text);
                        install.set_visible(flatpak && !latest.prerelease);
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
    let read = move || UpdateOptions { check_at_startup: startup_row.is_active(), skipped_version: skipped.borrow().clone(), prereleases: prerelease_row.is_active() };
    (page, read)
}
