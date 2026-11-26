//! The game errors dialog: the last session's `error.log`, grouped by the
//! mod each error comes from, with the merged patch first.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::diff_view::{Compare, Header, Pair};
use crate::merge::ai;
use crate::merge::patch::{Print, VANILLA};
use crate::merge::source::Source;
use crate::merge::{base_game, script_check, vocabulary};
use crate::paradox::script_docs::{self, Docs};
use crate::merge_dialog;
use crate::paradox::error_log::{self, Entry, Group, Kind, Report};
use crate::paradox::suggest::{self, HarmlessSuggestion, NoteSuggestion};
use crate::thinking_view::{self, ThinkingView};

/// Rows shown per group; the rest are summarised.
const ROWS_PER_GROUP: usize = 200;

/// What Explain needs from the window.
pub struct Explainer {
    pub provider: Option<crate::merge::ai::Provider>,
    pub game_version: Option<String>,
    pub open_preferences: Box<dyn Fn()>,
    /// Present when the game and its version are known.
    pub fixer: Option<Fixer>,
}

/// A problem on a line inside one of a mod's objects.
#[derive(Clone)]
pub struct Target {
    pub category: String,
    pub key: String,
    pub mod_id: String,
    pub mod_name: String,
    /// The mod's file, game-relative, and the object's text in it.
    pub mod_copy: Source,
    /// The game's current version; none when the mod defines the object itself.
    pub game: Option<Source>,
}

pub type IsMerged = Box<dyn Fn(&str, &str) -> bool>;

/// Saves a merged object to the patch: category, key, script and the
/// fingerprints of its sources.
pub type SaveObject = Box<dyn Fn(&str, &str, &str, Vec<Print>) -> Result<(), String>>;

/// What Fix with AI needs from the window.
pub struct Fixer {
    pub game_dir: PathBuf,
    pub game_version: String,
    /// The user folder, where the game writes its script documentation.
    pub user_dir: PathBuf,
    /// The mods' folders, for the scripted effects and triggers they define.
    pub mod_roots: Vec<PathBuf>,
    /// Fixable problems, by error message.
    pub targets: HashMap<String, Target>,
    /// Errors naming a keyword the game doesn't have, by message.
    pub notes: HashMap<String, String>,
    /// Whether the patch already holds a merge of `(category, key)`.
    pub is_merged: IsMerged,
    pub save: SaveObject,
    pub saved: Box<dyn Fn()>,
}

/// Shared by every Explain button in one dialog.
struct Context {
    explain: Explainer,
    /// Answers already fetched, by error message.
    answers: RefCell<HashMap<String, String>>,
    /// What Fix with AI said about each error, by message.
    ai_notes: RefCell<HashMap<String, String>>,
}

