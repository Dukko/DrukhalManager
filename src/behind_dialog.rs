//! The objects enabled mods replace that a game update changed: the mods
//! still load their own copies, so the game's changes don't apply to them.
//! Each can be brought up to date with AI and saved to the merged patch, one
//! at a time or all at once. Update All sends every request first, saving
//! nothing, then shows the results one by one to save or skip.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::conflicts_view::{Analysis, MergeHooks};
use crate::diff_view::{Compare, Header, Pair};
use crate::folder_merge_dialog::{dollars, tokens};
use crate::merge::ai::{self, Checked, Estimate, Progress, Provider, Versions};
use crate::merge::base_game::{self, Behind, Saved};
use crate::merge::patch::{Print, VANILLA};
use crate::merge::snapshot;
use crate::merge::source::{self, Source};
use crate::merge::update_all;
use crate::merge::vocabulary;
use crate::merge_dialog;
use crate::thinking_view::{self, ThinkingView};

pub fn present(parent: &impl IsA<gtk::Widget>, analysis: Rc<Analysis>, hooks: Rc<MergeHooks>) {
    let Some((old, new)) = analysis.behind_versions.clone() else { return };
    let navigation = adw::NavigationView::new();
    let dialog = adw::Dialog::builder().content_width(900).content_height(720).child(&navigation).build();

    let page = adw::PreferencesPage::new();
    let intro = adw::PreferencesGroup::builder()
        .description(t!("These mods still load their own copies, so the game's changes to these objects don't apply until the mods are updated."))
        .build();
    page.add(&intro);

    // Update All is offered while at least two of the objects are still to do.
    let all_button = gtk::Button::builder().label(t!("Update All with AI")).css_classes(["suggested-action"]).halign(gtk::Align::End).build();
    let all_group = adw::PreferencesGroup::new();
    all_group.add(&all_button);
    page.add(&all_group);
    // Each row's "Updated in the merged patch" label.
    let labels: Rc<RefCell<Vec<(String, String, gtk::Label)>>> = Rc::default();
    let refresh: Rc<dyn Fn()> = {
        let (analysis, hooks, labels, all_group) = (analysis.clone(), hooks.clone(), labels.clone(), all_group.clone());
        Rc::new(move || {
            for (category, key, label) in labels.borrow().iter() {
                label.set_visible((hooks.is_merged)(category, key));
            }
            all_group.set_visible(update_all::included(&analysis.behind, |c, k| (hooks.is_merged)(c, k)).len() >= 2);
        })
    };
    refresh();

    let mut by_mod: BTreeMap<usize, Vec<&Behind>> = BTreeMap::new();
    for b in &analysis.behind {
        by_mod.entry(b.mod_position).or_default().push(b);
    }
    for (position, objects) in by_mod {
        let group = adw::PreferencesGroup::builder().title(glib_escape(analysis.name(position))).build();
        for b in objects {
            let row = adw::ActionRow::builder().title(&b.key).subtitle(&b.category).use_markup(false).activatable(true).build();
            if b.now.is_none() {
                row.add_suffix(&gtk::Label::builder().label(t!("Removed from the game")).css_classes(["dim-label"]).build());
            }
            // Shown once the object is in the patch, including after a save
            // from the page this row opens.
            let updated = gtk::Label::builder().label(t!("Updated in the merged patch")).css_classes(["success"]).visible((hooks.is_merged)(&b.category, &b.key)).build();
            labels.borrow_mut().push((b.category.clone(), b.key.clone(), updated.clone()));
            row.add_suffix(&updated);
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let (navigation, analysis, hooks, b, old, new, refresh) = (navigation.clone(), analysis.clone(), hooks.clone(), b.clone(), old.clone(), new.clone(), refresh.clone());
            row.connect_activated(move |_| {
                let (updated, refresh) = (updated.clone(), refresh.clone());
                navigation.push(&detail(
                    &analysis,
                    &hooks,
                    &navigation,
                    &b,
                    &old,
                    &new,
                    Rc::new(move || {
                        updated.set_visible(true);
                        refresh();
                    }),
                ));
            });
            group.add(&row);
        }
        page.add(&group);
    }

    let batch = Rc::new(Batch {
        analysis: analysis.clone(),
        hooks: hooks.clone(),
        navigation: navigation.clone(),
        dialog: dialog.clone(),
        old: old.clone(),
        new: new.clone(),
        refresh,
        cancelled: Rc::default(),
    });
    // Closing the dialog stops a run once the requests in progress finish.
    dialog.connect_closed({
        let batch = batch.clone();
        move |_| batch.cancelled.set(true)
    });
    all_button.connect_clicked(move |_| batch.ask());

    navigation.add(&wrap(&t!("Changed by the Game Update"), Some(&tf!("Objects your enabled mods replace that changed between Stellaris {old} and {new}", old = &old, new = &new)), &page));
    dialog.present(Some(parent));
}

