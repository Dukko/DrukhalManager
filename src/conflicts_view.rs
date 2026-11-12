//! The Conflicts page: every conflict in a sidebar, grouped by folder, and
//! the selected one's versions side by side.
//!
//! An object conflict is resolved two versions at a time: the first mod's
//! version against the next one, then the result so far against the one
//! after, until one editable result is left to save to the merged patch
//! (see `object`). An overridden file shows two copies at a time, chosen
//! from a row of chips (see `file`).

mod file;
mod object;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::diff_view::{self, Pair};
use crate::merge::ai::Provider;
use crate::merge::conflicts::Report;
use crate::merge::file_merge;
use crate::merge::ignore_rules::Rules;
use crate::merge::patch::{Merge, Print, Stale};

pub use object::Steps;

/// Rows shown per folder in the sidebar; search finds the rest, so huge
/// folders (e.g. localisation) stay responsive.
const ROWS_PER_GROUP: usize = 300;

/// A lightning bolt, for conflicts that merge without AI.
const SIMPLE_ICON: &str = "io.github.dukko.DrukhalManager-simple-symbolic";

/// A finished scan of a particular load order.
pub struct Analysis {
    /// The load order revision this describes. Bumped in place for changes
    /// that don't affect the scan, like enabling the merged patch.
    pub revision: Cell<u64>,
    /// Mod names by scanned position, as of the scan.
    pub names: Vec<String>,
    /// Descriptor ids by scanned position.
    pub ids: Vec<String>,
    /// Each scanned mod's folder.
    pub roots: Vec<Option<PathBuf>>,
    /// Each scanned mod's position in the full load order (the merged patch
    /// itself is not scanned, so these can differ).
    pub load_positions: Vec<usize>,
    pub report: Report,
    /// Object conflicts that need no AI to merge, by index, with why.
    pub simple: HashMap<usize, String>,
    /// For each scanned mod, the scanned mods it depends on.
    pub depends_on: Vec<BTreeSet<usize>>,
}

/// What the page needs from the window.
#[allow(clippy::type_complexity)]
pub struct MergeHooks {
    /// Whether the patch already holds a merge of `(category, key)`.
    pub is_merged: Box<dyn Fn(&str, &str) -> bool>,
    /// The patch's merge of `(category, key)`, if any.
    pub existing: Box<dyn Fn(&str, &str) -> Option<Merge>>,
    /// Saves a merged object to the patch: category, key, script and the
    /// fingerprints of its sources.
    pub save_object: Box<dyn Fn(&str, &str, &str, Vec<Print>) -> Result<(), String>>,
    /// Saves a whole file to the patch at `path`.
    pub save_file: Box<dyn Fn(&str, &[u8], Vec<Print>) -> Result<(), String>>,
    /// Called after anything was saved to the patch.
    pub patch_saved: Box<dyn Fn()>,
    /// Whether the patch already holds a merged copy of a file.
    pub is_file_merged: Box<dyn Fn(&str) -> bool>,
    /// Opens the file merge dialog for a file override; the callback runs
    /// once it's saved.
    pub open_file: Box<dyn Fn(usize, Box<dyn Fn()>)>,
    /// Opens the AI merge of several object conflicts in one folder.
    pub merge_folder: Box<dyn Fn(Vec<usize>)>,
    /// Opens the AI merge of several overridden files in one folder.
    pub merge_files: Box<dyn Fn(Vec<usize>)>,
    /// Opens the fix dialog for a file that failed to parse.
    pub open_fix: Box<dyn Fn(usize, Box<dyn Fn()>)>,
    pub is_ignored: Box<dyn Fn(&str) -> bool>,
    pub set_ignored: Box<dyn Fn(&str, bool)>,
    /// The ignore rules, as of when the page was built.
    pub rules: Rules,
    /// Whether conflicts between mods and their dependencies are hidden.
    pub hide_dependencies: bool,
    /// Opens the ignore rules editor.
    pub edit_rules: Box<dyn Fn()>,
    /// Whether ignored conflicts are shown (dimmed) instead of hidden.
    pub show_ignored: bool,
    pub toggle_show_ignored: Box<dyn Fn()>,
    /// The scanned position of the mod whose conflicts alone are shown.
    pub filter: Option<usize>,
    pub set_filter: Box<dyn Fn(Option<usize>)>,
    /// Merges and fixes whose sources changed since they were saved.
    pub stale: RefCell<Vec<Stale>>,
    /// Checks the patch for out-of-date merges again, after a save.
    pub reload_stale: Box<dyn Fn() -> Vec<Stale>>,
    /// Removes an out-of-date merge or fix from the patch.
    pub remove_merge: Box<dyn Fn(&Stale)>,
    /// Starts tracking an untracked merge from its mods' current files.
    pub keep_merge: Box<dyn Fn(&Stale)>,
    /// Rebuilds the whole page, e.g. after a folder was ignored.
    pub rerender: Box<dyn Fn()>,
    pub game_dir: Option<PathBuf>,
    /// The AI provider, if one is set up.
    pub provider: Box<dyn Fn() -> Option<Provider>>,
    pub open_preferences: Box<dyn Fn()>,
    /// Opens Preferences where the merge tool is chosen.
    pub open_merge_tool_settings: Box<dyn Fn()>,
    /// The external merge tool's command; empty for none.
    pub merge_tool: Box<dyn Fn() -> String>,
    pub toast: Box<dyn Fn(&str)>,
    /// What the page remembers between rebuilds.
    pub session: Rc<RefCell<Session>>,
}

impl MergeHooks {
    fn stale_object(&self, category: &str, key: &str) -> Option<Vec<String>> {
        self.stale.borrow().iter().find(|s| !s.untracked && s.object.as_ref().is_some_and(|(c, k)| c == category && k == key)).map(|s| s.reasons.clone())
    }

    fn stale_file(&self, path: &str) -> Option<Vec<String>> {
        self.stale.borrow().iter().find(|s| !s.untracked && s.object.is_none() && s.path.eq_ignore_ascii_case(path)).map(|s| s.reasons.clone())
    }

    /// Notes that something was saved to the patch.
    fn saved(&self) {
        (self.patch_saved)();
        self.stale.replace((self.reload_stale)());
    }
}

/// What the Conflicts page keeps while it is rebuilt: the selection, the
/// search, and how far each object's merge has got. Cleared on a rescan,
/// except for the selection and the view options.
#[derive(Default)]
pub struct Session {
    /// Key of the selected item (see [`Page::key_of`]).
    pub selected: Option<String>,
    /// Merges in progress, by object key.
    pub steps: HashMap<String, Steps>,
    pub search: String,
    /// Whether cards mark each version's changes from the base game rather
    /// than the differences between them.
    pub compare_base: bool,
    pub ignore_whitespace: bool,
    /// Whether comparisons fold runs of unchanged lines.
    pub fold_unchanged: bool,
}

impl Session {
    /// Forgets merges in progress, whose versions a rescan may have changed.
    pub fn rescanned(&mut self) {
        self.steps.clear();
    }
}