/// `roots` maps descriptor ids to mod folders, for opening the files
/// errors mention.
pub fn present(parent: &impl IsA<gtk::Widget>, report: &Report, log: PathBuf, modified: Option<String>, roots: HashMap<String, PathBuf>, explain: Explainer) {
    let ctx = Rc::new(Context { explain, answers: RefCell::new(HashMap::new()), ai_notes: RefCell::new(HashMap::new()) });
    let page = adw::PreferencesPage::new();
    let lines = tnf!("{count} line in error.log", "{count} lines in error.log", count = report.lines);
    let problems = match report.problems() {
        0 => t!("None of them look like real problems."),
        n => tnf!("{count} is a real problem", "{count} are real problems", count = n),
    };
    let description = match modified {
        Some(when) => tf!("{lines}, written {when}. {errors}", lines = &lines, when = when, errors = &problems),
        None => tf!("{lines}. {errors}", lines = &lines, errors = &problems),
    };
    let summary = adw::PreferencesGroup::builder().description(description).build();
    let open_log = gtk::Button::builder().label(t!("Open error.log")).css_classes(["flat"]).valign(gtk::Align::Center).build();
    open_log.connect_clicked(move |b| open(b, &log));
    summary.set_header_suffix(Some(&open_log));
    page.add(&summary);

    let problem_group = adw::PreferencesGroup::builder().title(Kind::Problem.title()).build();
    let mut any_problem = false;
    for g in &report.groups {
        let shown: Vec<&Entry> = g.entries.iter().filter(|e| e.kind == Kind::Problem).collect();
        if shown.is_empty() {
            continue;
        }
        any_problem = true;
        problem_group.add(&group_row(g, &shown, roots.get(g.mod_id.as_deref().unwrap_or_default()).cloned(), Some(&ctx)));
    }
    if any_problem {
        page.add(&problem_group);
    }

    // One row per harmless kind, with the lines of every mod under it.
    let harmless = adw::PreferencesGroup::builder().title(t!("Probably Harmless")).build();
    let mut kinds: Vec<Kind> = report.groups.iter().flat_map(|g| &g.entries).map(|e| e.kind).filter(|k| *k != Kind::Problem).collect();
    kinds.sort();
    kinds.dedup();
    for &kind in &kinds {
        let mut rows: Vec<(&Group, &Entry)> = report.groups.iter().flat_map(|g| g.entries.iter().filter(move |e| e.kind == kind).map(move |e| (g, e))).collect();
        let n: usize = rows.iter().map(|(_, e)| e.count).sum();
        rows.sort_by_key(|(_, e)| std::cmp::Reverse(e.count));
        let row = adw::ExpanderRow::builder().title(kind.title()).subtitle(format!("{} · {}", tnf!("{count} line", "{count} lines", count = n), kind.explanation())).use_markup(false).subtitle_lines(3).build();
        for (g, e) in rows.iter().take(ROWS_PER_GROUP) {
            let mut subtitle = vec![g.label.clone()];
            if e.count > 1 {
                subtitle.push(format!("×{}", e.count));
            }
            // Why this line is harmless, when its kind's text isn't enough.
            subtitle.extend(e.reason.clone());
            row.add_row(&adw::ActionRow::builder().title(e.message.trim()).subtitle(subtitle.join(" · ")).use_markup(false).title_lines(3).build());
        }
        if rows.len() > ROWS_PER_GROUP {
            row.add_row(&adw::ActionRow::builder().title(tnf!("…and {count} more kind", "…and {count} more kinds", count = rows.len() - ROWS_PER_GROUP)).css_classes(["dim-label"]).build());
        }
        harmless.add(&row);
    }
    if !kinds.is_empty() {
        page.add(&harmless);
    }
    if report.groups.is_empty() {
        let empty = adw::StatusPage::builder().icon_name("emblem-ok-symbolic").title(t!("No Errors")).description(t!("The game didn't log any errors last time")).build();
        summary.set_description(None);
        let none = adw::PreferencesGroup::new();
        none.add(&empty);
        page.add(&none);
    }

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&t!("Game Errors"), &t!("From the last time Stellaris ran"))));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    adw::Dialog::builder().content_width(900).content_height(720).child(&toolbar).build().present(Some(parent));
}