/// Group titles are markup; mod names can hold `&` and `<`.
fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

/// A page with its own header bar.
fn wrap(title: &str, subtitle: Option<&str>, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, subtitle.unwrap_or_default())));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder().title(title).child(&toolbar).build()
}

/// Ties `value`'s lifetime to `widget`'s, so a pair keeps refreshing its
/// comparison while its widget is on screen.
fn keep_alive<T: 'static>(widget: &impl IsA<gtk::Widget>, value: Rc<T>) {
    let cell = std::cell::RefCell::new(Some(value));
    widget.connect_destroy(move |_| drop(cell.take()));
}

fn dim(text: String) -> gtk::Label {
    gtk::Label::builder().label(text).css_classes(["dim-label"]).wrap(true).xalign(0.0).margin_top(6).margin_bottom(12).margin_start(12).margin_end(12).build()
}

fn error_label() -> gtk::Label {
    gtk::Label::builder().css_classes(["error"]).wrap(true).xalign(0.0).margin_bottom(6).margin_start(12).margin_end(12).selectable(true).visible(false).build()
}

fn show_error(label: &gtk::Label, message: &str) {
    label.set_label(message);
    label.set_visible(true);
}

/// The mod's copy of the object as it is on disk now.
fn read_mod_copy(analysis: &Analysis, b: &Behind) -> Option<String> {
    read_copy(analysis.roots.get(b.mod_position)?.as_deref(), b)
}

/// As [`read_mod_copy`], from the mod's folder. Reads a file.
fn read_copy(root: Option<&Path>, b: &Behind) -> Option<String> {
    let bytes = std::fs::read(root?.join(&b.file)).ok()?;
    source::extract(&b.category, &b.key, String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}'))
}

/// Everything one update needs, so it can run off the UI thread.
#[derive(Clone)]
struct Job {
    category: String,
    key: String,
    before: Source,
    after: Source,
    mod_copy: Source,
    game_dir: Option<PathBuf>,
    /// The game's current version.
    version: String,
}

impl Job {
    fn new(analysis: &Analysis, hooks: &MergeHooks, b: &Behind, now: &Saved, old: &str, new: &str, mod_text: &str) -> Job {
        let source = |label: String, file: &str, text: &str| Source { label, file: file.to_owned(), text: text.to_owned() };
        Job {
            category: b.category.clone(),
            key: b.key.clone(),
            before: source(tf!("Stellaris {version}", version = old), &b.was.file, &b.was.text),
            after: source(tf!("Stellaris {version}", version = new), &now.file, &now.text),
            mod_copy: source(analysis.name(b.mod_position).to_owned(), &b.file, mod_text),
            game_dir: hooks.game_dir.clone(),
            version: new.to_owned(),
        }
    }

