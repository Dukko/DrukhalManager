//! The Conflicts mode: the queue of conflicts the last scan found, the
//! selected one's versions side by side with the one the game uses
//! marked, and what to do about it. Mod management stays in the Mods mode.
//!
//! The version the game uses is the merge being built. A highlighted block
//! in another version can be added above or below the merge's matching
//! block, replace it, or be merged into it with AI: from its right-click
//! menu, or from the keyboard (`[` `]` to move between blocks, then
//! `A`, `B`, `R` or `M`).
//!
//! Comparing with the base game instead shows what each mod changed from
//! vanilla, which tells a mod's own change from a line it left alone.

use std::collections::BTreeSet;
use std::ops::Range;

use drukhal_palette::{Palette, Rgb};
use drukhal_sample_data::{self as sample, Conflict};
use iced::widget::{button, column, container, mouse_area, opaque, pin, row, scrollable, space, stack, text};
use iced::{Background, Border, Color, Element, Fill, Length, Point, Shadow, Vector, mouse};

use crate::style::{MONO, alpha, c, chip, filled_chip, ghost, icon, icons, pill, surface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Queue,
    Patches,
    Ignored,
}

impl View {
    const ALL: [View; 3] = [View::Queue, View::Patches, View::Ignored];

    fn label(self) -> &'static str {
        match self {
            View::Queue => "Queue",
            View::Patches => "Patches",
            View::Ignored => "Ignored",
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            View::Queue => icons::WARNING,
            View::Patches => icons::PATCH,
            View::Ignored => icons::HIDE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    NeedsMerge,
    Simple,
    Done,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::NeedsMerge, Filter::Simple, Filter::Done];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::NeedsMerge => "Needs merge",
            Filter::Simple => "Simple",
            Filter::Done => "Done",
        }
    }
}

/// Where a conflict stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    NeedsMerge,
    Simple,
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
    /// Insert it before the merge's matching block.
    Above,
    /// Insert it after.
    Below,
    /// Put it in place of the merge's matching block.
    Replace,
    /// Have AI merge it with the matching block.
    Ai,
}

/// A block: a version (by index) and one of its hunks against the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub version: usize,
    pub hunk: usize,
}

/// What a rule for ignoring conflicts matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// This one object.
    Object { category: String, key: String },
    /// Everything in a folder.
    Folder(String),
    /// Every conflict between exactly these mods.
    Mods(BTreeSet<String>),
}

impl Scope {
    fn matches(&self, cf: &Conflict) -> bool {
        match self {
            Scope::Object { category, key } => cf.category == category && cf.key == key,
            Scope::Folder(folder) => cf.category == folder,
            Scope::Mods(mods) => mods_of(cf) == *mods,
        }
    }

    fn label(&self) -> String {
        match self {
            Scope::Object { key, .. } => format!("Only {key}"),
            Scope::Folder(folder) => format!("Everything in {folder}"),
            Scope::Mods(mods) => format!("Whenever {} conflict", join(mods.iter().map(|m| short(m)).collect())),
        }
    }
}

/// The ignore sheet: the scopes it offers for the selected conflict, and
/// the one picked.
#[derive(Debug, Clone)]
struct IgnoreSheet {
    scopes: Vec<Scope>,
    picked: usize,
}

pub struct State {
    conflicts: Vec<Conflict>,
    status: Vec<Status>,
    /// Each conflict's merge, by line, once edited; until then it's the
    /// version the game uses.
    merged: Vec<Option<Vec<String>>>,
    /// Earlier merges of the selected conflict, for Undo.
    undo: Vec<Option<Vec<String>>>,
    selected: usize,
    view: View,
    filter: Filter,
    compare: Compare,
    /// The right-click menu: its block, and where it opened.
    menu: Option<(Block, Point)>,
    /// The block the keyboard is on.
    focus: Option<Block>,
    ignore: Option<IgnoreSheet>,
    /// Rules hiding conflicts, oldest first.
    rules: Vec<Scope>,
}

#[derive(Debug, Clone)]
pub enum Message {
    View(View),
    Filter(Filter),
    Select(usize),
    Step(isize),
    Compare(Compare),
    /// Keep a version (by index), or merge the whole object with AI (`None`).
    Resolve(Option<usize>),
    /// Right-click on a block; the app places the menu at the cursor.
    OpenMenu(Block),
    CloseMenu,
    Apply(Op, Block),
    /// Move the keyboard to the next (or previous) block.
    StepBlock(isize),
    /// Apply an operation to the block the keyboard is on.
    ApplyFocused(Op),
    Undo,
    Reset,
    SaveMerge,
    OpenIgnore,
    PickScope(usize),
    ConfirmIgnore,
    CloseIgnore,
    /// Remove an ignore rule, by index.
    Unignore(usize),
}