/// `shown` are the entries of `g` to list, each with an Explain button.
fn group_row(g: &Group, shown: &[&Entry], root: Option<PathBuf>, ctx: Option<&Rc<Context>>) -> adw::ExpanderRow {
    let total: usize = shown.iter().map(|e| e.count).sum();
    let row = adw::ExpanderRow::builder()
        .title(&g.label)
        .subtitle(tf!("{errors} · {kinds}", errors = tnf!("{count} error", "{count} errors", count = total), kinds = tnf!("{count} kind", "{count} kinds", count = shown.len())))
        .use_markup(false)
        .build();
    if g.is_patch {
        row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").tooltip_text(t!("Errors from your merges and fixes")).css_classes(["warning"]).build());
        row.set_subtitle(&tf!("{errors} · from your merges and fixes", errors = tnf!("{count} error", "{count} errors", count = total)));
    }
    for e in shown.iter().take(ROWS_PER_GROUP) {
        let mut subtitle = Vec::new();
        if e.count > 1 {
            subtitle.push(format!("×{}", e.count));
        }
        if let Some(file) = &e.file {
            subtitle.push(file.clone());
        }
        let entry = adw::ActionRow::builder().title(e.message.trim()).subtitle(subtitle.join(" · ")).use_markup(false).title_lines(3).build();
        if let Some(ctx) = ctx {
            let button = gtk::Button::builder().label(t!("Explain")).tooltip_text(t!("Ask your AI what this error means and what to do")).valign(gtk::Align::Center).css_classes(["flat"]).build();
            let (explain_ctx, message, file, mod_name, root) = (ctx.clone(), e.message.clone(), e.file.clone(), g.mod_id.as_ref().map(|_| g.label.clone()), root.clone());
            button.connect_clicked(move |b| explain(b, &explain_ctx, message.clone(), file.clone(), mod_name.clone(), root.clone()));
            entry.add_suffix(&button);
            if let Some(fixer) = &ctx.explain.fixer
                && let Some(target) = fixer.targets.get(&e.message)
            {
                let done = gtk::Label::builder().label(t!("Fixed in the merged patch")).css_classes(["success"]).valign(gtk::Align::Center).build();
                let fix_button = gtk::Button::builder()
                    .label(t!("Fix with AI"))
                    .tooltip_text(t!("Write a corrected copy of this mod's object into the merged patch"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                if (fixer.is_merged)(&target.category, &target.key) {
                    fix_button.set_visible(false);
                } else {
                    done.set_visible(false);
                }
                let on_saved: Rc<dyn Fn()> = {
                    let (done, fix_button) = (done.clone(), fix_button.clone());
                    Rc::new(move || {
                        fix_button.set_visible(false);
                        done.set_visible(true);
                    })
                };
                let (ctx, message, target) = (ctx.clone(), e.message.clone(), target.clone());
                let edit_button = gtk::Button::builder()
                    .label(t!("Edit by Hand"))
                    .tooltip_text(t!("Write a corrected copy of this mod's object into the merged patch yourself"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                let (edit_ctx, edit_target, edit_saved) = (ctx.clone(), target.clone(), on_saved.clone());
                edit_button.connect_clicked(move |b| fix_by_hand(b, &edit_ctx, edit_target.clone(), edit_saved.clone()));
                let (fix_ctx, fix_target, fix_saved) = (ctx.clone(), target.clone(), on_saved.clone());
                fix_button.connect_clicked(move |b| fix(b, &fix_ctx, message.clone(), fix_target.clone(), fix_saved.clone()));
                entry.add_suffix(&edit_button);
                entry.add_suffix(&fix_button);
                entry.add_suffix(&done);
            }
        }
        if let Some(ctx) = ctx
            && !g.is_patch
        {
            entry.add_suffix(&suggest_menu(ctx, &e.message, g.mod_id.as_ref().map(|_| g.label.clone())));
        }
        if let (Some(root), Some(file)) = (&root, &e.file)
            && root.join(file).is_file()
        {
            let path = root.join(file);
            let button = gtk::Button::builder().icon_name("document-open-symbolic").tooltip_text(t!("Open this file")).valign(gtk::Align::Center).css_classes(["flat"]).build();
            button.connect_clicked(move |b| open(b, &path));
            entry.add_suffix(&button);
        }
        row.add_row(&entry);
    }
    if shown.len() > ROWS_PER_GROUP {
        row.add_row(&adw::ActionRow::builder().title(tnf!("…and {count} more kind", "…and {count} more kinds", count = shown.len() - ROWS_PER_GROUP)).css_classes(["dim-label"]).build());
    }
    row
}

/// The menu on a problem row for suggesting a game note or a harmless rule.
fn suggest_menu(ctx: &Rc<Context>, message: &str, mod_name: Option<String>) -> gtk::MenuButton {
    let popover = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    popover.set_child(Some(&list));
    let entry = |label: String| {
        let b = gtk::Button::builder().label(label).css_classes(["flat"]).build();
        list.append(&b);
        b
    };
    if let Some(keyword) = ctx.explain.fixer.as_ref().and_then(|f| f.notes.get(message)) {
        let (ctx, message, keyword, mod_name, popover) = (ctx.clone(), message.to_owned(), keyword.clone(), mod_name.clone(), popover.clone());
        entry(t!("Suggest a Game Note…")).connect_clicked(move |b| {
            popover.popdown();
            let error = suggest::scrub(&message, &home());
            let version = game_version(&ctx);
            let ai_notes = ctx.ai_notes.borrow().get(&message).cloned();
            let s = NoteSuggestion { keyword: &keyword, error: &error, mod_name: mod_name.as_deref(), game_version: &version, app_version: env!("CARGO_PKG_VERSION"), ai_notes: ai_notes.as_deref() };
            send_suggestion(b, s.title(), s.body());
        });
    }
    {
        let (ctx, message, popover) = (ctx.clone(), message.to_owned(), popover.clone());
        entry(t!("Report as Harmless…")).connect_clicked(move |b| {
            popover.popdown();
            let pattern = error_log::pattern(&suggest::scrub(&message, &home()));
            let version = game_version(&ctx);
            let s = HarmlessSuggestion { pattern: &pattern, mod_name: mod_name.as_deref(), game_version: &version, app_version: env!("CARGO_PKG_VERSION") };
            send_suggestion(b, s.title(), s.body());
        });
    }
    gtk::MenuButton::builder().icon_name("view-more-symbolic").tooltip_text(t!("Suggest an improvement")).valign(gtk::Align::Center).css_classes(["flat"]).popover(&popover).build()
}

fn home() -> String {
    dirs::home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

fn game_version(ctx: &Context) -> String {
    ctx.explain.game_version.clone().unwrap_or_else(|| "unknown".to_owned())
}

/// Shows exactly what will be put in the issue, and opens it on GitHub when
/// the player agrees.
fn send_suggestion(parent: &impl IsA<gtk::Widget>, title: String, body: String) {
    let body = suggest::fitted_body(&title, &body);
    let dialog = adw::AlertDialog::new(
        Some(&t!("Send a Suggestion")),
        Some(&t!("This opens GitHub in your browser with the text below filled in. Nothing is sent until you submit it there, which needs a GitHub account. Check it doesn't contain anything you'd rather keep private.")),
    );
    let text = gtk::TextView::builder().editable(false).cursor_visible(false).monospace(true).wrap_mode(gtk::WrapMode::WordChar).build();
    text.buffer().set_text(&format!("{title}\n\n{body}"));
    let scrolled = gtk::ScrolledWindow::builder().child(&text).min_content_height(200).build();
    dialog.set_extra_child(Some(&scrolled));
    dialog.add_responses(&[("cancel", &t!("Cancel")), ("open", &t!("Open GitHub"))]);
    dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let window = parent.root().and_downcast::<gtk::Window>();
    let launcher_window = window.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "open" {
            gtk::UriLauncher::new(&suggest::issue_url(&title, &body)).launch(launcher_window.as_ref(), None::<&gio::Cancellable>, |_| {});
        }
    });
    dialog.present(window.as_ref());
}

/// Asks the AI about one error in a child dialog; a repeat shows the saved answer.
fn explain(button: &gtk::Button, ctx: &Rc<Context>, message: String, file: Option<String>, mod_name: Option<String>, root: Option<PathBuf>) {
    let Some(provider) = ctx.explain.provider.clone() else {
        (ctx.explain.open_preferences)();
        return;
    };
    let error = gtk::Label::builder().label(message.trim()).wrap(true).selectable(true).xalign(0.0).css_classes(["monospace", "dim-label"]).build();
    let thinking = ThinkingView::new();
    let answer = gtk::Label::builder().wrap(true).selectable(true).xalign(0.0).build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&error);
    body.append(&thinking.widget);
    body.append(&answer);
    let scrolled = gtk::ScrolledWindow::builder().child(&body).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scrolled));
    let dialog = adw::Dialog::builder().title(t!("What This Error Means")).content_width(720).content_height(560).child(&toolbar).build();
    dialog.present(Some(button));

    if let Some(saved) = ctx.answers.borrow().get(&message) {
        thinking.widget.set_visible(false);
        answer.set_label(saved);
        return;
    }
    let ctx = ctx.clone();
    let game_version = ctx.explain.game_version.clone();
    glib::spawn_future_local(async move {
        let key = message.clone();
        let result = thinking_view::run(&thinking, None, move |on| {
            // Reading the mod's file happens here, off the UI thread.
            let excerpt = match (&root, &file) {
                (Some(root), Some(file)) if error_log::is_inside(file) => std::fs::read(root.join(file)).ok().map(|b| {
                    let text = String::from_utf8_lossy(&b);
                    error_log::excerpt(&text, error_log::find_line(&message, file), 15, 6000)
                }),
                _ => None,
            };
            let question = ai::ErrorQuestion {
                message: &message,
                mod_name: mod_name.as_deref(),
                game_version: game_version.as_deref(),
                file: file.as_deref().zip(excerpt.as_deref()),
            };
            ai::explain(&provider, &question, on)
        })
        .await;
        match result {
            Some(Ok(m)) => {
                ctx.answers.borrow_mut().insert(key, m.notes.clone());
                answer.set_label(&m.notes);
            }
            Some(Err(e)) => {
                answer.set_label(&tf!("Couldn't get an explanation: {error}", error = e));
                answer.add_css_class("error");
            }
            None => answer.set_label(&t!("Couldn't get an explanation.")),
        }
    });
}

