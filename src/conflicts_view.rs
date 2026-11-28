//! The Conflicts mode: the conflicts the last scan found, each one's
//! versions side by side with the one the game uses marked, and resolving
//! them into the collection's merged patch. Mod management stays in the
//! Mods mode.
//!
//! The version the game uses is the merge being built. A highlighted block
//! in another version can be added above or below the merge's matching
//! block, or replace it: from its right-click menu, or from the keyboard
//! (`[` `]` between blocks, then `A`, `B` or `R`). Comparing with the base
//! game shows what each mod changed from vanilla instead.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use drukhal_core::merge::conflicts::{self, FileConflict, ObjectConflict};
use drukhal_core::merge::ignore_rules::Rules;
use drukhal_core::merge::patch::{self, Print};
use drukhal_core::merge::scan::{self, Analysis, Ignores};
use drukhal_core::merge::source::Source;
use drukhal_core::merge::ai;
use drukhal_core::merge_dialog::load as merge_dialog_load;
use iced::widget::{button, column, container, mouse_area, opaque, pin, row, scrollable, space, stack, text, text_input};
use iced::{Background, Border, Color, Element, Fill, Length, Point, Shadow, Task, Vector, mouse};

use crate::library::Library;
use crate::palette::{Palette, Rgb};
use crate::style::{MONO, alpha, c, chip, field, filled_chip, ghost, icon, icons, pill, surface};

/// The most queue rows drawn at once; search narrows the rest.
const QUEUE_LIMIT: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Queue,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    NeedsMerge,
    Simple,
    Files,
    Done,
}

impl Filter {
    const ALL: [Filter; 5] = [Filter::All, Filter::NeedsMerge, Filter::Simple, Filter::Files, Filter::Done];

    fn label(self) -> String {
        match self {
            Filter::All => t!("All"),
            Filter::NeedsMerge => t!("Needs merge"),
            Filter::Simple => t!("Simple"),
            Filter::Files => t!("Files"),
            Filter::Done => t!("Done"),
        }
    }
}

/// A conflict in the queue: an object or a file override, by index into
/// the scan's report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Object(usize),
    File(usize),
}

/// Where a conflict stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    NeedsMerge,
    Simple,
    File,
    Done,
}

/// What the versions are compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    /// The merge being built: what each version would add to it.
    Merge,
    /// The base game: what each mod changed.
    Base,
}

/// A run of lines that differ between a version and the merge: `src` in
/// the version, `dst` in the merge. Either can be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub src: Range<usize>,
    pub dst: Range<usize>,
}

/// What to do with a block of another version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Above,
    Below,
    Replace,
}

/// A block: a version (by index into the sources) and one of its hunks
/// against the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub version: usize,
    pub hunk: usize,
}

/// One object's versions, as read from disk.
pub struct Versions {
    pub item: Item,
    pub vanilla: Option<Source>,
    /// The mods' versions, in load order.
    pub sources: Vec<Source>,
    /// Each source's scanned position.
    pub positions: Vec<usize>,
    pub prints: Vec<Print>,
    /// Mods whose version couldn't be read.
    pub missing: Vec<String>,
    /// The source the game uses, and why.
    pub winner: Option<(usize, conflicts::Reason)>,
}

impl std::fmt::Debug for Versions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Versions").field("item", &self.item).field("sources", &self.sources.len()).finish()
    }
}

/// What ignoring can cover, for the ignore sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    /// One key in the settings' `ignored` set.
    Key { key: String, label: String },
    /// An ignore rule line.
    Rule { rule: String, label: String },
}

impl Scope {
    fn label(&self) -> &str {
        match self {
            Scope::Key { label, .. } | Scope::Rule { label, .. } => label,
        }
    }
}

struct IgnoreSheet {
    scopes: Vec<(Scope, usize, String)>,
    picked: usize,
}

/// An AI merge running or finished for the selected object.
#[derive(Debug, Clone)]
enum Ai {
    Running,
    /// The model's notes on what it combined.
    Notes(String),
    Failed(String),
}

pub struct State {
    analysis: Option<Arc<Analysis>>,
    scanning: bool,
    /// The conflicts not ignored, in the scan's order.
    queue: Vec<Item>,
    selected: Option<Item>,
    view: View,
    filter: Filter,
    search: String,
    versions: Option<Arc<Versions>>,
    loading: bool,
    /// The merge being built, by line, once edited; until then it's the
    /// version the game uses.
    merged: Option<Vec<String>>,
    undo: Vec<Option<Vec<String>>>,
    compare: Compare,
    menu: Option<(Block, Point)>,
    focus: Option<Block>,
    ignore: Option<IgnoreSheet>,
    ai: Option<Ai>,
}

#[derive(Debug, Clone)]
pub enum Message {
    View(View),
    Filter(Filter),
    Search(String),
    Scan,
    Scanned(Arc<Analysis>),
    Select(Item),
    Step(isize),
    Loaded(Arc<Versions>),
    Compare(Compare),
    OpenMenu(Block),
    CloseMenu,
    Apply(Op, Block),
    StepBlock(isize),
    ApplyFocused(Op),
    Undo,
    Reset,
    SaveMerge,
    /// Save one mod's version (by source index) as the resolution.
    Keep(usize),
    /// Save one mod's copy of an overridden file (by scanned position).
    KeepFile(usize),
    MergeAi,
    AiDone(Item, Result<(String, String), String>),
    OpenIgnore,
    PickScope(usize),
    ConfirmIgnore,
    CloseIgnore,
    UnignoreKey(String),
    UnignoreRule(String),
}

/// What happened, for the app's status line.
pub type Notice = Option<(String, bool)>;

impl State {
    pub fn new() -> Self {
        Self {
            analysis: None,
            scanning: false,
            queue: Vec::new(),
            selected: None,
            view: View::Queue,
            filter: Filter::All,
            search: String::new(),
            versions: None,
            loading: false,
            merged: None,
            undo: Vec::new(),
            compare: Compare::Merge,
            menu: None,
            focus: None,
            ignore: None,
            ai: None,
        }
    }

    /// Whether the last scan no longer describes the load order.
    pub fn stale(&self, library: &Library) -> bool {
        !self.scanning && self.analysis.as_ref().is_none_or(|a| a.revision != library.state.revision)
    }

    pub fn has_sheet(&self) -> bool {
        self.ignore.is_some()
    }

