//! Merging every conflicting object in one folder with AI: shows how many
//! calls it takes and roughly what they cost, then merges the objects,
//! several at once if Preferences say so, saving each to the merged patch,
//! and reports the tokens and money actually used. A result the checks
//! reject goes back to the model once for a correction.
//!
//! The same works for a folder of overridden files: each file is planned
//! object by object as in the file merge dialog, its conflicting objects
//! are merged one AI call each, and the combined file is saved. Files are
//! merged several at once; the objects within one file, one after another.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::file_merge_dialog;
use crate::merge::ai::{self, Estimate, Price, Provider, Usage};
use crate::merge::file_merge::{self, Plan, Status};
use crate::merge::patch::{Print, SourceMod};
use crate::merge::source;
use crate::merge::trivial::{self, Resolved};
use crate::merge_dialog::{self, Loaded};
use crate::thinking_view::{self, ThinkingView};

/// One conflicting object.
pub struct Item {
    pub key: String,
    /// (mod id, name, folder, file) for each mod defining it, in load order.
    pub origins: Vec<(String, String, PathBuf, String)>,
    /// Already merged into the patch and still up to date.
    pub merged: bool,
    /// The merge saved in the patch, if any.
    pub saved: Option<String>,
}

/// One overridden file.
pub struct FileItem {
    pub path: String,
    /// (mod id, name, folder) for each mod shipping it, in load order.
    pub origins: Vec<(String, String, PathBuf)>,
    /// Already merged into the patch and still up to date.
    pub merged: bool,
}

pub enum Items {
    /// Object conflicts in `category`, e.g. `common/buildings`.
    Objects { category: String, items: Vec<Item> },
    /// Files several mods ship, all in one folder.
    Files { folder: String, files: Vec<FileItem> },
}

pub struct Request {
    pub items: Items,
    pub game_dir: Option<PathBuf>,
    pub provider: Option<Provider>,
    /// How many objects (or files) to merge at once.
    pub parallel: usize,
    /// Saves one merged object (key, script, fingerprints) or file (path,
    /// contents, fingerprints).
    pub save: Box<dyn Fn(&str, &str, Vec<Print>) -> Result<(), String>>,
    pub open_preferences: Box<dyn Fn()>,
    /// Called once at the end with how many objects were saved.
    pub on_done: Box<dyn Fn(usize)>,
}

impl Request {
    fn title(&self) -> &str {
        match &self.items {
            Items::Objects { category, .. } => category,
            Items::Files { folder, .. } => folder,
        }
    }

    /// The name row `i` is shown and saved under: the object's key, or the
    /// file's path.
    fn name(&self, i: usize) -> &str {
        match &self.items {
            Items::Objects { items, .. } => &items[i].key,
            Items::Files { files, .. } => &files[i].path,
        }
    }

    /// The objects' category; for files, as their objects are prompted.
    fn category(&self, i: usize) -> String {
        match &self.items {
            Items::Objects { category, .. } => category.clone(),
            Items::Files { files, .. } => file_merge::category(&files[i].path),
        }
    }
}

/// What was read for one row.
enum Unit {
    Object { loaded: Loaded, resolved: Option<Resolved> },
    File { plan: Plan, prints: Vec<Print> },
    /// Nothing that can be merged.
    None,
}

struct Row {
    check: gtk::CheckButton,
    row: adw::ExpanderRow,
    state: gtk::Stack,
    estimate: Estimate,
    /// The model's reasoning, streamed while this object is merged.
    thinking: Rc<ThinkingView>,
    /// Shows or hides the merged script.
    show: gtk::ToggleButton,
    result: gtk::Label,
    /// Whether it can be saved without the AI.
    free: bool,
    /// How many AI calls it takes.
    calls: usize,
}

impl Row {
    fn set_result(&self, script: &str) {
        self.result.set_label(script);
        self.show.set_sensitive(true);
    }
}

