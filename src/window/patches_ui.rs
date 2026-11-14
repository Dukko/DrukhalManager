//! Find Compatibility Patches: asks Steam, on request, which Workshop items
//! list each enabled mod as required, and suggests those whose required
//! items are all enabled and that aren't installed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use adw::prelude::*;
use gtk::{gio, glib};

use super::overview::ago;
use super::requirements_ui::steam_key;
use super::{Window, display_name};
use crate::conflicts_view::plural;
use crate::mods::patches::{self, Cache, Suggestion};
use crate::paradox::workshop::{self, Dependent};

/// What the search thread reports.
enum Progress {
    Found(u64, Vec<Dependent>),
    NoKey,
    Failed(String),
}

/// The open dialog's parts, kept by the window while it's open.
pub(super) struct Dialog {
    dialog: adw::Dialog,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    progress: gtk::ProgressBar,
    searching: adw::StatusPage,
    results: adw::Bin,
    problem: adw::StatusPage,
    toasts: adw::ToastOverlay,
    refresh: gtk::Button,
    /// Shows items whose names don't say they're patches too.
    show_all: Cell<bool>,
    /// Set when the dialog closes, to stop a search.
    cancel: Arc<AtomicBool>,
    /// Shows the result rows that pass the switch and the slider.
    filter: RefCell<Option<Rc<dyn Fn()>>>,
}

/// The slider's range, in weeks: a month to five years.
const MIN_WEEKS: u32 = 4;
const MAX_WEEKS: u32 = 260;

impl Window {
    pub(super) fn find_patches(self: &Rc<Self>) {
        let title = adw::WindowTitle::new(&t!("Compatibility Patches"), "");
        let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text(t!("Search Again")).build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        header.pack_start(&refresh);

        let progress = gtk::ProgressBar::builder().show_text(false).width_request(240).halign(gtk::Align::Center).build();
        let searching = adw::StatusPage::builder().icon_name("system-search-symbolic").title(t!("Searching the Workshop")).child(&progress).build();
        let problem = adw::StatusPage::new();
        let results = adw::Bin::new();
        let stack = gtk::Stack::new();
        stack.add_named(&searching, Some("searching"));
        stack.add_named(&results, Some("results"));
        stack.add_named(&problem, Some("problem"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));
        let dialog = adw::Dialog::builder().content_width(640).content_height(680).child(&toolbar).build();

        let d = Rc::new(Dialog { dialog, title, stack, progress, searching, results, problem, toasts, refresh, show_all: Cell::new(false), cancel: Arc::default(), filter: RefCell::default() });
        // The window keeps it while it's open: the handlers below hold it
        // only weakly.
        *self.patches_dialog.borrow_mut() = Some(d.clone());
        let weak = Rc::downgrade(self);
        let cancel = d.cancel.clone();
        d.dialog.connect_closed(move |_| {
            cancel.store(true, Ordering::Relaxed);
            if let Some(ui) = weak.upgrade() {
                ui.patches_dialog.borrow_mut().take();
                ui.save_settings("how recent patches must be");
            }
        });
        let (weak, dw) = (Rc::downgrade(self), Rc::downgrade(&d));
        d.refresh.connect_clicked(move |_| {
            if let (Some(ui), Some(d)) = (weak.upgrade(), dw.upgrade()) {
                ui.search_patches(&d, true);
            }
        });
        d.dialog.present(Some(&self.window));
        self.search_patches(&d, false);
    }

