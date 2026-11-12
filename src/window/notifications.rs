//! The bell in the header: every notification lands there, newest first,
//! to read later and dismiss one by one or all at once. Toasts still flash
//! briefly; they no longer stay on screen until closed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::Window;

/// How many notifications are kept; older ones drop off.
const KEPT: usize = 100;
/// How long a toast that used to stay until closed now shows, in seconds.
const BRIEF: u32 = 6;

const CSS: &str = "
.notification-badge { background: @accent_bg_color; color: @accent_fg_color; border-radius: 99px; min-width: 16px; min-height: 16px; padding: 0 4px; font-size: 10px; font-weight: 700; }
.notification-list { background: transparent; }
";

struct Entry {
    title: String,
    at: glib::DateTime,
    /// How many times it was said in a row; repeats are folded into one.
    count: u32,
    /// The toast it came from, whose button the entry offers again.
    toast: adw::Toast,
}

pub(super) struct Bell {
    pub(super) button: gtk::MenuButton,
    badge: gtk::Label,
    list: gtk::ListBox,
    empty: gtk::Label,
    clear: gtk::Button,
    entries: RefCell<Vec<Entry>>,
    unseen: Cell<usize>,
}

impl Bell {
    pub(super) fn new() -> Rc<Self> {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }

        let badge = gtk::Label::builder().halign(gtk::Align::End).valign(gtk::Align::Start).visible(false).css_classes(["notification-badge", "numeric"]).can_target(false).build();
        let icon = gtk::Overlay::builder().child(&gtk::Image::from_icon_name("preferences-system-notifications-symbolic")).build();
        icon.add_overlay(&badge);

        let heading = gtk::Label::builder().label("Notifications").xalign(0.0).hexpand(true).css_classes(["heading"]).build();
        let clear = gtk::Button::builder().label("Clear All").css_classes(["flat", "small"]).sensitive(false).build();
        let top = gtk::Box::builder().spacing(6).margin_start(6).margin_end(2).build();
        top.append(&heading);
        top.append(&clear);
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list", "notification-list"]).build();
        let empty = gtk::Label::builder().label("No notifications").margin_top(18).margin_bottom(18).css_classes(["dim-label"]).build();
        let scrolled = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).max_content_height(460).visible(false).build();
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).width_request(380).build();
        body.append(&top);
        body.append(&empty);
        body.append(&scrolled);
        let popover = gtk::Popover::builder().child(&body).build();

        let button = gtk::MenuButton::builder().child(&icon).popover(&popover).tooltip_text("Notifications").build();
        let bell = Rc::new(Bell { button, badge, list, empty, clear, entries: RefCell::new(Vec::new()), unseen: Cell::new(0) });

        let weak = Rc::downgrade(&bell);
        popover.connect_show(move |_| {
            if let Some(bell) = weak.upgrade() {
                bell.unseen.set(0);
                // Times are shown relative to today, so they're redrawn.
                bell.rebuild();
            }
        });
        let weak = Rc::downgrade(&bell);
        bell.clear.connect_clicked(move |_| {
            if let Some(bell) = weak.upgrade() {
                bell.entries.borrow_mut().clear();
                bell.rebuild();
            }
        });
        bell
    }

    /// Records `toast`. The same message again moves to the top, counted.
    pub(super) fn push(self: &Rc<Self>, toast: &adw::Toast) {
        let title = toast.title().map(|t| t.to_string()).unwrap_or_default();
        if title.trim().is_empty() {
            return;
        }
        {
            let mut entries = self.entries.borrow_mut();
            let count = match entries.iter().position(|e| e.title == title) {
                Some(at) => entries.remove(at).count + 1,
                None => 1,
            };
            let at = glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("the clock works"));
            entries.insert(0, Entry { title, at, count, toast: toast.clone() });
            entries.truncate(KEPT);
        }
        // Clicking the toast's own button also settles the notification.
        let weak = Rc::downgrade(self);
        let clicked = toast.clone();
        toast.connect_button_clicked(move |_| {
            if let Some(bell) = weak.upgrade() {
                bell.forget(&clicked);
            }
        });
        self.unseen.set(self.unseen.get() + 1);
        self.update_badge();
        if self.button.popover().is_some_and(|p| p.is_visible()) {
            self.rebuild();
        }
    }

    fn forget(self: &Rc<Self>, toast: &adw::Toast) {
        self.entries.borrow_mut().retain(|e| e.toast != *toast);
        self.rebuild();
    }

    fn update_badge(&self) {
        let unseen = self.unseen.get().min(self.entries.borrow().len());
        self.badge.set_visible(unseen > 0);
        self.badge.set_label(&if unseen > 99 { "99+".to_owned() } else { unseen.to_string() });
        let total = self.entries.borrow().len();
        self.button.set_tooltip_text(Some(&match (total, unseen) {
            (0, _) => "Notifications".to_owned(),
            (_, 0) => format!("Notifications ({total})"),
            _ => format!("Notifications ({unseen} new)"),
        }));
    }

    fn rebuild(self: &Rc<Self>) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let entries = self.entries.borrow();
        let now = glib::DateTime::now_local().ok();
        for entry in entries.iter() {
            self.list.append(&self.row(entry, now.as_ref()));
        }
        let any = !entries.is_empty();
        self.empty.set_visible(!any);
        if let Some(scrolled) = self.list.ancestor(gtk::ScrolledWindow::static_type()) {
            scrolled.set_visible(any);
        }
        self.clear.set_sensitive(any);
        drop(entries);
        self.update_badge();
    }

    fn row(self: &Rc<Self>, entry: &Entry, now: Option<&glib::DateTime>) -> adw::ActionRow {
        let mut when = when(&entry.at, now);
        if entry.count > 1 {
            when.push_str(&format!(" · {} times", entry.count));
        }
        let row = adw::ActionRow::builder().title(&entry.title).subtitle(when).use_markup(false).build();
        // The toast's button, such as Show or Restart, offered again.
        if let Some(label) = entry.toast.button_label().filter(|l| !l.is_empty()) {
            let act = gtk::Button::builder().label(label.replace('_', "")).valign(gtk::Align::Center).css_classes(["flat", "small"]).build();
            let weak = Rc::downgrade(self);
            let toast = entry.toast.clone();
            act.connect_clicked(move |_| {
                let Some(bell) = weak.upgrade() else { return };
                if let Some(popover) = bell.button.popover() {
                    popover.popdown();
                }
                // After the click is handled: this settles the notification,
                // which destroys the button.
                let toast = toast.clone();
                glib::idle_add_local_once(move || {
                    if let Some(action) = toast.action_name() {
                        let target = toast.action_target_value();
                        let _ = bell.button.activate_action(&action, target.as_ref());
                    }
                    // Runs what was connected to the toast's button, and
                    // forgets the notification.
                    toast.emit_by_name::<()>("button-clicked", &[]);
                });
            });
            row.add_suffix(&act);
        }
        let dismiss = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text("Dismiss").valign(gtk::Align::Center).css_classes(["flat", "circular"]).build();
        let weak = Rc::downgrade(self);
        let toast = entry.toast.clone();
        dismiss.connect_clicked(move |_| {
            if let Some(bell) = weak.upgrade() {
                // After the click is handled, since this destroys the button.
                let toast = toast.clone();
                glib::idle_add_local_once(move || bell.forget(&toast));
            }
        });
        row.add_suffix(&dismiss);
        row
    }
}