fn margins(widget: &impl IsA<gtk::Widget>) {
    widget.set_margin_start(12);
    widget.set_margin_end(12);
}

/// Asks the AI for a corrected copy of the mod's object, shows it beside the
/// mod's copy to edit, and saves it to the merged patch.
fn fix(button: &gtk::Button, ctx: &Rc<Context>, message: String, target: Target, on_saved: Rc<dyn Fn()>) {
    let (Some(provider), Some(fixer)) = (ctx.explain.provider.clone(), ctx.explain.fixer.as_ref()) else {
        (ctx.explain.open_preferences)();
        return;
    };
    let (game_dir, game_version) = (fixer.game_dir.clone(), fixer.game_version.clone());
    let (user_dir, mod_roots) = (fixer.user_dir.clone(), fixer.mod_roots.clone());

    let error = gtk::Label::builder().label(message.trim()).wrap(true).selectable(true).xalign(0.0).css_classes(["monospace", "dim-label"]).build();
    let intro = gtk::Label::builder()
        .label(if target.game.is_some() {
            tf!("{name} replaces {key}, which the game has changed. The fix starts from the game's current version and keeps the mod's changes.", name = &target.mod_name, key = &target.key)
        } else {
            tf!("{name} defines {key} itself, so the game has no version to compare it with. The fix changes only what the error is about.", name = &target.mod_name, key = &target.key)
        })
        .wrap(true)
        .xalign(0.0)
        .build();
    let failure = gtk::Label::builder().wrap(true).selectable(true).xalign(0.0).css_classes(["error"]).visible(false).build();
    let thinking = ThinkingView::new();
    let working = gtk::Box::new(gtk::Orientation::Vertical, 12);
    working.set_margin_top(12);
    working.set_margin_bottom(12);
    margins(&working);
    working.append(&error);
    working.append(&intro);
    working.append(&thinking.widget);
    working.append(&failure);
    let scrolled = gtk::ScrolledWindow::builder().child(&working).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
    let stack = gtk::Stack::new();
    stack.add_named(&scrolled, Some("working"));
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let stop_button = gtk::Button::builder().label(t!("Stop")).build();
    header.pack_end(&stop_button);
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let dialog = adw::Dialog::builder().title(t!("Fix This Error")).content_width(900).content_height(720).child(&toolbar).build();
    dialog.present(Some(button));

    stop_button.connect_clicked(glib::clone!(
        #[strong]
        thinking,
        move |btn| {
            btn.set_sensitive(false);
            thinking.stop();
        }
    ));
    dialog.connect_closed(glib::clone!(
        #[strong]
        thinking,
        move |_| {
            thinking.stop();
        }
    ));

    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        let job_target = target.clone();
        let job_message = message.clone();
        let result = thinking_view::run(&thinking, None, move |on| {
            let t = job_target;
            // With no game version to work from, the model gets the game's own
            // documentation, and the result may not use names it lacks.
            let own = if t.game.is_none() { OwnCheck::load(&user_dir, &game_dir, &game_version, &mod_roots) } else { None };
            let check = |text: &str| {
                let script = merge_dialog::validate(&t.category, &t.key, text)?;
                if let Some(own) = &own {
                    let added = script_check::introduced_names(&own.context, &own.known, &t.mod_copy.file, &t.mod_copy.text, &script);
                    if !added.is_empty() {
                        return Err(format!("it uses names the current game doesn't have: {}", added.join(", ")));
                    }
                }
                Ok(script)
            };
            let checked = match &t.game {
                Some(game) => ai::repair_checked(&provider, &t.category, &t.key, game, &t.mod_copy, &job_message, &check, on)?,
                None => {
                    let reference = own.as_ref().map(|o| o.reference(&job_message, &t.mod_copy)).unwrap_or_default();
                    ai::repair_own_checked(&provider, &t.category, &t.key, &t.mod_copy, &job_message, &reference, &check, on)?
                }
            };
            let text = draft(&checked).script;
            // Keywords found in neither the game nor the mod's copy.
            let warnings = match base_game::ensure(&base_game::dir(), &game_dir, &game_version) {
                Ok(snapshot) => vocabulary::unknown(&text, snapshot.texts().chain(t.game.as_ref().map(|g| g.text.as_str())).chain([t.mod_copy.text.as_str()])),
                Err(_) => Vec::new(),
            };
            Ok::<_, String>((checked, warnings))
        })
        .await;
        stop_button.set_visible(false);
        match result {
            Some(Ok((checked, warnings))) => {
                if let Some(rejected) = draft(&checked).rejected {
                    crate::diagnostics::log!("ai", "Error-log fix for {} {} failed validation: {rejected}", target.category, target.key);
                }
                ctx.ai_notes.borrow_mut().insert(message.clone(), checked.merged.notes.clone());
                let version = ctx.explain.fixer.as_ref().map(|f| f.game_version.clone()).unwrap_or_default();
                let page = review(&ctx, &dialog, &target, checked, &warnings, &version, on_saved);
                stack.add_named(&page, Some("review"));
                stack.set_visible_child_name("review");
            }
            Some(Err(e)) => {
                failure.set_label(&tf!("Couldn't get a fix: {error}", error = e));
                failure.set_visible(true);
            }
            None => {
                if thinking.is_stopped() {
                    failure.set_label(&t!("Fix stopped"));
                } else {
                    failure.set_label(&tf!("Couldn't get a fix: {error}", error = t!("The request failed unexpectedly")));
                }
                failure.set_visible(true);
            }
        }
    });
}