    /// Asks Steam about the enabled mods it hasn't lately (all of them if
    /// `again`), one at a time with progress, then shows what was found.
    fn search_patches(self: &Rc<Self>, d: &Rc<Dialog>, again: bool) {
        let ids = {
            let state = self.state.borrow();
            patches::enabled_ids(&state.mods, &state.enabled)
        };
        d.title.set_subtitle(&tnf!("For {count} enabled mod from the Workshop", "For {count} enabled mods from the Workshop", count = ids.len()));
        if ids.is_empty() {
            return show_problem(d, &t!("No Workshop Mods Enabled"), &t!("Patches are found through the Workshop mods you run"), None);
        }
        let stale = self.dependents.borrow().stale(&ids, again);
        if stale.is_empty() {
            return self.show_patches(d);
        }
        d.refresh.set_sensitive(false);
        d.stack.set_visible_child_name("searching");
        let total = stale.len();
        d.progress.set_fraction(0.0);
        d.searching.set_description(Some(&tnf!("Checked 0 of {count} mod", "Checked 0 of {count} mods", count = total)));

        let (tx, rx) = async_channel::unbounded();
        let cancel = d.cancel.clone();
        let in_file = self.settings.borrow().steam_key.clone();
        std::thread::spawn(move || {
            let Some(key) = steam_key(in_file) else {
                let _ = tx.send_blocking(Progress::NoKey);
                return;
            };
            for id in stale {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let sent = match workshop::dependents(&key, id) {
                    Ok(found) => tx.send_blocking(Progress::Found(id, found)),
                    Err(e) => {
                        let _ = tx.send_blocking(Progress::Failed(e));
                        return;
                    }
                };
                if sent.is_err() {
                    return;
                }
            }
        });

        let (weak, dw) = (Rc::downgrade(self), Rc::downgrade(d));
        glib::spawn_future_local(async move {
            let mut done = 0;
            let mut failed = None;
            while let Ok(progress) = rx.recv().await {
                let Some(ui) = weak.upgrade() else { return };
                match progress {
                    Progress::Found(id, found) => {
                        ui.dependents.borrow_mut().update(id, found);
                        done += 1;
                        if let Some(d) = dw.upgrade() {
                            d.progress.set_fraction(done as f64 / total as f64);
                            d.searching.set_description(Some(&format!("Checked {done} of {}", plural(total, "mod", "mods"))));
                        }
                    }
                    Progress::NoKey => failed = Some(None),
                    Progress::Failed(e) => failed = Some(Some(e)),
                }
            }
            let Some(ui) = weak.upgrade() else { return };
            // What was found is kept even if the dialog was closed, so the
            // next search picks up where this one stopped.
            if done > 0
                && let Err(e) = ui.dependents.borrow().save(&Cache::path())
            {
                crate::diagnostics::log!("patches", "Could not save the Workshop dependents: {e}");
            }
            let Some(d) = dw.upgrade() else { return };
            d.refresh.set_sensitive(true);
            match failed {
                Some(None) => {
                    let open = gtk::Button::builder().label(t!("_Open Preferences")).use_underline(true).halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
                    let (dialog, window) = (d.dialog.downgrade(), ui.window.downgrade());
                    open.connect_clicked(move |_| {
                        if let Some(dialog) = dialog.upgrade() {
                            dialog.close();
                        }
                        if let Some(window) = window.upgrade() {
                            WidgetExt::activate_action(&window, "win.preferences", None).ok();
                        }
                    });
                    show_problem(&d, &t!("A Steam Web API Key Is Needed"), &t!("Steam tells only apps with a key which Workshop items need a mod. Add yours in Preferences › Steam."), Some(open.upcast()));
                }
                Some(Some(e)) => {
                    crate::diagnostics::log!("patches", "Could not search the Workshop: {e}");
                    let again = gtk::Button::builder().label(t!("_Try Again")).use_underline(true).halign(gtk::Align::Center).css_classes(["pill"]).build();
                    let (weak, dw) = (Rc::downgrade(&ui), Rc::downgrade(&d));
                    again.connect_clicked(move |_| {
                        if let (Some(ui), Some(d)) = (weak.upgrade(), dw.upgrade()) {
                            ui.search_patches(&d, false);
                        }
                    });
                    show_problem(&d, &t!("Couldn't Search the Workshop"), &tf!("{error}. What was found so far is kept.", error = e), Some(again.upcast()));
                }
                None => ui.show_patches(&d),
            }
        });
    }

    /// Lists the suggestions from what's known. Every row is made once;
    /// the switch and the slider only hide and show them, so dragging the
    /// slider doesn't rebuild what it sits in.
    fn show_patches(self: &Rc<Self>, d: &Rc<Dialog>) {
        let (found, names) = {
            let state = self.state.borrow();
            let dismissed = &self.settings.borrow().dismissed_patches;
            let found = patches::suggest(&self.dependents.borrow(), &state.mods, &state.enabled, dismissed);
            let names: Vec<Vec<String>> = found.iter().map(|s| s.needs.iter().map(|&i| display_name(&state.mods[i]).to_owned()).collect()).collect();
            (found, names)
        };

        let page = adw::PreferencesPage::new();
        let options = adw::PreferencesGroup::builder()
            .description(t!("Patches for recent versions of the mods you run are usually updated with them, so older ones are hidden. Some mods that haven't been updated in years still work fine, but this list is meant for recent compatibility patches."))
            .build();
        let weeks = self.settings.borrow().patch_weeks.clamp(MIN_WEEKS, MAX_WEEKS);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, f64::from(MIN_WEEKS), f64::from(MAX_WEEKS), 1.0);
        scale.set_value(f64::from(weeks));
        scale.set_round_digits(0);
        scale.set_draw_value(false);
        scale.set_hexpand(true);
        scale.set_valign(gtk::Align::Center);
        scale.set_width_request(220);
        let marks: [(u32, Option<String>); 7] = [
            (4, None),
            (26, None),
            (52, Some(t!("1 year"))),
            (104, Some(t!("2 years"))),
            (156, None),
            (208, None),
            (MAX_WEEKS, Some(t!("5 years"))),
        ];
        for (at, mark) in marks {
            scale.add_mark(f64::from(at), gtk::PositionType::Bottom, mark.as_deref());
        }
        scale.update_property(&[gtk::accessible::Property::Label(&t!("Updated within, in weeks"))]);
        let age = adw::ActionRow::builder().title(t!("Updated Within")).subtitle(weeks_text(weeks)).build();
        age.add_suffix(&scale);
        options.add(&age);
        let all = adw::SwitchRow::builder().title(t!("Show Everything That Needs Only Enabled Mods")).subtitle(t!("Also items whose names don't say they're patches")).active(d.show_all.get()).build();
        options.add(&all);
        page.add(&options);