// Keys for ignored conflicts, stored in the settings.
pub fn file_key(path: &str) -> String {
    format!("file:{}", path.to_lowercase())
}
pub fn object_key(category: &str, key: &str) -> String {
    format!("object:{category}:{key}")
}
pub fn files_folder_key(folder: &str) -> String {
    format!("files-in:{}", folder.to_lowercase())
}
pub fn objects_folder_key(category: &str) -> String {
    format!("objects-in:{category}")
}

pub fn file_folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, f)| f)
}

/// What hides conflicts: keys ignored one at a time or by folder, and
/// the ignore rules, which need the mod names by scanned position.
pub struct Ignores<'a> {
    pub keys: &'a dyn Fn(&str) -> bool,
    pub rules: &'a Rules,
    pub names: &'a [String],
    /// Each mod's dependencies, when conflicts with them are hidden.
    pub dependencies: Option<&'a [BTreeSet<usize>]>,
}

/// Whether a conflict is ignored, directly, through its folder, or by a rule.
pub fn file_ignored(ignores: &Ignores, c: &crate::merge::conflicts::FileConflict) -> bool {
    (ignores.keys)(&file_key(&c.path))
        || (ignores.keys)(&files_folder_key(file_folder(&c.path)))
        || ignores.rules.hiding_file(c, ignores.names).is_some()
        || ignores.dependencies.is_some_and(|d| c.builds_on_dependencies(d))
}
pub fn object_ignored(ignores: &Ignores, c: &crate::merge::conflicts::ObjectConflict) -> bool {
    (ignores.keys)(&object_key(&c.category, &c.key))
        || (ignores.keys)(&objects_folder_key(&c.category))
        || ignores.rules.hiding_object(c, ignores.names).is_some()
        || ignores.dependencies.is_some_and(|d| c.builds_on_dependencies(d))
}

impl Analysis {
    pub fn name(&self, position: usize) -> &str {
        &self.names[position]
    }

    /// "#3 · file", for a card's subtitle.
    fn origin(&self, position: usize, file: &str) -> String {
        format!("#{} · {file}", self.load_positions[position] + 1)
    }
}

pub fn scanning() -> gtk::Widget {
    let page = adw::StatusPage::builder().title("Scanning Mods…").description("Reading every enabled mod's files").vexpand(true).build();
    page.set_paintable(Some(&adw::SpinnerPaintable::new(Some(&page))));
    page.upcast()
}

/// Something the sidebar lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    Object(usize),
    File(usize),
    /// Files the game will misread, and mods that couldn't be scanned.
    Problems,
    /// Merges whose mods changed since.
    Outdated,
    /// Merges saved before fingerprints existed.
    Untracked,
}

/// A heading in the sidebar and the items under it.
struct Group {
    title: String,
    /// The key that ignores the whole folder.
    folder_key: Option<String>,
    /// Whether "Merge All with AI" is offered for it.
    merge_all: bool,
    items: Vec<Item>,
}

/// How an item stands, for the dot before it in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Open,
    /// Open, but one version keeps every change, so no AI is needed.
    Simple,
    InProgress,
    Saved,
    Outdated,
    Ignored,
    Attention,
}

pub struct Page {
    pub analysis: Rc<Analysis>,
    pub hooks: Rc<MergeHooks>,
    split: adw::NavigationSplitView,
    list: gtk::ListBox,
    content: adw::Bin,
    summary: gtk::Label,
    details: gtk::Label,
    progress: gtk::ProgressBar,
    groups: Vec<Group>,
    /// The item of each list row, by row index; `None` for headings.
    rows: RefCell<Vec<Option<Item>>>,
    /// Each listed item's status holder and subtitle, to update in place.
    parts: RefCell<HashMap<Item, (gtk::Box, gtk::Label, gtk::ListBoxRow)>>,
    current: Cell<Option<Item>>,
    /// Keeps the selected item's view alive; its widgets hold only weak
    /// references to it.
    view: RefCell<Option<Rc<dyn Any>>>,
    /// The conflicts listed (not ignored, or shown although ignored), for
    /// the summary.
    files: Vec<usize>,
    objects: Vec<usize>,
    involved: usize,
    n_ignored: usize,
}

