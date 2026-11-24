//! The file merge dialog: combines several mods' copies of one file object
//! by object, uses AI only for objects the copies really disagree on, and
//! saves the result into the merged patch at the same path.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::merge::ai::{self, Provider};
use crate::merge::file_merge::{self, Plan, Status, Version};
use crate::merge::patch::{Print, VANILLA};
use crate::merge::source::Source;
use crate::thinking_view::{self, ThinkingView};

#[derive(Debug, Clone)]
pub struct WholeFilePlan {
    pub path: String,
    pub vanilla: Option<Source>,
    pub sources: Vec<Source>,
    pub initial_text: String,
}

pub enum LoadResult {
    Objects(Plan, Vec<Print>),
    WholeFile(WholeFilePlan, Vec<Print>),
}

#[allow(clippy::type_complexity)]
pub struct Request {
    pub path: String,
    /// `(descriptor id, mod name, mod folder)` for each copy, in load order.
    pub origins: Vec<(String, String, PathBuf)>,
    pub game_dir: Option<PathBuf>,
    pub provider: Option<Provider>,
    /// Whether the patch already holds a merge of this file.
    pub merged_before: bool,
    /// Existing merge contents from patch, if any.
    pub saved_text: Option<String>,
    pub save: Box<dyn Fn(&str, Vec<Print>) -> Result<(), String>>,
    /// The external merge tool's command; empty for none.
    pub merge_tool: Box<dyn Fn() -> String>,
    pub open_preferences: Box<dyn Fn()>,
    pub on_saved: Box<dyn Fn()>,
}