    pub fn update(&mut self, library: &mut Library, message: Message) -> (Task<Message>, Notice) {
        let mut notice = None;
        let task = match message {
            Message::View(v) => {
                self.view = v;
                Task::none()
            }
            Message::Filter(f) => {
                self.filter = f;
                Task::none()
            }
            Message::Search(s) => {
                self.search = s;
                Task::none()
            }
            Message::Scan => self.scan(library),
            Message::Scanned(analysis) => {
                self.scanning = false;
                self.analysis = Some(analysis);
                self.rebuild(library);
                let keep = self.selected.filter(|s| self.queue.contains(s));
                self.selected = None;
                match keep.or_else(|| self.first_open(library)) {
                    Some(item) => self.select(library, item),
                    None => Task::none(),
                }
            }
            Message::Select(item) => self.select(library, item),
            Message::Step(by) => {
                let shown = self.shown(library);
                match self.selected.and_then(|s| shown.iter().position(|&i| i == s)) {
                    Some(at) if !shown.is_empty() => self.select(library, shown[(at as isize + by).rem_euclid(shown.len() as isize) as usize]),
                    _ => shown.first().map_or_else(Task::none, |&i| self.select(library, i)),
                }
            }
            Message::Loaded(versions) => {
                if Some(versions.item) == self.selected {
                    self.loading = false;
                    self.versions = Some(versions);
                }
                Task::none()
            }
            Message::Compare(c) => {
                self.compare = c;
                Task::none()
            }
            Message::OpenMenu(block) => {
                self.menu = Some((block, Point::ORIGIN));
                self.focus = Some(block);
                Task::none()
            }
            Message::CloseMenu => {
                self.menu = None;
                Task::none()
            }
            Message::Apply(op, block) => {
                self.menu = None;
                self.apply(op, block);
                Task::none()
            }
            Message::StepBlock(by) => {
                let blocks = self.blocks();
                self.focus = if blocks.is_empty() {
                    None
                } else {
                    let at = self.focus.and_then(|f| blocks.iter().position(|b| *b == f));
                    Some(blocks[match at {
                        Some(at) => (at as isize + by).rem_euclid(blocks.len() as isize) as usize,
                        None if by < 0 => blocks.len() - 1,
                        None => 0,
                    }])
                };
                Task::none()
            }
            Message::ApplyFocused(op) => {
                if let Some(block) = self.focus.filter(|b| self.blocks().contains(b)) {
                    self.apply(op, block);
                }
                Task::none()
            }
            Message::Undo => {
                if let Some(before) = self.undo.pop() {
                    self.merged = before;
                    self.refocus();
                }
                Task::none()
            }
            Message::Reset => {
                self.undo.push(self.merged.take());
                self.refocus();
                Task::none()
            }
            Message::SaveMerge => {
                let (Some(versions), Some(Item::Object(i))) = (self.versions.clone(), self.selected) else { return (Task::none(), None) };
                let Some(analysis) = self.analysis.clone() else { return (Task::none(), None) };
                let c = &analysis.report.objects[i];
                let text = self.merge_lines().join("\n");
                match drukhal_core::merge_dialog::validate(&c.category, &c.key, &text).and_then(|script| library.save_merge(&c.category, &c.key, &script, versions.prints.clone())) {
                    Ok(message) => {
                        notice = Some((message, false));
                        self.next_open(library)
                    }
                    Err(e) => {
                        notice = Some((e, true));
                        Task::none()
                    }
                }
            }
            Message::Keep(n) => {
                let (Some(versions), Some(Item::Object(i))) = (self.versions.clone(), self.selected) else { return (Task::none(), None) };
                let Some(analysis) = self.analysis.clone() else { return (Task::none(), None) };
                let c = &analysis.report.objects[i];
                let Some(source) = versions.sources.get(n) else { return (Task::none(), None) };
                match library.save_merge(&c.category, &c.key, &source.text, versions.prints.clone()) {
                    Ok(message) => {
                        notice = Some((message, false));
                        self.next_open(library)
                    }
                    Err(e) => {
                        notice = Some((e, true));
                        Task::none()
                    }
                }
            }
            Message::KeepFile(position) => {
                let (Some(analysis), Some(Item::File(i))) = (self.analysis.clone(), self.selected) else { return (Task::none(), None) };
                let f = &analysis.report.files[i];
                let mods: Vec<patch::SourceMod> = f.mods.iter().filter_map(|&m| Some((analysis.ids[m].clone(), analysis.names[m].clone(), analysis.roots[m].clone()?))).collect();
                let prints = patch::file_prints(&f.path, library.game_dir().as_deref(), &mods);
                let read = analysis.roots[position].as_ref().map(|root| std::fs::read(root.join(&f.path)));
                match read {
                    Some(Ok(bytes)) => match library.save_file_merge(&f.path, &bytes, prints) {
                        Ok(message) => {
                            notice = Some((message, false));
                            self.next_open(library)
                        }
                        Err(e) => {
                            notice = Some((e, true));
                            Task::none()
                        }
                    },
                    Some(Err(e)) => {
                        notice = Some((tf!("Could not read {file}: {error}", file = f.path.as_str(), error = e), true));
                        Task::none()
                    }
                    None => Task::none(),
                }
            }
            Message::MergeAi => self.merge_ai(library),
            Message::AiDone(item, result) => {
                if Some(item) == self.selected {
                    match result {
                        Ok((script, notes)) => {
                            self.undo.push(self.merged.clone());
                            self.merged = Some(script.lines().map(str::to_owned).collect());
                            self.refocus();
                            self.ai = Some(Ai::Notes(notes));
                        }
                        Err(e) => self.ai = Some(Ai::Failed(e)),
                    }
                }
                Task::none()
            }
            Message::OpenIgnore => {
                self.open_ignore(library);
                Task::none()
            }
            Message::PickScope(i) => {
                if let Some(sheet) = &mut self.ignore {
                    sheet.picked = i;
                }
                Task::none()
            }
            Message::ConfirmIgnore => {
                if let Some(sheet) = self.ignore.take() {
                    let result = match sheet.scopes.into_iter().nth(sheet.picked).map(|s| s.0) {
                        Some(Scope::Key { key, .. }) => library.ignore(Some(key), None),
                        Some(Scope::Rule { rule, .. }) => library.ignore(None, Some(rule)),
                        None => Ok(()),
                    };
                    if let Err(e) = result {
                        notice = Some((e, true));
                    }
                    self.rebuild(library);
                    if self.selected.is_some_and(|s| !self.queue.contains(&s)) {
                        self.selected = None;
                        if let Some(next) = self.first_open(library) {
                            return (self.select(library, next), notice);
                        }
                    }
                }
                Task::none()
            }
            Message::CloseIgnore => {
                self.ignore = None;
                Task::none()
            }
            Message::UnignoreKey(key) => {
                if let Err(e) = library.unignore(Some(&key), None) {
                    notice = Some((e, true));
                }
                self.rebuild(library);
                Task::none()
            }
            Message::UnignoreRule(rule) => {
                if let Err(e) = library.unignore(None, Some(&rule)) {
                    notice = Some((e, true));
                }
                self.rebuild(library);
                Task::none()
            }
        };
        (task, notice)
    }

    /// Scans the load order in the background.
    pub fn scan(&mut self, library: &Library) -> Task<Message> {
        if self.scanning {
            return Task::none();
        }
        self.scanning = true;
        let mods = library.state.mods.clone();
        let enabled = library.state.enabled.clone();
        let revision = library.state.revision;
        let languages = library.settings.game.languages(&library.user_dir);
        let game_dir = library.game_dir();
        let version = library.game_version().map(str::to_owned);
        Task::perform(
            async move { tokio::task::spawn_blocking(move || scan::run(&mods, &enabled, revision, &languages, game_dir.as_deref(), version.as_deref())).await },
            |result| match result {
                Ok(analysis) => Message::Scanned(Arc::new(analysis)),
                // A panicking scan is a bug; show an empty result rather than hang.
                Err(_) => Message::Scanned(Arc::new(Analysis { revision: 0, names: Vec::new(), ids: Vec::new(), roots: Vec::new(), report: Default::default(), simple: Default::default(), depends_on: Vec::new(), seconds: 0.0 })),
            },
        )
    }

    /// The conflicts the settings don't hide, again.
    fn rebuild(&mut self, library: &Library) {
        let Some(a) = &self.analysis else { return };
        let rules = Rules::parse(&library.settings.ignore_rules);
        let ignores = Ignores { keys: &library.settings.ignored, rules: &rules, names: &a.names, dependencies: (!library.settings.show_dependency_conflicts).then_some(&a.depends_on[..]) };
        self.queue = (0..a.report.objects.len()).filter(|&i| !ignores.object(&a.report.objects[i])).map(Item::Object).chain((0..a.report.files.len()).filter(|&i| !ignores.file(&a.report.files[i])).map(Item::File)).collect();
    }

    fn status(&self, library: &Library, item: Item) -> Status {
        let Some(a) = &self.analysis else { return Status::NeedsMerge };
        match item {
            Item::Object(i) => {
                let c = &a.report.objects[i];
                if library.merged(&c.category, &c.key) {
                    Status::Done
                } else if a.simple.contains_key(&i) {
                    Status::Simple
                } else {
                    Status::NeedsMerge
                }
            }
            Item::File(i) => {
                if library.merged_file(&a.report.files[i].path) {
                    Status::Done
                } else {
                    Status::File
                }
            }
        }
    }

