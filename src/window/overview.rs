//! Around the load order: the views sidebar, the health strip above the
//! list, and the inspector showing the selected mod.

use std::rc::Rc;
use std::time::SystemTime;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::{Window, display_name};
use crate::conflicts_view::plural;
use crate::merge::patch;
use crate::mods::checks::{self, Severity};
use crate::mods::filter::View;
use crate::mods::smart_sort::{Category, Run};
use crate::paradox::steam::ItemState;

const CSS: &str = "
/* The side panels float as rounded cards beside the list, rather than
   filling the window's edges. */
.load-order-split > .sidebar-pane { background: transparent; box-shadow: none; border: none; }
.side-panel { background: @sidebar_bg_color; color: @sidebar_fg_color; border-radius: 12px; margin: 8px; box-shadow: 0 0 0 1px alpha(@borders, 0.6), 0 2px 6px alpha(black, 0.12); }
.side-panel .navigation-sidebar { background: transparent; }
.health-card { padding: 10px 14px; }
.health-card .number { font-size: 20px; font-weight: 700; }
.mod-list > row.mod-row { min-height: 0; }
.mod-list > row.mod-row > box.header { min-height: 40px; padding-top: 2px; padding-bottom: 2px; }
.conflict-count { padding: 1px 7px; border-radius: 99px; background: alpha(@accent_color, 0.15); color: @accent_color; }
.inspector-problem { padding: 10px 12px; border-radius: 10px; background: alpha(@warning_color, 0.12); }
.inspector-problem.info { background: alpha(currentColor, 0.06); }
.inspector-stat { padding: 8px 12px; }
.category-dot { min-width: 8px; min-height: 8px; border-radius: 99px; }
.category-header { padding: 7px 12px 5px 12px; background: alpha(currentColor, 0.03); border-bottom: 1px solid alpha(currentColor, 0.08); }
.category-dot.unofficial { background: #57e389; }
.category-dot.graphics { background: #62a0ea; }
.category-dot.flavor { background: #f08cbe; }
.category-dot.shipsets { background: #a3a7f2; }
.category-dot.galaxy { background: #5cc8bd; }
.category-dot.gameplay { background: #f5c211; }
.category-dot.overhaul { background: #f0875a; }
.category-dot.patches { background: #c9a27e; }
.category-dot.ui { background: #c061cb; }
.category-dot.ui-patches { background: #dc9ae0; }
.category-dot.universal { background: #c0bfbc; }
";

pub(super) fn add_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// The views list, each view's count label, and the game summary below.
pub(super) fn views_sidebar() -> (gtk::ListBox, Vec<(View, gtk::Label)>, gtk::Label) {
    let list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
    let mut counts = Vec::new();
    for view in View::ALL {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        line.append(&gtk::Image::from_icon_name(view.icon()));
        line.append(&gtk::Label::builder().label(view.title()).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build());
        let count = gtk::Label::builder().css_classes(["dim-label", "numeric", "caption"]).build();
        line.append(&count);
        list.append(&gtk::ListBoxRow::builder().child(&line).build());
        counts.push((view, count));
    }
    let info = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(12)
        .css_classes(["dim-label", "caption"])
        .build();
    (list, counts, info)
}

/// The Smart Sort categories, each with its row and count; a category with
/// no enabled mods is hidden.
pub(super) fn categories_sidebar() -> (gtk::ListBox, Vec<(Category, gtk::ListBoxRow, gtk::Label)>) {
    let list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
    let mut counts = Vec::new();
    for category in Category::ALL {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        line.append(&category_dot(category));
        line.append(&gtk::Label::builder().label(category.title()).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build());
        let count = gtk::Label::builder().css_classes(["dim-label", "numeric", "caption"]).build();
        line.append(&count);
        let row = gtk::ListBoxRow::builder().child(&line).tooltip_text(category.description()).visible(false).build();
        list.append(&row);
        counts.push((category, row, count));
    }
    (list, counts)
}

/// A category's colour, shared by the sidebar and the list's headers.
fn category_dot(category: Category) -> gtk::Box {
    gtk::Box::builder().valign(gtk::Align::Center).css_classes(["category-dot", category.key()]).build()
}

pub(super) fn sidebar_page(views: &gtk::ListBox, categories: &gtk::ListBox, info: &gtk::Label) -> gtk::Box {
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["side-panel"]).overflow(gtk::Overflow::Hidden).build();
    let heading = |text: &str| gtk::Label::builder().label(text).xalign(0.0).margin_start(18).margin_top(12).margin_bottom(4).css_classes(["heading", "dim-label"]).build();
    let lists = gtk::Box::new(gtk::Orientation::Vertical, 0);
    lists.append(&heading("Views"));
    lists.append(views);
    let categories_heading = heading("Categories");
    categories_heading.set_tooltip_text(Some("The sections Smart Sort puts enabled mods in"));
    lists.append(&categories_heading);
    lists.append(categories);
    // Hidden with the list while no category has a mod.
    categories.bind_property("visible", &categories_heading, "visible").sync_create().build();
    page.append(&gtk::ScrolledWindow::builder().child(&lists).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build());
    page.append(info);
    page
}

/// A header over a run of mods in one category: "Gameplay · 42".
fn category_header(run: &Run) -> gtk::Widget {
    let line = gtk::Box::builder().spacing(8).css_classes(["category-header"]).build();
    line.append(&category_dot(run.category));
    let title = if run.manual { format!("{} (manually ordered)", run.category.title()) } else { run.category.title().to_owned() };
    line.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["caption-heading"]).build());
    line.append(&gtk::Label::builder().label(format!("· {}", run.len)).xalign(0.0).css_classes(["caption", "dim-label", "numeric"]).build());
    line.set_tooltip_text(Some(&if run.manual {
        format!("Smart Sort would put {} elsewhere in {}: moved here by hand", if run.len == 1 { "this mod" } else { "these mods" }, run.category.title())
    } else {
        run.category.description().to_owned()
    }));
    line.upcast()
}

/// Something a button does to the window.
type Action = Rc<dyn Fn(&Rc<Window>)>;
/// What clicking a health card does.
type Card = Box<dyn Fn(&Rc<Window>)>;

/// What the health strip shows.
pub(super) struct Health {
    pub enabled: usize,
    pub total: usize,
    pub dirty: bool,
    pub version: Option<String>,
    pub outdated: usize,
    pub missing_required: usize,
    pub misordered: usize,
    pub no_achievements: Vec<String>,
    pub stale_merges: usize,
    /// Frozen mods Steam has updated since, by name.
    pub frozen_updates: Vec<String>,
}

/// "3 days ago" and the like, for when a mod was updated.
fn ago(t: SystemTime) -> String {
    let days = SystemTime::now().duration_since(t).map_or(0, |d| d.as_secs() / 86_400);
    match days {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        2..=13 => format!("{days} days ago"),
        14..=59 => format!("{} weeks ago", days / 7),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).css_classes(classes).build()
}

impl Window {
    pub(super) fn setup_overview(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.views_list.connect_row_selected(move |_, row| {
            let (Some(ui), Some(row)) = (weak.upgrade(), row) else { return };
            let view = View::ALL.get(row.index() as usize).copied().unwrap_or_default();
            ui.categories_list.unselect_all();
            ui.category.set(None);
            ui.view.set(view);
            ui.apply_view();
        });
        let weak = Rc::downgrade(self);
        self.categories_list.connect_row_selected(move |_, row| {
            let (Some(ui), Some(row)) = (weak.upgrade(), row) else { return };
            let Some(&category) = Category::ALL.get(row.index() as usize) else { return };
            ui.views_list.unselect_all();
            ui.category.set(Some(category));
            ui.view.set(View::All);
            ui.apply_view();
        });
        self.views_list.select_row(self.views_list.row_at_index(0).as_ref());
        self.setup_category_headers();
        for (list, other) in [(&self.enabled_list, &self.disabled_list), (&self.disabled_list, &self.enabled_list)] {
            let weak = Rc::downgrade(self);
            let other = other.downgrade();
            list.connect_row_selected(move |_, row| {
                let (Some(ui), Some(row)) = (weak.upgrade(), row) else { return };
                let Some(index) = row.widget_name().strip_prefix("mod-").and_then(|i| i.parse().ok()) else { return };
                if let Some(other) = other.upgrade() {
                    other.unselect_all();
                }
                let changed = ui.selected.replace(Some(index)) != Some(index);
                if changed || ui.inspector.child().is_none() {
                    ui.show_inspector();
                }
                if ui.inspector_split.is_collapsed() {
                    ui.inspector_split.set_show_sidebar(true);
                }
            });
        }
        self.show_inspector();
    }

    /// Shows `view` in the sidebar and the list.
    pub(super) fn set_view(&self, view: View) {
        let index = View::ALL.iter().position(|v| *v == view).unwrap_or(0);
        self.views_list.select_row(self.views_list.row_at_index(index as i32).as_ref());
    }

    fn apply_view(&self) {
        let view = self.view.get();
        let category = self.category.get();
        self.enabled_section.set_visible(view != View::Disabled);
        self.disabled_section.set_visible(category.is_none() && matches!(view, View::All | View::Attention | View::Disabled));
        let show_missing = self.missing_list.first_child().is_some() && view == View::All && category.is_none();
        self.missing_heading.set_visible(show_missing);
        self.missing_list.set_visible(show_missing);
        self.enabled_list.invalidate_filter();
        self.disabled_list.invalidate_filter();
        self.enabled_list.invalidate_headers();
    }

    /// Whether the load order shows its category headers: only in the
    /// whole load order, where its rows are all there, in order.
    fn shows_category_headers(&self) -> bool {
        !self.settings.borrow().hide_category_headers && self.view.get() == View::All && self.category.get().is_none() && self.search.text().trim().is_empty()
    }

    /// Puts a header over each run of mods in one category, and adds the
    /// action that turns them off.
    fn setup_category_headers(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.enabled_list.set_header_func(move |row, _| {
            let Some(ui) = weak.upgrade() else { return };
            let run = ui.shows_category_headers().then(|| row.widget_name().strip_prefix("mod-").and_then(|i| i.parse().ok()).and_then(|i: usize| ui.category_runs.borrow().get(&i).cloned())).flatten();
            match run {
                Some(run) => row.set_header(Some(&category_header(&run))),
                None if row.header().is_some() => row.set_header(None::<&gtk::Widget>),
                None => {}
            }
        });
        let shown = !self.settings.borrow().hide_category_headers;
        let action = gio::SimpleAction::new_stateful("category-headers", None, &shown.to_variant());
        let weak = Rc::downgrade(self);
        action.connect_change_state(move |action, value| {
            let (Some(ui), Some(shown)) = (weak.upgrade(), value.and_then(|v| v.get::<bool>())) else { return };
            action.set_state(&shown.to_variant());
            ui.settings.borrow_mut().hide_category_headers = !shown;
            ui.save_settings("whether category headers show");
            ui.enabled_list.invalidate_headers();
        });
        self.window.add_action(&action);
    }

    /// Updates the categories' counts, hiding those with no enabled mods,
    /// and leaves a category that emptied for the whole load order.
    pub(super) fn refresh_category_counts(&self) {
        let facts = self.search_facts.borrow();
        let mut any = false;
        let mut emptied = false;
        for (category, row, label) in &self.category_counts {
            let n = facts.values().filter(|f| f.category == Some(*category)).count();
            label.set_label(&n.to_string());
            row.set_visible(n > 0);
            any |= n > 0;
            emptied |= n == 0 && self.category.get() == Some(*category);
        }
        drop(facts);
        self.categories_list.set_visible(any);
        if emptied {
            self.set_view(View::All);
        }
    }

    /// Updates the views' counts from what the rows said about each mod.
    pub(super) fn refresh_view_counts(&self, enabled: usize) {
        let facts = self.search_facts.borrow();
        for (view, label) in &self.view_counts {
            let n = if *view == View::All { enabled } else { facts.values().filter(|f| view.matches(f)).count() };
            label.set_label(&n.to_string());
            label.set_visible(n > 0 || *view == View::All);
            if *view == View::Attention {
                label.set_css_classes(if n > 0 { &["warning", "numeric", "caption-heading"] } else { &["dim-label", "numeric", "caption"] });
            }
        }
    }

    /// Rebuilds the health strip and the game summary in the sidebar.
    pub(super) fn refresh_health(self: &Rc<Self>, h: Health) {
        while let Some(child) = self.health.first_child() {
            self.health.remove(&child);
        }
        let short = h.version.as_deref().map(|v| format!("v{}", crate::mods::game_update::major_minor(v)));
        let mut first = format!("of {} enabled", h.total);
        if h.dirty {
            first.push_str(" · unsaved");
        }
        self.health.append(&self.health_card(&h.enabled.to_string(), &first, None, false));
        if h.outdated > 0 {
            let text = format!("not updated for {}", short.as_deref().unwrap_or("this game"));
            self.health.append(&self.health_card(&h.outdated.to_string(), &text, Some(("Show", Box::new(|ui: &Rc<Window>| ui.set_view(View::Outdated)))), true));
        }
        if h.missing_required > 0 {
            let text = if h.missing_required == 1 { "required mod missing" } else { "required mods missing" };
            self.health.append(&self.health_card(&h.missing_required.to_string(), text, Some(("Show", Box::new(|ui: &Rc<Window>| ui.set_view(View::Attention)))), true));
        }
        if h.misordered > 0 {
            let text = if h.misordered == 1 { "loads before a mod it needs" } else { "load before mods they need" };
            self.health.append(&self.health_card(&h.misordered.to_string(), text, Some(("Sort", Box::new(|ui: &Rc<Window>| ui.sort_by_dependencies()))), true));
        }
        if h.stale_merges > 0 {
            let text = if h.stale_merges == 1 { "merge out of date" } else { "merges out of date" };
            self.health.append(&self.health_card(&h.stale_merges.to_string(), text, Some(("Review", Box::new(|ui: &Rc<Window>| ui.stack.set_visible_child_name("conflicts")))), true));
        }
        if !h.frozen_updates.is_empty() {
            let n = h.frozen_updates.len();
            let text = if n == 1 { "frozen mod updated on the Workshop" } else { "frozen mods updated on the Workshop" };
            let card = self.health_card(&n.to_string(), text, Some(("Show", Box::new(|ui: &Rc<Window>| ui.show_frozen_updates()))), false);
            card.set_tooltip_text(Some(&h.frozen_updates.join("\n")));
            self.health.append(&card);
        }
        if h.enabled > 0 {
            let card = if h.no_achievements.is_empty() {
                self.health_card("On", "achievements stay on", None, false)
            } else {
                let n = h.no_achievements.len();
                self.health_card(&n.to_string(), if n == 1 { "mod turns off achievements" } else { "mods turn off achievements" }, Some(("Show", Box::new(|ui: &Rc<Window>| ui.set_view(View::NoAchievements)))), false)
            };
            card.set_tooltip_text(Some(&if h.no_achievements.is_empty() {
                "No enabled mod changes the files the game checksums, so achievements stay on".to_owned()
            } else {
                format!("Achievements are off because these mods change the files the game checksums:\n{}", h.no_achievements.join("\n"))
            }));
            self.health.append(&card);
        }

        let mut info = Vec::new();
        if let Some(version) = &h.version {
            info.push(format!("Stellaris {version}"));
        }
        info.push(if h.no_achievements.is_empty() { "Achievements on".to_owned() } else { format!("Achievements off, because of {}", plural(h.no_achievements.len(), "mod", "mods")) });
        if let Some(last) = crate::paradox::session::last_save(&self.paths.user_dir) {
            let mut line = format!("Last save: {}", last.empire);
            if !last.date.is_empty() {
                line.push_str(&format!(", {}", last.date));
            }
            let played = crate::mods::saves::Saves::load(&crate::mods::saves::path()).campaigns.remove(&last.folder);
            if let Some(played) = played {
                let same = crate::mods::saves::compare(&played.mods, &self.state.borrow().enabled_ids()).is_empty();
                line.push_str(if same { " · same mods" } else { " · played with other mods" });
            }
            info.push(line);
        }
        self.game_info.set_label(&info.join("\n"));
    }

    /// A health strip card: a number, what it counts, and what clicking
    /// it does, if anything.
    fn health_card(self: &Rc<Self>, number: &str, text: &str, action: Option<(&str, Card)>, warn: bool) -> gtk::Widget {
        let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let number = gtk::Label::builder().label(number).xalign(0.0).css_classes(["number", "numeric"]).build();
        if warn {
            number.add_css_class("warning");
        }
        body.append(&number);
        body.append(&label(text, &["caption"]));
        let Some((verb, run)) = action else {
            body.add_css_class("card");
            body.add_css_class("health-card");
            return body.upcast();
        };
        body.append(&gtk::Label::builder().label(verb).xalign(0.0).css_classes(["caption-heading", "accent"]).build());
        let button = gtk::Button::builder().child(&body).css_classes(["card", "health-card"]).build();
        self.run_later_on_click(&button, Rc::from(run));
        button.upcast()
    }

    /// Fills the inspector with the selected mod, or says to pick one.
    pub(super) fn show_inspector(self: &Rc<Self>) {
        let built = self.selected.get().and_then(|i| self.inspector_for(i));
        let child = built.unwrap_or_else(|| {
            adw::StatusPage::builder()
                .icon_name("view-list-symbolic")
                .title("No Mod Selected")
                .description("Click a mod to see what's wrong with it, what it needs and what it overrides.")
                .css_classes(["compact"])
                .build()
                .upcast()
        });
        self.inspector.set_child(Some(&child));
    }

    fn inspector_for(self: &Rc<Self>, index: usize) -> Option<gtk::Widget> {
        let state = self.state.borrow();
        let m = state.mods.get(index)?;
        let position = state.enabled.iter().position(|&i| i == index);
        let d = &m.descriptor;
        let is_patch = patch::is_patch(&m.id);
        let alias = self.settings.borrow().aliases.get(&m.id).cloned();
        let name = alias.clone().unwrap_or_else(|| display_name(m).to_owned());
        let issues = self.last_issues.borrow().get(&index).cloned().unwrap_or_default();
        let conflicts = self.last_conflicts.borrow().get(&index).copied();
        let target = (index as u32).to_variant();

        let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();

        // Who it is.
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let thumb = self.thumbnail_sized(m, 64);
        top.append(&thumb);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        titles.set_valign(gtk::Align::Center);
        titles.append(&label(&name, &["title-3"]));
        if alias.is_some() {
            titles.append(&label(display_name(m), &["dim-label", "caption"]));
        }
        let mut line = vec![match position {
            Some(p) => format!("#{}", p + 1),
            None => "Disabled".to_owned(),
        }];
        line.push(match m.source {
            crate::mods::Source::Workshop => "Workshop".to_owned(),
            crate::mods::Source::Local => "Local".to_owned(),
        });
        if is_patch {
            line.push("Always loads last".to_owned());
        }
        titles.append(&label(&line.join(" · "), &["dim-label", "caption"]));
        let mut line = Vec::new();
        if !d.game_version().is_empty() {
            line.push(format!("Supports {}", d.game_version()));
        }
        if let Some(t) = m.updated() {
            line.push(format!("updated {}", ago(t)));
        }
        if !line.is_empty() {
            titles.append(&label(&line.join(" · "), &["dim-label", "caption"]));
        }
        top.append(&titles);
        page.append(&top);

        // The user's own note, ahead of what the app found.
        if let Some(note) = self.settings.borrow().notes.get(&m.id) {
            let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            card.add_css_class("inspector-problem");
            card.add_css_class("info");
            card.append(&gtk::Image::builder().icon_name("document-edit-symbolic").valign(gtk::Align::Start).css_classes(["dim-label"]).build());
            let text = label(note, &[]);
            text.set_hexpand(true);
            text.set_selectable(true);
            card.append(&text);
            let edit = gtk::Button::builder().label("Edit").valign(gtk::Align::Start).action_name("win.edit-note").action_target(&target).css_classes(["flat", "small"]).build();
            card.append(&edit);
            page.append(&card);
        }

        // What's wrong, each with its fix where there is one.
        let mut problems: Vec<(Severity, String, Option<gtk::Widget>)> = Vec::new();
        for (severity, message) in self.extra_problems(m) {
            let fix = (is_patch && message.contains("Conflicts page")).then(|| self.fix_button("Review", |ui| ui.stack.set_visible_child_name("conflicts")));
            problems.push((severity, message, fix));
        }
        for issue in &issues {
            let fix = if issue.fixed_by_sorting {
                Some(self.fix_button("Sort", |ui| ui.sort_by_dependencies()))
            } else if issue.message.starts_with("Requires “") {
                let get = gtk::Button::builder().label("Get").valign(gtk::Align::Center).action_name("win.get-requirements").action_target(&target).css_classes(["pill", "small"]).build();
                let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
                actions.append(&get);
                if let Some(r) = m.requires.iter().find(|r| issue.message.contains(&format!("“{}”", r.name))) {
                    let ignore = gtk::Button::builder().label("Not Needed").action_name("win.ignore-requirement").action_target(&r.steam_id.to_variant()).tooltip_text(format!("Stop treating “{}” as required", r.name)).css_classes(["flat", "small"]).build();
                    actions.append(&ignore);
                }
                Some(actions.upcast())
            } else {
                None
            };
            problems.push((issue.severity, issue.message.clone(), fix));
        }
        problems.sort_by_key(|p| std::cmp::Reverse(p.0));
        if problems.is_empty() {
            page.append(&label("No problems found", &["success"]));
        }
        for (severity, message, fix) in problems {
            let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            card.add_css_class("inspector-problem");
            let (icon, class) = match severity {
                Severity::Warning => ("dialog-warning-symbolic", "warning"),
                Severity::Info => ("dialog-information-symbolic", "dim-label"),
            };
            if severity == Severity::Info {
                card.add_css_class("info");
            }
            card.append(&gtk::Image::builder().icon_name(icon).valign(gtk::Align::Start).css_classes([class]).build());
            let text = label(&message, &[]);
            text.set_hexpand(true);
            card.append(&text);
            if let Some(fix) = fix {
                card.append(&fix);
            }
            page.append(&card);
        }

        // Two facts that matter before launching.
        let stats = gtk::Box::builder().spacing(8).homogeneous(true).build();
        let stat = |title: &str, value: &str| {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
            b.add_css_class("card");
            b.add_css_class("inspector-stat");
            b.append(&label(title, &["dim-label", "caption"]));
            b.append(&label(value, &["heading"]));
            b
        };
        let overrides = match conflicts {
            _ if position.is_none() => "—".to_owned(),
            Some(0) => "None".to_owned(),
            Some(n) => plural(n, "conflict", "conflicts"),
            None => "Not scanned".to_owned(),
        };
        stats.append(&stat("Conflicts", &overrides));
        let facts = self.search_facts.borrow().get(&index).cloned();
        stats.append(&stat("Achievements", if facts.is_some_and(|f| !f.achievements) { "Turns them off" } else { "Kept" }));
        page.append(&stats);

        // What it needs, and whether that's there.
        let mut needs: Vec<(String, &str)> = Vec::new();
        let enabled_named = |name: &str| state.mods.iter().position(|x| x.descriptor.name.trim().eq_ignore_ascii_case(name.trim()));
        for dep in &d.dependencies {
            let status = match enabled_named(dep) {
                None => "not installed",
                Some(i) if state.enabled.contains(&i) => "enabled",
                Some(_) => "disabled",
            };
            needs.push((dep.trim().to_owned(), status));
        }
        for r in &m.requires {
            let status = match state.mods.iter().position(|x| x.descriptor.remote_file_id == Some(r.steam_id)) {
                None => "not installed",
                Some(i) if state.enabled.contains(&i) => "enabled",
                Some(_) => "disabled",
            };
            if !needs.iter().any(|(n, _)| n.eq_ignore_ascii_case(&r.name)) {
                needs.push((r.name.clone(), status));
            }
        }
        if !needs.is_empty() {
            page.append(&label("Needs", &["heading"]));
            let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
            for (name, status) in needs {
                let row = adw::ActionRow::builder().title(&name).use_markup(false).build();
                let status_label = gtk::Label::builder().label(status).css_classes(if status == "enabled" { ["dim-label", "caption"] } else { ["warning", "caption"] }).build();
                row.add_suffix(&status_label);
                list.append(&row);
            }
            page.append(&list);
        }
        if !d.tags.is_empty() {
            page.append(&label(&d.tags.join(" · "), &["dim-label", "caption"]));
        }

        // What can be done with it.
        let actions = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        if !is_patch {
            let switch = adw::SwitchRow::builder().title("Enabled").active(position.is_some()).build();
            let weak = Rc::downgrade(self);
            switch.connect_active_notify(move |s| {
                let on = s.is_active();
                if let Some(ui) = weak.upgrade() {
                    ui.update(move |state| state.set_enabled(index, on));
                }
            });
            actions.append(&switch);
        }
        let mut items: Vec<(&str, &str)> = Vec::new();
        if d.remote_file_id.is_some() {
            items.push(("Open Workshop Page", "win.open-workshop"));
            items.push(("Open in Steam", "win.open-in-steam"));
        }
        if m.content.is_some() {
            items.push(("Open Folder", "win.open-folder"));
        }
        if conflicts.is_some_and(|n| n > 0) {
            items.push(("Show Conflicts", "win.show-conflicts"));
        }
        items.push(("Rename…", "win.rename-mod"));
        if !self.settings.borrow().notes.contains_key(&m.id) {
            items.push(("Add Note…", "win.edit-note"));
        }
        if position.is_some() && !is_patch {
            items.push(("Move to Top", "win.move-top"));
            items.push(("Move to Bottom", "win.move-bottom"));
        }
        if d.remote_file_id.is_some() && m.steam != Some(ItemState::Unsubscribed) {
            items.push(("Unsubscribe in Steam…", "win.unsubscribe"));
        }
        for (title, action) in items {
            let row = adw::ActionRow::builder().title(title).activatable(true).build();
            row.set_action_name(Some(action));
            row.set_action_target_value(Some(&target));
            actions.append(&row);
        }
        page.append(&actions);
        drop(state);

        let scrolled = gtk::ScrolledWindow::builder().child(&page).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        Some(scrolled.upcast())
    }

    /// Runs `run` after the click is handled, since what it does rebuilds
    /// the button.
    fn run_later_on_click(self: &Rc<Self>, button: &gtk::Button, run: Action) {
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            let (weak, run) = (weak.clone(), run.clone());
            glib::idle_add_local_once(move || {
                if let Some(ui) = weak.upgrade() {
                    run(&ui);
                }
            });
        });
    }

    fn fix_button(self: &Rc<Self>, text: &str, run: impl Fn(&Rc<Window>) + 'static) -> gtk::Widget {
        let button = gtk::Button::builder().label(text).valign(gtk::Align::Center).css_classes(["pill", "small"]).build();
        self.run_later_on_click(&button, Rc::new(run));
        button.upcast()
    }
}

/// Facts about a warning for the health strip: whether the issue is a
/// Workshop required item that isn't installed.
pub(super) fn is_missing_requirement(issue: &checks::Issue) -> bool {
    issue.message.starts_with("Requires “") && issue.message.ends_with("isn't installed")
}