pub fn present(parent: &impl IsA<gtk::Widget>, request: Request) {
    let request = Rc::new(request);
    let plan: Rc<RefCell<Option<Plan>>> = Rc::default();
    let whole_file: Rc<RefCell<Option<WholeFilePlan>>> = Rc::default();

    let merge_button = gtk::Button::builder().label(t!("Merge Conflicts")).css_classes(["suggested-action"]).sensitive(false).build();
    let save_button = gtk::Button::builder().label(t!("Save to Patch")).sensitive(false).build();
    let file_name = request.path.rsplit_once('/').map_or(request.path.as_str(), |(_, f)| f);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(file_name, &request.path)));
    header.pack_end(&save_button);
    header.pack_end(&merge_button);
    let banner = adw::Banner::builder().use_markup(false).build();
    let editor = gtk::TextView::builder().monospace(true).wrap_mode(gtk::WrapMode::None).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
    let tool_button = merge_tool_menu(&request, &editor, &banner);
    // Usable whenever saving is: once the copies are read, and not while
    // the AI is merging.
    save_button.bind_property("sensitive", &tool_button, "sensitive").sync_create().build();
    header.pack_start(&tool_button);

    let summary = adw::PreferencesGroup::builder().title(t!("Objects")).description(t!("Reading each mod's copy…")).build();
    let editor_scroll = gtk::ScrolledWindow::builder().child(&editor).min_content_height(320).css_classes(["card"]).build();
    let result = adw::PreferencesGroup::builder()
        .title(t!("Merged File"))
        .description(t!("Review and edit before saving. Merging conflicts rebuilds this from the objects above, replacing manual edits."))
        .build();
    let thinking = ThinkingView::new();
    let result_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    result_box.append(&thinking.widget);
    result_box.append(&crate::resize_grip::resizable(&editor_scroll));
    result.add(&result_box);

    let page = adw::PreferencesPage::new();
    page.add(&summary);
    page.add(&result);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().content_width(960).content_height(800).child(&toolbar).build();

    if request.merged_before {
        show(&banner, &t!("This file was merged before. Saving replaces that merge with this one."), false);
    }

    // One status label per conflicting object, updated as merges finish.
    let status_labels: Rc<RefCell<Vec<(usize, gtk::Label)>>> = Rc::default();

    let path = request.path.clone();
    let origins = request.origins.clone();
    let game_dir = request.game_dir.clone();
    let saved_text = request.saved_text.clone();
    let prints: Rc<RefCell<Vec<Print>>> = Rc::default();
    glib::spawn_future_local(glib::clone!(
        #[strong]
        prints,
        #[weak]
        summary,
        #[weak]
        editor,
        #[weak]
        merge_button,
        #[weak]
        save_button,
        #[weak]
        banner,
        #[strong]
        plan,
        #[strong]
        whole_file,
        #[strong]
        status_labels,
        #[strong]
        request,
        async move {
            let planned = gio::spawn_blocking(move || load(&path, &origins, game_dir.as_deref(), saved_text.as_deref())).await;
            let loaded = match planned {
                Ok(Ok(l)) => l,
                Ok(Err(message)) => {
                    summary.set_description(Some(&t!("This file could not be read.")));
                    show(&banner, &message, true);
                    return;
                }
                Err(_) => {
                    show(&banner, &t!("Reading the mods' copies failed unexpectedly"), true);
                    return;
                }
            };

            match loaded {
                LoadResult::Objects(p, found) => {
                    prints.replace(found);
                    let conflicts = p.conflicts().count();
                    summary.set_description(Some(&describe(&p)));

                    for (index, item) in p.items.iter().enumerate() {
                        let subtitle = match &item.status {
                            Status::Same => continue,
                            Status::Recovered(from) => tf!("Recovered from {from}", from = from.join(", ")),
                            Status::OneChange(from) => tf!("Only {from} changes this; using its version", from = from),
                            Status::Conflict { sources, .. } => tf!("Changed differently by {sources}", sources = sources.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(", ")),
                        };
                        let row = adw::ActionRow::builder().title(&item.key).subtitle(subtitle).use_markup(false).build();
                        if let Status::Conflict { sources, .. } = &item.status {
                            // The version picker shows whose version is used; this
                            // only shows while the AI works on the object.
                            let label = gtk::Label::builder().visible(false).css_classes(["dim-label", "caption"]).build();
                            row.add_suffix(&label);
                            // Choosing a version settles the conflict without AI.
                            let labels: Vec<&str> = sources.iter().map(|s| s.label.as_str()).collect();
                            let choice = gtk::DropDown::from_strings(&labels);
                            choice.set_factory(Some(&label_factory(Some(22))));
                            choice.set_list_factory(Some(&label_factory(None)));
                            choice.set_selected(labels.len() as u32 - 1);
                            choice.set_valign(gtk::Align::Center);
                            choice.set_tooltip_text(Some(&t!("Whose version to keep")));
                            let texts: Vec<(String, String)> = sources.iter().map(|s| (s.label.clone(), s.text.clone())).collect();
                            choice.connect_selected_notify(glib::clone!(
                                #[strong]
                                plan,
                                #[weak]
                                editor,
                                #[weak]
                                label,
                                move |choice| {
                                    let Some((_, text)) = texts.get(choice.selected() as usize) else { return };
                                    if let Some(p) = plan.borrow_mut().as_mut() {
                                        p.items[index].text = text.clone();
                                        p.items[index].resolved = true;
                                        editor.buffer().set_text(&p.render());
                                    }
                                    // The picker says it all now.
                                    label.set_visible(false);
                                }
                            ));
                            row.add_suffix(&choice);
                            status_labels.borrow_mut().push((index, label));
                            row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
                        }
                        summary.add(&row);
                    }

                    editor.buffer().set_text(&p.render());
                    save_button.set_sensitive(true);
                    if conflicts > 0 {
                        merge_button.set_label(&tnf!("Merge {count} Conflict with AI", "Merge {count} Conflicts with AI", count = conflicts));
                        merge_button.set_sensitive(true);
                        if let Some(provider) = &request.provider {
                            merge_button.set_tooltip_text(Some(&tf!("Ask {model} to merge each conflicting object", model = provider.model_name())));
                        }
                    } else {
                        merge_button.set_visible(false);
                    }
                    plan.replace(Some(p));
                }
                LoadResult::WholeFile(w, found) => {
                    prints.replace(found);
                    let winner = w.sources.last().map(|s| s.label.clone()).unwrap_or_default();
                    let is_inline = w.path.to_lowercase().starts_with("common/inline_scripts/");
                    summary.set_title(&if is_inline { t!("Inline Script") } else { t!("Conflicting File") });
                    summary.set_description(Some(&tnf!("{count} mod ships this file; {winner} wins in game unless merged.", "{count} mods ship this file; {winner} wins in game unless merged.", count = w.sources.len(), winner = &winner)));

                    if let Some(vanilla) = &w.vanilla {
                        let row = adw::ActionRow::builder().title(&vanilla.label).subtitle(t!("Base game version")).use_markup(false).build();
                        let use_btn = gtk::Button::builder().label(t!("Use as Base")).valign(gtk::Align::Center).css_classes(["flat"]).build();
                        let (editor_ref, text, banner_ref) = (editor.clone(), vanilla.text.clone(), banner.clone());
                        use_btn.connect_clicked(move |_| {
                            editor_ref.buffer().set_text(&text);
                            show(&banner_ref, &t!("Loaded the base game's version into the editor."), false);
                        });
                        row.add_suffix(&use_btn);
                        summary.add(&row);
                    }

                    for (i, source) in w.sources.iter().enumerate() {
                        let is_winner = i == w.sources.len() - 1;
                        let subtitle = if is_winner { t!("Used in game (loaded last)") } else { t!("Overridden by later mods") };
                        let row = adw::ActionRow::builder().title(&source.label).subtitle(subtitle).use_markup(false).build();
                        let use_btn = gtk::Button::builder().label(t!("Use as Base")).valign(gtk::Align::Center).css_classes(["flat"]).build();
                        let (editor_ref, text, banner_ref, name) = (editor.clone(), source.text.clone(), banner.clone(), source.label.clone());
                        use_btn.connect_clicked(move |_| {
                            editor_ref.buffer().set_text(&text);
                            show(&banner_ref, &tf!("Loaded {name}'s version into the editor.", name = &name), false);
                        });
                        row.add_suffix(&use_btn);
                        if !is_winner {
                            row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
                        } else {
                            row.add_prefix(&gtk::Image::builder().icon_name("emblem-ok-symbolic").css_classes(["success"]).build());
                        }
                        summary.add(&row);
                    }

                    editor.buffer().set_text(&w.initial_text);
                    save_button.set_sensitive(true);
                    merge_button.set_label(&t!("Merge with AI"));
                    merge_button.set_sensitive(true);
                    merge_button.set_visible(true);
                    if let Some(provider) = &request.provider {
                        merge_button.set_tooltip_text(Some(&tf!("Ask {model} to merge this file", model = provider.model_name())));
                    }
                    whole_file.replace(Some(w));
                }
            }
        }
    ));

    merge_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        thinking,
        #[strong]
        plan,
        #[strong]
        whole_file,
        #[strong]
        status_labels,
        #[weak]
        dialog,
        #[weak]
        editor,
        #[weak]
        save_button,
        #[weak]
        banner,
        move |button| {
            let Some(provider) = request.provider.clone() else {
                dialog.close();
                (request.open_preferences)();
                return;
            };
            banner.set_revealed(false);
            button.set_sensitive(false);
            save_button.set_sensitive(false);
            editor.set_editable(false);

            if let Some(w) = whole_file.borrow().clone() {
                let file_name = request.path.rsplit_once('/').map_or(request.path.as_str(), |(_, f)| f).to_owned();
                glib::spawn_future_local(glib::clone!(
                    #[strong]
                    thinking,
                    #[weak]
                    button,
                    #[weak]
                    editor,
                    #[weak]
                    save_button,
                    #[weak]
                    banner,
                    async move {
                        let path = w.path.clone();
                        let result = thinking_view::run(&thinking, Some(&file_name), move |on| {
                            let check = |text: &str| validate_file(&path, text);
                            ai::merge_file_checked(&provider, &w.path, w.vanilla.as_ref(), &w.sources, &check, on)
                        })
                        .await;
                        match result {
                            Some(Ok(checked)) => match checked.script {
                                Ok(script) => {
                                    let params = crate::merge::script_check::cached_scripted_params();
                                    let cleaned = crate::merge::script_check::strip_undeclared_scripted_effect_blocks(&script, &params);
                                    editor.buffer().set_text(&cleaned);
                                    button.set_label(&t!("Merged"));
                                    button.set_sensitive(false);
                                    show(&banner, &t!("Merged with AI. Review the result below or click Merge Tool… before saving."), false);
                                }
                                Err(e) => {
                                    button.set_label(&t!("Retry Failed"));
                                    button.set_sensitive(true);
                                    show(&banner, &tf!("AI merge was rejected: {error}", error = e), true);
                                }
                            },
                            Some(Err(e)) => {
                                button.set_label(&t!("Retry Failed"));
                                button.set_sensitive(true);
                                show(&banner, &tf!("AI merge failed: {error}", error = e), true);
                            }
                            None => {
                                button.set_sensitive(true);
                                show(&banner, &t!("AI merge was cancelled or failed unexpectedly"), true);
                            }
                        }
                        editor.set_editable(true);
                        save_button.set_sensitive(true);
                    }
                ));
                return;
            }

            glib::spawn_future_local(glib::clone!(
                #[strong]
                thinking,
                #[strong]
                plan,
                #[strong]
                status_labels,
                #[weak]
                button,
                #[weak]
                editor,
                #[weak]
                save_button,
                #[weak]
                banner,
                async move {
                    let pending: Vec<(usize, gtk::Label)> = status_labels.borrow().clone();
                    let mut failures = 0;
                    for (index, label) in pending {
                        let job = plan.borrow().as_ref().and_then(|p| {
                            let item = &p.items[index];
                            match &item.status {
                                Status::Conflict { sources, vanilla } if !item.resolved => Some((p.category.clone(), item.key.clone(), sources.clone(), vanilla.clone())),
                                _ => None,
                            }
                        });
                        let Some((category, key, sources, vanilla)) = job else { continue };
                        label.set_label(&t!("Merging…"));
                        label.set_tooltip_text(None);
                        label.set_css_classes(&["dim-label", "caption"]);
                        label.set_visible(true);
                        let provider = provider.clone();
                        let (c, k) = (category.clone(), key.clone());
                        let heading = key.clone();
                        let result = thinking_view::run(&thinking, Some(&heading), move |on| {
                            let check = |text: &str| crate::merge_dialog::validate(&c, &k, text);
                            ai::merge_checked(&provider, &c, &k, vanilla.as_ref(), &sources, &check, on)
                        })
                        .await;
                        let merged = match result {
                            Some(Ok(checked)) => checked
                                .script
                                .and_then(|_| plan.borrow().as_ref().map_or(Err(t!("the merge was closed")), |p| p.validate_item(index, &checked.merged.script))),
                            Some(Err(e)) => Err(e),
                            None => Err(t!("failed unexpectedly")),
                        };
                        match merged {
                            Ok(text) => {
                                if let Some(p) = plan.borrow_mut().as_mut() {
                                    p.items[index].text = text;
                                    p.items[index].resolved = true;
                                    editor.buffer().set_text(&p.render());
                                }
                                label.set_label(&t!("Merged"));
                                label.set_css_classes(&["success", "caption"]);
                            }
                            Err(e) => {
                                failures += 1;
                                label.set_label(&t!("Merge failed"));
                                label.set_tooltip_text(Some(&e));
                                label.set_css_classes(&["error", "caption"]);
                            }
                        }
                    }
                    editor.set_editable(true);
                    save_button.set_sensitive(true);
                    let unresolved = plan.borrow().as_ref().map_or(0, |p| p.conflicts().filter(|(_, i)| !i.resolved).count());
                    button.set_sensitive(unresolved > 0);
                    button.set_label(&if unresolved > 0 { t!("Retry Failed") } else { t!("Merged") });
                    if failures > 0 {
                        show(&banner, &tnf!("{count} object couldn't be merged; hover the row for the reason. They keep the last mod's version.", "{count} objects couldn't be merged; hover the row for the reason. They keep the last mod's version.", count = failures), true);
                    }
                }
            ));
        }
    ));

    save_button.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        prints,
        #[weak]
        dialog,
        #[weak]
        editor,
        #[weak]
        banner,
        move |_| {
            let category = file_merge::category(&request.path);
            let buffer = editor.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let params = crate::merge::script_check::cached_scripted_params();
            let text = crate::merge::script_check::strip_undeclared_scripted_effect_blocks(&text, &params);
            if let Err(message) = file_merge::validate(&category, &text) {
                show(&banner, &message, true);
                return;
            }
            match (request.save)(&text, prints.borrow().clone()) {
                Ok(()) => {
                    dialog.close();
                    (request.on_saved)();
                }
                Err(message) => show(&banner, &message, true),
            }
        }
    ));

    dialog.present(Some(parent));
}