        let group = adw::PreferencesGroup::builder()
            .description(t!("Workshop items made for mods you run, all of whose required items are enabled. After subscribing, reload the mods (F5): Smart Sort places patches after the mods they patch."))
            .build();
        let none = adw::ActionRow::builder().css_classes(["dim-label"]).build();
        group.add(&none);
        let rows: Vec<(adw::ActionRow, Suggestion)> = found.iter().zip(&names).map(|(s, names)| (self.patch_row(d, &group, s, names), s.clone())).collect();
        for (row, _) in &rows {
            group.add(row);
        }
        page.add(&group);

        // Shows the rows that pass, and counts them in the heading.
        let filter: Rc<dyn Fn()> = {
            let (weak, dw, group, none) = (Rc::downgrade(self), Rc::downgrade(d), group.downgrade(), none.downgrade());
            let rows = Rc::new(rows);
            Rc::new(move || {
                let (Some(ui), Some(d), Some(group), Some(none)) = (weak.upgrade(), dw.upgrade(), group.upgrade(), none.upgrade()) else { return };
                let weeks = ui.settings.borrow().patch_weeks;
                let mut shown = 0;
                let mut too_old = 0;
                // Dismissed rows have been taken out of the group.
                for (row, s) in rows.iter().filter(|(row, _)| row.parent().is_some()) {
                    let kind = d.show_all.get() || s.patch;
                    let recent = s.updated_within(weeks);
                    row.set_visible(kind && recent);
                    shown += usize::from(kind && recent);
                    too_old += usize::from(kind && !recent);
                }
                group.set_title(&if shown == 0 { t!("Nothing to Suggest") } else { tn!("{count} Suggestion", "{count} Suggestions", shown) });
                none.set_visible(shown == 0);
                none.set_title(&if too_old > 0 {
                    tnf!(
                        "{count} patch for your enabled mods has not been updated within {weeks}",
                        "{count} patches for your enabled mods have not been updated within {weeks}",
                        count = too_old,
                        weeks = weeks_text(weeks)
                    )
                } else {
                    t!("Every patch found for your enabled mods is installed or dismissed")
                });
            })
        };
        filter();
        *d.filter.borrow_mut() = Some(filter.clone());

        let (weak, on_change) = (Rc::downgrade(self), filter.clone());
        scale.connect_value_changed(move |scale| {
            let Some(ui) = weak.upgrade() else { return };
            let weeks = scale.value().round() as u32;
            if ui.settings.borrow().patch_weeks == weeks {
                return;
            }
            // Saved when the dialog closes, not at every step of a drag.
            ui.settings.borrow_mut().patch_weeks = weeks;
            age.set_subtitle(&weeks_text(weeks));
            on_change();
        });
        let (dw, on_change) = (Rc::downgrade(d), filter);
        all.connect_active_notify(move |row| {
            if let Some(d) = dw.upgrade() {
                d.show_all.set(row.is_active());
                on_change();
            }
        });