/// Builds the page. The window keeps the returned value for as long as it
/// shows the widget: the widgets only hold weak references to the page.
pub fn build(analysis: &Rc<Analysis>, rescan: gtk::Button, hooks: Rc<MergeHooks>) -> (gtk::Widget, Rc<dyn Any>) {
    diff_view::install_css();
    let report = &analysis.report;
    let ignored = |k: &str| (hooks.is_ignored)(k);
    let ignored = Ignores { keys: &ignored, rules: &hooks.rules, names: &analysis.names, dependencies: hooks.hide_dependencies.then_some(&analysis.depends_on[..]) };
    let filter = hooks.filter;
    let files: Vec<usize> = (0..report.files.len())
        .filter(|&i| hooks.show_ignored || !file_ignored(&ignored, &report.files[i]))
        .filter(|&i| filter.is_none_or(|p| report.files[i].mods.contains(&p)))
        .collect();
    let objects: Vec<usize> = (0..report.objects.len())
        .filter(|&i| hooks.show_ignored || !object_ignored(&ignored, &report.objects[i]))
        .filter(|&i| filter.is_none_or(|p| report.objects[i].definitions.iter().any(|d| d.mod_position == p)))
        .collect();
    let n_ignored = report.files.iter().filter(|c| file_ignored(&ignored, c)).count() + report.objects.iter().filter(|c| object_ignored(&ignored, c)).count();
    let involved: BTreeSet<usize> = files
        .iter()
        .flat_map(|&i| report.files[i].mods.iter().copied())
        .chain(objects.iter().flat_map(|&i| report.objects[i].definitions.iter().map(|d| d.mod_position)))
        .collect();
    let has_errors = report.errors.iter().any(|e| filter.is_none_or(|p| e.mod_position == p)) || report.skipped.iter().any(|(p, _)| filter.is_none_or(|f| *p == f));

    let mut groups = Vec::new();
    let mut attention = Vec::new();
    if hooks.stale.borrow().iter().any(|s| !s.untracked) {
        attention.push(Item::Outdated);
    }
    if hooks.stale.borrow().iter().any(|s| s.untracked) {
        attention.push(Item::Untracked);
    }
    if has_errors {
        attention.push(Item::Problems);
    }
    if !attention.is_empty() {
        groups.push(Group { title: "needs attention".into(), folder_key: None, merge_all: false, items: attention });
    }
    let mut by_category: BTreeMap<&str, Vec<Item>> = BTreeMap::new();
    for &i in &objects {
        by_category.entry(&report.objects[i].category).or_default().push(Item::Object(i));
    }
    for (category, items) in by_category {
        groups.push(Group { title: category.to_owned(), folder_key: Some(objects_folder_key(category)), merge_all: true, items });
    }
    let mut by_folder: BTreeMap<&str, Vec<Item>> = BTreeMap::new();
    for &i in &files {
        by_folder.entry(file_folder(&report.files[i].path)).or_default().push(Item::File(i));
    }
    for (folder, items) in by_folder {
        let title = if folder.is_empty() { "overridden files".to_owned() } else { format!("overridden files · {folder}") };
        let merge_all = items.iter().any(|i| matches!(i, Item::File(f) if file_merge::mergeable(&report.files[*f].path)));
        groups.push(Group { title, folder_key: Some(files_folder_key(folder)), merge_all, items });
    }

    // The sidebar.
    let summary = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["title-4"]).build();
    let details = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build();
    let progress = gtk::ProgressBar::builder().margin_top(6).build();
    let top = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(12).margin_start(12).margin_end(12).margin_bottom(6).build();
    top.append(&summary);
    top.append(&details);
    top.append(&progress);
    if let Some(dropdown) = mod_filter(analysis, &hooks, &involved) {
        dropdown.set_margin_top(6);
        top.append(&dropdown);
    }
    let search = gtk::SearchEntry::builder().placeholder_text("Search conflicts").text(hooks.session.borrow().search.as_str()).build();
    top.append(&search);

    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).css_classes(["navigation-sidebar", "conflict-list"]).build();
    let list_scroll = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();

    let footer = gtk::Box::builder().spacing(6).margin_top(6).margin_bottom(6).margin_start(12).margin_end(6).build();
    let show_ignored = gtk::CheckButton::builder().label("Show Ignored").active(hooks.show_ignored).hexpand(true).build();
    {
        let hooks = hooks.clone();
        show_ignored.connect_toggled(move |_| (hooks.toggle_show_ignored)());
    }
    rescan.add_css_class("flat");
    let rules = gtk::Button::builder().label("Rules…").tooltip_text("Ignore conflicts by path, like gfx/** or localisation/*").css_classes(["flat"]).build();
    {
        let hooks = hooks.clone();
        rules.connect_clicked(move |_| (hooks.edit_rules)());
    }
    footer.append(&show_ignored);
    footer.append(&rules);
    footer.append(&rescan);

    // A rounded card inset from the window's edges, like the code cards.
    let sidebar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["conflict-sidebar"])
        .overflow(gtk::Overflow::Hidden)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .build();
    sidebar.append(&top);
    sidebar.append(&list_scroll);
    sidebar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    sidebar.append(&footer);

    let content = adw::Bin::builder().hexpand(true).vexpand(true).build();
    let split = adw::NavigationSplitView::builder()
        .sidebar(&adw::NavigationPage::builder().title("Conflicts").child(&sidebar).build())
        // Scrolls sideways when the window is too narrow for a conflict's
        // toolbar and cards, instead of asking for more width than there is.
        .content(&adw::NavigationPage::builder().title("Conflict").child(&gtk::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk::PolicyType::Automatic).vscrollbar_policy(gtk::PolicyType::Never).build()).build())
        .min_sidebar_width(260.0)
        .max_sidebar_width(380.0)
        .sidebar_width_fraction(0.24)
        .css_classes(["conflict-split"])
        .build();

    let page = Rc::new(Page {
        analysis: analysis.clone(),
        hooks: hooks.clone(),
        split: split.clone(),
        list: list.clone(),
        content,
        summary,
        details,
        progress,
        groups,
        rows: RefCell::default(),
        parts: RefCell::default(),
        current: Cell::new(None),
        view: RefCell::new(None),
        files,
        objects,
        involved: involved.len(),
        n_ignored,
    });
    page.update_summary();

    if page.groups.is_empty() {
        let (title, description) = match filter {
            Some(p) => (format!("{} Has No Conflicts", analysis.name(p)), "Choose All Mods to see the others".to_owned()),
            None if n_ignored > 0 => ("No Conflicts Left".to_owned(), format!("{} ignored", plural(n_ignored, "conflict is", "conflicts are"))),
            None => ("No Conflicts".to_owned(), "None of the enabled mods change the same files or objects".to_owned()),
        };
        page.content.set_child(Some(&adw::StatusPage::builder().icon_name("emblem-ok-symbolic").title(title).description(description).build()));
    }

    {
        let weak = Rc::downgrade(&page);
        list.connect_row_selected(move |_, row| {
            let (Some(page), Some(row)) = (weak.upgrade(), row) else { return };
            let item = page.rows.borrow().get(row.index() as usize).copied().flatten();
            if let Some(item) = item
                && page.current.get() != Some(item)
            {
                page.show(item);
            }
        });
    }
    {
        // Clicking the selected row again, e.g. after going back on a
        // narrow window, shows it again.
        let weak = Rc::downgrade(&page);
        list.connect_row_activated(move |_, row| {
            let Some(page) = weak.upgrade() else { return };
            let item = page.rows.borrow().get(row.index() as usize).copied().flatten();
            if item.is_some() && item == page.current.get() {
                page.split.set_show_content(true);
            }
        });
    }
    {
        let weak = Rc::downgrade(&page);
        search.connect_search_changed(move |entry| {
            if let Some(page) = weak.upgrade() {
                page.hooks.session.borrow_mut().search = entry.text().to_string();
                page.fill();
            }
        });
    }
    page.fill();
    page.select_initial();

    // Collapses to one pane at a time on narrow windows.
    let bin = adw::BreakpointBin::builder().width_request(360).height_request(300).child(&split).vexpand(true).build();
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 760sp").unwrap());
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    bin.add_breakpoint(breakpoint);
    (bin.upcast(), page)
}