pub fn present(parent: &impl IsA<gtk::Widget>, request: Request) {
    let request = Rc::new(request);
    let model = request.provider.as_ref().map(|p| p.model_name().to_owned());

    let start = gtk::Button::builder().label(t!("Merge")).css_classes(["suggested-action"]).sensitive(false).build();
    let stop = gtk::Button::builder().label(t!("Stop")).visible(false).tooltip_text(t!("Stop after the object being merged")).build();
    let header = adw::HeaderBar::new();
    let subtitle = model.as_deref().map_or(t!("No AI provider set up"), |m| tf!("with {model}", model = m));
    header.set_title_widget(Some(&adw::WindowTitle::new(&tf!("Merge {title}", title = request.title()), &subtitle)));
    header.pack_end(&start);
    header.pack_end(&stop);

    let summary = gtk::Label::builder().xalign(0.0).wrap(true).label(t!("Reading every version…")).build();
    let detail = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build();
    let progress = gtk::ProgressBar::builder().show_text(true).visible(false).build();
    let top = gtk::Box::new(gtk::Orientation::Vertical, 6);
    top.append(&summary);
    top.append(&detail);
    top.append(&progress);
    let overview = adw::PreferencesGroup::new();
    overview.add(&top);
    let files = matches!(request.items, Items::Files { .. });
    let objects = adw::PreferencesGroup::builder()
        .title(if files { t!("Files") } else { t!("Objects") })
        .description(if files {
            t!("Each file is saved to the patch as soon as its conflicting objects are merged. Review any of them later from its conflict.")
        } else {
            t!("Each merge is saved to the patch as soon as it's done. Review any of them later with Edit Merge.")
        })
        .build();

    let page = adw::PreferencesPage::new();
    page.add(&overview);
    page.add(&objects);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().content_width(760).content_height(700).child(&toolbar).build();

    let rows: Rc<RefCell<Vec<Row>>> = Rc::default();
    let units: Rc<RefCell<Vec<Unit>>> = Rc::default();
    let price: Rc<Cell<Option<Price>>> = Rc::default();
    let price_known = Rc::new(Cell::new(false));
    let running = Rc::new(Cell::new(false));
    let cancelled = Rc::new(Cell::new(false));

    // Recomputes the call count and estimate from the ticked objects.
    let update: Rc<dyn Fn()> = {
        let (rows, price, price_known, running) = (rows.clone(), price.clone(), price_known.clone(), running.clone());
        let (summary, detail, start) = (summary.clone(), detail.clone(), start.clone());
        let has_provider = request.provider.is_some();
        Rc::new(move || {
            if running.get() {
                return;
            }
            let rows = rows.borrow();
            let chosen: Vec<&Row> = rows.iter().filter(|r| r.check.is_active()).collect();
            let mut total = Estimate::default();
            for r in &chosen {
                total += r.estimate;
            }
            let n = chosen.len();
            let free = chosen.iter().filter(|r| r.free).count();
            let calls = chosen.iter().map(|r| r.calls).sum::<usize>();
            let merge_label = if n == 0 {
                t!("Merge")
            } else if files {
                tnf!("Merge {count} File", "Merge {count} Files", count = n)
            } else {
                tnf!("Merge {count} Object", "Merge {count} Objects", count = n)
            };
            start.set_label(&merge_label);
            start.set_sensitive(n > 0);
            if calls > 0 && !has_provider {
                start.set_label(&t!("Set Up AI…"));
            }
            if n == 0 {
                summary.set_label(&t!("Nothing selected to merge"));
                detail.set_label("");
                return;
            }
            let without_ai = if free > 0 { tf!(" · {count} without AI", count = free) } else { String::new() };
            if calls == 0 {
                if files {
                    summary.set_label(&tnf!("No AI calls needed · {count} file can be combined as they are", "No AI calls needed · {count} files can be combined as they are", count = free));
                    detail.set_label(&t!("In each of these, every object is the same in every copy, only in some of them, or changed by only one mod."));
                } else {
                    summary.set_label(&tnf!("No AI calls needed · {count} version can be used as it is", "No AI calls needed · {count} versions can be used as they are", count = free));
                    detail.set_label(&t!("Each of these was changed by only one mod, or is the same in every mod."));
                }
                return;
            }
            let calls = tnf!("{count} AI call", "{count} AI calls", count = calls);
            summary.set_label(&tf!(
                "{calls}{without_ai} · about {input} tokens in, {low}–{high} out",
                calls = &calls, without_ai = &without_ai,
                input = tokens(total.input), low = tokens(total.output_low), high = tokens(total.output_high)
            ));
            detail.set_label(&match (price.get(), price_known.get()) {
                (Some(p), _) => tf!(
                    "Estimated cost: {low} to {high}, depending on how much the model thinks. Prices: ${input:.3} per million tokens in, ${output:.3} out.",
                    low = dollars(p.cost(total.input, total.output_low)),
                    high = dollars(p.cost(total.input, total.output_high)),
                    input = p.input,
                    output = p.output
                ),
                (None, true) => t!("The provider doesn't publish this model's price, so the cost can't be estimated."),
                (None, false) => t!("Looking up the model's price…"),
            });
        })
    };

    // Read every version, and the price, off the main thread.
    {
        let request = request.clone();
        let (rows, units, objects, update) = (rows.clone(), units.clone(), objects.clone(), update.clone());
        enum Job {
            Objects(String, Vec<String>, Vec<Vec<(String, String, PathBuf, String)>>),
            Files(Vec<(String, Vec<SourceMod>)>),
        }
        let job = match &request.items {
            Items::Objects { category, items } => Job::Objects(category.clone(), items.iter().map(|i| i.key.clone()).collect(), items.iter().map(|i| i.origins.clone()).collect()),
            Items::Files { files, .. } => Job::Files(files.iter().map(|f| (f.path.clone(), f.origins.clone())).collect()),
        };
        let game_dir = request.game_dir.clone();
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking(move || match job {
                Job::Objects(category, keys, origins) => {
                    let vanilla = game_dir.as_deref().map(|d| source::vanilla_many(d, &category, &keys)).unwrap_or_default();
                    keys.iter()
                        .zip(origins)
                        .map(|(key, origins)| {
                            let loaded = merge_dialog::load_with(&category, key, vanilla.get(key).cloned(), origins);
                            let resolved = loaded.mergeable().then(|| trivial::resolve(loaded.vanilla.as_ref(), &loaded.sources)).flatten();
                            Ok(Unit::Object { loaded, resolved })
                        })
                        .collect::<Vec<Result<Unit, String>>>()
                }
                Job::Files(files) => files
                    .iter()
                    .map(|(path, origins)| {
                        if !file_merge::mergeable(path) {
                            return Err(t!("This kind of file can't be merged object by object"));
                        }
                        file_merge_dialog::load(path, origins, game_dir.as_deref()).map(|(plan, prints)| Unit::File { plan, prints })
                    })
                    .collect(),
            })
            .await
            .unwrap_or_default();
            let mut built = Vec::new();
            let mut kept = Vec::new();
            for (i, unit) in read.into_iter().enumerate() {
                let (merged, saved) = match &request.items {
                    Items::Objects { items, .. } => (items[i].merged, items[i].saved.clone()),
                    Items::Files { files, .. } => (files[i].merged, None),
                };
                let name = request.name(i);
                let title = match &request.items {
                    Items::Objects { .. } => name,
                    Items::Files { .. } => name.rsplit_once('/').map_or(name, |(_, f)| f),
                };
                let row = adw::ExpanderRow::builder().title(title).use_markup(false).subtitle_lines(3).build();
                let check = gtk::CheckButton::builder().valign(gtk::Align::Center).tooltip_text(if files { t!("Merge this file") } else { t!("Merge this object") }).build();
                row.add_prefix(&check);
                let state = gtk::Stack::builder().valign(gtk::Align::Center).build();
                row.add_suffix(&state);
                let show = gtk::ToggleButton::builder()
                    .icon_name("view-reveal-symbolic")
                    .tooltip_text(t!("Show the merged result"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .sensitive(false)
                    .build();
                row.add_suffix(&show);

                let thinking = ThinkingView::new();
                let result = gtk::Label::builder()
                    .xalign(0.0)
                    .yalign(0.0)
                    .selectable(true)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .css_classes(["monospace"])
                    .margin_top(8)
                    .margin_bottom(8)
                    .margin_start(10)
                    .margin_end(10)
                    .build();
                let scroll = gtk::ScrolledWindow::builder().child(&result).hscrollbar_policy(gtk::PolicyType::Never).max_content_height(360).propagate_natural_height(true).css_classes(["card"]).build();
                let revealer = gtk::Revealer::builder().child(&crate::resize_grip::resizable(&scroll)).transition_type(gtk::RevealerTransitionType::SlideDown).build();
                let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(8).margin_bottom(8).margin_start(12).margin_end(12).build();
                content.append(&thinking.widget);
                content.append(&revealer);
                row.add_row(&content);
                row.set_enable_expansion(false);
                show.connect_toggled(glib::clone!(
                    #[weak]
                    row,
                    #[weak]
                    revealer,
                    move |show| {
                        revealer.set_reveal_child(show.is_active());
                        show.set_tooltip_text(Some(&if show.is_active() { t!("Hide the merged result") } else { t!("Show the merged result") }));
                        if show.is_active() {
                            row.set_enable_expansion(true);
                            row.set_expanded(true);
                        }
                    }
                ));
                let already = |subtitle: String| if merged { tf!("Already merged · {subtitle}", subtitle = &subtitle) } else { subtitle };
                let mut estimate = Estimate::default();
                let (unit, free, calls) = match unit {
                    Ok(Unit::Object { loaded, resolved }) if loaded.mergeable() => {
                        let names: Vec<&str> = loaded.sources.iter().map(|s| s.label.as_str()).collect();
                        let mut subtitle = names.join(" · ");
                        if let Some(r) = &resolved {
                            subtitle = tf!("No AI needed: {reason} · {subtitle}", reason = &r.reason, subtitle = &subtitle);
                        } else {
                            estimate = ai::estimate(&request.category(i), name, loaded.vanilla.as_ref(), &loaded.sources);
                        }
                        row.set_subtitle(&already(subtitle));
                        let free = resolved.is_some();
                        (Unit::Object { loaded, resolved }, free, usize::from(!free))
                    }
                    Ok(Unit::Object { loaded, .. }) => {
                        row.set_subtitle(&if loaded.missing.is_empty() { t!("Nothing to merge") } else { tf!("Could not find it in: {mods}", mods = loaded.missing.join(", ")) });
                        (Unit::None, false, 0)
                    }
                    Ok(Unit::File { plan, prints }) => {
                        let category = request.category(i);
                        for (_, item) in plan.conflicts() {
                            if let Status::Conflict { sources, vanilla } = &item.status {
                                estimate += ai::estimate(&category, &item.key, vanilla.as_ref(), sources);
                            }
                        }
                        let calls = plan.conflicts().count();
                        row.set_subtitle(&already(file_merge_dialog::describe(&plan)));
                        (Unit::File { plan, prints }, calls == 0, calls)
                    }
                    Ok(Unit::None) => (Unit::None, false, 0),
                    Err(e) => {
                        row.set_subtitle(&e);
                        (Unit::None, false, 0)
                    }
                };
                if matches!(unit, Unit::None) {
                    check.set_sensitive(false);
                } else {
                    check.set_active(!merged);
                }
                let update = update.clone();
                check.connect_active_notify(move |_| update());
                objects.add(&row);
                let r = Row { check, row, state, estimate, thinking, show, result, free, calls };
                if let Some(saved) = &saved {
                    r.set_result(saved);
                }
                built.push(r);
                kept.push(unit);
            }
            rows.replace(built);
            units.replace(kept);
            update();
        });
    }
    if let Some(provider) = request.provider.clone() {
        let (price, price_known, update) = (price.clone(), price_known.clone(), update.clone());
        glib::spawn_future_local(async move {
            let found = gio::spawn_blocking(move || ai::price(&provider)).await.ok().flatten();
            price.set(found);
            price_known.set(true);
            update();
        });
    }

    stop.connect_clicked(glib::clone!(
        #[strong]
        cancelled,
        move |stop| {
            cancelled.set(true);
            stop.set_sensitive(false);
            stop.set_label(&t!("Stopping…"));
        }
    ));
    // Closing stops the run once the merges in progress finish; what's done
    // is kept.
    dialog.connect_closed(glib::clone!(
        #[strong]
        cancelled,
        move |_| cancelled.set(true)
    ));

    start.connect_clicked(glib::clone!(
        #[strong]
        request,
        #[strong]
        rows,
        #[strong]
        units,
        #[strong]
        running,
        #[strong]
        cancelled,
        #[weak]
        dialog,
        #[weak]
        summary,
        #[weak]
        detail,
        #[weak]
        progress,
        #[weak]
        stop,
        move |start| {
            let chosen: Vec<usize> = rows.borrow().iter().enumerate().filter(|(_, r)| r.check.is_active()).map(|(i, _)| i).collect();
            if chosen.is_empty() {
                return;
            }
            let needs_ai = chosen.iter().any(|&i| rows.borrow()[i].calls > 0);
            let provider = request.provider.clone();
            if needs_ai && provider.is_none() {
                dialog.close();
                (request.open_preferences)();
                return;
            }
            running.set(true);
            start.set_visible(false);
            stop.set_visible(true);
            progress.set_visible(true);
            for r in rows.borrow().iter() {
                r.check.set_sensitive(false);
            }
            let (request, rows, units, running, cancelled) = (request.clone(), rows.clone(), units.clone(), running.clone(), cancelled.clone());
            glib::spawn_future_local(async move {
                let total = chosen.len();
                let totals: Rc<RefCell<Totals>> = Rc::default();
                let queue: Rc<RefCell<VecDeque<usize>>> = Rc::new(RefCell::new(chosen.into_iter().collect()));
                // Each worker takes the next object until none are left or
                // Stop is pressed; merges already running finish.
                let workers: Vec<_> = (0..request.parallel.clamp(1, total))
                    .map(|_| {
                        let run = Run {
                            request: request.clone(),
                            rows: rows.clone(),
                            units: units.clone(),
                            provider: provider.clone(),
                            totals: totals.clone(),
                            queue: queue.clone(),
                            cancelled: cancelled.clone(),
                            progress: progress.clone(),
                            total,
                        };
                        glib::spawn_future_local(run.work())
                    })
                    .collect();
                for worker in workers {
                    let _ = worker.await;
                }
                let Totals { saved, failed, partial, used, counted, without_ai, ai_calls, .. } = totals.take();
                running.set(false);
                progress.set_fraction(1.0);
                let skipped = total - saved - failed;
                progress.set_text(Some(&if skipped > 0 { tf!("Stopped · {count} not merged", count = skipped) } else { t!("Done") }));
                stop.set_visible(false);
                let mut line = tf!("Merged {saved} of {total}", saved = saved, total = total);
                if without_ai > 0 {
                    line.push_str(&tf!(" · {count} without AI", count = without_ai));
                }
                if partial > 0 {
                    line.push_str(&tf!(" · {count} with objects left unmerged", count = partial));
                }
                if failed > 0 {
                    line.push_str(&tf!(" · {count} failed", count = failed));
                }
                summary.set_label(&line);
                detail.set_label(&if ai_calls == 0 {
                    t!("No AI calls were needed")
                } else if counted {
                    let cost = used.cost.map(|c| tf!(" · cost {cost}", cost = dollars(c))).unwrap_or_default();
                    tf!("Used {input} tokens in and {output} out{cost}", input = tokens(used.input), output = tokens(used.output), cost = &cost)
                } else {
                    t!("The provider didn't report token usage")
                });
                if saved > 0 {
                    (request.on_done)(saved);
                }
            });
        }
    ));

    dialog.present(Some(parent));
}