/// When a notification came, as `14:03`, or `Yesterday 14:03` and so on.
fn when(at: &glib::DateTime, now: Option<&glib::DateTime>) -> String {
    let time = at.format("%H:%M").map(|s| s.to_string()).unwrap_or_default();
    let Some(now) = now else { return time };
    let days = now.difference_in_days(at);
    match days {
        0 => time,
        1 => format!("Yesterday {time}"),
        _ => at.format("%b %-d, %H:%M").map(|s| s.to_string()).unwrap_or(time),
    }
}

trait DayDifference {
    fn difference_in_days(&self, earlier: &glib::DateTime) -> i32;
}

impl DayDifference for glib::DateTime {
    /// Calendar days between `earlier` and this, ignoring the time of day.
    fn difference_in_days(&self, earlier: &glib::DateTime) -> i32 {
        let day = |d: &glib::DateTime| glib::DateTime::new(&d.timezone(), d.year(), d.month(), d.day_of_month(), 0, 0, 0.0).ok();
        match (day(self), day(earlier)) {
            (Some(a), Some(b)) => (a.difference(&b).as_seconds() / 86_400) as i32,
            _ => 0,
        }
    }
}

impl Window {
    /// Shows a toast that reports progress and is dismissed when the work
    /// is done. It's not kept in the bell.
    pub(super) fn show_progress(&self, toast: &adw::Toast) {
        self.toasts.add_toast(toast.clone());
    }

    /// Keeps `toast` in the bell, unless it's an Undo offer, which only
    /// makes sense in the moment, and makes it brief.
    pub(super) fn notify(&self, toast: &adw::Toast) {
        if toast.timeout() == 0 {
            toast.set_timeout(BRIEF);
        }
        if toast.button_label().is_some_and(|l| l.replace('_', "") == "Undo") {
            return;
        }
        self.bell.push(toast);
    }
}