    /// The queue as filtered and searched.
    fn shown(&self, library: &Library) -> Vec<Item> {
        let Some(a) = &self.analysis else { return Vec::new() };
        let query = self.search.trim().to_lowercase();
        self.queue
            .iter()
            .copied()
            .filter(|&item| {
                let status = self.status(library, item);
                let by_filter = match self.filter {
                    Filter::All => true,
                    Filter::NeedsMerge => status == Status::NeedsMerge,
                    Filter::Simple => status == Status::Simple,
                    Filter::Files => matches!(item, Item::File(_)),
                    Filter::Done => status == Status::Done,
                };
                by_filter
                    && (query.is_empty()
                        || match item {
                            Item::Object(i) => {
                                let c = &a.report.objects[i];
                                c.key.to_lowercase().contains(&query) || c.category.to_lowercase().contains(&query)
                            }
                            Item::File(i) => a.report.files[i].path.to_lowercase().contains(&query),
                        })
            })
            .collect()
    }

    fn first_open(&self, library: &Library) -> Option<Item> {
        self.queue.iter().copied().find(|&i| self.status(library, i) != Status::Done).or_else(|| self.queue.first().copied())
    }

    /// After resolving one: the next conflict not done yet.
    fn next_open(&mut self, library: &Library) -> Task<Message> {
        let start = self.selected.and_then(|s| self.queue.iter().position(|&i| i == s)).map_or(0, |p| p + 1);
        let n = self.queue.len();
        let next = (0..n).map(|k| self.queue[(start + k) % n.max(1)]).find(|&i| self.status(library, i) != Status::Done);
        match next {
            Some(item) => self.select(library, item),
            None => Task::none(),
        }
    }

    fn select(&mut self, library: &Library, item: Item) -> Task<Message> {
        if self.selected == Some(item) && (self.versions.is_some() || self.loading) {
            return Task::none();
        }
        self.selected = Some(item);
        self.versions = None;
        self.merged = None;
        self.undo.clear();
        self.menu = None;
        self.focus = None;
        self.ai = None;
        let Item::Object(i) = item else {
            self.loading = false;
            return Task::none();
        };
        let Some(a) = self.analysis.clone() else { return Task::none() };
        self.loading = true;
        let game_dir = library.game_dir();
        Task::perform(async move { tokio::task::spawn_blocking(move || load_versions(&a, i, game_dir)).await }, move |r| match r {
            Ok(v) => Message::Loaded(Arc::new(v)),
            Err(_) => Message::Loaded(Arc::new(Versions { item, vanilla: None, sources: Vec::new(), positions: Vec::new(), prints: Vec::new(), missing: Vec::new(), winner: None })),
        })
    }

