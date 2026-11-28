//! The Conflicts mode: the queue of conflicts the last scan found, the
//! selected one's versions side by side with the one the game uses
//! marked, and what to do about it. Mod management stays in the Mods mode.

use std::collections::HashSet;

use drukhal_palette::{Palette, Rgb};
use drukhal_sample_data::{self as sample, Conflict};
use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Background, Border, Element, Fill, Length};

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

pub struct State {
    conflicts: Vec<Conflict>,
    status: Vec<Status>,
    selected: usize,
    view: View,
    filter: Filter,
}

#[derive(Debug, Clone)]
pub enum Message {
    View(View),
    Filter(Filter),
    Select(usize),
    Step(isize),
    /// Keep a version (by index), or merge with AI (`None`).
    Resolve(Option<usize>),
}

impl State {
    pub fn new() -> Self {
        let conflicts = sample::conflicts();
        let status = conflicts
            .iter()
            .map(|c| if c.simple.is_some() { Status::Simple } else if c.category == "localisation" { Status::Done } else { Status::NeedsMerge })
            .collect();
        Self { conflicts, status, selected: 0, view: View::Queue, filter: Filter::All }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::View(v) => self.view = v,
            Message::Filter(f) => self.filter = f,
            Message::Select(i) => self.selected = i,
            Message::Step(by) => {
                let n = self.conflicts.len() as isize;
                self.selected = ((self.selected as isize + by).rem_euclid(n)) as usize;
            }
            Message::Resolve(kept) => {
                let cf = &self.conflicts[self.selected];
                match kept {
                    Some(n) => println!("mockup: keep {}'s {}", cf.versions[n].mod_name, cf.key),
                    None => println!("mockup: merge or ignore {}", cf.key),
                }
                self.status[self.selected] = Status::Done;
                if let Some(next) = (0..self.conflicts.len()).map(|k| (self.selected + 1 + k) % self.conflicts.len()).find(|&i| self.status[i] != Status::Done) {
                    self.selected = next;
                }
            }
        }
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
        let mods = cf.versions.iter().map(|v| initials(v.mod_name)).collect::<Vec<_>>().join(" · ");
        let content = row![
            container(space()).width(4).height(40).style(move |_| surface(tone, tone, 2.0)),
            column![text(cf.key).font(MONO).size(13).wrapping(text::Wrapping::Glyph), text(format!("{} · {mods}", cf.category)).size(11).color(c(p.muted))].spacing(4).width(Fill),
            chip(label, tone),
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
        let winner = cf.versions[cf.winner].mod_name;
        let why = match cf.simple {
            Some(simple) => format!("{simple}, so there's nothing to merge. In game now: {winner}'s version."),
            None => format!("In game now: {winner}'s version, because {}.", cf.why),
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
        let panes = row(cf.versions.iter().enumerate().map(|(n, _)| code_pane(p, cf, n))).spacing(12).height(Fill);
        let mut keep = row![].spacing(8);
        for (n, v) in cf.versions.iter().enumerate() {
            keep = keep.push(button(text(format!("Keep {}", short(v.mod_name))).size(13)).padding([10, 16]).on_press(Message::Resolve(Some(n))).style(move |_, s| pill(p.container, p.on_window, s)));
        }
        let actions = container(
            row![
                button(row![icon(icons::AI, 18, p.on_accent), text("Merge with AI").size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center)).padding([10, 18]).on_press(Message::Resolve(None)).style(move |_, s| pill(p.accent, p.on_accent, s)),
                text("≈ $0.004 · Claude").size(12).color(c(p.muted)),
                space().width(Fill),
                keep,
                button(text("Ignore").size(13).color(c(p.muted))).padding([10, 14]).on_press(Message::Resolve(None)).style(move |_, s| ghost(p.on_window, s)),
            ]
            .spacing(10)
            .align_y(iced::Center),
        )
        .padding(14)
        .width(Fill)
        .style(move |_| surface(p.container_high, p.on_window, 24.0));
        column![head, banner, panes, actions].spacing(12).width(Fill).into()
    }
}

/// One version of the conflict, its lines no other version has marked.
fn code_pane<'a>(p: &'a Palette, cf: &'a Conflict, n: usize) -> Element<'a, Message> {
    let v = &cf.versions[n];
    let wins = n == cf.winner;
    let others: HashSet<&str> = cf.versions.iter().enumerate().filter(|(m, _)| *m != n).flat_map(|(_, o)| o.text.lines().map(str::trim)).collect();
    let mark = if wins { p.success } else { p.error };
    // Lines don't wrap; the pane scrolls sideways instead. The rows get a
    // fixed width so their highlight spans the longest line.
    let longest = v.text.lines().map(|l| l.replace('\t', "    ").chars().count()).max().unwrap_or(0);
    let width = (longest as f32 * 7.6 + 70.0).max(420.0);
    let lines = column(v.text.lines().enumerate().map(|(k, line)| {
        let changed = !others.contains(line.trim());
        container(
            row![
                container(space()).width(3).height(20).style(move |_| if changed { surface(mark, mark, 0.0) } else { container::Style::default() }),
                text(format!("{:>3}", k + 1)).font(MONO).size(12).color(alpha(p.muted, 0.6)),
                text(line.replace('\t', "    ")).font(MONO).size(12.5).wrapping(text::Wrapping::None),
            ]
            .spacing(12)
            .align_y(iced::Center),
        )
        .width(width)
        .style(move |_| if changed { container::Style { background: Some(Background::Color(alpha(mark, 0.16))), ..Default::default() } } else { container::Style::default() })
        .into()
    }));
    let badge: Element<'a, Message> = if wins { filled_chip("In game", p.accent, p.on_accent) } else { space().into() };
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
            container(scrollable(container(lines).padding([8, 0])).direction(scrollable::Direction::Both { vertical: scrollable::Scrollbar::default(), horizontal: scrollable::Scrollbar::default() }).width(Fill).height(Fill)).width(Fill).height(Fill).style(move |_| surface(p.window, p.on_window, 12.0)),
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Length::FillPortion(1))
    .height(Fill)
    .style(move |_| container::Style { border: Border { radius: 20.0.into(), width: if wins { 2.0 } else { 0.0 }, color: c(p.accent) }, ..surface(p.container, p.on_window, 20.0) })
    .into()
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