/// What one Merge All run shares between its workers.
struct Run {
    request: Rc<Request>,
    rows: Rc<RefCell<Vec<Row>>>,
    units: Rc<RefCell<Vec<Unit>>>,
    provider: Option<Provider>,
    totals: Rc<RefCell<Totals>>,
    /// Objects not started yet, by index.
    queue: Rc<RefCell<VecDeque<usize>>>,
    cancelled: Rc<Cell<bool>>,
    progress: gtk::ProgressBar,
    total: usize,
}

#[derive(Default)]
struct Totals {
    saved: usize,
    failed: usize,
    /// Files saved with some conflicting objects left unmerged.
    partial: usize,
    without_ai: usize,
    ai_calls: usize,
    used: Usage,
    /// Whether the provider reported any usage.
    counted: bool,
    finished: usize,
    /// The keys being merged right now.
    active: Vec<String>,
}

impl Run {
    /// Merges objects from the queue, one at a time, until it's empty or
    /// the run is stopped.
    async fn work(self) {
        loop {
            if self.cancelled.get() {
                break;
            }
            let Some(i) = self.queue.borrow_mut().pop_front() else { break };
            let key = self.request.name(i).to_owned();
            self.totals.borrow_mut().active.push(key.clone());
            self.show_progress();
            self.merge(i, key.clone()).await;
            {
                let mut totals = self.totals.borrow_mut();
                totals.finished += 1;
                totals.active.retain(|k| *k != key);
            }
            self.show_progress();
        }
    }