    /// The selected object's merge so far.
    fn merge_lines(&self) -> Vec<String> {
        if let Some(m) = &self.merged {
            return m.clone();
        }
        match &self.versions {
            Some(v) => v.winner.and_then(|(w, _)| v.sources.get(w)).map(|s| s.text.lines().map(str::to_owned).collect()).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    fn winner(&self) -> Option<usize> {
        self.versions.as_ref()?.winner.map(|(w, _)| w)
    }

    /// Every block of the other versions that the merge lacks, in reading
    /// order (version by version).
    fn blocks(&self) -> Vec<Block> {
        let Some(v) = &self.versions else { return Vec::new() };
        let winner = self.winner();
        let merge = self.merge_lines();
        (0..v.sources.len())
            .filter(|&n| Some(n) != winner)
            .flat_map(|n| {
                let lines: Vec<&str> = v.sources[n].text.lines().collect();
                hunks(&lines, &merge).into_iter().enumerate().filter(|(_, h)| !h.src.is_empty()).map(move |(hunk, _)| Block { version: n, hunk })
            })
            .collect()
    }

    fn refocus(&mut self) {
        let blocks = self.blocks();
        self.focus = self.focus.and_then(|f| blocks.iter().copied().find(|b| *b == f).or_else(|| blocks.iter().copied().find(|b| b.version == f.version && b.hunk >= f.hunk)).or_else(|| blocks.last().copied()));
    }

    fn apply(&mut self, op: Op, block: Block) {
        let Some(v) = &self.versions else { return };
        let source: Vec<&str> = v.sources[block.version].text.lines().collect();
        let merge = self.merge_lines();
        let Some(h) = hunks(&source, &merge).get(block.hunk).cloned() else { return };
        let theirs: Vec<String> = source[h.src.clone()].iter().map(|l| l.to_string()).collect();
        let mut next = merge.clone();
        match op {
            Op::Above => drop(next.splice(h.dst.start..h.dst.start, theirs)),
            Op::Below => drop(next.splice(h.dst.end..h.dst.end, theirs)),
            Op::Replace => drop(next.splice(h.dst.clone(), theirs)),
        }
        self.undo.push(self.merged.clone());
        self.merged = Some(next);
        self.refocus();
    }

    /// Asks the configured AI provider to merge the object, off the UI thread.
    fn merge_ai(&mut self, library: &Library) -> Task<Message> {
        let (Some(versions), Some(item @ Item::Object(i)), Some(a)) = (self.versions.clone(), self.selected, self.analysis.clone()) else { return Task::none() };
        if matches!(self.ai, Some(Ai::Running)) {
            return Task::none();
        }
        self.ai = Some(Ai::Running);
        let settings = library.settings.clone();
        let game_version = library.game_version().map(str::to_owned);
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    // The key lives in the system keyring; reading it can block.
                    let settings = settings.with_keyring().settings;
                    let provider = settings.ready_provider().ok_or_else(|| t!("Set up an AI provider in the settings first"))?;
                    ai::set_instructions(&settings.ai_instructions);
                    let c = &a.report.objects[i];
                    let ctx = ai::MergeContext {
                        category: c.category.clone(),
                        key: c.key.clone(),
                        game_version,
                        is_fios: conflicts::is_fios(&c.category),
                        winner: versions.winner.map(|(w, reason)| (versions.positions[w], versions.sources[w].label.clone(), reason)),
                        load_positions: versions.positions.clone(),
                        prints: versions.prints.clone(),
                        ..Default::default()
                    };
                    let checked = ai::merge_checked_cancellable(&provider, &ctx, versions.vanilla.as_ref(), &versions.sources, None, None, &mut |_| {})?;
                    checked.script.map(|script| (script, checked.merged.notes)).map_err(|why| tf!("The AI's merge didn't pass the checks: {reason}", reason = why))
                })
                .await
                .unwrap_or_else(|_| Err(t!("The merge failed")))
            },
            move |result| Message::AiDone(item, result),
        )
    }

    fn open_ignore(&mut self, library: &Library) {
        let (Some(a), Some(item)) = (&self.analysis, self.selected) else { return };
        let count = |test: &dyn Fn(Item) -> bool| self.queue.iter().filter(|&&i| test(i)).count();
        let scopes = match item {
            Item::Object(i) => {
                let c = &a.report.objects[i];
                let mods: Vec<&str> = mods_of_object(c).into_iter().map(|m| a.names[m].as_str()).collect();
                let same_mods = mods_of_object(c);
                vec![
                    (Scope::Key { key: scan::object_key(&c.category, &c.key), label: tf!("Only {key}", key = c.key.as_str()) }, 1, t!("Just this conflict.")),
                    (
                        Scope::Key { key: scan::objects_folder_key(&c.category), label: tf!("Everything in {folder}", folder = c.category.as_str()) },
                        count(&|it| matches!(it, Item::Object(k) if a.report.objects[k].category == c.category)),
                        t!("Every object conflict in this folder, now and after later scans."),
                    ),
                    (
                        Scope::Rule { rule: scan::mods_rule(&mods), label: tf!("Whenever {mods} conflict", mods = mods.join(" & ")) },
                        count(&|it| match it {
                            Item::Object(k) => mods_of_object(&a.report.objects[k]) == same_mods,
                            Item::File(k) => a.report.files[k].mods.iter().copied().collect::<BTreeSet<_>>() == same_mods,
                        }),
                        t!("Any conflict between exactly these mods, wherever it is. Saved as an ignore rule."),
                    ),
                ]
            }
            Item::File(i) => {
                let f = &a.report.files[i];
                let mods: Vec<&str> = f.mods.iter().map(|&m| a.names[m].as_str()).collect();
                let folder = scan::file_folder(&f.path).to_owned();
                vec![
                    (Scope::Key { key: scan::file_key(&f.path), label: tf!("Only {file}", file = f.path.as_str()) }, 1, t!("Just this file.")),
                    (
                        Scope::Key { key: scan::files_folder_key(&folder), label: tf!("Every file in {folder}", folder = folder.as_str()) },
                        count(&|it| matches!(it, Item::File(k) if scan::file_folder(&a.report.files[k].path).eq_ignore_ascii_case(&folder))),
                        t!("Every file override in this folder."),
                    ),
                    (Scope::Rule { rule: scan::mods_rule(&mods), label: tf!("Whenever {mods} conflict", mods = mods.join(" & ")) }, 0, t!("Any conflict between exactly these mods, wherever it is. Saved as an ignore rule.")),
                ]
            }
        };
        let _ = library;
        self.ignore = Some(IgnoreSheet { scopes, picked: 0 });
    }

    pub fn header_actions<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let when = match (&self.analysis, self.scanning) {
            (_, true) => t!("Scanning…"),
            (Some(a), false) => tf!("Scanned in {seconds} s", seconds = format!("{:.1}", a.seconds)),
            (None, false) => String::new(),
        };
        row![
            text(when).size(13).color(c(p.muted)),
            button(row![icon(icons::REFRESH, 16, p.on_window), text(t!("Rescan")).size(14)].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press_maybe((!self.scanning).then_some(Message::Scan)).style(move |_, s| pill(p.container_high, p.on_window, s)),
        ]
        .spacing(12)
        .align_y(iced::Center)
        .into()
    }

    pub fn status_line(&self, library: &Library) -> String {
        match &self.analysis {
            Some(a) => {
                let done = self.queue.iter().filter(|&&i| self.status(library, i) == Status::Done).count();
                let hidden = a.report.objects.len() + a.report.files.len() - self.queue.len();
                tf!("{count} conflicts between {mods} mods · {done} done · {hidden} ignored", count = self.queue.len(), mods = a.names.len(), done = done, hidden = hidden)
            }
            None => String::new(),
        }
    }

    pub fn view<'a>(&'a self, library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
        let main: Element<'a, Message> = match (self.view, &self.analysis) {
            (_, None) => placeholder(p, if self.scanning { t!("Scanning your mods…") } else { t!("Not scanned yet") }, if self.scanning { t!("Reading every enabled mod's files. A big load order takes a minute the first time; later scans only read what changed.") } else { t!("Scan the load order to find where mods overwrite each other.") }),
            (View::Ignored, Some(_)) => self.ignored(library, p),
            (View::Queue, Some(a)) => row![self.queue_pane(library, p, a), self.detail(library, p, a)].spacing(12).into(),
        };
        row![self.rail(p), main].spacing(12).height(Fill).into()
    }

    /// What the app draws over everything: the context menu or the ignore sheet.
    pub fn overlay<'a>(&'a self, p: &'a Palette) -> Option<Element<'a, Message>> {
        self.ignore_sheet(p).or_else(|| self.menu(p))
    }

    pub fn place_menu(&mut self, at: Point) {
        if let Some((_, place)) = &mut self.menu {
            *place = at;
        }
    }

    fn rail<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let item = |v: View, label: String, glyph: &'static [u8]| {
            let selected = v == self.view;
            let fg = if selected { p.on_window } else { p.muted };
            column![
                button(container(icon(glyph, 20, fg)).center_x(Fill)).width(52).padding([6, 0]).on_press(Message::View(v)).style(move |_, s| if selected { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) }),
                text(label).size(11).color(c(fg)),
            ]
            .spacing(4)
            .align_x(iced::Center)
        };
        container(column![item(View::Queue, t!("Queue"), icons::WARNING), item(View::Ignored, t!("Ignored"), icons::HIDE)].spacing(14).align_x(iced::Center))
            .width(76)
            .height(Fill)
            .padding([14, 8])
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }

    fn queue_pane<'a>(&'a self, library: &'a Library, p: &'a Palette, a: &'a Analysis) -> Element<'a, Message> {
        let total = self.queue.len();
        let done = self.queue.iter().filter(|&&i| self.status(library, i) == Status::Done).count();
        let progress = {
            let w = 300.0 * done as f32 / total.max(1) as f32;
            row![container(space()).width(w).height(8).style(move |_| surface(p.success, p.success, 4.0)), container(space()).width(Fill).height(8).style(move |_| surface(p.container_high, p.container_high, 4.0))].spacing(2)
        };
        let filters = row(Filter::ALL.iter().map(|&f| {
            let on = f == self.filter;
            button(text(f.label()).size(12))
                .padding([5, 10])
                .on_press(Message::Filter(f))
                .style(move |_, s| {
                    let mut st = if on { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) };
                    if !on {
                        st.border = Border { radius: 999.0.into(), width: 1.0, color: alpha(p.outline, 0.5) };
                    }
                    st
                })
                .into()
        }))
        .spacing(4)
        .wrap();
        let shown = self.shown(library);
        let more = shown.len().saturating_sub(QUEUE_LIMIT);
        let mut list = column(shown.into_iter().take(QUEUE_LIMIT).map(|i| self.queue_row(library, p, a, i))).spacing(4);
        if more > 0 {
            list = list.push(container(text(tnf!("{count} more; search to narrow them", "{count} more; search to narrow them", count = more)).size(12).color(c(p.muted))).padding(10));
        }
        let mut errors: Element<'a, Message> = space().into();
        if !a.report.errors.is_empty() || !a.report.skipped.is_empty() {
            errors = text(tf!("{files} files couldn't be read, {mods} mods were skipped", files = a.report.errors.len(), mods = a.report.skipped.len())).size(11).color(c(p.warning)).into();
        }
        container(
            column![
                row![text(tnf!("{count} conflict", "{count} conflicts", count = total)).size(20), space().width(Fill), text(tnf!("{count} mod", "{count} mods", count = a.names.len())).size(12).color(c(p.muted))].align_y(iced::Center),
                progress,
                text(tf!("{done} done · {left} to go", done = done, left = total - done)).size(11).color(c(p.muted)),
                errors,
                text_input(&t!("Search objects and files"), &self.search).on_input(Message::Search).padding([8, 14]).style(move |_, _| field(p, 999.0, false)),
                filters,
                scrollable(list).height(Fill),
            ]
            .spacing(10),
        )
        .width(360)
        .height(Fill)
        .padding(14)
        .style(move |_| surface(p.container_low, p.on_window, 24.0))
        .into()
    }

    fn queue_row<'a>(&'a self, library: &'a Library, p: &'a Palette, a: &'a Analysis, item: Item) -> Element<'a, Message> {
        let selected = self.selected == Some(item);
        let status = self.status(library, item);
        let (label, tone) = status_look(p, status);
        let edited = selected && self.merged.is_some();
        let (title, subtitle) = match item {
            Item::Object(i) => {
                let cf = &a.report.objects[i];
                let mods = mods_of_object(cf).into_iter().map(|m| initials(&a.names[m])).collect::<Vec<_>>().join(" · ");
                (cf.key.clone(), format!("{} · {mods}", cf.category))
            }
            Item::File(i) => {
                let f = &a.report.files[i];
                let mods = f.mods.iter().map(|&m| initials(&a.names[m])).collect::<Vec<_>>().join(" · ");
                (f.path.rsplit('/').next().unwrap_or(&f.path).to_owned(), format!("{} · {mods}", scan::file_folder(&f.path)))
            }
        };
        let content = row![
            container(space()).width(4).height(40).style(move |_| surface(tone, tone, 2.0)),
            column![text(title).font(MONO).size(13).wrapping(text::Wrapping::Glyph), text(subtitle).size(11).color(c(p.muted)).wrapping(text::Wrapping::Glyph)].spacing(4).width(Fill),
            if edited { chip(t!("Editing"), p.accent) } else { chip(label, tone) },
        ]
        .spacing(12)
        .align_y(iced::Center);
        button(content)
            .width(Fill)
            .padding([10, 12])
            .on_press(Message::Select(item))
            .style(move |_, s| {
                let mut st = if selected { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) };
                st.border.radius = 16.0.into();
                st
            })
            .into()
    }

    fn detail<'a>(&'a self, library: &'a Library, p: &'a Palette, a: &'a Analysis) -> Element<'a, Message> {
        match self.selected {
            None => placeholder(p, t!("Nothing left to resolve"), t!("Every conflict here is merged or ignored.")),
            Some(Item::File(i)) => self.file_detail(library, p, a, &a.report.files[i]),
            Some(Item::Object(i)) => self.object_detail(library, p, a, i),
        }
    }

    fn file_detail<'a>(&'a self, library: &'a Library, p: &'a Palette, a: &'a Analysis, f: &'a FileConflict) -> Element<'a, Message> {
        let last = f.mods.last().copied();
        let rows = column(f.mods.iter().map(|&m| {
            let wins = Some(m) == last;
            let badge: Element<'a, Message> = if wins { filled_chip(t!("In game"), p.accent, p.on_accent) } else { space().into() };
            container(
                row![
                    text(format!("#{}", m + 1)).font(MONO).size(12).color(c(p.muted)),
                    text(a.names[m].as_str()).size(15).width(Fill),
                    badge,
                    button(text(tf!("Keep {name}'s", name = short(&a.names[m]))).size(13)).padding([8, 14]).on_press_maybe(a.roots[m].is_some().then_some(Message::KeepFile(m))).style(move |_, s| pill(p.container_high, p.on_window, s)),
                ]
                .spacing(12)
                .align_y(iced::Center),
            )
            .padding([12, 16])
            .style(move |_| container::Style { border: Border { radius: 16.0.into(), width: if wins { 2.0 } else { 0.0 }, color: c(p.accent) }, ..surface(p.container, p.on_window, 16.0) })
            .into()
        }))
        .spacing(8);
        let done = library.merged_file(&f.path);
        column![
            column![text(scan::file_folder(&f.path).to_owned()).size(12).color(c(p.accent)), text(f.path.rsplit('/').next().unwrap_or(&f.path).to_owned()).font(MONO).size(24)].spacing(2).padding([0, 8]),
            banner(p, if done { t!("The merged patch ships its own copy of this file.") } else { t!("Several mods ship this file; the game loads only the copy from the mod that loads last. Keeping one copies it into the merged patch.") }),
            scrollable(rows).height(Fill),
            container(row![space().width(Fill), button(row![icon(icons::HIDE, 16, p.muted), text(t!("Ignore…")).size(13).color(c(p.muted))].spacing(6).align_y(iced::Center)).padding([10, 14]).on_press(Message::OpenIgnore).style(move |_, s| ghost(p.on_window, s))]).padding(14).width(Fill).style(move |_| surface(p.container_high, p.on_window, 24.0)),
        ]
        .spacing(12)
        .width(Fill)
        .into()
    }

    fn object_detail<'a>(&'a self, library: &'a Library, p: &'a Palette, a: &'a Analysis, i: usize) -> Element<'a, Message> {
        let cf = &a.report.objects[i];
        let step = |label: String, glyph: &'static [u8], by: isize| {
            button(row![icon(glyph, 16, p.on_window), text(label).size(13)].spacing(6).align_y(iced::Center)).padding([8, 14]).on_press(Message::Step(by)).style(move |_, s| pill(p.container_high, p.on_window, s))
        };
        let has_vanilla = self.versions.as_ref().is_some_and(|v| v.vanilla.is_some());
        let segment = |label: String, mode: Compare| {
            let on = self.compare == mode;
            let enabled = mode == Compare::Merge || has_vanilla;
            button(text(label).size(12).color(c(if enabled { if on { p.on_window } else { p.muted } } else { p.outline })))
                .padding([6, 12])
                .on_press_maybe(enabled.then_some(Message::Compare(mode)))
                .style(move |_, s| if on { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) })
        };
        let compare = container(row![text(t!("Compare with")).size(12).color(c(p.muted)), segment(t!("the merge"), Compare::Merge), segment(t!("the base game"), Compare::Base)].spacing(4).align_y(iced::Center))
            .padding([2, 4])
            .style(move |_| container::Style { border: Border { radius: 999.0.into(), width: 1.0, color: alpha(p.outline, 0.5) }, ..Default::default() });
        let head = row![
            column![text(cf.category.as_str()).size(12).color(c(p.accent)), text(cf.key.as_str()).font(MONO).size(24)].spacing(2).width(Fill),
            compare,
            step(t!("Previous"), icons::PREVIOUS, -1),
            step(t!("Next"), icons::NEXT, 1),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .padding([0, 8]);
        let Some(v) = &self.versions else {
            return column![head, placeholder(p, t!("Reading the versions…"), String::new())].spacing(12).width(Fill).into();
        };
        let merged = library.merged(&cf.category, &cf.key);
        let winner_name = self.winner().map(|w| v.sources[w].label.clone()).unwrap_or_default();
        let why = match (&self.ai, self.compare, self.merged.is_some()) {
            (Some(Ai::Running), _, _) => t!("Merging with AI…"),
            (Some(Ai::Failed(e)), _, _) => e.clone(),
            (Some(Ai::Notes(notes)), _, _) => tf!("AI merge: {notes} Review it, then save.", notes = notes.as_str()),
            (None, Compare::Base, _) => t!("Coloured lines are what each mod changed from the base game; green where every mod made the same change. A bar at the left marks what you can still add to the merge."),
            (None, _, true) => tf!("You're building a merge on {name}'s version, the one the game uses. Right-click a highlighted block in another version, or use [ ] and A B R.", name = winner_name.as_str()),
            (None, _, false) if merged => t!("The merged patch has a merge of this object. Saving again replaces it."),
            (None, _, false) => match (a.simple.get(&i), v.winner) {
                (Some(simple), _) => tf!("{reason}. In game now: {name}'s version.", reason = simple.as_str(), name = winner_name.as_str()),
                (None, Some((_, reason))) => tf!("In game now: {name}'s version, because {reason}.", name = winner_name.as_str(), reason = reason.explain(conflicts::is_fios(&cf.category))),
                (None, None) => String::new(),
            },
        };
        let mut panes = row![].spacing(12).height(Fill);
        if self.compare == Compare::Base
            && let Some(vanilla) = &v.vanilla
        {
            panes = panes.push(base_pane(p, vanilla, library.game_version()));
        }
        let merge = self.merge_lines();
        for n in 0..v.sources.len() {
            panes = panes.push(self.code_pane(p, v, n, &merge));
        }
        if v.sources.is_empty() {
            panes = panes.push(placeholder(p, t!("No version could be read"), tf!("Missing: {mods}", mods = v.missing.join(", "))));
        }
        let ai_ready = library.settings.ready_provider().is_some() || library.settings.key_in_keyring;
        let actions = if self.merged.is_some() {
            row![
                button(row![icon(icons::OK, 18, p.on_accent), text(t!("Save merge")).size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press(Message::SaveMerge).style(move |_, s| pill(p.accent, p.on_accent, s)),
                text(tf!("{count} lines · Ctrl+S", count = merge.len())).size(12).color(c(p.muted)),
                space().width(Fill),
                button(text(t!("Undo")).size(13)).padding([10, 16]).on_press_maybe((!self.undo.is_empty()).then_some(Message::Undo)).style(move |_, s| pill(p.container, p.on_window, s)),
                button(text(t!("Start over")).size(13).color(c(p.muted))).padding([10, 14]).on_press(Message::Reset).style(move |_, s| ghost(p.on_window, s)),
            ]
        } else {
            let mut keep = row![].spacing(8);
            for (n, s) in v.sources.iter().enumerate() {
                keep = keep.push(button(text(tf!("Keep {name}", name = short(&s.label))).size(13)).padding([10, 16]).on_press(Message::Keep(n)).style(move |_, st| pill(p.container, p.on_window, st)));
            }
            let running = matches!(self.ai, Some(Ai::Running));
            row![
                button(row![icon(icons::AI, 18, p.on_accent), text(if running { t!("Merging…") } else { t!("Merge with AI") }).size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center))
                    .padding([10, 18])
                    .on_press_maybe((!running && ai_ready && v.sources.len() + usize::from(v.vanilla.is_some()) >= 2).then_some(Message::MergeAi))
                    .style(move |_, s| pill(p.accent, p.on_accent, s)),
                text(if ai_ready { String::new() } else { t!("Set up AI in the settings") }).size(12).color(c(p.muted)),
                space().width(Fill),
                keep,
                button(row![icon(icons::HIDE, 16, p.muted), text(t!("Ignore…")).size(13).color(c(p.muted))].spacing(6).align_y(iced::Center)).padding([10, 14]).on_press(Message::OpenIgnore).style(move |_, s| ghost(p.on_window, s)),
            ]
        };
        let actions = container(actions.spacing(10).align_y(iced::Center)).padding(14).width(Fill).style(move |_| surface(p.container_high, p.on_window, 24.0));
        column![head, banner(p, why), panes, actions].spacing(12).width(Fill).into()
    }

    /// One version. The game's version shows the merge so far; the others
    /// mark the blocks it lacks, which a right-click or the keyboard adds.
    /// Compared with the base game, the colours show what each mod changed
    /// from vanilla instead.
    fn code_pane<'a>(&'a self, p: &'a Palette, v: &'a Versions, n: usize, merge: &[String]) -> Element<'a, Message> {
        let source = &v.sources[n];
        let wins = Some(n) == self.winner();
        let lines: Vec<String> = if wins { merge.to_vec() } else { source.text.lines().map(str::to_owned).collect() };
        let as_str: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut block = vec![None; lines.len()];
        let mut differs = vec![false; lines.len()];
        if wins {
            for other in v.sources.iter().enumerate().filter(|(m, _)| *m != n).map(|(_, o)| o) {
                let theirs: Vec<&str> = other.text.lines().collect();
                for h in hunks(&theirs, merge) {
                    h.dst.for_each(|l| differs[l] = true);
                }
            }
        } else {
            for (k, h) in hunks(&as_str, merge).iter().enumerate() {
                for line in h.src.clone() {
                    block[line] = Some(Block { version: n, hunk: k });
                    differs[line] = true;
                }
            }
        }
        let mut from_base = vec![false; lines.len()];
        let mut shared = vec![false; lines.len()];
        let base_mode = self.compare == Compare::Base && v.vanilla.is_some();
        if base_mode && let Some(vanilla) = &v.vanilla {
            let base: Vec<String> = vanilla.text.lines().map(str::to_owned).collect();
            for h in hunks(&as_str, &base) {
                h.src.for_each(|l| from_base[l] = true);
            }
            // Same change: every version changed a line from the base game
            // into this one. A lone brace says nothing, so it never counts.
            let changes: Vec<Vec<String>> = v
                .sources
                .iter()
                .map(|o| {
                    let theirs: Vec<&str> = o.text.lines().collect();
                    hunks(&theirs, &base).into_iter().flat_map(|h| theirs[h.src].iter().map(|x| x.trim_end().to_owned()).collect::<Vec<_>>()).collect()
                })
                .collect();
            for (l, line) in lines.iter().enumerate().filter(|(l, _)| from_base[*l]) {
                let content = line.trim().trim_matches(['{', '}']).trim();
                shared[l] = !content.is_empty() && changes.iter().all(|c| c.iter().any(|x| x == line.trim_end()));
            }
        }
        let mark = if wins { p.success } else { p.error };
        let focus = self.focus.filter(|f| f.version == n).map(|f| f.hunk);
        let open = self.menu.filter(|(b, _)| b.version == n).map(|(b, _)| b.hunk);
        // Lines don't wrap; the pane scrolls sideways instead. The rows get a
        // fixed width so their highlight spans the longest line.
        let longest = lines.iter().map(|l| l.replace('\t', "    ").chars().count()).max().unwrap_or(0);
        let width = (longest as f32 * 7.6 + 70.0).max(360.0);
        let rows = column(lines.iter().enumerate().map(|(k, line)| {
            let b = block[k];
            let active = b.is_some() && (b.map(|b| b.hunk) == focus || b.map(|b| b.hunk) == open);
            let (tint, bar) = if base_mode {
                let tint = if !from_base[k] { None } else if shared[k] { Some(p.success) } else { Some(p.accent) };
                (tint, b.map(|_| if active { p.accent } else { p.outline }))
            } else {
                (differs[k].then_some(mark), differs[k].then_some(if active { p.accent } else { mark }))
            };
            let strength = if active { 0.34 } else { 0.16 };
            let row_el = container(
                row![
                    container(space()).width(if active { 5 } else { 3 }).height(20).style(move |_| match bar {
                        Some(col) => surface(col, col, 0.0),
                        None => container::Style::default(),
                    }),
                    text(format!("{:>3}", k + 1)).font(MONO).size(12).color(alpha(p.muted, 0.6)),
                    text(line.replace('\t', "    ")).font(MONO).size(12.5).wrapping(text::Wrapping::None),
                ]
                .spacing(12)
                .align_y(iced::Center),
            )
            .width(width)
            .style(move |_| match tint {
                Some(col) => container::Style { background: Some(Background::Color(alpha(col, strength))), ..Default::default() },
                None => container::Style::default(),
            });
            match b {
                Some(b) => mouse_area(row_el).on_right_press(Message::OpenMenu(b)).interaction(mouse::Interaction::ContextMenu).into(),
                None => row_el.into(),
            }
        }));
        let edited = self.merged.is_some();
        let badge: Element<'a, Message> = match (wins, edited) {
            (true, true) => filled_chip(t!("In game · your merge"), p.accent, p.on_accent),
            (true, false) => filled_chip(t!("In game"), p.accent, p.on_accent),
            _ => text(t!("right-click a block")).size(11).color(c(p.muted)).into(),
        };
        let keys: Element<'a, Message> = if wins { space().into() } else { text(t!("[ ]  blocks   A above   B below   R replace")).font(MONO).size(11).color(c(p.muted)).into() };
        container(
            column![
                row![
                    container(text(format!("#{}", v.positions[n] + 1)).size(11).color(c(p.muted))).padding([5, 8]).style(move |_| surface(p.container_high, p.muted, 999.0)),
                    column![text(source.label.as_str()).size(15), text(source.file.as_str()).font(MONO).size(11).color(c(p.muted))].spacing(2).width(Fill),
                    badge,
                ]
                .spacing(10)
                .align_y(iced::Center),
                container(scrollable(container(rows).padding([8, 0])).direction(scrollable::Direction::Both { vertical: scrollable::Scrollbar::default(), horizontal: scrollable::Scrollbar::default() }).width(Fill).height(Fill))
                    .width(Fill)
                    .height(Fill)
                    .style(move |_| surface(p.window, p.on_window, 12.0)),
                keys,
            ]
            .spacing(10),
        )
        .padding(16)
        .width(Length::FillPortion(2))
        .height(Fill)
        .style(move |_| container::Style { border: Border { radius: 20.0.into(), width: if wins { 2.0 } else { 0.0 }, color: c(p.accent) }, ..surface(p.container, p.on_window, 20.0) })
        .into()
    }

    fn menu<'a>(&'a self, p: &'a Palette) -> Option<Element<'a, Message>> {
        let (block, at) = self.menu?;
        let v = self.versions.as_ref()?;
        let source: Vec<&str> = v.sources[block.version].text.lines().collect();
        let merge = self.merge_lines();
        let h = hunks(&source, &merge).get(block.hunk)?.clone();
        let into = short(&v.sources[self.winner()?].label);
        let what = tnf!("{count} line", "{count} lines", count = h.src.len());
        let lines = |r: &Range<usize>| if r.len() == 1 { tf!("line {n}", n = r.start + 1) } else { tf!("lines {from}–{to}", from = r.start + 1, to = r.end) };
        let (above, below) = if h.dst.is_empty() {
            (tf!("Before line {n}", n = h.dst.start + 1), tf!("Before line {n}", n = h.dst.start + 1))
        } else {
            (tf!("Before line {n}", n = h.dst.start + 1), tf!("After line {n}", n = h.dst.end))
        };
        let item = |glyph: &'static [u8], label: String, key: &'static str, hint: String, op: Op| {
            button(row![icon(glyph, 18, p.on_window), text(label).size(14).width(Fill), text(hint).size(12).color(c(p.muted)), text(key).size(11).font(MONO).color(c(p.muted)).width(14)].spacing(12).align_y(iced::Center))
                .width(Fill)
                .padding([9, 12])
                .on_press(Message::Apply(op, block))
                .style(move |_, s| {
                    let mut st = ghost(p.on_window, s);
                    st.border.radius = 10.0.into();
                    st
                })
        };
        let mut items = column![
            container(text(tf!("{what} from {from} into {into}'s version", what = what, from = short(&v.sources[block.version].label), into = into)).size(12).color(c(p.muted))).padding([6, 12]),
            item(icons::UP, t!("Add above"), "A", above, Op::Above),
            item(icons::DOWN_ARROW, t!("Add below"), "B", below, Op::Below),
        ]
        .spacing(2);
        if !h.dst.is_empty() {
            items = items.push(item(icons::SWAP, t!("Replace"), "R", tf!("{lines} of the merge", lines = lines(&h.dst)), Op::Replace));
        }
        let card = container(items).padding(6).width(380).style(move |_| container::Style {
            shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.35), offset: Vector::new(0.0, 8.0), blur_radius: 24.0 },
            ..surface(p.container_high, p.on_window, 16.0)
        });
        let away = mouse_area(container(space()).width(Fill).height(Fill)).on_press(Message::CloseMenu).on_right_press(Message::CloseMenu);
        Some(stack![away, pin(card).x(at.x).y(at.y).width(Fill).height(Fill)].into())
    }

    fn ignore_sheet<'a>(&'a self, p: &'a Palette) -> Option<Element<'a, Message>> {
        let sheet = self.ignore.as_ref()?;
        let options = column(sheet.scopes.iter().enumerate().map(|(i, (scope, hides, detail))| {
            let picked = i == sheet.picked;
            let dot = container(if picked { Element::from(container(space()).width(10).height(10).style(move |_| surface(p.accent, p.accent, 5.0))) } else { space().into() })
                .center(20)
                .style(move |_| container::Style { border: Border { radius: 10.0.into(), width: 2.0, color: c(if picked { p.accent } else { p.outline }) }, ..Default::default() });
            let count: Element<'a, Message> = if *hides > 0 { chip(tnf!("hides {count}", "hides {count}", count = *hides), if picked { p.accent } else { p.muted }) } else { space().into() };
            button(row![dot, column![text(scope.label()).size(14), text(detail.as_str()).size(12).color(c(p.muted))].spacing(2).width(Fill), count].spacing(14).align_y(iced::Center))
                .width(Fill)
                .padding([12, 14])
                .on_press(Message::PickScope(i))
                .style(move |_, s| {
                    let mut st = if picked { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) };
                    st.border.radius = 14.0.into();
                    st
                })
                .into()
        }))
        .spacing(4);
        let card = container(
            column![
                text(t!("Ignore conflicts")).size(20),
                text(t!("Ignored conflicts leave the queue. You can bring them back from Ignored.")).size(13).color(c(p.muted)),
                options,
                row![
                    space().width(Fill),
                    button(text(t!("Cancel")).size(14)).padding([10, 18]).on_press(Message::CloseIgnore).style(move |_, s| ghost(p.on_window, s)),
                    button(row![icon(icons::HIDE, 16, p.on_accent), text(t!("Ignore")).size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press(Message::ConfirmIgnore).style(move |_, s| pill(p.accent, p.on_accent, s)),
                ]
                .spacing(8),
            ]
            .spacing(14),
        )
        .padding(22)
        .width(580)
        .style(move |_| container::Style { shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.45), offset: Vector::new(0.0, 12.0), blur_radius: 40.0 }, ..surface(p.container_high, p.on_window, 28.0) });
        let scrim = mouse_area(container(space()).width(Fill).height(Fill).style(|_| container::Style { background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.5))), ..Default::default() })).on_press(Message::CloseIgnore);
        Some(stack![scrim, container(opaque(card)).center(Fill)].into())
    }

    fn ignored<'a>(&'a self, library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
        let card = |label: String, detail: String, msg: Message| -> Element<'a, Message> {
            container(
                row![
                    icon(icons::HIDE, 18, p.muted),
                    column![text(label).size(14), text(detail).size(12).color(c(p.muted))].spacing(2).width(Fill),
                    button(text(t!("Show again")).size(13)).padding([8, 14]).on_press(msg).style(move |_, s| pill(p.container_high, p.on_window, s)),
                ]
                .spacing(14)
                .align_y(iced::Center),
            )
            .padding([12, 16])
            .style(move |_| surface(p.container, p.on_window, 16.0))
            .into()
        };
        let mut list = column![].spacing(8);
        for key in &library.settings.ignored {
            let (label, detail) = describe_key(key);
            list = list.push(card(label, detail, Message::UnignoreKey(key.clone())));
        }
        for rule in library.settings.ignore_rules.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            list = list.push(card(rule.to_owned(), t!("Ignore rule"), Message::UnignoreRule(rule.to_owned())));
        }
        if library.settings.ignored.is_empty() && library.settings.ignore_rules.trim().is_empty() {
            list = list.push(text(t!("Nothing ignored yet. Ignore a conflict from its action bar.")).size(13).color(c(p.muted)));
        }
        container(column![text(t!("Ignored")).size(20), text(t!("What keeps conflicts out of the queue: single conflicts, whole folders, and ignore rules.")).size(13).color(c(p.muted)), scrollable(list).height(Fill)].spacing(12).max_width(760))
            .padding(20)
            .width(Fill)
            .height(Fill)
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }
}