/// Opens the same review starting from the mod's copy, without a provider.
fn fix_by_hand(button: &gtk::Button, ctx: &Rc<Context>, target: Target, on_saved: Rc<dyn Fn()>) {
    let Some(version) = ctx.explain.fixer.as_ref().map(|fixer| fixer.game_version.clone()) else { return };
    let stack = gtk::Stack::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&stack));
    let dialog = adw::Dialog::builder().title(t!("Fix This Error")).content_width(900).content_height(720).child(&toolbar).build();
    let page = review_manual(ctx, &dialog, &target, &version, on_saved);
    stack.add_named(&page, Some("review"));
    dialog.present(Some(button));
}

/// What checks a fix of a mod's own object: the game's documentation and the
/// names it and the enabled mods define.
struct OwnCheck {
    docs: Docs,
    context: script_check::Context,
    known: script_check::Known,
}

impl OwnCheck {
    /// `None` when the game hasn't written its script documentation. Reads
    /// many files, so it runs off the UI thread.
    fn load(user_dir: &std::path::Path, game_dir: &std::path::Path, version: &str, mod_roots: &[PathBuf]) -> Option<OwnCheck> {
        let docs = script_docs::load(user_dir)?;
        let context = script_check::ensure_context(&base_game::dir().join("script-context"), game_dir, &docs, version);
        let mut folders: Vec<&std::path::Path> = vec![game_dir];
        folders.extend(mod_roots.iter().map(PathBuf::as_path));
        let known = script_check::known(&docs, &folders);
        Some(OwnCheck { docs, context, known })
    }