impl State {
    pub fn new() -> Self {
        let conflicts = sample::conflicts();
        let status = conflicts
            .iter()
            .map(|c| if c.simple.is_some() { Status::Simple } else if c.category == "localisation" { Status::Done } else { Status::NeedsMerge })
            .collect();
        let merged = vec![None; conflicts.len()];
        Self { conflicts, status, merged, undo: Vec::new(), selected: 0, view: View::Queue, filter: Filter::All, compare: Compare::Merge, menu: None, focus: None, ignore: None, rules: Vec::new() }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::View(v) => self.view = v,
            Message::Filter(f) => self.filter = f,
            Message::Select(i) => self.select(i),
            Message::Step(by) => {
                let shown: Vec<usize> = (0..self.conflicts.len()).filter(|&i| !self.hidden(i)).collect();
                if let Some(at) = shown.iter().position(|&i| i == self.selected) {
                    let n = shown.len() as isize;
                    self.select(shown[(at as isize + by).rem_euclid(n) as usize]);
                }
            }
            Message::Compare(c) => self.compare = c,
            Message::Resolve(kept) => {
                let cf = &self.conflicts[self.selected];
                match kept {
                    Some(n) => println!("mockup: keep {}'s {}", cf.versions[n].mod_name, cf.key),
                    None => println!("mockup: merge {} with AI", cf.key),
                }
                self.finish();
            }
            Message::OpenMenu(block) => {
                self.menu = Some((block, Point::ORIGIN));
                self.focus = Some(block);
            }
            Message::CloseMenu => self.menu = None,
            Message::Apply(op, block) => {
                self.menu = None;
                self.apply(op, block);
            }
            Message::StepBlock(by) => {
                let blocks = self.blocks();
                if blocks.is_empty() {
                    self.focus = None;
                    return;
                }
                let at = self.focus.and_then(|f| blocks.iter().position(|b| *b == f));
                let next = match at {
                    Some(at) => (at as isize + by).rem_euclid(blocks.len() as isize) as usize,
                    None if by < 0 => blocks.len() - 1,
                    None => 0,
                };
                self.focus = Some(blocks[next]);
            }
            Message::ApplyFocused(op) => {
                if let Some(block) = self.focus.filter(|b| self.blocks().contains(b)) {
                    self.apply(op, block);
                }
            }
            Message::Undo => {
                if let Some(before) = self.undo.pop() {
                    self.merged[self.selected] = before;
                    self.refocus();
                }
            }
            Message::Reset => {
                self.undo.push(self.merged[self.selected].take());
                self.refocus();
            }
            Message::SaveMerge => {
                println!("mockup: save the merge of {}:\n{}", self.conflicts[self.selected].key, self.merge_lines().join("\n"));
                self.finish();
            }
            Message::OpenIgnore => {
                let cf = &self.conflicts[self.selected];
                let scopes = vec![Scope::Object { category: cf.category.to_owned(), key: cf.key.to_owned() }, Scope::Folder(cf.category.to_owned()), Scope::Mods(mods_of(cf))];
                self.ignore = Some(IgnoreSheet { scopes, picked: 0 });
            }
            Message::PickScope(i) => {
                if let Some(sheet) = &mut self.ignore {
                    sheet.picked = i;
                }
            }
            Message::ConfirmIgnore => {
                if let Some(sheet) = self.ignore.take() {
                    self.rules.push(sheet.scopes[sheet.picked].clone());
                    if self.hidden(self.selected)
                        && let Some(next) = (0..self.conflicts.len()).find(|&i| !self.hidden(i) && self.status[i] != Status::Done).or_else(|| (0..self.conflicts.len()).find(|&i| !self.hidden(i)))
                    {
                        self.select(next);
                    }
                }
            }
            Message::CloseIgnore => self.ignore = None,
            Message::Unignore(i) => {
                if i < self.rules.len() {
                    self.rules.remove(i);
                }
            }
        }
    }

    /// Where the menu opened, in window coordinates; the app tracks the cursor.
    pub fn place_menu(&mut self, at: Point) {
        if let Some((_, place)) = &mut self.menu {
            *place = at;
        }
    }

    pub fn has_sheet(&self) -> bool {
        self.ignore.is_some()
    }

    /// For screenshots: the menu open on the first block of another version.
    pub fn demo_menu(&mut self, at: Point) {
        let block = self.blocks()[0];
        self.menu = Some((block, at));
        self.focus = Some(block);
    }

    /// For screenshots: a block already added.
    pub fn demo_added(&mut self) {
        let block = self.blocks()[0];
        self.apply(Op::Below, block);
    }

    /// For screenshots: the comparison with the base game.
    pub fn demo_base(&mut self) {
        self.compare = Compare::Base;
        self.focus = self.blocks().get(1).copied();
    }

    /// For screenshots: the ignore sheet with "whenever these mods conflict" picked.
    pub fn demo_ignore(&mut self) {
        self.update(Message::OpenIgnore);
        self.update(Message::PickScope(2));
    }

    fn select(&mut self, i: usize) {
        if i != self.selected {
            self.undo.clear();
            self.menu = None;
            self.focus = None;
        }
        self.selected = i;
    }

    fn finish(&mut self) {
        self.status[self.selected] = Status::Done;
        if let Some(next) = (0..self.conflicts.len()).map(|k| (self.selected + 1 + k) % self.conflicts.len()).find(|&i| self.status[i] != Status::Done && !self.hidden(i)) {
            self.select(next);
        }
    }

    /// Whether an ignore rule hides conflict `i`.
    fn hidden(&self, i: usize) -> bool {
        self.rules.iter().any(|r| r.matches(&self.conflicts[i]))
    }

    /// The selected conflict's merge so far.
    fn merge_lines(&self) -> Vec<String> {
        let cf = &self.conflicts[self.selected];
        self.merged[self.selected].clone().unwrap_or_else(|| cf.versions[cf.winner].text.lines().map(str::to_owned).collect())
    }

    /// Every block of the other versions that the merge lacks, in reading
    /// order (version by version).
    fn blocks(&self) -> Vec<Block> {
        let cf = &self.conflicts[self.selected];
        let merge = self.merge_lines();
        (0..cf.versions.len())
            .filter(|&n| n != cf.winner)
            .flat_map(|n| {
                let lines: Vec<&str> = cf.versions[n].text.lines().collect();
                hunks(&lines, &merge).into_iter().enumerate().filter(|(_, h)| !h.src.is_empty()).map(move |(hunk, _)| Block { version: n, hunk })
            })
            .collect()
    }

    /// After the merge changes, keep the keyboard on a block that exists.
    fn refocus(&mut self) {
        let blocks = self.blocks();
        self.focus = self.focus.and_then(|f| blocks.iter().copied().find(|b| *b == f).or_else(|| blocks.iter().copied().find(|b| b.version == f.version && b.hunk >= f.hunk)).or_else(|| blocks.last().copied()));
    }

    fn apply(&mut self, op: Op, block: Block) {
        let cf = &self.conflicts[self.selected];
        let source: Vec<&str> = cf.versions[block.version].text.lines().collect();
        let merge = self.merge_lines();
        let Some(h) = hunks(&source, &merge).get(block.hunk).cloned() else { return };
        let theirs: Vec<String> = source[h.src.clone()].iter().map(|l| l.to_string()).collect();
        let mut next = merge.clone();
        match op {
            Op::Above => drop(next.splice(h.dst.start..h.dst.start, theirs)),
            Op::Below => drop(next.splice(h.dst.end..h.dst.end, theirs)),
            Op::Replace => drop(next.splice(h.dst.clone(), theirs)),
            Op::Ai => {
                let merged = ai_merge(&merge[h.dst.clone()], &theirs);
                drop(next.splice(h.dst.clone(), merged));
            }
        }
        self.undo.push(self.merged[self.selected].clone());
        self.merged[self.selected] = Some(next);
        self.refocus();
    }

    pub fn header_actions<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        row![
            text("Scanned 4 min ago").size(13).color(c(p.muted)),
            button(row![icon(icons::REFRESH, 16, p.on_window), text("Rescan").size(14)].spacing(8).align_y(iced::Center)).padding([10, 18]).style(move |_, s| pill(p.container_high, p.on_window, s)),
        ]
        .spacing(12)
        .align_y(iced::Center)
        .into()
    }

    pub fn status(&self, p: &Palette) -> String {
        let done = self.status.iter().enumerate().filter(|(i, s)| **s == Status::Done && !self.hidden(*i)).count();
        let ignored = (0..self.conflicts.len()).filter(|&i| self.hidden(i)).count();
        format!("{} conflicts between 10 enabled mods · {done} done · {ignored} ignored · colours from {}", self.conflicts.len(), p.source)
    }

    pub fn view<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let main: Element<'a, Message> = match self.view {
            View::Queue => row![self.queue(p), self.resolve(p)].spacing(12).into(),
            View::Patches => placeholder(p, "Patches", "The merges you've saved, as the patch mod that loads last. Each says which mods it came from and when they changed since."),
            View::Ignored => self.ignored(p),
        };
        row![self.rail(p), main].spacing(12).height(Fill).into()
    }

    /// What the app draws over everything: the context menu or the ignore sheet.
    pub fn overlay<'a>(&'a self, p: &'a Palette) -> Option<Element<'a, Message>> {
        self.ignore_sheet(p).or_else(|| self.menu(p))
    }

    fn menu<'a>(&'a self, p: &'a Palette) -> Option<Element<'a, Message>> {
        let (block, at) = self.menu?;
        let cf = &self.conflicts[self.selected];
        let source: Vec<&str> = cf.versions[block.version].text.lines().collect();
        let merge = self.merge_lines();
        let h = hunks(&source, &merge).get(block.hunk)?.clone();
        let into = short(cf.versions[cf.winner].mod_name);
        let what = if h.src.len() == 1 { "1 line".to_owned() } else { format!("{} lines", h.src.len()) };
        let lines = |r: &Range<usize>| if r.len() == 1 { format!("line {}", r.start + 1) } else { format!("lines {}–{}", r.start + 1, r.end) };
        // Where each choice lands, in the merge's line numbers.
        let (above, below) = if h.dst.is_empty() {
            (format!("Before line {}", h.dst.start + 1), format!("Before line {}", h.dst.start + 1))
        } else {
            (format!("Before line {}", h.dst.start + 1), format!("After line {}", h.dst.end))
        };
        let item = |glyph: &'static [u8], label: &'static str, key: &'static str, hint: String, op: Op| {
            button(
                row![icon(glyph, 18, p.on_window), text(label).size(14).width(Fill), text(hint).size(12).color(c(p.muted)), text(key).size(11).font(MONO).color(c(p.muted)).width(14)]
                    .spacing(12)
                    .align_y(iced::Center),
            )
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
            container(text(format!("{what} from {} into {into}'s version", short(cf.versions[block.version].mod_name))).size(12).color(c(p.muted))).padding([6, 12]),
            item(icons::UP, "Add above", "A", above, Op::Above),
            item(icons::DOWN_ARROW, "Add below", "B", below, Op::Below),
        ]
        .spacing(2);
        if !h.dst.is_empty() {
            items = items.push(item(icons::SWAP, "Replace", "R", format!("{} of the merge", lines(&h.dst)), Op::Replace));
        }
        items = items.push(container(space()).height(1).width(Fill).style(move |_| surface(p.outline.mix(p.container_high, 0.6), p.on_window, 0.0)));
        items = items.push(item(icons::AI, "Merge this block with AI", "M", "≈ $0.001".to_owned(), Op::Ai));
        let card = container(items).padding(6).width(380).style(move |_| container::Style {
            shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.35), offset: Vector::new(0.0, 8.0), blur_radius: 24.0 },
            ..surface(p.container_high, p.on_window, 16.0)
        });
        // A click anywhere else closes it.
        let away = mouse_area(container(space()).width(Fill).height(Fill)).on_press(Message::CloseMenu).on_right_press(Message::CloseMenu);
        Some(stack![away, pin(card).x(at.x).y(at.y).width(Fill).height(Fill)].into())
    }

    /// Ignore, with a choice of how much, and how many conflicts each hides.
    fn ignore_sheet<'a>(&'a self, p: &'a Palette) -> Option<Element<'a, Message>> {
        let sheet = self.ignore.as_ref()?;
        let options = column(sheet.scopes.iter().enumerate().map(|(i, scope)| {
            let picked = i == sheet.picked;
            let hides = (0..self.conflicts.len()).filter(|&k| !self.hidden(k) && scope.matches(&self.conflicts[k])).count();
            let detail = match scope {
                Scope::Object { .. } => "Just this conflict. A new conflict over the same object shows again.",
                Scope::Folder(_) => "Every conflict in this folder, now and after later scans.",
                Scope::Mods(_) => "Any conflict between exactly these mods, wherever it is.",
            };
            let dot = container(if picked { Element::from(container(space()).width(10).height(10).style(move |_| surface(p.accent, p.accent, 5.0))) } else { space().into() })
                .center(20)
                .style(move |_| container::Style { border: Border { radius: 10.0.into(), width: 2.0, color: c(if picked { p.accent } else { p.outline }) }, ..Default::default() });
            button(
                row![
                    dot,
                    column![text(scope.label()).size(14), text(detail).size(12).color(c(p.muted))].spacing(2).width(Fill),
                    chip(if hides == 1 { "hides 1".to_owned() } else { format!("hides {hides}") }, if picked { p.accent } else { p.muted }),
                ]
                .spacing(14)
                .align_y(iced::Center),
            )
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
                text("Ignore conflicts").size(20),
                text("Ignored conflicts leave the queue. You can bring them back from Ignored.").size(13).color(c(p.muted)),
                options,
                row![
                    space().width(Fill),
                    button(text("Cancel").size(14)).padding([10, 18]).on_press(Message::CloseIgnore).style(move |_, s| ghost(p.on_window, s)),
                    button(row![icon(icons::HIDE, 16, p.on_accent), text("Ignore").size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press(Message::ConfirmIgnore).style(move |_, s| pill(p.accent, p.on_accent, s)),
                ]
                .spacing(8),
            ]
            .spacing(14),
        )
        .padding(22)
        .width(560)
        .style(move |_| container::Style { shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.45), offset: Vector::new(0.0, 12.0), blur_radius: 40.0 }, ..surface(p.container_high, p.on_window, 28.0) });
        let scrim = mouse_area(container(space()).width(Fill).height(Fill).style(|_| container::Style { background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.5))), ..Default::default() })).on_press(Message::CloseIgnore);
        Some(stack![scrim, container(opaque(card)).center(Fill)].into())
    }

    /// A narrow rail: this mode's views are few and the panes need the room.
    fn rail<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let items = View::ALL.iter().map(|&v| {
            let selected = v == self.view;
            let fg = if selected { p.on_window } else { p.muted };
            column![
                button(container(icon(v.icon(), 20, fg)).center_x(Fill)).width(52).padding([6, 0]).on_press(Message::View(v)).style(move |_, s| if selected { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) }),
                text(v.label()).size(11).color(c(fg)),
            ]
            .spacing(4)
            .align_x(iced::Center)
            .into()
        });
        container(column(items).spacing(14).align_x(iced::Center)).width(76).height(Fill).padding([14, 8]).style(move |_| surface(p.container_low, p.on_window, 24.0)).into()
    }

    fn queue<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let visible: Vec<usize> = (0..self.conflicts.len()).filter(|&i| !self.hidden(i)).collect();
        let total = visible.len();
        let done = visible.iter().filter(|&&i| self.status[i] == Status::Done).count();
        let ignored = self.conflicts.len() - total;
        let progress = {
            let w = 300.0 * done as f32 / total.max(1) as f32;
            row![container(space()).width(w).height(8).style(move |_| surface(p.success, p.success, 4.0)), container(space()).width(Fill).height(8).style(move |_| surface(p.container_high, p.container_high, 4.0))].spacing(2)
        };
        let filters = row(Filter::ALL.iter().map(|&f| {
            let on = f == self.filter;
            button(text(f.label()).size(12))
                .padding([5, 12])
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
        .spacing(6);
        let shown = visible.into_iter().filter(|&i| match self.filter {
            Filter::All => true,
            Filter::NeedsMerge => self.status[i] == Status::NeedsMerge,
            Filter::Simple => self.status[i] == Status::Simple,
            Filter::Done => self.status[i] == Status::Done,
        });
        let list = column(shown.map(|i| self.queue_row(p, i))).spacing(4);
        let summary = if ignored > 0 { format!("{done} done · {} to go · {ignored} ignored", total - done) } else { format!("{done} done · {} to go", total - done) };
        container(
            column![
                row![text(format!("{total} conflicts")).size(20), space().width(Fill), text("10 mods").size(12).color(c(p.muted))].align_y(iced::Center),
                progress,
                text(summary).size(11).color(c(p.muted)),
                filters,
                scrollable(list).height(Fill),
            ]
            .spacing(10),
        )
        .width(350)
        .height(Fill)
        .padding(14)
        .style(move |_| surface(p.container_low, p.on_window, 24.0))
        .into()
    }

    fn queue_row<'a>(&'a self, p: &'a Palette, i: usize) -> Element<'a, Message> {
        let cf = &self.conflicts[i];
        let selected = i == self.selected;
        let (label, tone) = status_look(p, self.status[i]);
        let edited = self.merged[i].is_some() && self.status[i] != Status::Done;
        let mods = cf.versions.iter().map(|v| initials(v.mod_name)).collect::<Vec<_>>().join(" · ");
        let content = row![
            container(space()).width(4).height(40).style(move |_| surface(tone, tone, 2.0)),
            column![text(cf.key).font(MONO).size(13).wrapping(text::Wrapping::Glyph), text(format!("{} · {mods}", cf.category)).size(11).color(c(p.muted))].spacing(4).width(Fill),
            if edited { chip("Editing", p.accent) } else { chip(label, tone) },
        ]
        .spacing(12)
        .align_y(iced::Center);
        button(content)
            .width(Fill)
            .padding([10, 12])
            .on_press(Message::Select(i))
            .style(move |_, s| {
                let mut st = if selected { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) };
                st.border.radius = 16.0.into();
                st
            })
            .into()
    }

    fn ignored<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let mut list = column![].spacing(8);
        for (i, rule) in self.rules.iter().enumerate() {
            let hides = self.conflicts.iter().filter(|cf| rule.matches(cf)).count();
            list = list.push(
                container(
                    row![
                        icon(icons::HIDE, 18, p.muted),
                        column![text(rule.label()).size(14), text(if hides == 1 { "hides 1 conflict".to_owned() } else { format!("hides {hides} conflicts") }).size(12).color(c(p.muted))].spacing(2).width(Fill),
                        button(text("Show again").size(13)).padding([8, 14]).on_press(Message::Unignore(i)).style(move |_, s| pill(p.container_high, p.on_window, s)),
                    ]
                    .spacing(14)
                    .align_y(iced::Center),
                )
                .padding([12, 16])
                .style(move |_| surface(p.container, p.on_window, 16.0)),
            );
        }
        if self.rules.is_empty() {
            list = list.push(text("Nothing ignored yet. Ignore a conflict from its action bar.").size(13).color(c(p.muted)));
        }
        container(column![text("Ignored").size(20), text("Rules that keep conflicts out of the queue, newest last.").size(13).color(c(p.muted)), list].spacing(12).max_width(720))
            .padding(20)
            .width(Fill)
            .height(Fill)
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }

    fn resolve<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        if self.hidden(self.selected) {
            return placeholder(p, "Nothing to resolve", "Every conflict here is done or ignored.");
        }
        let cf = &self.conflicts[self.selected];
        let edited = self.merged[self.selected].is_some();
        let winner = cf.versions[cf.winner].mod_name;
        let why = match (self.compare, edited, cf.simple) {
            (Compare::Base, _, _) => "Coloured lines are what each mod changed from the base game; green where every mod made the same change. A bar at the left marks what you can still add to the merge.".to_owned(),
            (_, true, _) => format!("You're building a merge on {winner}'s version, the one the game uses. Right-click a highlighted block on the left, or use [ ] and A B R M."),
            (_, false, Some(simple)) => format!("{simple}, so there's nothing to merge. In game now: {winner}'s version."),
            (_, false, None) => format!("In game now: {winner}'s version, because {}. Right-click a highlighted block in another version to add it to this one.", cf.why),
        };
        let step = |label: &'static str, glyph: &'static [u8], by: isize| {
            button(row![icon(glyph, 16, p.on_window), text(label).size(13)].spacing(6).align_y(iced::Center)).padding([8, 14]).on_press(Message::Step(by)).style(move |_, s| pill(p.container_high, p.on_window, s))
        };
        // Compare with the merge, or with the base game when it has the object.
        let segment = |label: &'static str, mode: Compare| {
            let on = self.compare == mode;
            let enabled = mode == Compare::Merge || cf.vanilla.is_some();
            button(text(label).size(12).color(c(if enabled { if on { p.on_window } else { p.muted } } else { p.outline })))
                .padding([6, 12])
                .on_press_maybe(enabled.then_some(Message::Compare(mode)))
                .style(move |_, s| if on { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) })
        };
        let compare = container(row![text("Compare with").size(12).color(c(p.muted)), segment("the merge", Compare::Merge), segment("the base game", Compare::Base)].spacing(4).align_y(iced::Center))
            .padding([2, 4])
            .style(move |_| outlined_soft(p));
        let head = row![
            column![text(cf.category).size(12).color(c(p.accent)), text(cf.key).font(MONO).size(24)].spacing(2).width(Fill),
            compare,
            step("Previous", icons::PREVIOUS, -1),
            step("Next", icons::NEXT, 1),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .padding([0, 8]);
        let banner = container(row![icon(icons::INFO, 20, p.accent), text(why).size(13)].spacing(12).align_y(iced::Center))
            .padding([14, 16])
            .width(Fill)
            .style(move |_| container::Style { background: Some(Background::Color(alpha(p.accent, 0.14))), ..surface(p.container, p.on_window, 16.0) });
        let merge = self.merge_lines();
        let mut panes = row![].spacing(12).height(Fill);
        if self.compare == Compare::Base
            && let Some(vanilla) = cf.vanilla
        {
            panes = panes.push(base_pane(p, vanilla));
        }
        for n in 0..cf.versions.len() {
            panes = panes.push(self.code_pane(p, n, &merge));
        }
        let actions = if edited {
            row![
                button(row![icon(icons::OK, 18, p.on_accent), text("Save merge").size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press(Message::SaveMerge).style(move |_, s| pill(p.accent, p.on_accent, s)),
                text(format!("{} lines · Ctrl+S", merge.len())).size(12).color(c(p.muted)),
                space().width(Fill),
                button(text("Undo").size(13)).padding([10, 16]).on_press_maybe((!self.undo.is_empty()).then_some(Message::Undo)).style(move |_, s| pill(p.container, p.on_window, s)),
                button(text("Start over").size(13).color(c(p.muted))).padding([10, 14]).on_press(Message::Reset).style(move |_, s| ghost(p.on_window, s)),
            ]
        } else {
            let mut keep = row![].spacing(8);
            for (n, v) in cf.versions.iter().enumerate() {
                keep = keep.push(button(text(format!("Keep {}", short(v.mod_name))).size(13)).padding([10, 16]).on_press(Message::Resolve(Some(n))).style(move |_, s| pill(p.container, p.on_window, s)));
            }
            row![
                button(row![icon(icons::AI, 18, p.on_accent), text("Merge with AI").size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press(Message::Resolve(None)).style(move |_, s| pill(p.accent, p.on_accent, s)),
                text("≈ $0.004 · Claude").size(12).color(c(p.muted)),
                space().width(Fill),
                keep,
                button(row![icon(icons::HIDE, 16, p.muted), text("Ignore…").size(13).color(c(p.muted))].spacing(6).align_y(iced::Center)).padding([10, 14]).on_press(Message::OpenIgnore).style(move |_, s| ghost(p.on_window, s)),
            ]
        };
        let actions = container(actions.spacing(10).align_y(iced::Center)).padding(14).width(Fill).style(move |_| surface(p.container_high, p.on_window, 24.0));
        column![head, banner, panes, actions].spacing(12).width(Fill).into()
    }

    /// One version of the conflict. The game's version shows the merge so
    /// far; the others mark the blocks it lacks, which a right-click or the
    /// keyboard adds. Compared with the base game, the colours show what
    /// each mod changed from vanilla instead.
    fn code_pane<'a>(&'a self, p: &'a Palette, n: usize, merge: &[String]) -> Element<'a, Message> {
        let cf = &self.conflicts[self.selected];
        let v = &cf.versions[n];
        let wins = n == cf.winner;
        let edited = self.merged[self.selected].is_some();
        let lines: Vec<String> = if wins { merge.to_vec() } else { v.text.lines().map(str::to_owned).collect() };
        let as_str: Vec<&str> = lines.iter().map(String::as_str).collect();
        // For each line: the block it's in (other versions only), and
        // whether it differs from what it's compared with.
        let mut block = vec![None; lines.len()];
        let mut differs = vec![false; lines.len()];
        if wins {
            for other in cf.versions.iter().enumerate().filter(|(m, _)| *m != n).map(|(_, o)| o) {
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
        // Against the base game: which lines this version changed, and
        // which of those every mod changed the same way.
        let mut from_base = vec![false; lines.len()];
        let mut shared = vec![false; lines.len()];
        if self.compare == Compare::Base
            && let Some(vanilla) = cf.vanilla
        {
            let base: Vec<String> = vanilla.lines().map(str::to_owned).collect();
            for h in hunks(&as_str, &base) {
                h.src.for_each(|l| from_base[l] = true);
            }
            // Same change: every version changed a line from the base game
            // into this one. A lone brace says nothing, so it never counts.
            let changes: Vec<Vec<String>> = cf
                .versions
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
        let base_mode = self.compare == Compare::Base && cf.vanilla.is_some();
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
            // The bar: this line can still go into the merge. The tint: it
            // differs from what it's compared with.
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
                // Only the other versions' blocks can go into the merge.
                Some(b) => mouse_area(row_el).on_right_press(Message::OpenMenu(b)).interaction(mouse::Interaction::ContextMenu).into(),
                None => row_el.into(),
            }
        }));
        let badge: Element<'a, Message> = match (wins, edited) {
            (true, true) => filled_chip("In game · your merge", p.accent, p.on_accent),
            (true, false) => filled_chip("In game", p.accent, p.on_accent),
            _ => text("right-click a block").size(11).color(c(p.muted)).into(),
        };
        let position = sample::mods().iter().position(|m| m.name == v.mod_name).map_or(0, |i| i + 1);
        let keys: Element<'a, Message> = if wins {
            space().into()
        } else {
            text("[ ]  blocks   A above   B below   R replace   M AI").font(MONO).size(11).color(c(p.muted)).into()
        };
        container(
            column![
                row![
                    container(text(format!("#{position}")).size(11).color(c(p.muted))).padding([5, 8]).style(move |_| surface(p.container_high, p.muted, 999.0)),
                    column![text(v.mod_name).size(15), text(v.file).font(MONO).size(11).color(c(p.muted))].spacing(2).width(Fill),
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
}

/// The base game's version, for reference: read-only and narrower.
fn base_pane<'a>(p: &'a Palette, vanilla: &'a str) -> Element<'a, Message> {
    let rows = column(vanilla.lines().enumerate().map(|(k, line)| {
        row![
            text(format!("{:>3}", k + 1)).font(MONO).size(12).color(alpha(p.muted, 0.6)),
            text(line.replace('\t', "    ")).font(MONO).size(12.5).color(c(p.muted)).wrapping(text::Wrapping::None),
        ]
        .spacing(12)
        .height(20)
        .align_y(iced::Center)
        .into()
    }));
    container(
        column![
            row![
                container(icon(icons::GAMES, 14, p.muted)).padding([5, 7]).style(move |_| surface(p.container_high, p.muted, 999.0)),
                column![text("Base game").size(15), text("Stellaris 4.1.2").font(MONO).size(11).color(c(p.muted))].spacing(2).width(Fill),
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

/// Stands in for asking a model to merge one block: keeps the merge's
/// lines, and adds the lines of `theirs` that set something the merge's
/// don't (by the key before `=`). The real app sends both blocks, with the
/// object around them, to the configured provider.
fn ai_merge(ours: &[String], theirs: &[String]) -> Vec<String> {
    let key = |l: &str| l.split('=').next().map(|k| k.trim().to_owned()).unwrap_or_default();
    let have: BTreeSet<String> = ours.iter().map(|l| key(l)).collect();
    ours.iter().cloned().chain(theirs.iter().filter(|l| !have.contains(&key(l))).cloned()).collect()
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
/// then land inside the merge's next object. The lines taken in are the
/// same on both sides. Blocks that come to touch are joined.
fn balance<S: AsRef<str>>(hunks: Vec<Hunk>, src: &[&str], dst: &[S]) -> Vec<Hunk> {
    let depth = |lines: &mut dyn Iterator<Item = &str>| -> i32 {
        lines.map(|l| l.split('#').next().unwrap_or("")).map(|l| l.matches('{').count() as i32 - l.matches('}').count() as i32).sum()
    };
    let same = |i: usize, j: usize| src[i].trim_end() == dst[j].as_ref().trim_end();
    let mut out: Vec<Hunk> = Vec::new();
    for (k, mut h) in hunks.iter().cloned().enumerate() {
        // Not past the next block, nor back into the previous one.
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

fn mods_of(cf: &Conflict) -> BTreeSet<String> {
    cf.versions.iter().map(|v| v.mod_name.to_owned()).collect()
}

fn outlined_soft(p: &Palette) -> container::Style {
    container::Style { border: Border { radius: 999.0.into(), width: 1.0, color: alpha(p.outline, 0.5) }, ..Default::default() }
}

fn status_look(p: &Palette, s: Status) -> (&'static str, Rgb) {
    match s {
        Status::NeedsMerge => ("Needs merge", p.error),
        Status::Simple => ("Simple", p.warning),
        Status::Done => ("Done", p.success),
    }
}

fn placeholder<'a>(p: &'a Palette, title: &'static str, about: &'static str) -> Element<'a, Message> {
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

fn join(items: Vec<String>) -> String {
    match items.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
        assert_eq!(hunks(&[], &["a"]), vec![Hunk { src: 0..0, dst: 0..1 }]);
    }

    /// building_research_lab_1: the game uses the second version, whose
    /// line 5 is `cost = { minerals = 250 }`; the first's block 0 is its
    /// `cost = { minerals = 300 }`.
    #[test]
    fn adds_above_below_or_replaces() {
        let mut state = State::new();
        let len = state.merge_lines().len();
        let first = state.blocks()[0];
        state.update(Message::Apply(Op::Above, first));
        let above = state.merge_lines();
        assert_eq!(above.len(), len + 1);
        assert!(above[4].contains("300") && above[5].contains("250"));
        state.update(Message::Undo);
        state.update(Message::Apply(Op::Below, first));
        let below = state.merge_lines();
        assert!(below[4].contains("250") && below[5].contains("300"));
        state.update(Message::Undo);
        state.update(Message::Apply(Op::Replace, first));
        let replaced = state.merge_lines();
        assert_eq!(replaced.len(), len);
        assert!(replaced[4].contains("300") && !replaced.iter().any(|l| l.contains("250")));
    }

    /// GPM's `triggered_planet_modifier` block added below goes after ECC's
    /// `country_modifier` object, not inside it.
    #[test]
    fn blocks_are_whole_objects() {
        let mut state = State::new();
        let second = state.blocks()[1];
        state.update(Message::Apply(Op::Below, second));
        let merge = state.merge_lines();
        let at = |needle: &str| merge.iter().position(|l| l.contains(needle)).unwrap();
        let country = at("country_modifier");
        let triggered = at("triggered_planet_modifier");
        assert_eq!(merge[country + 2].trim(), "}", "country_modifier keeps its closing brace");
        assert_eq!(triggered, country + 3);
        assert_eq!(merge[triggered + 3].trim(), "}", "and the added block brings its own");
        let opened: usize = merge.iter().map(|l| l.matches('{').count()).sum();
        let closed: usize = merge.iter().map(|l| l.matches('}').count()).sum();
        assert_eq!(opened, closed);
    }

    #[test]
    fn keyboard_walks_the_blocks() {
        let mut state = State::new();
        let blocks = state.blocks();
        assert!(blocks.len() >= 2);
        state.update(Message::StepBlock(1));
        assert_eq!(state.focus, Some(blocks[0]));
        state.update(Message::StepBlock(1));
        assert_eq!(state.focus, Some(blocks[1]));
        state.update(Message::StepBlock(-1));
        state.update(Message::ApplyFocused(Op::Below));
        // The block went in, so it's no longer one; the keyboard moves on.
        assert_eq!(state.blocks().len(), blocks.len() - 1);
        assert!(state.focus.is_some());
    }

    #[test]
    fn ai_keeps_the_merge_and_adds_what_it_lacks() {
        let ours = vec!["\tcost = { minerals = 250 }".to_owned()];
        let theirs = vec!["\tcost = { minerals = 300 }".to_owned(), "\tbuildtime = 30".to_owned()];
        assert_eq!(ai_merge(&ours, &theirs), vec!["\tcost = { minerals = 250 }".to_owned(), "\tbuildtime = 30".to_owned()]);
    }

    #[test]
    fn ignoring_hides_by_scope() {
        let mut state = State::new();
        state.update(Message::OpenIgnore);
        state.update(Message::PickScope(1));
        state.update(Message::ConfirmIgnore);
        assert!(state.hidden(0), "the folder rule hides the selected conflict");
        assert!(!state.hidden(1));
        assert_ne!(state.selected, 0, "and the queue moves on");
        state.update(Message::Unignore(0));
        assert!(!state.hidden(0));
    }
}