    fn versions(&self) -> Versions<'_> {
        Versions { before: &self.before, after: &self.after, mod_copy: &self.mod_copy }
    }

    /// Asks the model, and looks for keywords found in neither game version,
    /// the game's other objects, nor the mod's copy. Blocks on the network
    /// and reads files, so it runs off the UI thread.
    fn run(&self, provider: &Provider, on: &mut dyn FnMut(Progress)) -> Result<(Checked, Vec<String>), String> {
        let check = |text: &str| merge_dialog::validate(&self.category, &self.key, text);
        let checked = ai::update_checked(provider, &self.category, &self.key, &self.versions(), &check, on)?;
        let text = match &checked.script {
            Ok(script) => script.clone(),
            Err(_) => checked.merged.script.clone(),
        };
        let snapshot = self.game_dir.as_deref().and_then(|dir| base_game::ensure(&base_game::dir(), dir, &self.version).ok());
        let known = snapshot.iter().flat_map(|s| s.texts()).chain([self.before.text.as_str(), self.after.text.as_str(), self.mod_copy.text.as_str()]);
        let warnings = vocabulary::unknown(&text, known);
        Ok((checked, warnings))
    }
}

/// Saves `text` as the updated copy of `b` to the merged patch.
fn save_update(analysis: &Analysis, hooks: &MergeHooks, b: &Behind, now: &Saved, mod_text: &str, text: &str) -> Result<(), String> {
    let script = merge_dialog::validate(&b.category, &b.key, text)?;
    // Fingerprints of both sources, so the patch flags this copy as
    // outdated if the game or the mod changes again.
    let position = b.mod_position;
    let prints = vec![Print::new(VANILLA, "Vanilla", &now.file, &now.text), Print::new(&analysis.ids[position], analysis.name(position), &b.file, mod_text)];
    (hooks.save_object)(&b.category, &b.key, &script, prints)
}

/// The result next to the mod's copy, to edit, with the notes, the keyword
/// warning and the model's remaining problem, if any. The caller adds the
/// buttons.
struct Review {
    body: gtk::Box,
    pair: Rc<Pair>,
    error: gtk::Label,
    buttons: gtk::Box,
}

fn review_parts(analysis: &Analysis, b: &Behind, mod_text: &str, version: &str, checked: Checked, warnings: &[String]) -> Review {
    let (script, problem) = match checked.script {
        Ok(script) => (script, None),
        // Kept even when still rejected, to fix by hand.
        Err(problem) => (checked.merged.script, Some(problem)),
    };
    let pair = Pair::new(true);
    pair.show(mod_text, &script, &Compare::Each, false);
    pair.left.set_header(&Header { title: analysis.name(b.mod_position).to_owned(), subtitle: b.file.clone(), color: None, badge: None });
    pair.right.set_header(&Header { title: t!("Updated Copy"), subtitle: String::new(), color: None, badge: None });
    pair.widget.set_vexpand(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&pair.widget);
    if !checked.merged.notes.trim().is_empty() {
        body.append(&dim(checked.merged.notes.clone()));
    }
    if !warnings.is_empty() {
        let warning = gtk::Label::builder()
            .label(tf!("Not found in Stellaris {version} or the mod's copy: {keys}. Check these before saving.", version = version, keys = warnings.join(", ")))
            .wrap(true)
            .xalign(0.0)
            .selectable(true)
            .css_classes(["warning"])
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();
        body.append(&warning);
    }
    let error = error_label();
    if let Some(problem) = problem {
        show_error(&error, &tf!("The model's result still has a problem after a correction: {problem}. Fix it by hand, or undo and try again.", problem = problem));
    }
    body.append(&error);
    let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).margin_start(12).margin_end(12).margin_bottom(12).build();
    body.append(&buttons);
    Review { body, pair, error, buttons }
}

/// What the page needs once the player asks for an update.
struct Update {
    analysis: Rc<Analysis>,
    hooks: Rc<MergeHooks>,
    navigation: adw::NavigationView,
    b: Behind,
    old: String,
    new: String,
    now: Saved,
    mod_text: String,
    /// Holds the comparison, and the review once there is a result.
    stack: gtk::Stack,
    on_saved: Rc<dyn Fn()>,
}