    /// The documentation entries for the names in the error and the object.
    fn reference(&self, error: &str, mod_copy: &Source) -> String {
        let flagged = script_check::check_text(&self.context, &self.known, &mod_copy.file, &mod_copy.text).into_iter().map(|(_, name, _, _)| name);
        let names = script_check::reference_names(error, &mod_copy.file, flagged);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        self.docs.reference(&names, 6000)
    }
}

/// The fix next to the mod's copy, to edit and save.
fn review(ctx: &Rc<Context>, dialog: &adw::Dialog, target: &Target, checked: ai::Checked, warnings: &[String], version: &str, on_saved: Rc<dyn Fn()>) -> gtk::Box {
    let Draft { script, rejected } = draft(&checked);
    show_review(ctx, dialog, target, script, &checked.merged.notes, rejected, warnings, version, on_saved)
}

/// The script to show for a checked fix.
#[derive(Debug, PartialEq)]
struct Draft {
    script: String,
    /// Why the validator rejected `script`; none when it passed.
    rejected: Option<String>,
}

/// A rejected answer is kept as a draft, with the reason, rather than passed
/// off as the fix.
fn draft(checked: &ai::Checked) -> Draft {
    match &checked.script {
        Ok(script) => Draft { script: script.clone(), rejected: None },
        Err(summary) => {
            let mut reason = summary.clone();
            for problem in &checked.problems {
                if !reason.contains(&problem.message) {
                    reason.push_str("; ");
                    reason.push_str(&problem.message);
                }
            }
            Draft { script: checked.merged.script.clone(), rejected: Some(reason) }
        }
    }
}