#[allow(clippy::type_complexity)]
fn read_versions(path: &str, origins: &[(String, String, PathBuf)], game_dir: Option<&Path>) -> Result<(Vec<Version>, Option<String>, Vec<Print>), String> {
    let versions: Vec<Version> = origins
        .iter()
        .map(|(_, label, root)| {
            let bytes = std::fs::read(root.join(path)).map_err(|e| tf!("Could not read {label}'s copy: {error}", label = label, error = e))?;
            Ok(Version { label: label.clone(), text: String::from_utf8_lossy(&bytes).into_owned() })
        })
        .collect::<Result<_, String>>()?;
    let vanilla = game_dir.and_then(|dir| std::fs::read(dir.join(path)).ok()).map(|b| String::from_utf8_lossy(&b).into_owned());
    let mut found: Vec<Print> = vanilla.iter().map(|v| Print::new(VANILLA, "Vanilla", path, v)).collect();
    found.extend(origins.iter().zip(&versions).map(|((id, label, _), v)| Print::new(id, label, path, &v.text)));
    Ok((versions, vanilla, found))
}

/// Reads each copy of the file at `path` from `origins` (descriptor id, mod
/// name, folder; in load order) and the base game's, and plans merging them
/// object by object.
pub fn load_objects(path: &str, origins: &[(String, String, PathBuf)], game_dir: Option<&Path>) -> Result<(Plan, Vec<Print>), String> {
    let (versions, vanilla, found) = read_versions(path, origins, game_dir)?;
    file_merge::plan(path, vanilla.as_deref(), &versions).map(|plan| (plan, found))
}