/// The "All Mods" dropdown, when several mods have conflicts.
fn mod_filter(analysis: &Rc<Analysis>, hooks: &Rc<MergeHooks>, involved: &BTreeSet<usize>) -> Option<gtk::DropDown> {
    let filter = hooks.filter;
    // Mods with conflicts, in load order, plus the filtered one if it has none.
    let choices: Vec<usize> = (0..analysis.names.len()).filter(|p| involved.contains(p) || filter == Some(*p)).collect();
    if choices.len() <= 1 && filter.is_none() {
        return None;
    }
    let mut labels = vec!["All Mods"];
    labels.extend(choices.iter().map(|&p| analysis.name(p)));
    let dropdown = gtk::DropDown::from_strings(&labels);
    dropdown.set_enable_search(true);
    dropdown.set_tooltip_text(Some("Show only the conflicts of one mod"));
    dropdown.set_selected(filter.and_then(|f| choices.iter().position(|&p| p == f)).map_or(0, |i| i as u32 + 1));
    // Search by the shown text.
    dropdown.set_expression(Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")));
    let hooks = hooks.clone();
    dropdown.connect_selected_notify(move |d| {
        let position = (d.selected() as usize).checked_sub(1).and_then(|i| choices.get(i).copied());
        (hooks.set_filter)(position);
    });
    Some(dropdown)
}

impl Page {
    /// A stable key for `item`, to keep it selected across rebuilds.
    fn key_of(&self, item: Item) -> String {
        match item {
            Item::Object(i) => {
                let c = &self.analysis.report.objects[i];
                object_key(&c.category, &c.key)
            }
            Item::File(i) => file_key(&self.analysis.report.files[i].path),
            Item::Problems => "problems".into(),
            Item::Outdated => "outdated".into(),
            Item::Untracked => "untracked".into(),
        }
    }

    fn title_of(&self, item: Item) -> String {
        let n_stale = |untracked: bool| self.hooks.stale.borrow().iter().filter(|s| s.untracked == untracked).count();
        match item {
            Item::Object(i) => self.analysis.report.objects[i].key.clone(),
            Item::File(i) => file_name(&self.analysis.report.files[i].path).to_owned(),
            Item::Problems => "Problems".into(),
            Item::Outdated => plural(n_stale(false), "outdated merge", "outdated merges"),
            Item::Untracked => plural(n_stale(true), "merge to check", "merges to check"),
        }
    }

    fn matches(&self, item: Item, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let text = match item {
            Item::Object(i) => self.analysis.report.objects[i].key.clone(),
            Item::File(i) => self.analysis.report.files[i].path.clone(),
            _ => self.title_of(item),
        };
        text.to_lowercase().contains(query)
    }

    fn is_ignored(&self, item: Item) -> bool {
        let ignored = |k: &str| (self.hooks.is_ignored)(k);
        let ignored = Ignores { keys: &ignored, rules: &self.hooks.rules, names: &self.analysis.names, dependencies: self.hooks.hide_dependencies.then_some(&self.analysis.depends_on[..]) };
        match item {
            Item::Object(i) => object_ignored(&ignored, &self.analysis.report.objects[i]),
            Item::File(i) => file_ignored(&ignored, &self.analysis.report.files[i]),
            _ => false,
        }
    }

    /// The line of the ignore rule hiding `item`, if one does and it isn't
    /// also ignored by hand.
    fn hidden_by_rule(&self, item: Item) -> Option<usize> {
        let keys = |k: &str| (self.hooks.is_ignored)(k);
        let none = Rules::default();
        let by_hand = Ignores { keys: &keys, rules: &none, names: &self.analysis.names, dependencies: None };
        let report = &self.analysis.report;
        match item {
            Item::Object(i) if !object_ignored(&by_hand, &report.objects[i]) => self.hooks.rules.hiding_object(&report.objects[i], &self.analysis.names),
            Item::File(i) if !file_ignored(&by_hand, &report.files[i]) => self.hooks.rules.hiding_file(&report.files[i], &self.analysis.names),
            _ => None,
        }
    }

    /// Whether `item` is hidden only because its later mods depend on the
    /// earlier ones.
    fn hidden_by_dependencies(&self, item: Item) -> bool {
        if !self.hooks.hide_dependencies || self.hidden_by_rule(item).is_some() {
            return false;
        }
        let keys = |k: &str| (self.hooks.is_ignored)(k);
        let none = Rules::default();
        let by_hand = Ignores { keys: &keys, rules: &none, names: &self.analysis.names, dependencies: None };
        let (report, depends) = (&self.analysis.report, &self.analysis.depends_on);
        match item {
            Item::Object(i) => !object_ignored(&by_hand, &report.objects[i]) && report.objects[i].builds_on_dependencies(depends),
            Item::File(i) => !file_ignored(&by_hand, &report.files[i]) && report.files[i].builds_on_dependencies(depends),
            _ => false,
        }
    }

    /// "ignored", or why it's hidden when that isn't by hand.
    fn ignored_label(&self, item: Item) -> String {
        if let Some(line) = self.hidden_by_rule(item) {
            return format!("ignored by rule {}", line + 1);
        }
        if self.hidden_by_dependencies(item) {
            return "a patch of its dependency".to_owned();
        }
        "ignored".to_owned()
    }

    fn is_saved(&self, item: Item) -> bool {
        match item {
            Item::Object(i) => {
                let c = &self.analysis.report.objects[i];
                (self.hooks.is_merged)(&c.category, &c.key) && self.hooks.stale_object(&c.category, &c.key).is_none()
            }
            Item::File(i) => {
                let path = &self.analysis.report.files[i].path;
                (self.hooks.is_file_merged)(path) && self.hooks.stale_file(path).is_none()
            }
            _ => false,
        }
    }

    /// The item's status and the subtitle under its name.
    fn status_of(&self, item: Item) -> (Status, String) {
        let analysis = &self.analysis;
        match item {
            Item::Object(i) => {
                let c = &analysis.report.objects[i];
                let mods: BTreeSet<usize> = c.definitions.iter().map(|d| d.mod_position).collect();
                let mods = plural(mods.len(), "mod", "mods");
                if self.is_ignored(item) {
                    return (Status::Ignored, format!("{mods} · {}", self.ignored_label(item)));
                }
                if self.hooks.stale_object(&c.category, &c.key).is_some() {
                    return (Status::Outdated, format!("{mods} · merge out of date"));
                }
                let session = self.hooks.session.borrow();
                if let Some((step, total)) = session.steps.get(&object_key(&c.category, &c.key)).and_then(Steps::progress) {
                    return if step > total { (Status::InProgress, format!("{mods} · ready to save")) } else { (Status::InProgress, format!("{mods} · step {step} of {total}")) };
                }
                if (self.hooks.is_merged)(&c.category, &c.key) {
                    return (Status::Saved, format!("{mods} · saved to patch"));
                }
                match analysis.simple.contains_key(&i) {
                    true => (Status::Simple, format!("{mods} · simple, no AI needed")),
                    false => match c.winner() {
                        Some((w, _)) => (Status::Open, format!("{mods} · {} wins", analysis.name(c.definitions[w].mod_position))),
                        None => (Status::Open, mods),
                    },
                }
            }
            Item::File(i) => {
                let c = &analysis.report.files[i];
                let mods = plural(c.mods.len(), "mod", "mods");
                if self.is_ignored(item) {
                    (Status::Ignored, format!("{mods} · {}", self.ignored_label(item)))
                } else if self.hooks.stale_file(&c.path).is_some() {
                    (Status::Outdated, format!("{mods} · merge out of date"))
                } else if (self.hooks.is_file_merged)(&c.path) {
                    (Status::Saved, format!("{mods} · saved to patch"))
                } else {
                    (Status::Open, format!("{mods} · {} wins", analysis.name(*c.mods.last().unwrap())))
                }
            }
            Item::Problems => {
                let filter = self.hooks.filter;
                let errors = analysis.report.errors.iter().filter(|e| filter.is_none_or(|p| e.mod_position == p)).count();
                let skipped = analysis.report.skipped.iter().filter(|(p, _)| filter.is_none_or(|f| *p == f)).count();
                let mut parts = Vec::new();
                if errors > 0 {
                    parts.push(plural(errors, "file with errors", "files with errors"));
                }
                if skipped > 0 {
                    parts.push(plural(skipped, "mod not scanned", "mods not scanned"));
                }
                (Status::Attention, parts.join(" · "))
            }
            Item::Outdated => (Status::Attention, outdated_summary(&self.hooks.stale.borrow()).into()),
            Item::Untracked => (Status::Attention, "Saved by an older version".into()),
        }
    }

    /// Rebuilds the sidebar list for the current search.
    fn fill(self: &Rc<Self>) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        let query = self.hooks.session.borrow().search.to_lowercase();
        let mut rows = Vec::new();
        let mut parts = HashMap::new();
        for group in &self.groups {
            let items: Vec<Item> = group.items.iter().copied().filter(|&i| self.matches(i, &query)).collect();
            if items.is_empty() {
                continue;
            }
            self.list.append(&self.group_heading(group, items.len()));
            rows.push(None);
            for &item in items.iter().take(ROWS_PER_GROUP) {
                let (row, status, subtitle) = self.item_row(item);
                self.list.append(&row);
                rows.push(Some(item));
                parts.insert(item, (status, subtitle, row));
            }
            if items.len() > ROWS_PER_GROUP {
                let more = gtk::ListBoxRow::builder().selectable(false).activatable(false).build();
                more.set_child(Some(&gtk::Label::builder().label(format!("…and {} more. Search to find them.", items.len() - ROWS_PER_GROUP)).xalign(0.0).css_classes(["dim-label", "caption"]).margin_start(12).build()));
                self.list.append(&more);
                rows.push(None);
            }
        }
        if rows.is_empty() {
            let none = gtk::ListBoxRow::builder().selectable(false).activatable(false).build();
            none.set_child(Some(&gtk::Label::builder().label("No matches").css_classes(["dim-label"]).margin_top(24).build()));
            self.list.append(&none);
            rows.push(None);
        }
        self.rows.replace(rows);
        self.parts.replace(parts);
        for item in self.parts.borrow().keys() {
            self.update_status(*item);
        }
        // Keep the selection on the current item, if it's still listed.
        if let Some(current) = self.current.get()
            && let Some((_, _, row)) = self.parts.borrow().get(&current)
        {
            self.list.select_row(Some(row));
        }
    }

    fn group_heading(self: &Rc<Self>, group: &Group, count: usize) -> gtk::ListBoxRow {
        let heading = gtk::Box::builder().spacing(6).margin_start(6).build();
        heading.append(&gtk::Label::builder().label(&group.title).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::Start).tooltip_text(&group.title).css_classes(["dim-label"]).build());
        heading.append(&gtk::Label::builder().label(count.to_string()).css_classes(["dim-label", "numeric"]).build());
        if let Some(folder_key) = &group.folder_key {
            let menu = gio::Menu::new();
            let actions = gio::SimpleActionGroup::new();
            if group.merge_all {
                menu.append(Some("Merge All with AI…"), Some("folder.merge-all"));
                let action = gio::SimpleAction::new("merge-all", None);
                let (weak, items) = (Rc::downgrade(self), group.items.clone());
                action.connect_activate(move |_, _| {
                    if let Some(page) = weak.upgrade() {
                        let shown: Vec<Item> = items.iter().copied().filter(|&i| !page.is_ignored(i)).collect();
                        let objects: Vec<usize> = shown.iter().filter_map(|i| if let Item::Object(i) = i { Some(*i) } else { None }).collect();
                        let files: Vec<usize> = shown.iter().filter_map(|i| if let Item::File(i) = i { Some(*i) } else { None }).collect();
                        if !objects.is_empty() {
                            (page.hooks.merge_folder)(objects);
                        } else if !files.is_empty() {
                            (page.hooks.merge_files)(files);
                        }
                    }
                });
                actions.add_action(&action);
            }
            let ignored = (self.hooks.is_ignored)(folder_key);
            menu.append(Some(if ignored { "Stop Ignoring Folder" } else { "Ignore Folder" }), Some("folder.ignore"));
            let action = gio::SimpleAction::new("ignore", None);
            let (weak, folder_key) = (Rc::downgrade(self), folder_key.clone());
            action.connect_activate(move |_, _| {
                if let Some(page) = weak.upgrade() {
                    (page.hooks.set_ignored)(&folder_key, !ignored);
                    (page.hooks.rerender)();
                }
            });
            actions.add_action(&action);
            let button = gtk::MenuButton::builder().icon_name("view-more-symbolic").menu_model(&menu).tooltip_text("Folder actions").css_classes(["flat", "circular"]).valign(gtk::Align::Center).build();
            button.insert_action_group("folder", Some(&actions));
            heading.append(&button);
        }
        gtk::ListBoxRow::builder().child(&heading).selectable(false).activatable(false).css_classes(["folder"]).build()
    }

    fn item_row(&self, item: Item) -> (gtk::ListBoxRow, gtk::Box, gtk::Label) {
        let status = gtk::Box::builder().width_request(18).valign(gtk::Align::Center).halign(gtk::Align::Center).build();
        let title = self.title_of(item);
        let title_label = gtk::Label::builder().label(&title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
        let subtitle = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["dim-label", "caption"]).build();
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).build();
        text.append(&title_label);
        text.append(&subtitle);
        let b = gtk::Box::builder().spacing(10).build();
        b.append(&status);
        b.append(&text);
        let row = gtk::ListBoxRow::builder().child(&b).css_classes(["conflict"]).build();
        row.set_tooltip_text(Some(&match item {
            Item::File(i) => self.analysis.report.files[i].path.clone(),
            Item::Object(i) => {
                let c = &self.analysis.report.objects[i];
                c.definitions.iter().map(|d| format!("{}: {}", self.analysis.name(d.mod_position), d.file)).collect::<Vec<_>>().join("\n")
            }
            _ => title,
        }));
        (row, status, subtitle)
    }

    /// Refreshes the dot and subtitle of `item`'s row, and the summary.
    pub fn update_status(&self, item: Item) {
        let parts = self.parts.borrow();
        let Some((holder, subtitle, row)) = parts.get(&item) else { return };
        let (status, text) = self.status_of(item);
        subtitle.set_label(&text);
        while let Some(child) = holder.first_child() {
            holder.remove(&child);
        }
        let icon = |name: &str, class: &str, tooltip: &str| gtk::Image::builder().icon_name(name).css_classes([class]).tooltip_text(tooltip).build().upcast::<gtk::Widget>();
        let widget = match status {
            Status::Open => gtk::Box::builder().css_classes(["status-dot"]).tooltip_text("Not resolved yet").build().upcast(),
            Status::Simple => {
                let reason = match item {
                    Item::Object(i) => self.analysis.simple.get(&i).cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                icon(SIMPLE_ICON, "accent", &format!("Simple merge, no AI needed: {reason}"))
            }
            Status::InProgress => gtk::Box::builder().css_classes(["status-dot", "progress"]).tooltip_text("Being merged").build().upcast(),
            Status::Saved => icon("emblem-ok-symbolic", "success", "Saved to the merged patch"),
            Status::Outdated => icon("dialog-warning-symbolic", "warning", "Its mods or the base game changed since this was merged"),
            Status::Ignored => icon("view-conceal-symbolic", "dim-label", "Ignored"),
            Status::Attention => icon("dialog-warning-symbolic", "warning", ""),
        };
        holder.append(&widget);
        if status == Status::Ignored {
            row.add_css_class("dim-label");
        } else {
            row.remove_css_class("dim-label");
        }
        drop(parts);
        self.update_summary();
    }

    fn update_summary(&self) {
        let listed: Vec<Item> = self.objects.iter().map(|&i| Item::Object(i)).chain(self.files.iter().map(|&i| Item::File(i))).filter(|&i| !self.is_ignored(i)).collect();
        let saved = listed.iter().filter(|&&i| self.is_saved(i)).count();
        self.summary.set_label(&format!("{} · {saved} saved", plural(listed.len(), "conflict", "conflicts")));
        let mut details = match self.hooks.filter {
            Some(p) => format!("Only {}'s conflicts", self.analysis.name(p)),
            None => format!("{} of {} mods involved", self.involved, self.analysis.names.len()),
        };
        if self.n_ignored > 0 {
            details.push_str(&format!(" · {} ignored", self.n_ignored));
        }
        self.details.set_label(&details);
        self.progress.set_fraction(if listed.is_empty() { 0.0 } else { saved as f64 / listed.len() as f64 });
        self.progress.set_visible(!listed.is_empty());
    }

    /// Selects the item that was selected before the rebuild, or the first
    /// conflict.
    fn select_initial(self: &Rc<Self>) {
        let selected = self.hooks.session.borrow().selected.clone();
        let items: Vec<Item> = self.rows.borrow().iter().flatten().copied().collect();
        let item = selected
            .and_then(|key| items.iter().copied().find(|&i| self.key_of(i) == key))
            .or_else(|| items.iter().copied().find(|i| matches!(i, Item::Object(_) | Item::File(_)) && !self.is_saved(*i)))
            .or_else(|| items.first().copied());
        if let Some(item) = item {
            self.show(item);
        }
    }

    /// Shows `item` in the main pane and selects its row.
    pub fn show(self: &Rc<Self>, item: Item) {
        self.current.set(Some(item));
        self.hooks.session.borrow_mut().selected = Some(self.key_of(item));
        if let Some((_, _, row)) = self.parts.borrow().get(&item) {
            self.list.select_row(Some(row));
        }
        let (widget, view): (gtk::Widget, Option<Rc<dyn Any>>) = match item {
            Item::Object(i) => {
                let (w, v) = object::view(self, i);
                (w, Some(v))
            }
            Item::File(i) => {
                let (w, v) = file::view(self, i);
                (w, Some(v))
            }
            Item::Problems => (self.problems_view(), None),
            Item::Outdated => (self.stale_view(false), None),
            Item::Untracked => (self.stale_view(true), None),
        };
        self.view.replace(view);
        self.content.set_child(Some(&widget));
        self.split.set_show_content(true);
    }

    /// Moves on from `item` to the next conflict still to resolve.
    pub fn advance(self: &Rc<Self>, item: Item) {
        let rows: Vec<Item> = self.rows.borrow().iter().flatten().copied().collect();
        let start = rows.iter().position(|&i| i == item).map_or(0, |p| p + 1);
        let next = rows[start..].iter().chain(&rows[..start]).copied().find(|&i| i != item && matches!(i, Item::Object(_) | Item::File(_)) && !self.is_saved(i) && !self.is_ignored(i));
        match next {
            Some(next) => self.show(next),
            None => self.show(item),
        }
    }

    /// Ignores `item`, or stops ignoring it.
    pub fn toggle_ignored(self: &Rc<Self>, item: Item) {
        if self.hidden_by_rule(item).is_some() || self.hidden_by_dependencies(item) {
            return (self.hooks.edit_rules)();
        }
        let key = self.key_of(item);
        let ignore = !self.is_ignored(item);
        (self.hooks.set_ignored)(&key, ignore);
        self.update_status(item);
        if ignore && !self.hooks.show_ignored {
            self.advance(item);
        } else {
            self.show(item);
        }
    }

    /// The top of the main pane: a back button when collapsed, the title,
    /// subtitle, a row for chips, and the actions on the right.
    pub fn detail_header(&self, title: &str, subtitle: &str) -> DetailHeader {
        let back = gtk::Button::builder().icon_name("go-previous-symbolic").tooltip_text("Back to the list").valign(gtk::Align::Center).css_classes(["flat"]).build();
        self.split.bind_property("collapsed", &back, "visible").sync_create().build();
        let split = self.split.downgrade();
        back.connect_clicked(move |_| {
            if let Some(split) = split.upgrade() {
                split.set_show_content(false);
            }
        });
        let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
        titles.append(&gtk::Label::builder().label(title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).tooltip_text(title).css_classes(["conflict-title"]).build());
        titles.append(&gtk::Label::builder().label(subtitle).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).tooltip_text(subtitle).css_classes(["dim-label"]).build());
        let actions = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
        let top = gtk::Box::builder().spacing(12).build();
        top.append(&back);
        top.append(&titles);
        top.append(&actions);
        let chips = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(4).row_spacing(4).max_children_per_line(64).homogeneous(false).build();
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        widget.append(&top);
        widget.append(&chips);
        DetailHeader { widget, chips, actions }
    }

    /// The eye button that ignores `item`.
    pub fn ignore_button(self: &Rc<Self>, item: Item) -> gtk::Button {
        let ignored = self.is_ignored(item);
        let tooltip = match self.hidden_by_rule(item) {
            Some(line) => format!("Ignored by rule {}; click to edit the ignore rules", line + 1),
            None if self.hidden_by_dependencies(item) => "Hidden because the later mods depend on the earlier ones, as patches do; click to change this".to_owned(),
            None if ignored => "Stop ignoring this conflict".to_owned(),
            None => "Ignore this conflict, so it no longer shows or counts".to_owned(),
        };
        let button = gtk::Button::builder()
            .icon_name(if ignored { "view-reveal-symbolic" } else { "view-conceal-symbolic" })
            .tooltip_text(tooltip)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.toggle_ignored(item);
            }
        });
        button
    }

    fn problems_view(self: &Rc<Self>) -> gtk::Widget {
        let analysis = &self.analysis;
        let report = &analysis.report;
        let hooks = &self.hooks;
        let filter = hooks.filter;
        let header = self.detail_header("Problems", "Files the game will misread, and mods that couldn't be scanned");
        let list = boxed_list();
        for (position, reason) in report.skipped.iter().filter(|(p, _)| filter.is_none_or(|f| *p == f)) {
            list.append(&row(analysis.name(*position), reason));
        }
        for (index, e) in report.errors.iter().enumerate().filter(|(_, e)| filter.is_none_or(|p| e.mod_position == p)) {
            let r = row(&e.file, &format!("{} · {}", analysis.name(e.mod_position), e.message));
            r.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
            // Parse errors read "line N: …"; others are read failures.
            if e.message.starts_with("line ") && analysis.roots[e.mod_position].is_some() {
                let stale = hooks.stale_file(&e.file);
                let (fixed, button) = merge_controls((hooks.is_file_merged)(&e.file), stale.as_deref());
                fixed.set_tooltip_text(Some("Fixed through the merged patch"));
                button.set_label(match (stale, fixed.is_visible()) {
                    (Some(_), _) => "Fix Again…",
                    (None, true) => "Edit Fix…",
                    (None, false) => "Fix…",
                });
                let (fixed_ref, button_ref) = (fixed.downgrade(), button.downgrade());
                let hooks = hooks.clone();
                button.connect_clicked(move |_| {
                    let (fixed, button) = (fixed_ref.clone(), button_ref.clone());
                    (hooks.open_fix)(
                        index,
                        Box::new(move || {
                            if let (Some(f), Some(b)) = (fixed.upgrade(), button.upgrade()) {
                                f.set_icon_name(Some("emblem-ok-symbolic"));
                                f.set_css_classes(&["success"]);
                                f.set_visible(true);
                                b.set_label("Edit Fix…");
                            }
                        }),
                    )
                });
                r.add_suffix(&fixed);
                r.add_suffix(&button);
            }
            list.append(&r);
        }
        let intro = gtk::Label::builder()
            .label("The game keeps reading after a missing or extra }, but everything after it gets nested inside the broken object and stops working. Fix… repairs the braces, and the fixed copy is saved through the merged patch, so the mod itself is never modified.")
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        scrolled_detail(&[header.widget.upcast(), intro.upcast(), list.upcast()])
    }

    fn stale_view(self: &Rc<Self>, untracked: bool) -> gtk::Widget {
        let all = self.hooks.stale.borrow().clone();
        let (title, subtitle, description) = if untracked {
            ("Merges to Check", "Saved by an older version", "These were merged by an older version of DrukhalManager, which didn't record what the mods looked like, so their updates can't be noticed. If one still looks right, keep it to track changes from now on; otherwise re-merge it from the list, or remove it.".to_owned())
        } else {
            let what = if all.iter().any(|s| !s.untracked && s.base_game) { "Mods or the base game" } else { "Mods" };
            ("Outdated Merges", outdated_summary(&all), format!("{what} changed since these were merged or fixed, so the patch may be undoing their updates. Re-merge them from the list, or remove them from the patch."))
        };
        let header = self.detail_header(title, subtitle);
        let intro = gtk::Label::builder().label(&description).wrap(true).xalign(0.0).css_classes(["dim-label"]).build();
        let stale: Vec<&Stale> = all.iter().filter(|s| s.untracked == untracked).collect();
        scrolled_detail(&[header.widget.upcast(), intro.upcast(), self.stale_list(&stale).upcast()])
    }

    /// Rows for merges that need attention, each with a way to deal with it.
    fn stale_list(self: &Rc<Self>, stale: &[&Stale]) -> gtk::ListBox {
        let hooks = &self.hooks;
        let list = boxed_list();
        for s in stale {
            let title = s.object.as_ref().map_or(file_name(&s.path), |(_, key)| key.as_str());
            let subtitle = match (&s.object, s.untracked) {
                // The heading already says why.
                (Some((category, _)), true) => category.clone(),
                (None, true) => s.path.clone(),
                (Some((category, _)), false) => format!("{category} · {}", s.reasons.join("; ")),
                (None, false) => format!("{} · {}", s.path, s.reasons.join("; ")),
            };
            let r = row(title, &subtitle);
            let icon = if s.untracked { "dialog-question-symbolic" } else { "dialog-warning-symbolic" };
            r.add_prefix(&gtk::Image::builder().icon_name(icon).css_classes(["warning"]).build());
            // Jump to the conflict, if it's still listed.
            let target = self.rows.borrow().iter().flatten().copied().find(|&i| match (i, &s.object) {
                (Item::Object(i), Some((c, k))) => &self.analysis.report.objects[i].category == c && &self.analysis.report.objects[i].key == k,
                (Item::File(i), None) => self.analysis.report.files[i].path.eq_ignore_ascii_case(&s.path),
                _ => false,
            });
            if let Some(target) = target {
                let open = gtk::Button::builder().label("Re-merge…").valign(gtk::Align::Center).css_classes(["flat"]).build();
                let weak = Rc::downgrade(self);
                open.connect_clicked(move |_| {
                    if let Some(page) = weak.upgrade() {
                        page.show(target);
                    }
                });
                r.add_suffix(&open);
            }
            if s.untracked {
                let keep = gtk::Button::builder().label("Keep As Is").tooltip_text("Track changes to its mods from their current versions").valign(gtk::Align::Center).css_classes(["flat"]).build();
                let (hooks, stale) = (hooks.clone(), (*s).clone());
                keep.connect_clicked(move |_| (hooks.keep_merge)(&stale));
                r.add_suffix(&keep);
            }
            let remove = gtk::Button::builder().label("Remove from Patch").valign(gtk::Align::Center).css_classes(["flat"]).build();
            let (hooks, stale, row_ref) = (hooks.clone(), (*s).clone(), r.downgrade());
            remove.connect_clicked(move |_| {
                (hooks.remove_merge)(&stale);
                if let Some(r) = row_ref.upgrade() {
                    r.set_visible(false);
                }
            });
            r.add_suffix(&remove);
            list.append(&r);
        }
        list
    }
}