    fn show_progress(&self) {
        let totals = self.totals.borrow();
        self.progress.set_fraction(totals.finished as f64 / self.total as f64);
        let text = match totals.active.as_slice() {
            [] => tf!("{done} of {total} done", done = totals.finished, total = self.total),
            [key] => tf!("Merging {done} of {total}: {key}", done = totals.finished + 1, total = self.total, key = key.rsplit_once('/').map_or(key.as_str(), |(_, f)| f)),
            keys => tf!("{done} of {total} done · merging {count} at once", done = totals.finished, total = self.total, count = keys.len()),
        };
        self.progress.set_text(Some(&text));
    }

    /// Merges row `i` and saves it, showing the outcome in it.
    async fn merge(&self, i: usize, key: String) {
        let file = matches!(self.units.borrow()[i], Unit::File { .. });
        if file {
            self.merge_file(i, key).await;
        } else {
            self.merge_object(i, key).await;
        }
    }

    /// Shows how row `i` ended: saved (with a note for its subtitle),
    /// saved with a warning, or failed.
    fn finish(&self, i: usize, outcome: Outcome) {
        let rows = self.rows.borrow();
        let r = &rows[i];
        r.row.set_expanded(false);
        let mut totals = self.totals.borrow_mut();
        let (icon, class, tooltip, subtitle) = match outcome {
            Outcome::Saved { note, tooltip } => {
                totals.saved += 1;
                ("emblem-ok-symbolic", "success", tooltip, note)
            }
            Outcome::Partial(note) => {
                totals.saved += 1;
                totals.partial += 1;
                ("dialog-warning-symbolic", "warning", note.clone(), note)
            }
            Outcome::Failed(e) => {
                totals.failed += 1;
                ("dialog-warning-symbolic", "warning", e.clone(), e)
            }
        };
        if !subtitle.is_empty() {
            r.row.set_subtitle(&subtitle);
        }
        let icon = gtk::Image::builder().icon_name(icon).css_classes([class]).tooltip_text(tooltip).build();
        r.state.add_child(&icon);
        r.state.set_visible_child(&icon);
    }