fn detail(analysis: &Rc<Analysis>, hooks: &Rc<MergeHooks>, navigation: &adw::NavigationView, b: &Behind, old: &str, new: &str, on_saved: Rc<dyn Fn()>) -> adw::NavigationPage {
    let pair = Pair::new(false);
    pair.show(&b.was.text, b.now.as_ref().map_or("", |n| n.text.as_str()), &Compare::Each, false);
    pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = old), subtitle: b.was.file.clone(), color: None, badge: None });
    pair.right.set_header(&Header {
        title: tf!("Stellaris {version}", version = new),
        subtitle: b.now.as_ref().map_or_else(|| t!("Removed from the game"), |n| n.file.clone()),
        color: None,
        badge: None,
    });
    pair.widget.set_vexpand(true);

    let note = dim(tf!("{name} still uses its own copy, from {file}.", name = analysis.name(b.mod_position), file = &b.file));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&pair.widget);
    body.append(&note);
    keep_alive(&body, pair);

    let stack = gtk::Stack::new();
    stack.add_named(&body, Some("compare"));

    // Objects the update removed have nothing to bring up to date with.
    if let Some(now) = &b.now {
        match read_mod_copy(analysis, b) {
            None => body.append(&dim(t!("The mod's copy couldn't be read, so it can't be updated here."))),
            Some(mod_text) => {
                let update = Rc::new(Update {
                    analysis: analysis.clone(),
                    hooks: hooks.clone(),
                    navigation: navigation.clone(),
                    b: b.clone(),
                    old: old.to_owned(),
                    new: new.to_owned(),
                    now: now.clone(),
                    mod_text,
                    stack: stack.clone(),
                    on_saved,
                });
                let error = error_label();
                let thinking = ThinkingView::new();
                let button = gtk::Button::builder().label(t!("Update with AI")).css_classes(["suggested-action"]).halign(gtk::Align::End).margin_start(12).margin_end(12).margin_bottom(12).build();
                body.append(&error);
                body.append(&button);
                let thinking_box = gtk::Box::builder().margin_start(12).margin_end(12).margin_bottom(12).build();
                thinking_box.append(&thinking.widget);
                thinking.widget.set_hexpand(true);
                body.append(&thinking_box);
                let (button_ref, error) = (button.clone(), error);
                button.connect_clicked(move |_| update.start(&button_ref, &error, &thinking));
            }
        }
    }
    wrap(&b.key, None, &stack)
}

impl Update {
    fn start(self: &Rc<Self>, button: &gtk::Button, error: &gtk::Label, thinking: &Rc<ThinkingView>) {
        let Some(provider) = (self.hooks.provider)() else {
            (self.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
            (self.hooks.open_preferences)();
            return;
        };
        let job = Job::new(&self.analysis, &self.hooks, &self.b, &self.now, &self.old, &self.new, &self.mod_text);
        button.set_sensitive(false);
        error.set_visible(false);
        let (update, button, error, thinking) = (self.clone(), button.clone(), error.clone(), thinking.clone());
        glib::spawn_future_local(async move {
            let result = thinking_view::run(&thinking, None, move |on| job.run(&provider, on)).await;
            button.set_sensitive(true);
            match result {
                Some(Ok((checked, warnings))) => update.review(checked, &warnings),
                Some(Err(message)) => show_error(&error, &message),
                None => show_error(&error, &t!("The merge failed unexpectedly")),
            }
        });
    }

    /// Shows the result next to the mod's copy, to edit and save.
    fn review(self: &Rc<Self>, checked: Checked, warnings: &[String]) {
        let Review { body, pair, error, buttons } = review_parts(&self.analysis, &self.b, &self.mod_text, &self.new, checked, warnings);
        let discard = gtk::Button::builder().label(t!("Discard")).build();
        let save = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).build();
        buttons.append(&discard);
        buttons.append(&save);

        let stack = self.stack.clone();
        discard.connect_clicked(move |_| stack.set_visible_child_name("compare"));
        let (update, weak) = (self.clone(), Rc::downgrade(&pair));
        save.connect_clicked(move |_| {
            if let Some(pair) = weak.upgrade() {
                update.save(&pair.right.text(), &error);
            }
        });
        keep_alive(&body, pair);

        if let Some(old) = self.stack.child_by_name("review") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&body, Some("review"));
        self.stack.set_visible_child_name("review");
    }

    fn save(&self, text: &str, error: &gtk::Label) {
        match save_update(&self.analysis, &self.hooks, &self.b, &self.now, &self.mod_text, text) {
            Ok(()) => {
                self.hooks.saved();
                (self.on_saved)();
                (self.hooks.toast)(&tf!("Saved the updated {key} to the merged patch", key = &self.b.key));
                self.navigation.pop();
            }
            Err(message) => show_error(error, &message),
        }
    }
}