/// Reads every version of object `i`, and the base game's. Blocking.
fn load_versions(a: &Analysis, i: usize, game_dir: Option<PathBuf>) -> Versions {
    let c = &a.report.objects[i];
    let origins: Vec<(usize, (String, String, PathBuf, String))> =
        c.definitions.iter().filter_map(|d| Some((d.mod_position, (a.ids[d.mod_position].clone(), a.names[d.mod_position].clone(), a.roots[d.mod_position].clone()?, d.file.clone())))).collect();
    let loaded = merge_dialog_load(&c.category, &c.key, game_dir.as_deref(), origins.iter().map(|(_, o)| o.clone()).collect());
    // Which source each read version is, to place the winner and positions.
    let positions: Vec<usize> = loaded.sources.iter().map(|s| origins.iter().find(|(_, o)| o.1 == s.label && o.3 == s.file).map_or(0, |(p, _)| *p)).collect();
    let winner = c.winner().and_then(|(w, reason)| {
        let d = &c.definitions[w];
        loaded.sources.iter().position(|s| s.file == d.file && s.label == a.names[d.mod_position]).map(|at| (at, reason))
    });
    Versions { item: Item::Object(i), vanilla: loaded.vanilla, sources: loaded.sources, positions, prints: loaded.prints, missing: loaded.missing, winner }
}