/// Reads each copy of the file at `path` from `origins` and the base game's,
/// planning an object-by-object merge when possible or whole-file merge otherwise.
pub fn load(path: &str, origins: &[(String, String, PathBuf)], game_dir: Option<&Path>, saved: Option<&str>) -> Result<LoadResult, String> {
    let (versions, vanilla, found) = read_versions(path, origins, game_dir)?;
    if file_merge::can_split(path)
        && let Ok(plan) = file_merge::plan(path, vanilla.as_deref(), &versions)
    {
        return Ok(LoadResult::Objects(plan, found));
    }
    let vanilla_src = vanilla.as_ref().map(|t| Source { label: "Vanilla".into(), file: path.to_owned(), text: t.clone() });
    let sources: Vec<Source> = origins.iter().zip(&versions).map(|((_, label, _), v)| Source { label: label.clone(), file: path.to_owned(), text: v.text.clone() }).collect();
    let initial_text = saved.map(str::to_owned).unwrap_or_else(|| versions.last().map(|v| v.text.clone()).unwrap_or_default());
    Ok(LoadResult::WholeFile(WholeFilePlan { path: path.to_owned(), vanilla: vanilla_src, sources, initial_text }, found))
}

/// Validates a whole file merge result.
fn validate_file(path: &str, text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(t!("The merged script is empty"));
    }
    for placeholder in ["... rest of", "... remaining", "... existing", "rest of file", "remainder of file"] {
        if trimmed.to_lowercase().contains(placeholder) {
            return Err(format!("The merged script contains a truncation placeholder ('{placeholder}') instead of the full file"));
        }
    }
    let mut depth: i32 = 0;
    let mut in_quote = false;
    let mut in_comment = false;
    for ch in text.chars() {
        match ch {
            '\n' | '\r' => in_comment = false,
            '#' if !in_quote => in_comment = true,
            '"' if !in_comment => in_quote = !in_quote,
            '{' if !in_quote && !in_comment => depth += 1,
            '}' if !in_quote && !in_comment => {
                depth -= 1;
                if depth < 0 {
                    return Err(t!("The merged script has an unexpected closing brace `}` before an opening brace"));
                }
            }
            _ => {}
        }
    }
    if depth > 0 {
        return Err(tf!("The merged script is missing {depth} closing brace `}}`", depth = depth));
    }
    let category = file_merge::category(path);
    if category != "localisation" {
        if let Err(e) = crate::paradox::script::parse(text) {
            return Err(format!("The merged script has a syntax error at {e}"));
        }
        let params = crate::merge::script_check::cached_scripted_params();
        let violations = crate::merge::script_check::find_undeclared_scripted_effect_blocks(text, &params);
        if let Some(v) = violations.first() {
            return Err(format!(
                "The merged script contains invalid child block `{param} = {{ ... }}` inside scripted effect/trigger `{effect}` near line {line}. Scripted effect calls cannot contain child blocks unless declared as a parameter.",
                param = v.param, effect = v.effect, line = v.line
            ));
        }
    }
    Ok(text.to_owned())
}