/// The page of the list and what Update All needs from it.
struct Batch {
    analysis: Rc<Analysis>,
    hooks: Rc<MergeHooks>,
    navigation: adw::NavigationView,
    dialog: adw::Dialog,
    old: String,
    new: String,
    /// Refreshes the list's labels and the Update All button.
    refresh: Rc<dyn Fn()>,
    /// Set by Stop and by closing the dialog.
    cancelled: Rc<Cell<bool>>,
}

/// One object queued for Update All.
struct Queued {
    /// Index into the analysis' `behind`.
    index: usize,
    now: Saved,
    mod_text: String,
    job: Job,
}

/// A finished request: the result and the keywords it uses that nothing has.
type Done = (Checked, Vec<String>);

/// Finished requests, by position in the queue.
type Finished = Vec<(usize, Result<Done, String>)>;

impl Batch {
    /// Reads the mods' copies, then asks the player to confirm with the
    /// number of requests and what they may cost.
    fn ask(self: &Rc<Self>) {
        let Some(provider) = (self.hooks.provider)() else {
            (self.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
            (self.hooks.open_preferences)();
            return;
        };
        let chosen = update_all::included(&self.analysis.behind, |c, k| (self.hooks.is_merged)(c, k));
        if chosen.is_empty() {
            return;
        }
        let wanted: Vec<(usize, Option<PathBuf>, Behind)> = chosen.iter().map(|&i| (i, self.analysis.roots.get(self.analysis.behind[i].mod_position).cloned().flatten(), self.analysis.behind[i].clone())).collect();
        let batch = self.clone();
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking(move || wanted.into_iter().map(|(i, root, b)| (i, read_copy(root.as_deref(), &b))).collect::<Vec<_>>()).await.unwrap_or_default();
            let mut items = Vec::new();
            let mut unreadable = Vec::new();
            for (index, text) in read {
                let b = &batch.analysis.behind[index];
                match (text, &b.now) {
                    (Some(mod_text), Some(now)) => {
                        let job = Job::new(&batch.analysis, &batch.hooks, b, now, &batch.old, &batch.new, &mod_text);
                        items.push(Queued { index, now: now.clone(), mod_text, job });
                    }
                    _ => unreadable.push((index, t!("The mod's copy couldn't be read, so it can't be updated here."))),
                }
            }
            if items.is_empty() {
                // Nothing to send; show why.
                let run = Rc::new(Run::new(batch.clone(), items, unreadable));
                return run.finish(0);
            }
            batch.confirm(provider, items, unreadable);
        });
    }

    /// Shows how many requests will be sent and the estimated cost.
    fn confirm(self: &Rc<Self>, provider: Provider, items: Vec<Queued>, unreadable: Vec<(usize, String)>) {
        let total = items.iter().fold(Estimate::default(), |mut sum, q| {
            sum += ai::estimate_update(&q.job.category, &q.job.key, &q.job.versions());
            sum
        });
        let parallel = self.hooks.merges_at_once.clamp(1, items.len());
        let count = items.len();
        let alert = adw::AlertDialog::new(Some(&tnf!("Update {count} Object with AI?", "Update {count} Objects with AI?", count = count)), None);
        let intro = tf!("Each object is sent to {model} on its own, up to {parallel} at a time. Nothing is saved until you have looked at each result.", model = provider.model_name(), parallel = parallel);
        let calls = tnf!("{count} AI call", "{count} AI calls", count = count);
        let summary = tf!("{calls}{without_ai} · about {input} tokens in, {low}–{high} out", calls = &calls, without_ai = "", input = tokens(total.input), low = tokens(total.output_low), high = tokens(total.output_high));
        let body = {
            let (intro, summary) = (intro.clone(), summary.clone());
            move |price_line: &str| format!("{intro}\n\n{summary}\n{price_line}")
        };
        alert.set_body(&body(&t!("Looking up the model's price…")));
        alert.add_responses(&[("cancel", &t!("Cancel")), ("update", &t!("Update All"))]);
        alert.set_response_appearance("update", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let items = Rc::new(RefCell::new(Some(items)));
        let batch = self.clone();
        let (run_provider, unreadable) = (provider.clone(), RefCell::new(Some(unreadable)));
        alert.connect_response(None, move |_, response| {
            if response != "update" {
                return;
            }
            if let (Some(items), Some(unreadable)) = (items.borrow_mut().take(), unreadable.borrow_mut().take()) {
                batch.cancelled.set(false);
                Rc::new(Run::new(batch.clone(), items, unreadable)).start(run_provider.clone());
            }
        });
        alert.present(Some(&self.dialog));

        // The price comes from the provider, so it's looked up while the
        // question is already on screen.
        let alert = alert.downgrade();
        glib::spawn_future_local(async move {
            let found = gio::spawn_blocking(move || ai::price(&provider)).await.ok().flatten();
            let line = match found {
                Some(p) => tf!(
                    "Estimated cost: {low} to {high}, depending on how much the model thinks. Prices: ${input:.3} per million tokens in, ${output:.3} out.",
                    low = dollars(p.cost(total.input, total.output_low)),
                    high = dollars(p.cost(total.input, total.output_high)),
                    input = p.input,
                    output = p.output
                ),
                None => t!("The provider doesn't publish this model's price, so the cost can't be estimated."),
            };
            if let Some(alert) = alert.upgrade() {
                alert.set_body(&body(&line));
            }
        });
    }
}