pub struct DetailHeader {
    pub widget: gtk::Box,
    pub chips: gtk::FlowBox,
    pub actions: gtk::Box,
}

/// A chip naming one version: a coloured dot and the mod's name. `class`
/// is `done`, `current` or `later`.
fn chip(label: &str, color: Option<usize>, class: &str) -> gtk::Box {
    let b = gtk::Box::builder().spacing(6).css_classes(["chip", class]).valign(gtk::Align::Center).build();
    if let Some(color) = color {
        b.append(&gtk::Box::builder().css_classes(["mod-dot", &format!("mod-color-{}", color % 8)]).valign(gtk::Align::Center).build());
    }
    b.append(&gtk::Label::builder().label(label).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(32).build());
    b
}

fn arrow() -> gtk::Image {
    gtk::Image::builder().icon_name("go-next-symbolic").css_classes(["dim-label"]).build()
}

/// The row above a pair of cards: how many differences, buttons to step
/// through them, and room for more controls.
pub struct DiffBar {
    pub widget: gtk::Box,
    pub extra: gtk::Box,
}

fn diff_bar(pair: &Rc<Pair>) -> DiffBar {
    let count = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
    let prev = gtk::Button::builder().icon_name("go-up-symbolic").tooltip_text("Previous difference (Alt+Up)").css_classes(["flat"]).build();
    let next = gtk::Button::builder().icon_name("go-down-symbolic").tooltip_text("Next difference (Alt+Down)").css_classes(["flat"]).build();
    let nav = gtk::Box::builder().css_classes(["linked"]).build();
    nav.append(&prev);
    nav.append(&next);
    let extra = gtk::Box::builder().spacing(6).hexpand(true).halign(gtk::Align::End).build();
    let widget = gtk::Box::builder().spacing(6).build();
    widget.append(&nav);
    widget.append(&count);
    widget.append(&extra);
    {
        let (count, prev, next) = (count.downgrade(), prev.downgrade(), next.downgrade());
        pair.connect_change(move |n, current, base| {
            if let (Some(count), Some(prev), Some(next)) = (count.upgrade(), prev.upgrade(), next.upgrade()) {
                count.set_label(&match (n, current) {
                    _ if base => "Marking each version's changes from the base game".to_owned(),
                    (0, _) => "No differences".to_owned(),
                    (n, Some(i)) => format!("Difference {} of {n}", i + 1),
                    (n, None) => plural(n, "difference", "differences"),
                });
                prev.set_sensitive(n > 0);
                next.set_sensitive(n > 0);
            }
        });
    }
    for (button, forward) in [(&prev, false), (&next, true)] {
        let pair = Rc::downgrade(pair);
        button.connect_clicked(move |_| {
            if let Some(pair) = pair.upgrade() {
                pair.step(forward);
            }
        });
    }
    // Alt+Up and Alt+Down step through the differences.
    let keys = gtk::ShortcutController::new();
    keys.set_scope(gtk::ShortcutScope::Managed);
    for (key, forward) in [("<Alt>Up", false), ("<Alt>Down", true)] {
        let pair = Rc::downgrade(pair);
        keys.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(key),
            Some(gtk::CallbackAction::new(move |_, _| {
                if let Some(pair) = pair.upgrade() {
                    pair.step(forward);
                }
                glib::Propagation::Stop
            })),
        ));
    }
    pair.widget.add_controller(keys);
    DiffBar { widget, extra }
}