/// How a plan combines the copies' objects, e.g. "12 objects kept as is ·
/// 2 conflicts to merge".
pub fn describe(p: &Plan) -> String {
    let count = |f: fn(&Status) -> bool| p.items.iter().filter(|i| f(&i.status)).count();
    let (same, recovered, one, conflicts) = (
        count(|s| *s == Status::Same),
        count(|s| matches!(s, Status::Recovered(_))),
        count(|s| matches!(s, Status::OneChange(_))),
        count(|s| matches!(s, Status::Conflict { .. })),
    );
    let mut parts = vec![tnf!("{count} object kept as is", "{count} objects kept as is", count = same)];
    if recovered > 0 {
        parts.push(tf!("{count} recovered from overridden copies", count = recovered));
    }
    if one > 0 {
        parts.push(tf!("{count} taken from the only mod that changed them", count = one));
    }
    parts.push(if conflicts == 0 { t!("no conflicts") } else { tnf!("{count} conflict to merge", "{count} conflicts to merge", count = conflicts) });
    parts.join(" · ")
}

/// Merge Tool…: merges the file as it stands in the editor with one mod's
/// copy in the external merge tool, the base game's copy as the base, and
/// puts what's saved there in the editor.
fn merge_tool_menu(request: &Rc<Request>, editor: &gtk::TextView, banner: &adw::Banner) -> gtk::MenuButton {
    let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    list.append(&gtk::Label::builder().label(t!("Merge the file below with")).xalign(0.0).margin_start(10).margin_end(10).margin_top(6).margin_bottom(6).css_classes(["dim-label", "caption-heading"]).build());
    let popover = gtk::Popover::builder().child(&list).build();
    let button = gtk::MenuButton::builder().label(t!("Merge Tool…")).popover(&popover).tooltip_text(t!("Merge the file below with a mod's copy in your merge tool; what you save there replaces it")).build();
    for (_, name, folder) in &request.origins {
        let item = gtk::Button::builder().label(tf!("{name}'s Copy", name = name)).css_classes(["flat"]).build();
        if let Some(label) = item.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        list.append(&item);
        let (name, copy) = (name.clone(), folder.join(&request.path));
        let (request, editor, banner, button, popover) = (request.clone(), editor.clone(), banner.clone(), button.clone(), popover.clone());
        item.connect_clicked(move |_| {
            popover.popdown();
            let command = (request.merge_tool)();
            if command.is_empty() {
                return show(&banner, &t!("Choose a merge tool in Preferences, on the Merging page, first."), true);
            }
            let Ok(theirs) = std::fs::read(&copy) else {
                return show(&banner, &tf!("Could not read {name}'s copy.", name = &name), true);
            };
            let base = request.game_dir.as_ref().and_then(|dir| std::fs::read(dir.join(&request.path)).ok()).map(|b| String::from_utf8_lossy(&b).into_owned());
            let buffer = editor.buffer();
            let ours = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let tool = crate::conflicts_view::merge_tool_name(&command);
            let left = crate::conflicts_view::ToolSide { label: t!("Merged file"), text: ours };
            let right = crate::conflicts_view::ToolSide { label: name.clone(), text: String::from_utf8_lossy(&theirs).into_owned() };
            let (editor, banner) = (editor.clone(), banner.clone());
            crate::conflicts_view::run_merge_tool(&button, command, left, right, base, move |result| match result {
                Ok(Some(merged)) => {
                    editor.buffer().set_text(&merged);
                    show(&banner, &t!("The merge tool's result is below. Review it, then save it to the patch."), false);
                }
                Ok(None) => show(&banner, &tf!("{tool} was closed without saving, so nothing changed.", tool = tool), false),
                Err(message) => show(&banner, &message, true),
            });
        });
    }
    button
}