        let dismissed = self.settings.borrow().dismissed_patches.len();
        if dismissed > 0 {
            let restore_group = adw::PreferencesGroup::new();
            let restore = adw::ButtonRow::builder().title(format!("Show {} Again", plural(dismissed, "Dismissed Item", "Dismissed Items"))).build();
            let (weak, dw) = (Rc::downgrade(self), Rc::downgrade(d));
            restore.connect_activated(move |_| {
                if let (Some(ui), Some(d)) = (weak.upgrade(), dw.upgrade()) {
                    ui.settings.borrow_mut().dismissed_patches.clear();
                    ui.save_settings("dismissed patches");
                    ui.show_patches(&d);
                }
            });
            restore_group.add(&restore);
            page.add(&restore_group);
        }
        d.results.set_child(Some(&page));
        d.stack.set_visible_child_name("results");
    }

    fn patch_row(self: &Rc<Self>, d: &Rc<Dialog>, group: &adw::PreferencesGroup, s: &Suggestion, names: &[String]) -> adw::ActionRow {
        let item = &s.item;
        let title = if item.title.is_empty() { format!("Workshop item {}", item.id) } else { item.title.clone() };
        let mut facts = vec![format!("{} {}", thousands(item.subscriptions), if item.subscriptions == 1 { "subscriber" } else { "subscribers" })];
        if item.updated > 0 {
            facts.push(format!("updated {}", ago(UNIX_EPOCH + Duration::from_secs(item.updated))));
        }
        let row = adw::ActionRow::builder().title(&title).use_markup(false).subtitle(format!("{}\n{}", needs(names), facts.join(" · "))).subtitle_lines(0).build();
        row.add_prefix(&self.workshop_thumbnail(item.id, 48));

        let open = gtk::Button::builder().icon_name("web-browser-symbolic").tooltip_text(t!("Open Workshop Page")).valign(gtk::Align::Center).css_classes(["flat"]).build();
        let id = item.id;
        let window = self.window.downgrade();
        open.connect_clicked(move |_| {
            let window = window.upgrade();
            // In Steam, where the player is signed in to subscribe; in the
            // browser if Steam doesn't take the link.
            let fallback = window.as_ref().map(|w| w.downgrade());
            gtk::UriLauncher::new(&format!("steam://url/CommunityFilePage/{id}")).launch(window.as_ref(), None::<&gio::Cancellable>, move |result| {
                if result.is_err() {
                    let window = fallback.and_then(|w| w.upgrade());
                    gtk::UriLauncher::new(&workshop::page_url(id)).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
                }
            });
        });
        row.add_suffix(&open);

        let dismiss = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text(t!("Dismiss")).valign(gtk::Align::Center).css_classes(["flat"]).build();
        let (weak, dw) = (Rc::downgrade(self), Rc::downgrade(d));
        let (group, row_ref) = (group.downgrade(), row.downgrade());
        dismiss.connect_clicked(move |_| {
            let (Some(ui), Some(d)) = (weak.upgrade(), dw.upgrade()) else { return };
            ui.settings.borrow_mut().dismissed_patches.insert(id);
            ui.save_settings("dismissed patches");
            // Just the row, so the list doesn't jump back to the top.
            if let (Some(group), Some(row)) = (group.upgrade(), row_ref.upgrade()) {
                group.remove(&row);
            }
            let filter = d.filter.borrow().clone();
            if let Some(filter) = filter {
                filter();
            }
            let toast = adw::Toast::builder().title(format!("Dismissed “{title}”")).button_label("_Undo").build();
            let (weak, dw) = (Rc::downgrade(&ui), Rc::downgrade(&d));
            toast.connect_button_clicked(move |_| {
                if let (Some(ui), Some(d)) = (weak.upgrade(), dw.upgrade()) {
                    ui.settings.borrow_mut().dismissed_patches.remove(&id);
                    ui.save_settings("dismissed patches");
                    ui.show_patches(&d);
                }
            });
            d.toasts.add_toast(toast);
        });
        row.add_suffix(&dismiss);
        row
    }
}

fn show_problem(d: &Dialog, title: &str, description: &str, action: Option<gtk::Widget>) {
    d.problem.set_icon_name(Some("dialog-information-symbolic"));
    d.problem.set_title(title);
    d.problem.set_description(Some(&glib::markup_escape_text(description)));
    d.problem.set_child(action.as_ref());
    d.stack.set_visible_child_name("problem");
}

/// "26 weeks", or in years when it's whole years.
fn weeks_text(weeks: u32) -> String {
    match weeks {
        52 => t!("1 year"),
        w if w % 52 == 0 => tnf!("{count} year", "{count} years", count = w / 52),
        w => tnf!("{count} week", "{count} weeks", count = w),
    }
}

/// "Needs A and B, both enabled".
fn needs(names: &[String]) -> String {
    let list = match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => tf!("{list} and {last}", list = rest.join(", "), last = last),
    };
    let all = match names.len() {
        1 => t!("enabled"),
        2 => t!("both enabled"),
        _ => t!("all enabled"),
    };
    tf!("Needs {list}, {status}", list = list, status = all)
}

/// 12345 as "12,345".
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_what_a_patch_needs() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(needs(&names(&["Giga"])), "Needs Giga, enabled");
        assert_eq!(needs(&names(&["Giga", "ACOT"])), "Needs Giga and ACOT, both enabled");
        assert_eq!(needs(&names(&["A", "B", "C"])), "Needs A, B and C, all enabled");
        assert_eq!(weeks_text(26), "26 weeks");
        assert_eq!(weeks_text(104), "2 years");
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