/// A toggle button for one of the session's view options.
fn option_toggle(hooks: &Rc<MergeHooks>, label: &str, tooltip: &str, get: fn(&Session) -> bool, set: fn(&mut Session, bool), on_change: impl Fn() + 'static) -> gtk::ToggleButton {
    let toggle = gtk::ToggleButton::builder().label(label).tooltip_text(tooltip).active(get(&hooks.session.borrow())).css_classes(["flat"]).build();
    let session = hooks.session.clone();
    toggle.connect_toggled(move |t| {
        set(&mut session.borrow_mut(), t.is_active());
        on_change();
    });
    toggle
}

/// A main pane that scrolls as a whole, for the lists of problems and
/// merges.
fn scrolled_detail(children: &[gtk::Widget]) -> gtk::Widget {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    for c in children {
        b.append(c);
    }
    let clamp = adw::Clamp::builder().maximum_size(1000).child(&b).margin_top(18).margin_bottom(24).margin_start(18).margin_end(18).build();
    gtk::ScrolledWindow::builder().child(&clamp).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build().upcast()
}

/// The merged-state icon and the merge button for a row. An out-of-date
/// merge shows a warning with the reasons instead of a check mark.
/// What changed since the outdated merges were saved.
fn outdated_summary(stale: &[Stale]) -> &'static str {
    let outdated = || stale.iter().filter(|s| !s.untracked);
    match (outdated().any(|s| s.base_game), outdated().all(|s| s.base_game)) {
        (true, true) => "The base game changed since they were merged",
        (true, false) => "Mods or the base game changed since they were merged",
        _ => "Mods changed since they were merged",
    }
}