fn show(banner: &adw::Banner, message: &str, error: bool) {
    banner.set_title(message);
    if error {
        banner.add_css_class("error");
    } else {
        banner.remove_css_class("error");
    }
    banner.set_revealed(true);
}

/// Shows a `gtk::StringList`'s strings, wrapped at `max_chars` when given.
fn label_factory(max_chars: Option<i32>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let label = gtk::Label::builder().xalign(0.0).build();
        if let Some(n) = max_chars {
            // Long names wrap onto more lines rather than cut off.
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_width_chars(n.min(12));
            label.set_max_width_chars(n);
            // The button's padding is sized for one line.
            label.set_margin_top(6);
            label.set_margin_bottom(6);
        }
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&label));
        }
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(label), Some(text)) = (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>()) else { return };
        label.set_label(&text.string());
    });
    factory
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_whole_files() {
        assert!(validate_file("common/inline_scripts/test.txt", "triggered_name = { text = a }").is_ok());
        assert!(validate_file("common/inline_scripts/test.txt", "").is_err());
        assert!(validate_file("common/inline_scripts/test.txt", "triggered_name = { # ... rest of file ... }").is_err());
        assert!(validate_file("common/inline_scripts/test.txt", "triggered_name = { text = a").is_err());
        assert!(validate_file("common/inline_scripts/test.txt", "triggered_name = text = a }").is_err());
    }

    #[test]
    fn loads_inline_scripts_as_whole_files() {
        let temp = std::env::temp_dir().join(format!("drukhal-file-merge-test-{}", std::process::id()));
        let path = "common/inline_scripts/districts/test.txt";
        let dir_a = temp.join("mod_a");
        let dir_b = temp.join("mod_b");
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(dir_a.join("common/inline_scripts/districts")).unwrap();
        std::fs::create_dir_all(dir_b.join("common/inline_scripts/districts")).unwrap();
        std::fs::write(dir_a.join(path), "triggered_name = { text = a }\ntriggered_name = { text = a2 }\n").unwrap();
        std::fs::write(dir_b.join(path), "triggered_name = { text = b }\ntriggered_name = { text = b2 }\n").unwrap();

        let origins = vec![
            ("id_a".to_owned(), "Mod A".to_owned(), dir_a),
            ("id_b".to_owned(), "Mod B".to_owned(), dir_b),
        ];

        let result = load(path, &origins, None, None).expect("load should succeed");
        match result {
            LoadResult::WholeFile(w, prints) => {
                assert_eq!(w.sources.len(), 2);
                assert_eq!(w.sources[0].label, "Mod A");
                assert_eq!(w.sources[1].label, "Mod B");
                assert_eq!(w.initial_text, "triggered_name = { text = b }\ntriggered_name = { text = b2 }\n");
                assert_eq!(prints.len(), 2);
            }
            LoadResult::Objects(..) => panic!("inline scripts should be WholeFile, not Objects"),
        }
        let _ = std::fs::remove_dir_all(&temp);
    }
}