/// One Update All: the requests, then the review of their results.
struct Run {
    batch: Rc<Batch>,
    items: Vec<Queued>,
    /// Objects that couldn't be sent, by index into `behind`, with why.
    unreadable: Vec<(usize, String)>,
    /// The pages of the run: progress, one result at a time, the summary.
    stack: gtk::Stack,
    title: adw::WindowTitle,
    page: adw::NavigationPage,
    /// Results still to review, as positions in `items`.
    ready: RefCell<VecDeque<(usize, Done)>>,
    /// Objects that failed, by index into `behind`, with why.
    failed: RefCell<Vec<(usize, String)>>,
    reviewed: Cell<usize>,
    to_review: Cell<usize>,
    saved: Cell<usize>,
    skipped: Cell<usize>,
    not_started: Cell<usize>,
    /// Whether the merged patch has been backed up for this run.
    backed_up: Cell<bool>,
}

impl Run {
    fn new(batch: Rc<Batch>, items: Vec<Queued>, unreadable: Vec<(usize, String)>) -> Run {
        let stack = gtk::Stack::new();
        let title = adw::WindowTitle::new(&t!("Update All with AI"), "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder().title(t!("Update All with AI")).child(&toolbar).build();
        Run {
            batch,
            items,
            failed: RefCell::new(unreadable.clone()),
            unreadable,
            stack,
            title,
            page,
            ready: RefCell::default(),
            reviewed: Cell::new(0),
            to_review: Cell::new(0),
            saved: Cell::new(0),
            skipped: Cell::new(0),
            not_started: Cell::new(0),
            backed_up: Cell::new(false),
        }
    }

    /// Sends every request, several at once, and shows the progress. Nothing
    /// is saved until the review.
    fn start(self: Rc<Self>, provider: Provider) {
        let total = self.items.len();
        let progress = gtk::ProgressBar::builder().show_text(true).text(tf!("{done} of {total} done", done = 0, total = total)).build();
        let stop = gtk::Button::builder().label(t!("Stop")).halign(gtk::Align::Center).build();
        let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Center).margin_start(24).margin_end(24).build();
        column.append(&progress);
        column.append(&stop);
        self.stack.add_named(&column, Some("progress"));
        self.page.set_can_pop(false);
        self.batch.navigation.push(&self.page);

        let cancelled = self.batch.cancelled.clone();
        stop.connect_clicked(move |stop| {
            cancelled.set(true);
            stop.set_sensitive(false);
            stop.set_label(&t!("Stopping…"));
        });

        let jobs: Rc<Vec<Job>> = Rc::new(self.items.iter().map(|q| q.job.clone()).collect());
        let queue: Rc<RefCell<VecDeque<usize>>> = Rc::new(RefCell::new((0..total).collect()));
        let results: Rc<RefCell<Finished>> = Rc::default();
        let finished = Rc::new(Cell::new(0));
        glib::spawn_future_local(async move {
            let workers: Vec<_> = (0..self.batch.hooks.merges_at_once.clamp(1, total))
                .map(|_| {
                    let (provider, jobs, queue, results, finished, cancelled, progress) = (provider.clone(), jobs.clone(), queue.clone(), results.clone(), finished.clone(), self.batch.cancelled.clone(), progress.clone());
                    glib::spawn_future_local(async move {
                        // Each worker takes the next object until none are
                        // left or Stop is pressed; requests under way finish.
                        loop {
                            if cancelled.get() {
                                break;
                            }
                            let Some(position) = queue.borrow_mut().pop_front() else { break };
                            let (job, provider) = (jobs[position].clone(), provider.clone());
                            let thinking = ThinkingView::new();
                            let result = thinking_view::run(&thinking, None, move |on| job.run(&provider, on)).await.unwrap_or_else(|| Err(t!("The merge failed unexpectedly")));
                            // A result that still fails the checks goes to the
                            // failures, to be opened on its own.
                            let result = result.and_then(|(checked, warnings)| match checked.script {
                                Ok(_) => Ok((checked, warnings)),
                                Err(e) => Err(tf!("The model's result was rejected, even after a correction: {error}", error = e)),
                            });
                            results.borrow_mut().push((position, result));
                            finished.set(finished.get() + 1);
                            progress.set_fraction(finished.get() as f64 / total as f64);
                            progress.set_text(Some(&tf!("{done} of {total} done", done = finished.get(), total = total)));
                        }
                    })
                })
                .collect();
            for worker in workers {
                let _ = worker.await;
            }
            let sorted = update_all::sort(total, results.take());
            self.failed.borrow_mut().extend(sorted.failed.into_iter().map(|(position, message)| (self.items[position].index, message)));
            self.ready.replace(sorted.ready.into_iter().collect());
            self.finish(sorted.not_started.len());
        });
    }