fn merge_controls(is_merged: bool, stale: Option<&[String]>) -> (gtk::Image, gtk::Button) {
    let merged = gtk::Image::builder().icon_name("emblem-ok-symbolic").tooltip_text("Merged into the patch").css_classes(["success"]).visible(is_merged).build();
    let label = match (stale, is_merged) {
        (Some(reasons), _) => {
            merged.set_icon_name(Some("dialog-warning-symbolic"));
            merged.set_css_classes(&["warning"]);
            merged.set_tooltip_text(Some(&format!("Out of date: {}", reasons.join("; "))));
            merged.set_visible(true);
            "Re-merge…"
        }
        (None, true) => "Edit Merge…",
        (None, false) => "Merge…",
    };
    let button = gtk::Button::builder().label(label).valign(gtk::Align::Center).css_classes(["flat"]).build();
    (merged, button)
}

fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder().title(title).subtitle(subtitle).use_markup(false).subtitle_lines(2).build()
}

fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build()
}

/// A note above the cards, e.g. the AI's notes or why a version keeps
/// every change.
fn note(text: &str, class: Option<&str>) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).wrap(true).selectable(true).build();
    match class {
        Some(c) => label.add_css_class(c),
        None => label.add_css_class("dim-label"),
    }
    label
}

/// The program a merge tool command starts, for messages ("meld").
pub fn merge_tool_name(command: &str) -> String {
    glib::shell_parse_argv(command).ok().and_then(|a| a.first().map(|p| std::path::Path::new(p).file_name().unwrap_or_default().to_string_lossy().into_owned())).unwrap_or_default()
}