fn mods_of_object(c: &ObjectConflict) -> BTreeSet<usize> {
    c.definitions.iter().map(|d| d.mod_position).collect()
}

/// What an ignored key covers, for the Ignored list.
fn describe_key(key: &str) -> (String, String) {
    if let Some(rest) = key.strip_prefix("object:") {
        (rest.replacen(':', "/", 1), t!("Object"))
    } else if let Some(rest) = key.strip_prefix("objects-in:") {
        (rest.to_owned(), t!("Every object in this folder"))
    } else if let Some(rest) = key.strip_prefix("files-in:") {
        (rest.to_owned(), t!("Every file in this folder"))
    } else if let Some(rest) = key.strip_prefix("file:") {
        (rest.to_owned(), t!("File"))
    } else {
        (key.to_owned(), String::new())
    }
}

fn banner<'a>(p: &'a Palette, message: String) -> Element<'a, Message> {
    container(row![icon(icons::INFO, 20, p.accent), text(message).size(13)].spacing(12).align_y(iced::Center))
        .padding([14, 16])
        .width(Fill)
        .style(move |_| container::Style { background: Some(Background::Color(alpha(p.accent, 0.14))), ..surface(p.container, p.on_window, 16.0) })
        .into()
}

/// The base game's version, for reference: read-only and narrower.
fn base_pane<'a>(p: &'a Palette, vanilla: &'a Source, version: Option<&'a str>) -> Element<'a, Message> {
    let rows = column(vanilla.text.lines().enumerate().map(|(k, line)| {
        row![text(format!("{:>3}", k + 1)).font(MONO).size(12).color(alpha(p.muted, 0.6)), text(line.replace('\t', "    ")).font(MONO).size(12.5).color(c(p.muted)).wrapping(text::Wrapping::None)].spacing(12).height(20).align_y(iced::Center).into()
    }));
    container(
        column![
            row![
                container(icon(icons::GAMES, 14, p.muted)).padding([5, 7]).style(move |_| surface(p.container_high, p.muted, 999.0)),
                column![text(t!("Base game")).size(15), text(version.map_or_else(|| vanilla.file.clone(), |v| format!("Stellaris {v}"))).font(MONO).size(11).color(c(p.muted))].spacing(2).width(Fill),
            ]
            .spacing(10)
            .align_y(iced::Center),
            container(scrollable(container(rows).padding([8, 12])).direction(scrollable::Direction::Both { vertical: scrollable::Scrollbar::default(), horizontal: scrollable::Scrollbar::default() }).width(Fill).height(Fill))
                .width(Fill)
                .height(Fill)
                .style(move |_| surface(p.window, p.on_window, 12.0)),
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Length::FillPortion(1))
    .height(Fill)
    .style(move |_| surface(p.container_low, p.on_window, 20.0))
    .into()
}

