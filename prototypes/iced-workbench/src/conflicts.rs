//! The Conflicts mode: the queue of conflicts the last scan found, the
//! selected one's versions side by side with the one the game uses
//! marked, and what to do about it. Mod management stays in the Mods mode.
//!
//! The version the game uses is the merge being built. Right-clicking a
//! highlighted block in another version offers to add it above or below
//! the matching block of that merge.

use std::ops::Range;

use drukhal_palette::{Palette, Rgb};
use drukhal_sample_data::{self as sample, Conflict};
use iced::widget::{button, column, container, mouse_area, pin, row, scrollable, space, stack, text};
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

/// A run of lines that differ between a version and the merge: `src` in
/// the version, `dst` in the merge. Either can be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub src: Range<usize>,
    pub dst: Range<usize>,
}

/// The open context menu: which version's block, and where it was opened.
#[derive(Debug, Clone)]
pub struct Menu {
    version: usize,
    hunk: usize,
    at: Point,
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
    menu: Option<Menu>,
}

#[derive(Debug, Clone)]
pub enum Message {
    View(View),
    Filter(Filter),
    Select(usize),
    Step(isize),
    /// Keep a version (by index), or merge with AI (`None`).
    Resolve(Option<usize>),
    /// Right-click on a block; the app fills in where with [`State::place_menu`].
    OpenMenu { version: usize, hunk: usize },
    CloseMenu,
    /// Add the menu's block above (or below) the merge's matching block.
    Add { below: bool },
    Undo,
    Reset,
    SaveMerge,
}