/// One version for the merge tool: where it's from, and its text.
pub struct ToolSide {
    pub label: String,
    pub text: String,
}

/// Runs the external merge tool on `left` and `right` off the main thread.
/// `button` (a Button or MenuButton) says it's waiting until the tool
/// exits; then `done` gets what was saved, `None` if nothing was, or why
/// it failed.
pub fn run_merge_tool(button: &impl IsA<gtk::Widget>, command: String, left: ToolSide, right: ToolSide, base: Option<String>, done: impl FnOnce(Result<Option<String>, String>) + 'static) {
    let button = button.clone().upcast::<gtk::Widget>();
    let label: Option<String> = button.property("label");
    button.set_sensitive(false);
    button.set_property("label", format!("Waiting for {}…", merge_tool_name(&command)));
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use crate::merge::external::{Side, run};
            run(&command, Side { label: &left.label, text: &left.text }, Side { label: &right.label, text: &right.text }, base.as_deref())
        })
        .await
        .unwrap_or_else(|_| Err("The merge tool failed".into()));
        button.set_property("label", label);
        button.set_sensitive(true);
        done(result);
    });
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::conflicts::{Definition, FileConflict, ObjectConflict};

    #[test]
    fn ignores_directly_by_folder_or_by_rule() {
        let ignored: std::collections::BTreeSet<String> = [file_key("common/Buildings/A.txt"), objects_folder_key("common/portrait_sets")].into();
        let is = |k: &str| ignored.contains(k);
        let (none, names) = (Rules::default(), vec!["A".to_owned(), "B".to_owned()]);
        let ignores = Ignores { keys: &is, rules: &none, names: &names, dependencies: None };
        let file = |p: &str| FileConflict { path: p.into(), mods: vec![0, 1] };
        let object = |c: &str| ObjectConflict { category: c.into(), key: "x".into(), definitions: vec![Definition { mod_position: 0, file: "f".into() }] };
        assert!(file_ignored(&ignores, &file("common/buildings/a.txt")), "paths match case-insensitively");
        assert!(!file_ignored(&ignores, &file("common/buildings/b.txt")));
        assert!(object_ignored(&ignores, &object("common/portrait_sets")));
        assert!(!object_ignored(&ignores, &object("common/buildings")));
        let folder: std::collections::BTreeSet<String> = [files_folder_key("gfx/models")].into();
        let in_folder = |k: &str| folder.contains(k);
        assert!(file_ignored(&Ignores { keys: &in_folder, rules: &none, names: &names, dependencies: None }, &file("gfx/models/ship.mesh")));
        let rules = Rules::parse("common/buildings/b.txt | a | b");
        assert!(file_ignored(&Ignores { keys: &is, rules: &rules, names: &names, dependencies: None }, &file("common/buildings/b.txt")));
        let depends = [BTreeSet::new(), BTreeSet::from([0])];
        assert!(file_ignored(&Ignores { keys: &is, rules: &none, names: &names, dependencies: Some(&depends) }, &file("common/x.txt")), "B depends on A");
    }
}