/// The mod's own copy next to itself, to correct by hand and save.
fn review_manual(ctx: &Rc<Context>, dialog: &adw::Dialog, target: &Target, version: &str, on_saved: Rc<dyn Fn()>) -> gtk::Box {
    let hint = t!("Change the right-hand copy, then save it to the merged patch. Rescan to see what's left.");
    show_review(ctx, dialog, target, target.mod_copy.text.clone(), &hint, None, &[], version, on_saved)
}

/// `script` next to the mod's copy, to edit and save. `notes` is what the
/// model said, or how to start; `problem` is why a model's result was rejected,
/// which keeps Save off until the user chooses to use it anyway.
#[allow(clippy::too_many_arguments)]
fn show_review(ctx: &Rc<Context>, dialog: &adw::Dialog, target: &Target, script: String, notes: &str, problem: Option<String>, warnings: &[String], version: &str, on_saved: Rc<dyn Fn()>) -> gtk::Box {
    let has_problem = problem.is_some();
    let pair = Pair::new(true);
    pair.show(&target.mod_copy.text, &script, &Compare::Each, false);
    pair.left.set_header(&Header { title: target.mod_name.clone(), subtitle: target.mod_copy.file.clone(), color: None, badge: None });
    let right_title = if problem.is_some() { t!("Rejected Draft") } else { t!("Fixed Copy") };
    pair.right.set_header(&Header { title: right_title, subtitle: String::new(), color: None, badge: None });
    pair.widget.set_vexpand(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_margin_top(6);
    body.set_margin_bottom(12);
    body.append(&pair.widget);
    let label = |text: String, classes: &[&str]| {
        let l = gtk::Label::builder().label(text).wrap(true).xalign(0.0).selectable(true).css_classes(classes.to_vec()).build();
        margins(&l);
        l
    };
    if !notes.trim().is_empty() {
        body.append(&label(notes.to_owned(), &["dim-label"]));
    }
    if !warnings.is_empty() {
        body.append(&label(tf!("Not found in Stellaris {version} or the mod's copy: {keys}. Check these before saving.", version = version, keys = warnings.join(", ")), &["warning"]));
    }
    let error = label(String::new(), &["error"]);
    error.set_visible(false);
    if let Some(problem) = problem {
        error.set_label(&tf!("The model's result failed validation: {problem}. It is shown as a draft only. Fix it by hand, use it anyway, or discard it and try again.", problem = problem));
        error.set_visible(true);
    }
    body.append(&error);
    let use_anyway = gtk::Button::builder().label(t!("Use It Anyway")).build();
    let discard = gtk::Button::builder().label(t!("Discard")).build();
    let save = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).build();
    let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).build();
    margins(&buttons);
    buttons.append(&discard);
    if has_problem {
        save.set_sensitive(false);
        buttons.append(&use_anyway);
    }
    buttons.append(&save);
    body.append(&buttons);

    use_anyway.connect_clicked(glib::clone!(
        #[strong]
        save,
        move |btn| {
            save.set_sensitive(true);
            btn.set_visible(false);
        }
    ));

    let close = dialog.clone();
    discard.connect_clicked(move |_| {
        close.close();
    });
    let (ctx, dialog, target, weak) = (ctx.clone(), dialog.clone(), target.clone(), Rc::downgrade(&pair));
    save.connect_clicked(move |_| {
        let Some(pair) = weak.upgrade() else { return };
        let Some(fixer) = &ctx.explain.fixer else { return };
        let show = |message: &str| {
            error.set_label(message);
            error.set_visible(true);
        };
        let script = match merge_dialog::validate(&target.category, &target.key, &pair.right.text()) {
            Ok(script) => script,
            Err(message) => return show(&message),
        };
        // Fingerprints of both sources, so the patch flags this copy as
        // outdated if the game or the mod changes again.
        let mut prints = Vec::new();
        if let Some(game) = &target.game {
            prints.push(Print::new(VANILLA, "Vanilla", &game.file, &game.text));
        }
        prints.push(Print::new(&target.mod_id, &target.mod_name, &target.mod_copy.file, &target.mod_copy.text));
        match (fixer.save)(&target.category, &target.key, &script, prints) {
            Ok(()) => {
                (fixer.saved)();
                on_saved();
                dialog.close();
            }
            Err(message) => show(&message),
        }
    });
    // Keeps the comparison refreshing while it's on screen.
    let cell = std::cell::RefCell::new(Some(pair));
    body.connect_destroy(move |_| drop(cell.take()));
    body
}

fn open(widget: &impl IsA<gtk::Widget>, path: &std::path::Path) {
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(script: Result<String, String>) -> ai::Checked {
        ai::Checked {
            merged: ai::Merged { script: "a = { b = 1 ".into(), notes: String::new(), usage: None },
            script,
            problems: Vec::new(),
            retried: true,
            scripts: Vec::new(),
        }
    }

    #[test]
    fn rejected_answer_is_a_draft_carrying_the_reason() {
        let draft = draft(&checked(Err("unbalanced braces".into())));

        assert_eq!(draft, Draft { script: "a = { b = 1 ".into(), rejected: Some("unbalanced braces".into()) });
    }

    #[test]
    fn accepted_answer_is_used_without_a_warning() {
        let draft = draft(&checked(Ok("a = { b = 1 }".into())));

        assert_eq!(draft, Draft { script: "a = { b = 1 }".into(), rejected: None });
    }
}