    /// Starts the review of the results, or the summary when there are none.
    fn finish(self: &Rc<Self>, not_started: usize) {
        if self.stack.child_by_name("progress").is_none() {
            // Nothing was sent: the summary alone.
            self.batch.navigation.push(&self.page);
        }
        self.page.set_can_pop(true);
        self.not_started.set(not_started);
        self.to_review.set(self.ready.borrow().len());
        self.next();
    }

    /// Shows the next result to review, or the summary after the last.
    fn next(self: &Rc<Self>) {
        let Some((position, (checked, warnings))) = self.ready.borrow_mut().pop_front() else {
            return self.summary();
        };
        self.reviewed.set(self.reviewed.get() + 1);
        let q = &self.items[position];
        let b = &self.batch.analysis.behind[q.index];
        self.title.set_title(&b.key);
        self.title.set_subtitle(&tf!("{n} of {total}", n = self.reviewed.get(), total = self.to_review.get()));
        let Review { body, pair, error, buttons } = review_parts(&self.batch.analysis, b, &q.mod_text, &self.batch.new, checked, &warnings);
        let skip = gtk::Button::builder().label(t!("Skip")).build();
        let save = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).build();
        buttons.append(&skip);
        buttons.append(&save);
        let run = self.clone();
        skip.connect_clicked(move |_| {
            run.skipped.set(run.skipped.get() + 1);
            run.next();
        });
        let (run, weak) = (self.clone(), Rc::downgrade(&pair));
        save.connect_clicked(move |_| {
            if let Some(pair) = weak.upgrade() {
                run.save(position, &pair.right.text(), &error);
            }
        });
        keep_alive(&body, pair);
        if let Some(old) = self.stack.child_by_name("review") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&body, Some("review"));
        self.stack.set_visible_child_name("review");
    }

    /// Saves the reviewed result, after backing the patch up once per run.
    fn save(self: &Rc<Self>, position: usize, text: &str, error: &gtk::Label) {
        let hooks = &self.batch.hooks;
        if !self.backed_up.get() {
            if let Err(message) = (hooks.backup)(snapshot::BEFORE_UPDATING_ALL) {
                return show_error(error, &message);
            }
            self.backed_up.set(true);
        }
        let q = &self.items[position];
        match save_update(&self.batch.analysis, hooks, &self.batch.analysis.behind[q.index], &q.now, &q.mod_text, text) {
            Ok(()) => {
                hooks.saved();
                (self.batch.refresh)();
                self.saved.set(self.saved.get() + 1);
                self.next();
            }
            Err(message) => show_error(error, &message),
        }
    }

    /// What was saved, skipped and failed, with the failures to open singly.
    fn summary(self: &Rc<Self>) {
        (self.batch.refresh)();
        self.title.set_title(&t!("Update All with AI"));
        self.title.set_subtitle("");
        let failed = self.failed.borrow().clone();
        let total = self.items.len() + self.unreadable.len();
        let mut line = tf!("Updated {saved} of {total}", saved = self.saved.get(), total = total);
        if self.skipped.get() > 0 {
            line.push_str(&tf!(" · {count} skipped", count = self.skipped.get()));
        }
        if !failed.is_empty() {
            line.push_str(&tf!(" · {count} failed", count = failed.len()));
        }
        if self.not_started.get() > 0 {
            line.push_str(&tf!(" · {count} not started", count = self.not_started.get()));
        }
        let page = adw::PreferencesPage::new();
        let overview = adw::PreferencesGroup::builder().description(line).build();
        let done = gtk::Button::builder().label(t!("Done")).css_classes(["suggested-action"]).halign(gtk::Align::End).build();
        overview.add(&done);
        page.add(&overview);
        if !failed.is_empty() {
            let group = adw::PreferencesGroup::builder().title(t!("Not Updated")).description(t!("Open one to try it on its own.")).build();
            for (index, message) in failed {
                let b = self.batch.analysis.behind[index].clone();
                let row = adw::ActionRow::builder().title(&b.key).subtitle(&message).subtitle_lines(3).use_markup(false).activatable(true).build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let batch = self.batch.clone();
                row.connect_activated(move |_| {
                    let refresh = batch.refresh.clone();
                    batch.navigation.push(&detail(&batch.analysis, &batch.hooks, &batch.navigation, &b, &batch.old, &batch.new, Rc::new(move || refresh())));
                });
                group.add(&row);
            }
            page.add(&group);
        }
        let navigation = self.batch.navigation.clone();
        done.connect_clicked(move |_| {
            navigation.pop();
        });
        if let Some(old) = self.stack.child_by_name("summary") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&page, Some("summary"));
        self.stack.set_visible_child_name("summary");
    }
}