    /// Opens row `i` to show the model thinking, returning its view.
    fn start_thinking(&self, i: usize) -> Rc<ThinkingView> {
        let rows = self.rows.borrow();
        let r = &rows[i];
        let spinner = adw::Spinner::new();
        r.state.add_child(&spinner);
        r.state.set_visible_child(&spinner);
        r.row.set_enable_expansion(true);
        r.row.set_expanded(true);
        r.thinking.widget.set_expanded(true);
        r.show.set_active(false);
        r.thinking.clone()
    }

    /// Counts what one AI merge used.
    fn count(&self, checked: &ai::Checked) {
        let mut totals = self.totals.borrow_mut();
        totals.ai_calls += 1 + usize::from(checked.retried);
        if let Some(u) = checked.merged.usage {
            totals.used += u;
            totals.counted = true;
        }
    }

    async fn merge_object(&self, i: usize, key: String) {
        let category = self.request.category(i);
        let (vanilla, sources, prints, resolved) = match &self.units.borrow()[i] {
            Unit::Object { loaded, resolved } => (loaded.vanilla.clone(), loaded.sources.clone(), loaded.prints.clone(), resolved.clone()),
            _ => return,
        };
        // One version keeps every change: save it without asking.
        if let Some(resolved) = resolved {
            let text = sources[resolved.index].text.clone();
            let outcome = merge_dialog::validate(&category, &key, &text).and_then(|script| (self.request.save)(&key, &script, prints));
            self.rows.borrow()[i].set_result(&text);
            if outcome.is_ok() {
                self.totals.borrow_mut().without_ai += 1;
            }
            return self.finish(i, match outcome {
                Ok(()) => Outcome::Saved { note: tf!("Used as it is: {reason}", reason = &resolved.reason), tooltip: t!("Saved to the patch without AI") },
                Err(e) => Outcome::Failed(e),
            });
        }
        let Some(provider) = self.provider.clone() else { return };
        let thinking = self.start_thinking(i);
        let (c, k) = (category.clone(), key.clone());
        let result = thinking_view::run(&thinking, None, move |on| {
            let check = |text: &str| merge_dialog::validate(&c, &k, text);
            ai::merge_checked(&provider, &c, &k, vanilla.as_ref(), &sources, &check, on)
        })
        .await
        .unwrap_or_else(|| Err(t!("The merge failed unexpectedly")));
        // Keep the reasoning, but folded, once the object is done.
        thinking.widget.set_expanded(false);
        let outcome = result.and_then(|checked| {
            self.count(&checked);
            // Shown even when rejected, to see what went wrong.
            self.rows.borrow()[i].set_result(&checked.merged.script);
            let script = checked.script.map_err(|e| tf!("The model's result was rejected, even after a correction: {error}", error = e))?;
            (self.request.save)(&key, &script, prints)?;
            let notes = checked.merged.notes;
            Ok(if checked.retried { tf!("Corrected after a rejected first try. {notes}", notes = &notes).trim_end().to_owned() } else { notes })
        });
        self.finish(i, match outcome {
            Ok(note) => Outcome::Saved { note, tooltip: t!("Merged into the patch") },
            Err(e) => Outcome::Failed(e),
        });
    }