/// The blocks where `src` and `dst` differ, in order, by the longest
/// common run of lines. Trailing spaces don't count; indentation does, or
/// a nested `}` would match the object's closing one.
pub fn hunks<S: AsRef<str>>(src: &[&str], dst: &[S]) -> Vec<Hunk> {
    let (n, m) = (src.len(), dst.len());
    let same = |i: usize, j: usize| src[i].trim_end() == dst[j].as_ref().trim_end();
    // lcs[i][j]: the longest common run of src[i..] and dst[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if same(i, j) { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    while i < n || j < m {
        if i < n && j < m && same(i, j) {
            if let Some((si, sj)) = open.take() {
                out.push(Hunk { src: si..i, dst: sj..j });
            }
            i += 1;
            j += 1;
            continue;
        }
        open.get_or_insert((i, j));
        if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            j += 1;
        } else {
            i += 1;
        }
    }
    if let Some((si, sj)) = open {
        out.push(Hunk { src: si..n, dst: sj..m });
    }
    balance(out, src, dst)
}

/// Widens each block until its braces balance on both sides, so a block
/// is whole objects: the diff alone would leave a block's closing `}`
/// out, since another block's `}` matches it, and adding the block would
/// then land inside the merge's next object. Blocks that come to touch
/// are joined.
fn balance<S: AsRef<str>>(hunks: Vec<Hunk>, src: &[&str], dst: &[S]) -> Vec<Hunk> {
    let depth = |lines: &mut dyn Iterator<Item = &str>| -> i32 { lines.map(|l| l.split('#').next().unwrap_or("")).map(|l| l.matches('{').count() as i32 - l.matches('}').count() as i32).sum() };
    let same = |i: usize, j: usize| src[i].trim_end() == dst[j].as_ref().trim_end();
    let mut out: Vec<Hunk> = Vec::new();
    for (k, mut h) in hunks.iter().cloned().enumerate() {
        let (next_src, next_dst) = hunks.get(k + 1).map_or((src.len(), dst.len()), |n| (n.src.start, n.dst.start));
        let (prev_src, prev_dst) = out.last().map_or((0, 0), |p| (p.src.end, p.dst.end));
        loop {
            let ds = depth(&mut src[h.src.clone()].iter().copied());
            let dd = depth(&mut dst[h.dst.clone()].iter().map(|l| l.as_ref()));
            if (ds > 0 || dd > 0) && h.src.end < next_src && h.dst.end < next_dst && same(h.src.end, h.dst.end) {
                h.src.end += 1;
                h.dst.end += 1;
            } else if (ds < 0 || dd < 0) && h.src.start > prev_src && h.dst.start > prev_dst && same(h.src.start - 1, h.dst.start - 1) {
                h.src.start -= 1;
                h.dst.start -= 1;
            } else {
                break;
            }
        }
        match out.last_mut() {
            Some(last) if last.src.end >= h.src.start && last.dst.end >= h.dst.start => {
                last.src.end = last.src.end.max(h.src.end);
                last.dst.end = last.dst.end.max(h.dst.end);
            }
            _ => out.push(h),
        }
    }
    out
}

