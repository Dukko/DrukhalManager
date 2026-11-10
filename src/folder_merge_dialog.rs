//! Merging every conflicting object in one folder with AI: shows how many
//! calls it takes and roughly what they cost, then merges the objects,
//! several at once if Preferences say so, saving each to the merged patch,
//! and reports the tokens and money actually used. A result the checks
//! reject goes back to the model once for a correction.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::merge::ai::{self, Estimate, Price, Provider, Usage};
use crate::merge::patch::Print;
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

pub struct Request {
    /// The objects' category, e.g. `common/buildings`.
    pub category: String,
    pub items: Vec<Item>,
    pub game_dir: Option<PathBuf>,
    pub provider: Option<Provider>,
    /// How many objects to merge at once.
    pub parallel: usize,
    /// Saves one merged object: key, script, fingerprints.
    pub save: Box<dyn Fn(&str, &str, Vec<Print>) -> Result<(), String>>,
    pub open_preferences: Box<dyn Fn()>,
    /// Called once at the end with how many objects were saved.
    pub on_done: Box<dyn Fn(usize)>,
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
    /// Set when one version can be used as it is, without the AI.
    resolved: Option<Resolved>,
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

    let start = gtk::Button::builder().label("Merge").css_classes(["suggested-action"]).sensitive(false).build();
    let stop = gtk::Button::builder().label("Stop").visible(false).tooltip_text("Stop after the object being merged").build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&format!("Merge {}", request.category), model.as_deref().map_or("No AI provider set up".to_owned(), |m| format!("with {m}")).as_str())));
    header.pack_end(&start);
    header.pack_end(&stop);

    let summary = gtk::Label::builder().xalign(0.0).wrap(true).label("Reading every version…").build();
    let detail = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build();
    let progress = gtk::ProgressBar::builder().show_text(true).visible(false).build();
    let top = gtk::Box::new(gtk::Orientation::Vertical, 6);
    top.append(&summary);
    top.append(&detail);
    top.append(&progress);
    let overview = adw::PreferencesGroup::new();
    overview.add(&top);
    let objects = adw::PreferencesGroup::builder()
        .title("Objects")
        .description("Each merge is saved to the patch as soon as it's done. Review any of them later with Edit Merge.")
        .build();

    let page = adw::PreferencesPage::new();
    page.add(&overview);
    page.add(&objects);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().content_width(760).content_height(700).child(&toolbar).build();

    let rows: Rc<RefCell<Vec<Row>>> = Rc::default();
    let loaded: Rc<RefCell<Vec<Loaded>>> = Rc::default();
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
            let free = chosen.iter().filter(|r| r.resolved.is_some()).count();
            let calls = n - free;
            start.set_label(&if n == 0 { "Merge".to_owned() } else { format!("Merge {}", crate::conflicts_view::plural(n, "Object", "Objects")) });
            start.set_sensitive(n > 0);
            if calls > 0 && !has_provider {
                start.set_label("Set Up AI…");
            }
            if n == 0 {
                summary.set_label("Nothing selected to merge");
                detail.set_label("");
                return;
            }
            let without_ai = if free > 0 { format!(" · {free} without AI") } else { String::new() };
            if calls == 0 {
                summary.set_label(&format!("No AI calls needed · {} can be used as they are", crate::conflicts_view::plural(free, "version", "versions")));
                detail.set_label("Each of these was changed by only one mod, or is the same in every mod.");
                return;
            }
            summary.set_label(&format!(
                "{}{without_ai} · about {} tokens in, {}–{} out",
                crate::conflicts_view::plural(calls, "AI call", "AI calls"),
                tokens(total.input),
                tokens(total.output_low),
                tokens(total.output_high)
            ));
            detail.set_label(&match (price.get(), price_known.get()) {
                (Some(p), _) => format!(
                    "Estimated cost: {} to {}, depending on how much the model thinks. Prices: ${:.3} per million tokens in, ${:.3} out.",
                    dollars(p.cost(total.input, total.output_low)),
                    dollars(p.cost(total.input, total.output_high)),
                    p.input,
                    p.output
                ),
                (None, true) => "The provider doesn't publish this model's price, so the cost can't be estimated.".to_owned(),
                (None, false) => "Looking up the model's price…".to_owned(),
            });
        })
    };

    // Read every version, and the price, off the main thread.
    {
        let request = request.clone();
        let (rows, loaded, objects, update) = (rows.clone(), loaded.clone(), objects.clone(), update.clone());
        let keys: Vec<String> = request.items.iter().map(|i| i.key.clone()).collect();
        let origins: Vec<_> = request.items.iter().map(|i| i.origins.clone()).collect();
        let (category, game_dir) = (request.category.clone(), request.game_dir.clone());
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let vanilla = game_dir.as_deref().map(|d| source::vanilla_many(d, &category, &keys)).unwrap_or_default();
                keys.iter()
                    .zip(origins)
                    .map(|(key, origins)| merge_dialog::load_with(&category, key, vanilla.get(key).cloned(), origins))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            let mut built = Vec::new();
            for (item, l) in request.items.iter().zip(&result) {
                let names: Vec<&str> = l.sources.iter().map(|s| s.label.as_str()).collect();
                let row = adw::ExpanderRow::builder().title(&item.key).use_markup(false).subtitle_lines(3).build();
                let check = gtk::CheckButton::builder().valign(gtk::Align::Center).tooltip_text("Merge this object").build();
                row.add_prefix(&check);
                let state = gtk::Stack::builder().valign(gtk::Align::Center).build();
                row.add_suffix(&state);
                let show = gtk::ToggleButton::builder()
                    .icon_name("view-reveal-symbolic")
                    .tooltip_text("Show the merged result")
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
                let revealer = gtk::Revealer::builder().child(&scroll).transition_type(gtk::RevealerTransitionType::SlideDown).build();
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
                        show.set_tooltip_text(Some(if show.is_active() { "Hide the merged result" } else { "Show the merged result" }));
                        if show.is_active() {
                            row.set_enable_expansion(true);
                            row.set_expanded(true);
                        }
                    }
                ));
                let resolved = l.mergeable().then(|| trivial::resolve(l.vanilla.as_ref(), &l.sources)).flatten();
                let estimate = if l.mergeable() {
                    check.set_active(!item.merged);
                    let mut subtitle = names.join(" · ");
                    if let Some(r) = &resolved {
                        subtitle = format!("No AI needed: {} · {subtitle}", r.reason);
                    }
                    row.set_subtitle(&if item.merged { format!("Already merged · {subtitle}") } else { subtitle });
                    if resolved.is_some() { Estimate::default() } else { ai::estimate(&request.category, &item.key, l.vanilla.as_ref(), &l.sources) }
                } else {
                    check.set_sensitive(false);
                    row.set_subtitle(&if l.missing.is_empty() { "Nothing to merge".to_owned() } else { format!("Could not find it in: {}", l.missing.join(", ")) });
                    Estimate::default()
                };
                let update = update.clone();
                check.connect_active_notify(move |_| update());
                objects.add(&row);
                let r = Row { check, row, state, estimate, thinking, show, result, resolved };
                if let Some(saved) = &item.saved {
                    r.set_result(saved);
                }
                built.push(r);
            }
            rows.replace(built);
            loaded.replace(result);
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
            stop.set_label("Stopping…");
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
        loaded,
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
            let needs_ai = chosen.iter().any(|&i| rows.borrow()[i].resolved.is_none());
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
            let (request, rows, loaded, running, cancelled) = (request.clone(), rows.clone(), loaded.clone(), running.clone(), cancelled.clone());
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
                            loaded: loaded.clone(),
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
                let Totals { saved, failed, used, counted, without_ai, ai_calls, .. } = totals.take();
                running.set(false);
                progress.set_fraction(1.0);
                let skipped = total - saved - failed;
                progress.set_text(Some(&if skipped > 0 { format!("Stopped · {skipped} not merged") } else { "Done".to_owned() }));
                stop.set_visible(false);
                let mut line = format!("Merged {saved} of {total}");
                if without_ai > 0 {
                    line.push_str(&format!(" · {without_ai} without AI"));
                }
                if failed > 0 {
                    line.push_str(&format!(" · {failed} failed"));
                }
                summary.set_label(&line);
                detail.set_label(&if ai_calls == 0 {
                    "No AI calls were needed".to_owned()
                } else if counted {
                    let cost = used.cost.map(|c| format!(" · cost {}", dollars(c))).unwrap_or_default();
                    format!("Used {} tokens in and {} out{cost}", tokens(used.input), tokens(used.output))
                } else {
                    "The provider didn't report token usage".to_owned()
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
    loaded: Rc<RefCell<Vec<Loaded>>>,
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
            let key = self.request.items[i].key.clone();
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
            [] => format!("{} of {} done", totals.finished, self.total),
            [key] => format!("Merging {} of {}: {key}", totals.finished + 1, self.total),
            keys => format!("{} of {} done · merging {} at once", totals.finished, self.total, keys.len()),
        };
        self.progress.set_text(Some(&text));
    }

    /// Merges object `i` and saves it, showing the outcome in its row.
    async fn merge(&self, i: usize, key: String) {
        // One version keeps every change: save it without asking.
        let resolved = self.rows.borrow()[i].resolved.clone();
        if let Some(resolved) = resolved {
            let (text, prints) = {
                let l = &self.loaded.borrow()[i];
                (l.sources[resolved.index].text.clone(), l.prints.clone())
            };
            let outcome = merge_dialog::validate(&self.request.category, &key, &text).and_then(|script| (self.request.save)(&key, &script, prints));
            let rows = self.rows.borrow();
            let r = &rows[i];
            r.set_result(&text);
            let icon = match outcome {
                Ok(()) => {
                    self.totals.borrow_mut().saved += 1;
                    self.totals.borrow_mut().without_ai += 1;
                    r.row.set_subtitle(&format!("Used as it is: {}", resolved.reason));
                    gtk::Image::builder().icon_name("emblem-ok-symbolic").css_classes(["success"]).tooltip_text("Saved to the patch without AI").build()
                }
                Err(e) => {
                    self.totals.borrow_mut().failed += 1;
                    r.row.set_subtitle(&e);
                    gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).tooltip_text(e.as_str()).build()
                }
            };
            r.state.add_child(&icon);
            r.state.set_visible_child(&icon);
            return;
        }
        let Some(provider) = self.provider.clone() else { return };
        self.totals.borrow_mut().ai_calls += 1;
        // The row being merged opens to show the model thinking.
        let thinking = {
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
        };
        let (vanilla, sources, prints) = {
            let l = &self.loaded.borrow()[i];
            (l.vanilla.clone(), l.sources.clone(), l.prints.clone())
        };
        let (category, key2, provider) = (self.request.category.clone(), key.clone(), provider.clone());
        let result = thinking_view::run(&thinking, None, move |on| {
            let check = |text: &str| merge_dialog::validate(&category, &key2, text);
            ai::merge_checked(&provider, &category, &key2, vanilla.as_ref(), &sources, &check, on)
        })
        .await
        .unwrap_or_else(|| Err("The merge failed unexpectedly".into()));
        // Keep the reasoning, but folded, once the object is done.
        thinking.widget.set_expanded(false);
        let outcome = result.and_then(|checked| {
            if let Some(u) = checked.merged.usage {
                let mut totals = self.totals.borrow_mut();
                totals.used += u;
                totals.counted = true;
            }
            if checked.retried {
                self.totals.borrow_mut().ai_calls += 1;
            }
            let rows = self.rows.borrow();
            // Shown even when rejected, to see what went wrong.
            rows[i].set_result(&checked.merged.script);
            let script = checked.script.map_err(|e| format!("The model's result was rejected, even after a correction: {e}"))?;
            (self.request.save)(&key, &script, prints)?;
            let notes = checked.merged.notes;
            Ok(if checked.retried { format!("Corrected after a rejected first try. {notes}").trim_end().to_owned() } else { notes })
        });
        let rows = self.rows.borrow();
        let r = &rows[i];
        r.row.set_expanded(false);
        let icon = match &outcome {
            Ok(notes) => {
                self.totals.borrow_mut().saved += 1;
                if !notes.is_empty() {
                    r.row.set_subtitle(notes);
                }
                gtk::Image::builder().icon_name("emblem-ok-symbolic").css_classes(["success"]).tooltip_text("Merged into the patch").build()
            }
            Err(e) => {
                self.totals.borrow_mut().failed += 1;
                r.row.set_subtitle(e);
                gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).tooltip_text(e.as_str()).build()
            }
        };
        r.state.add_child(&icon);
        r.state.set_visible_child(&icon);
    }
}

fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.2}M", n as f64 / 1_000_000.0),
    }
}

fn dollars(d: f64) -> String {
    if d < 0.01 { "under $0.01".to_owned() } else { format!("${d:.2}") }
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