impl State {
    pub fn new() -> Self {
        let conflicts = sample::conflicts();
        let status = conflicts
            .iter()
            .map(|c| if c.simple.is_some() { Status::Simple } else if c.category == "localisation" { Status::Done } else { Status::NeedsMerge })
            .collect();
        let merged = vec![None; conflicts.len()];
        Self { conflicts, status, merged, undo: Vec::new(), selected: 0, view: View::Queue, filter: Filter::All, menu: None }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::View(v) => self.view = v,
            Message::Filter(f) => self.filter = f,
            Message::Select(i) => self.select(i),
            Message::Step(by) => {
                let n = self.conflicts.len() as isize;
                self.select(((self.selected as isize + by).rem_euclid(n)) as usize);
            }
            Message::Resolve(kept) => {
                let cf = &self.conflicts[self.selected];
                match kept {
                    Some(n) => println!("mockup: keep {}'s {}", cf.versions[n].mod_name, cf.key),
                    None => println!("mockup: merge or ignore {}", cf.key),
                }
                self.finish();
            }
            Message::OpenMenu { version, hunk } => self.menu = Some(Menu { version, hunk, at: Point::ORIGIN }),
            Message::CloseMenu => self.menu = None,
            Message::Add { below } => {
                let Some(menu) = self.menu.take() else { return };
                let source: Vec<&str> = self.conflicts[self.selected].versions[menu.version].text.lines().collect();
                let merge = self.merge_lines();
                let Some(h) = hunks(&source, &merge).get(menu.hunk).cloned() else { return };
                let at = if below { h.dst.end } else { h.dst.start };
                let mut next = merge.clone();
                next.splice(at..at, source[h.src].iter().map(|l| l.to_string()));
                self.undo.push(self.merged[self.selected].clone());
                self.merged[self.selected] = Some(next);
            }
            Message::Undo => {
                if let Some(before) = self.undo.pop() {
                    self.merged[self.selected] = before;
                }
            }
            Message::Reset => {
                self.undo.push(self.merged[self.selected].take());
            }
            Message::SaveMerge => {
                println!("mockup: save the merge of {}:\n{}", self.conflicts[self.selected].key, self.merge_lines().join("\n"));
                self.finish();
            }
        }
    }

    /// Where the menu opened, in window coordinates; the app tracks the cursor.
    pub fn place_menu(&mut self, at: Point) {
        if let Some(menu) = &mut self.menu {
            menu.at = at;
        }
    }

    /// For screenshots: the menu open on the first block of another version.
    pub fn demo_menu(&mut self, at: Point) {
        let winner = self.conflicts[self.selected].winner;
        let version = (winner + 1) % self.conflicts[self.selected].versions.len();
        self.menu = Some(Menu { version, hunk: 0, at });
    }

    /// For screenshots: a block already added.
    pub fn demo_added(&mut self) {
        self.demo_menu(Point::ORIGIN);
        self.update(Message::Add { below: true });
    }

    fn select(&mut self, i: usize) {
        if i != self.selected {
            self.undo.clear();
            self.menu = None;
        }
        self.selected = i;
    }

    fn finish(&mut self) {
        self.status[self.selected] = Status::Done;
        if let Some(next) = (0..self.conflicts.len()).map(|k| (self.selected + 1 + k) % self.conflicts.len()).find(|&i| self.status[i] != Status::Done) {
            self.select(next);
        }
    }

    /// The selected conflict's merge so far.
    fn merge_lines(&self) -> Vec<String> {
        let cf = &self.conflicts[self.selected];
        self.merged[self.selected].clone().unwrap_or_else(|| cf.versions[cf.winner].text.lines().map(str::to_owned).collect())
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
        let done = self.status.iter().filter(|s| **s == Status::Done).count();
        format!("{} conflicts between 10 enabled mods · {done} done · colours from {}", self.conflicts.len(), p.source)
    }

    pub fn view<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let main: Element<'a, Message> = match self.view {
            View::Queue => row![self.queue(p), self.resolve(p)].spacing(12).into(),
            View::Patches => placeholder(p, "Patches", "The merges you've saved, as the patch mod that loads last. Each says which mods it came from and when they changed since."),
            View::Ignored => placeholder(p, "Ignored", "Conflicts you've said are fine, and the rules that hide whole folders of them."),
        };
        row![self.rail(p), main].spacing(12).height(Fill).into()
    }

    /// The context menu, drawn by the app over everything, if one is open.
    pub fn menu<'a>(&'a self, p: &'a Palette) -> Option<Element<'a, Message>> {
        let menu = self.menu.as_ref()?;
        let cf = &self.conflicts[self.selected];
        let source: Vec<&str> = cf.versions[menu.version].text.lines().collect();
        let merge = self.merge_lines();
        let h = hunks(&source, &merge).get(menu.hunk)?.clone();
        let into = short(cf.versions[cf.winner].mod_name);
        let lines = h.src.len();
        let what = if lines == 1 { "1 line".to_owned() } else { format!("{lines} lines") };
        // Where each choice lands, in the merge's line numbers.
        let (above, below) = if h.dst.is_empty() {
            (format!("Before line {}", h.dst.start + 1), format!("Before line {}", h.dst.start + 1))
        } else {
            (format!("Before line {}", h.dst.start + 1), format!("After line {}", h.dst.end))
        };
        let item = |glyph: &'static [u8], label: &'static str, hint: String, below: bool| {
            button(row![icon(glyph, 18, p.on_window), text(label).size(14).width(Fill), text(hint).size(12).color(c(p.muted))].spacing(12).align_y(iced::Center))
                .width(Fill)
                .padding([9, 12])
                .on_press(Message::Add { below })
                .style(move |_, s| {
                    let mut st = ghost(p.on_window, s);
                    st.border.radius = 10.0.into();
                    st
                })
        };
        let card = container(
            column![
                container(text(format!("{what} from {} into {into}'s version", short(cf.versions[menu.version].mod_name))).size(12).color(c(p.muted))).padding([6, 12]),
                item(icons::UP, "Add above", above, false),
                item(icons::DOWN_ARROW, "Add below", below, true),
            ]
            .spacing(2),
        )
        .padding(6)
        .width(300)
        .style(move |_| container::Style { shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.35), offset: Vector::new(0.0, 8.0), blur_radius: 24.0 }, ..surface(p.container_high, p.on_window, 16.0) });
        // A click anywhere else closes it.
        let away = mouse_area(container(space()).width(Fill).height(Fill)).on_press(Message::CloseMenu).on_right_press(Message::CloseMenu);
        Some(stack![away, pin(card).x(menu.at.x).y(menu.at.y).width(Fill).height(Fill)].into())
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
        let total = self.conflicts.len();
        let done = self.status.iter().filter(|s| **s == Status::Done).count();
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
        let shown = (0..total).filter(|&i| match self.filter {
            Filter::All => true,
            Filter::NeedsMerge => self.status[i] == Status::NeedsMerge,
            Filter::Simple => self.status[i] == Status::Simple,
            Filter::Done => self.status[i] == Status::Done,
        });
        let list = column(shown.map(|i| self.queue_row(p, i))).spacing(4);
        container(
            column![
                row![text(format!("{total} conflicts")).size(20), space().width(Fill), text("10 mods").size(12).color(c(p.muted))].align_y(iced::Center),
                progress,
                text(format!("{done} done · {} to go", total - done)).size(11).color(c(p.muted)),
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

    fn resolve<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let cf = &self.conflicts[self.selected];
        let edited = self.merged[self.selected].is_some();
        let winner = cf.versions[cf.winner].mod_name;
        let why = match (edited, cf.simple) {
            (true, _) => format!("You're building a merge on {winner}'s version, the one the game uses. Right-click a highlighted block on the left to add it above or below."),
            (false, Some(simple)) => format!("{simple}, so there's nothing to merge. In game now: {winner}'s version."),
            (false, None) => format!("In game now: {winner}'s version, because {}. Right-click a highlighted block in another version to add it to this one.", cf.why),
        };
        let step = |label: &'static str, glyph: &'static [u8], by: isize| {
            button(row![icon(glyph, 16, p.on_window), text(label).size(13)].spacing(6).align_y(iced::Center)).padding([8, 14]).on_press(Message::Step(by)).style(move |_, s| pill(p.container_high, p.on_window, s))
        };
        let head = row![
            column![text(cf.category).size(12).color(c(p.accent)), text(cf.key).font(MONO).size(24)].spacing(2).width(Fill),
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
        let panes = row((0..cf.versions.len()).map(|n| self.code_pane(p, n, &merge))).spacing(12).height(Fill);
        let actions = if edited {
            row![
                button(row![icon(icons::OK, 18, p.on_accent), text("Save merge").size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press(Message::SaveMerge).style(move |_, s| pill(p.accent, p.on_accent, s)),
                text(format!("{} lines", merge.len())).size(12).color(c(p.muted)),
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
                button(text("Ignore").size(13).color(c(p.muted))).padding([10, 14]).on_press(Message::Resolve(None)).style(move |_, s| ghost(p.on_window, s)),
            ]
        };
        let actions = container(actions.spacing(10).align_y(iced::Center)).padding(14).width(Fill).style(move |_| surface(p.container_high, p.on_window, 24.0));
        column![head, banner, panes, actions].spacing(12).width(Fill).into()
    }

    /// One version of the conflict. The game's version shows the merge so
    /// far; the others mark the blocks it lacks, which a right-click adds.
    fn code_pane<'a>(&'a self, p: &'a Palette, n: usize, merge: &[String]) -> Element<'a, Message> {
        let cf = &self.conflicts[self.selected];
        let v = &cf.versions[n];
        let wins = n == cf.winner;
        let edited = self.merged[self.selected].is_some();
        // The lines to show, and for each the block it's in, if it differs.
        let (lines, marks): (Vec<String>, Vec<Option<usize>>) = if wins {
            let mut marks = vec![None; merge.len()];
            for (m, other) in cf.versions.iter().enumerate().filter(|(m, _)| *m != n) {
                let theirs: Vec<&str> = other.text.lines().collect();
                for (k, h) in hunks(&theirs, merge).iter().enumerate() {
                    for line in h.dst.clone() {
                        marks[line] = Some(k + m * 1000);
                    }
                }
            }
            (merge.to_vec(), marks)
        } else {
            let mine: Vec<&str> = v.text.lines().collect();
            let mut marks = vec![None; mine.len()];
            for (k, h) in hunks(&mine, merge).iter().enumerate() {
                for line in h.src.clone() {
                    marks[line] = Some(k);
                }
            }
            (mine.iter().map(|l| l.to_string()).collect(), marks)
        };
        let mark = if wins { p.success } else { p.error };
        let open = self.menu.as_ref().filter(|m| m.version == n).map(|m| m.hunk);
        // Lines don't wrap; the pane scrolls sideways instead. The rows get a
        // fixed width so their highlight spans the longest line.
        let longest = lines.iter().map(|l| l.replace('\t', "    ").chars().count()).max().unwrap_or(0);
        let width = (longest as f32 * 7.6 + 70.0).max(420.0);
        let rows = column(lines.into_iter().zip(marks).enumerate().map(|(k, (line, hunk))| {
            let active = hunk.is_some() && hunk == open;
            let tint = if active { 0.32 } else { 0.16 };
            let row_el = container(
                row![
                    container(space()).width(3).height(20).style(move |_| if hunk.is_some() { surface(mark, mark, 0.0) } else { container::Style::default() }),
                    text(format!("{:>3}", k + 1)).font(MONO).size(12).color(alpha(p.muted, 0.6)),
                    text(line.replace('\t', "    ")).font(MONO).size(12.5).wrapping(text::Wrapping::None),
                ]
                .spacing(12)
                .align_y(iced::Center),
            )
            .width(width)
            .style(move |_| if hunk.is_some() { container::Style { background: Some(Background::Color(alpha(mark, tint))), ..Default::default() } } else { container::Style::default() });
            match hunk {
                // Only the other versions' blocks can be added to the merge.
                Some(h) if !wins => mouse_area(row_el).on_right_press(Message::OpenMenu { version: n, hunk: h }).interaction(mouse::Interaction::ContextMenu).into(),
                _ => row_el.into(),
            }
        }));
        let badge: Element<'a, Message> = match (wins, edited) {
            (true, true) => filled_chip("In game · your merge", p.accent, p.on_accent),
            (true, false) => filled_chip("In game", p.accent, p.on_accent),
            _ => text("right-click a block to add it").size(11).color(c(p.muted)).into(),
        };
        let position = sample::mods().iter().position(|m| m.name == v.mod_name).map_or(0, |i| i + 1);
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
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Length::FillPortion(1))
        .height(Fill)
        .style(move |_| container::Style { border: Border { radius: 20.0.into(), width: if wins { 2.0 } else { 0.0 }, color: c(p.accent) }, ..surface(p.container, p.on_window, 20.0) })
        .into()
    }
}

/// The blocks where `src` and `dst` differ, in order, by the longest
/// common run of lines (compared without surrounding spaces).
pub fn hunks(src: &[&str], dst: &[String]) -> Vec<Hunk> {
    let (n, m) = (src.len(), dst.len());
    let same = |i: usize, j: usize| src[i].trim() == dst[j].trim();
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
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    #[test]
    fn finds_changed_added_and_removed_blocks() {
        let src = ["a", "b", "x", "c", "y", "z"];
        let dst = lines("a\nb\nc\nw\nd");
        assert_eq!(hunks(&src, &dst), vec![Hunk { src: 2..3, dst: 2..2 }, Hunk { src: 4..6, dst: 3..5 }]);
        assert_eq!(hunks(&["a"], &lines("a")), vec![]);
        assert_eq!(hunks(&[], &lines("a")), vec![Hunk { src: 0..0, dst: 0..1 }]);
    }

    #[test]
    fn adds_a_block_above_or_below() {
        let mut state = State::new();
        // building_research_lab_1: the game uses the second version.
        let winner_len = state.merge_lines().len();
        state.demo_menu(Point::ORIGIN);
        state.update(Message::Add { below: false });
        let above = state.merge_lines();
        assert_eq!(above.len(), winner_len + 1, "one line, `cost = {{ minerals = 300 }}`");
        assert!(above[4].contains("300") && above[5].contains("250"), "added above the game's cost line");
        state.update(Message::Undo);
        assert_eq!(state.merge_lines().len(), winner_len);
        state.demo_menu(Point::ORIGIN);
        state.update(Message::Add { below: true });
        let below = state.merge_lines();
        assert!(below[4].contains("250") && below[5].contains("300"), "added below it");
    }
}