fn status_look(p: &Palette, s: Status) -> (String, Rgb) {
    match s {
        Status::NeedsMerge => (t!("Needs merge"), p.error),
        Status::Simple => (t!("Simple"), p.warning),
        Status::File => (t!("File"), p.accent),
        Status::Done => (t!("Done"), p.success),
    }
}

fn placeholder<'a>(p: &'a Palette, title: String, about: String) -> Element<'a, Message> {
    container(column![text(title).size(20), text(about).size(13).color(c(p.muted))].spacing(8).max_width(640)).padding(20).width(Fill).height(Fill).style(move |_| surface(p.container_low, p.on_window, 24.0)).into()
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).filter(|c| c.is_alphanumeric()).take(3).collect()
}

/// A mod's name short enough for a button.
fn short(name: &str) -> String {
    let first: Vec<&str> = name.split_whitespace().take(2).collect();
    let s = first.join(" ");
    if s.len() < name.len() { format!("{}…", s.trim_end_matches(['\'', 's'])) } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_changed_added_and_removed_blocks() {
        let src = ["a", "b", "x", "c", "y", "z"];
        let dst = ["a", "b", "c", "w", "d"];
        assert_eq!(hunks(&src, &dst), vec![Hunk { src: 2..3, dst: 2..2 }, Hunk { src: 4..6, dst: 3..5 }]);
        assert_eq!(hunks(&["a"], &["a"]), vec![]);
    }

    #[test]
    fn blocks_are_whole_objects() {
        let theirs = ["x = {", "\tcost = 300", "\tgpm = {", "\t\ta = 1", "\t}", "}"];
        let ours = ["x = {", "\tcost = 250", "\tecc = {", "\t\tb = 2", "\t}", "}"];
        let h = hunks(&theirs, &ours);
        // The `gpm` block takes its own closing brace with it.
        assert!(h.iter().any(|h| h.src.contains(&2) && h.src.contains(&4)), "{h:?}");
    }
}
