//! The game errors dialog: the last session's `error.log`, grouped by the
//! mod each error comes from, with the merged patch first.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::merge::ai;
use crate::paradox::error_log::{self, Entry, Group, Kind, Report};
use crate::thinking_view::{self, ThinkingView};

/// Rows shown per group; the rest are summarised.
const ROWS_PER_GROUP: usize = 200;

/// What Explain needs from the window.
pub struct Explainer {
    pub provider: Option<crate::merge::ai::Provider>,
    pub game_version: Option<String>,
    pub open_preferences: Box<dyn Fn()>,
}

/// Shared by every Explain button in one dialog.
struct Context {
    explain: Explainer,
    /// Answers already fetched, by error message.
    answers: RefCell<HashMap<String, String>>,
}

/// `roots` maps descriptor ids to mod folders, for opening the files
/// errors mention.
pub fn present(parent: &impl IsA<gtk::Widget>, report: &Report, log: PathBuf, modified: Option<String>, roots: HashMap<String, PathBuf>, explain: Explainer) {
    let ctx = Rc::new(Context { explain, answers: RefCell::new(HashMap::new()) });
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
            let (ctx, message, file, mod_name, root) = (ctx.clone(), e.message.clone(), e.file.clone(), g.mod_id.as_ref().map(|_| g.label.clone()), root.clone());
            button.connect_clicked(move |b| explain(b, &ctx, message.clone(), file.clone(), mod_name.clone(), root.clone()));
            entry.add_suffix(&button);
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

fn open(widget: &impl IsA<gtk::Widget>, path: &std::path::Path) {
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
}