    /// Merges a file's conflicting objects one after another, then saves the
    /// file. Objects that fail, or aren't reached because the run was
    /// stopped, keep the last mod's version, as they do in the game today.
    async fn merge_file(&self, i: usize, path: String) {
        let (mut plan, prints) = match &self.units.borrow()[i] {
            Unit::File { plan, prints } => (plan.clone(), prints.clone()),
            _ => return,
        };
        let conflicts: Vec<usize> = plan.conflicts().filter(|(_, item)| !item.resolved).map(|(c, _)| c).collect();
        let thinking = (!conflicts.is_empty()).then(|| self.start_thinking(i));
        let mut problems: Vec<String> = Vec::new();
        let mut merged = 0;
        for &c in &conflicts {
            let (Some(thinking), Some(provider)) = (&thinking, self.provider.clone()) else { break };
            if self.cancelled.get() {
                problems.push(tf!("{key}: not reached before the run was stopped", key = &plan.items[c].key));
                continue;
            }
            let Status::Conflict { sources, vanilla } = plan.items[c].status.clone() else { continue };
            let (category, key) = (plan.category.clone(), plan.items[c].key.clone());
            let (cat, k) = (category.clone(), key.clone());
            let result = thinking_view::run(thinking, Some(&key), move |on| {
                let check = |text: &str| merge_dialog::validate(&cat, &k, text);
                ai::merge_checked(&provider, &cat, &k, vanilla.as_ref(), &sources, &check, on)
            })
            .await
            .unwrap_or_else(|| Err(t!("failed unexpectedly")));
            let text = result.and_then(|checked| {
                self.count(&checked);
                checked.script.map_err(|e| tf!("rejected, even after a correction: {error}", error = e))?;
                plan.validate_item(c, &checked.merged.script)
            });
            match text {
                Ok(text) => {
                    plan.items[c].text = text;
                    plan.items[c].resolved = true;
                    merged += 1;
                }
                Err(e) => problems.push(tf!("{key}: {error}", key = &key, error = e)),
            }
        }
        if let Some(thinking) = &thinking {
            thinking.widget.set_expanded(false);
        }
        let contents = plan.render();
        self.rows.borrow()[i].set_result(&contents);
        let saved = file_merge::validate(&plan.category, &contents).and_then(|()| (self.request.save)(&path, &contents, prints));
        if conflicts.is_empty() && saved.is_ok() {
            self.totals.borrow_mut().without_ai += 1;
        }
        self.finish(i, match saved {
            Err(e) => Outcome::Failed(e),
            Ok(()) if problems.is_empty() => Outcome::Saved {
                note: if conflicts.is_empty() { t!("Combined without AI") } else { tnf!("Merged {count} conflicting object", "Merged {count} conflicting objects", count = merged) },
                tooltip: t!("Merged into the patch"),
            },
            Ok(()) => Outcome::Partial(tf!(
                "Saved, but {objects} the last mod's version: {problems}",
                objects = tnf!("{count} object keeps", "{count} objects keep", count = problems.len()),
                problems = problems.join("; ")
            )),
        });
    }
}

/// How merging one row ended.
enum Outcome {
    Saved { note: String, tooltip: String },
    /// A file saved with some objects left unmerged, and why.
    Partial(String),
    Failed(String),
}

fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.2}M", n as f64 / 1_000_000.0),
    }
}

fn dollars(d: f64) -> String {
    if d < 0.01 { t!("under $0.01") } else { format!("${d:.2}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_tokens_and_dollars() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_345), "12.3k");
        assert_eq!(tokens(2_500_000), "2.50M");
        assert_eq!(dollars(0.004), "under $0.01");
        assert_eq!(dollars(1.234), "$1.23");
    }
}
