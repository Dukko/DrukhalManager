//! The Mods mode: what's installed, what's on and in what order, playsets
//! and collections, and launching the game. Nothing here mentions
//! conflicts; that's the other mode's job.

use drukhal_palette::{Palette, Rgb};
use drukhal_sample_data::{self as sample, Category, Mod};
use iced::widget::{button, column, container, mouse_area, row, rule, scrollable, space, text, text_input, toggler};
use iced::{Element, Fill, Length, mouse};

use crate::style::{DISPLAY, MONO, c, chip, field, ghost, icon, icons, pill, surface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    LoadOrder,
    Collections,
    Launch,
}

impl View {
    const ALL: [View; 3] = [View::LoadOrder, View::Collections, View::Launch];

    fn label(self) -> &'static str {
        match self {
            View::LoadOrder => "Load order",
            View::Collections => "Collections",
            View::Launch => "Launch & logs",
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            View::LoadOrder => icons::LIST,
            View::Collections => icons::COLLECTIONS,
            View::Launch => icons::GAMES,
        }
    }
}

/// A mod being dragged in the load order.
#[derive(Debug, Clone, Copy)]
struct Drag {
    from: usize,
    /// The row under the cursor, and whether the drop goes below it.
    over: Option<(usize, bool)>,
}

/// About how tall a load order row is, to tell its top half from its bottom.
const ROW_HEIGHT: f32 = 58.0;

pub struct State {
    mods: Vec<Mod>,
    selected: usize,
    view: View,
    search: String,
    drag: Option<Drag>,
}

#[derive(Debug, Clone)]
pub enum Message {
    View(View),
    Select(usize),
    Toggle(usize, bool),
    Move(usize, isize),
    Search(String),
    /// Pressed on a row's handle.
    DragStart(usize),
    /// The cursor moved over a row while dragging, `y` from its top.
    DragOver(usize, f32),
    /// The mouse button came up anywhere; the app sends this.
    DragEnd,
    DragCancel,
}

impl State {
    pub fn new() -> Self {
        Self { mods: sample::mods(), selected: 3, view: View::LoadOrder, search: String::new(), drag: None }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::View(v) => self.view = v,
            Message::Select(i) => self.selected = i,
            Message::Toggle(i, on) => self.mods[i].enabled = on,
            Message::Move(i, by) => {
                let to = i as isize + by;
                if to >= 0 && (to as usize) < self.mods.len() {
                    self.mods.swap(i, to as usize);
                    self.selected = to as usize;
                }
            }
            Message::Search(s) => self.search = s,
            Message::DragStart(i) => {
                self.selected = i;
                self.drag = Some(Drag { from: i, over: None });
            }
            Message::DragOver(i, y) => {
                if let Some(drag) = &mut self.drag {
                    drag.over = Some((i, y > ROW_HEIGHT / 2.0));
                }
            }
            Message::DragEnd => {
                if let Some(Drag { from, over: Some((target, below)) }) = self.drag.take() {
                    self.drop_at(from, target, below);
                }
            }
            Message::DragCancel => self.drag = None,
        }
    }

    /// Moves mod `from` next to `target`. It joins the target's category,
    /// so the list stays grouped.
    fn drop_at(&mut self, from: usize, target: usize, below: bool) {
        let category = self.mods[target].category;
        let mut to = if below { target + 1 } else { target };
        let mut moved = self.mods.remove(from);
        if from < to {
            to -= 1;
        }
        moved.category = category;
        println!("mockup: moved {} to #{}", moved.name, to + 1);
        self.mods.insert(to, moved);
        self.selected = to;
    }

    /// The name of the mod being dragged, for the app to draw by the cursor.
    pub fn dragging(&self) -> Option<&str> {
        self.drag.map(|d| self.mods[d.from].name)
    }

    /// For screenshots: a drag under way.
    pub fn demo_drag(&mut self) {
        self.drag = Some(Drag { from: 6, over: Some((1, true)) });
    }

    pub fn header_actions<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let playset = button(row![text("Playset").size(13).color(c(p.muted)), text("Grand Campaign").size(14), icon(icons::DOWN, 16, p.on_window)].spacing(8).align_y(iced::Center))
            .padding([8, 16])
            .style(move |_, s| pill(p.container_high, p.on_window, s));
        let search = text_input("Search mods", &self.search).on_input(Message::Search).padding([10, 16]).width(280).style(move |_, _| field(p, 999.0, false));
        let launch = button(row![icon(icons::PLAY, 18, p.on_accent), text("Launch Stellaris").size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center))
            .padding([10, 20])
            .style(move |_, s| pill(p.accent, p.on_accent, s));
        row![playset, search, launch].spacing(10).align_y(iced::Center).into()
    }

    pub fn status(&self, p: &Palette) -> String {
        let on = self.mods.iter().filter(|m| m.enabled).count();
        format!("{} mods installed · {on} on · Stellaris 4.1.2 · colours from {}", self.mods.len(), p.source)
    }

    pub fn view<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let main = match self.view {
            View::LoadOrder => row![self.load_order(p), self.inspector(p)].spacing(12).into(),
            View::Collections => collections(p),
            View::Launch => launch(p),
        };
        row![self.rail(p), main].spacing(12).height(Fill).into()
    }

    fn rail<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let items = View::ALL.iter().map(|&v| {
            let selected = v == self.view;
            let fg = if selected { p.on_window } else { p.muted };
            button(row![icon(v.icon(), 20, fg), text(v.label()).size(14).color(c(fg))].spacing(12).align_y(iced::Center))
                .width(Fill)
                .padding([10, 14])
                .on_press(Message::View(v))
                .style(move |_, s| if selected { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) })
                .into()
        });
        let updates = container(
            column![
                row![icon(icons::UPDATE, 16, p.accent), text("2 updates").size(13)].spacing(8).align_y(iced::Center),
                text("Steam is downloading them; they apply on the next launch.").size(12).color(c(p.muted)),
            ]
            .spacing(6),
        )
        .padding(14)
        .width(Fill)
        .style(move |_| surface(p.container, p.on_window, 16.0));
        let colours = container(
            column![
                text("Colours").size(12).color(c(p.muted)),
                text(crate::source_name(&p.source)).size(13),
                row([p.accent, p.secondary, p.container_high, p.success, p.warning, p.error].map(|col| container(space()).width(18).height(18).style(move |_| surface(col, col, 999.0)).into())).spacing(4),
            ]
            .spacing(6),
        )
        .padding(14)
        .width(Fill)
        .style(move |_| surface(p.container, p.on_window, 16.0));
        container(column![column(items).spacing(4), space().height(Fill), updates, colours].spacing(8))
            .width(220)
            .height(Fill)
            .padding(8)
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }

    fn load_order<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let query = self.search.to_lowercase();
        let mut list = column![].spacing(2);
        for category in Category::ALL {
            let in_category: Vec<usize> = (0..self.mods.len()).filter(|&i| self.mods[i].category == category && self.mods[i].name.to_lowercase().contains(&query)).collect();
            if in_category.is_empty() {
                continue;
            }
            list = list.push(container(text(category.label().to_uppercase()).size(11).color(c(p.accent))).padding([12, 16]));
            for i in in_category {
                let line = |at: bool| -> Element<'a, Message> {
                    if at { container(space()).width(Fill).height(3).style(move |_| surface(p.accent, p.accent, 2.0)).into() } else { space().height(0).into() }
                };
                let over = self.drag.and_then(|d| d.over).filter(|(t, _)| *t == i);
                list = list.push(line(over.is_some_and(|(_, below)| !below)));
                list = list.push(self.mod_row(p, i));
                list = list.push(line(over.is_some_and(|(_, below)| below)));
            }
        }
        let on = self.mods.iter().filter(|m| m.enabled).count();
        let head = row![text("Load order").size(20), space().width(Fill), chip("Sorted by category", p.outline), chip(format!("{} installed · {on} on", self.mods.len()), p.outline)].spacing(8).align_y(iced::Center).padding([4, 8]);
        container(column![head, scrollable(list).height(Fill)].spacing(8))
            .width(Length::FillPortion(3))
            .height(Fill)
            .padding(12)
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }

    fn mod_row<'a>(&'a self, p: &'a Palette, i: usize) -> Element<'a, Message> {
        let m = &self.mods[i];
        let selected = i == self.selected;
        let fg = if m.enabled { p.on_window } else { p.muted };
        // Things a player acts on in this mode: an old game version, a
        // missing requirement.
        let mut flags = row![].spacing(6);
        if m.outdated {
            flags = flags.push(chip(format!("for {}", m.version), p.warning));
        }
        let missing: Vec<&str> = sample::needs(m.name).iter().copied().filter(|need| !self.mods.iter().any(|o| o.name == *need && o.enabled)).collect();
        if !missing.is_empty() {
            flags = flags.push(chip("needs a mod", p.error));
        }
        // The handle starts a drag; the button around the row only selects.
        // Not while searching: hidden rows would make a drop land unseen.
        let handle = mouse_area(container(icon(icons::DRAG, 18, if self.search.is_empty() { p.muted } else { p.outline })).padding([8, 4]));
        let handle = if self.search.is_empty() { handle.on_press(Message::DragStart(i)).interaction(mouse::Interaction::Grab) } else { handle };
        let content = row![
            handle,
            text(format!("{:>2}", i + 1)).font(MONO).size(13).color(c(p.muted)),
            column![text(m.name).size(15).color(c(fg)), text(m.author).size(12).color(c(p.muted))].width(Fill),
            flags,
            text(m.version).font(MONO).size(12).color(c(p.muted)),
            toggler(m.enabled).on_toggle(move |on| Message::Toggle(i, on)).size(20),
        ]
        .spacing(14)
        .align_y(iced::Center);
        let dragged = self.drag.is_some_and(|d| d.from == i);
        let row_button = button(content)
            .width(Fill)
            .padding([10, 16])
            .on_press(Message::Select(i))
            .style(move |_, s| {
                let mut st = if selected { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) };
                st.border.radius = 16.0.into();
                if dragged {
                    // Where it was: an outline, so the gap reads as its old place.
                    st.background = None;
                    st.text_color = crate::style::alpha(p.on_window, 0.4);
                    st.border = iced::Border { radius: 16.0.into(), width: 1.5, color: c(p.outline) };
                }
                st
            });
        if self.drag.is_some() {
            mouse_area(row_button).on_move(move |pt| Message::DragOver(i, pt.y)).interaction(mouse::Interaction::Grabbing).into()
        } else {
            row_button.into()
        }
    }

    fn inspector<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let m = &self.mods[self.selected];
        let fact = |value: String, label: &'static str| column![text(value).size(20), text(label).size(12).color(c(p.muted))].spacing(2).width(Fill);
        let needs = sample::needs(m.name);
        let mut requirements = column![].spacing(6);
        for need in needs {
            let ok = self.mods.iter().any(|o| o.name == *need && o.enabled);
            let (glyph, col, note) = if ok { (icons::OK, p.success, "on, loads first") } else { (icons::WARNING, p.error, "turned off") };
            requirements = requirements.push(
                container(row![icon(glyph, 18, col), column![text(*need).size(14), text(note).size(12).color(c(p.muted))]].spacing(12).align_y(iced::Center))
                    .padding([10, 14])
                    .width(Fill)
                    .style(move |_| surface(p.container_high, p.on_window, 14.0)),
            );
        }
        if needs.is_empty() {
            requirements = requirements.push(text("Needs no other mod.").size(13).color(c(p.muted)));
        }
        let outdated: Element<'a, Message> = if m.outdated {
            container(row![icon(icons::INFO, 18, p.warning), text(format!("Made for Stellaris {}. It may still work; check its page.", m.version)).size(13)].spacing(10).align_y(iced::Center))
                .padding([10, 14])
                .width(Fill)
                .style(move |_| container::Style { background: Some(iced::Background::Color(crate::style::alpha(p.warning, 0.14))), ..surface(p.container_high, p.on_window, 14.0) })
                .into()
        } else {
            space().into()
        };
        let quiet = |label: &'static str, glyph: &'static [u8], msg: Option<Message>| {
            let b = button(row![icon(glyph, 16, p.on_window), text(label).size(13)].spacing(6).align_y(iced::Center)).padding([8, 14]).style(move |_, s| pill(p.container_high, p.on_window, s));
            match msg {
                Some(msg) => b.on_press(msg),
                None => b,
            }
        };
        let actions = row![
            quiet("Up", icons::UP, Some(Message::Move(self.selected, -1))),
            quiet("Down", icons::DOWN_ARROW, Some(Message::Move(self.selected, 1))),
            space().width(Fill),
            quiet("Folder", icons::FOLDER, None),
            quiet("Workshop", icons::LINK, None),
        ]
        .spacing(8);
        container(
            column![
                text(m.category.label().to_uppercase()).size(11).color(c(p.accent)),
                text(m.name).font(DISPLAY).size(28),
                text(format!("by {}", m.author)).size(13).color(c(p.muted)),
                text(sample::about(m.name)).size(14),
                outdated,
                rule::horizontal(1).style(move |_| rule::Style { color: c(p.outline.mix(p.window, 0.5)), radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true }),
                row![fact(m.version.to_owned(), "game version"), fact(fmt_n(m.files), "files"), fact(format!("#{}", self.selected + 1), "in load order")],
                text("Needs").size(16),
                scrollable(requirements).height(Fill),
                actions,
            ]
            .spacing(12),
        )
        .width(Length::FillPortion(2))
        .height(Fill)
        .padding(20)
        .style(move |_| surface(p.container, p.on_window, 24.0))
        .into()
    }
}

fn collections<'a>(p: &'a Palette) -> Element<'a, Message> {
    let card = |name: &'static str, detail: &'static str, active: bool| {
        container(
            column![
                row![text(name).size(18), space().width(Fill), if active { crate::style::filled_chip("Playing", p.accent, p.on_accent) } else { space().into() }].align_y(iced::Center),
                text(detail).size(13).color(c(p.muted)),
            ]
            .spacing(6),
        )
        .padding(18)
        .width(Fill)
        .style(move |_| if active { container::Style { border: iced::Border { radius: 20.0.into(), width: 2.0, color: c(p.accent) }, ..surface(p.container, p.on_window, 20.0) } } else { surface(p.container, p.on_window, 20.0) })
    };
    container(
        column![
            text("Playsets & collections").size(20),
            text("A playset is a load order you can switch to. A collection is one frozen in time, so Workshop updates can't break it.").size(13).color(c(p.muted)),
            card("Grand Campaign", "11 mods · edited today", true),
            card("Vanilla+", "4 mods · edited last week", false),
            card("Multiplayer with Ana (frozen)", "23 mods as of 12 October · 3 have updates since", false),
        ]
        .spacing(12)
        .max_width(760),
    )
    .padding(20)
    .width(Fill)
    .height(Fill)
    .style(move |_| surface(p.container_low, p.on_window, 24.0))
    .into()
}

fn launch<'a>(p: &'a Palette) -> Element<'a, Message> {
    let line = |when: &'static str, what: &'static str, tone: Rgb| row![container(space()).width(10).height(10).style(move |_| surface(tone, tone, 5.0)), text(when).size(13).font(MONO).color(c(p.muted)).width(150), text(what).size(14)].spacing(12).align_y(iced::Center);
    container(
        column![
            text("Launch & logs").size(20),
            text("What happened the last few times the game ran with this playset.").size(13).color(c(p.muted)),
            container(
                column![
                    line("Today 21:40", "Played 2 h 14 min. No crash.", p.success),
                    line("Today 19:02", "Crashed on load. The game's log points at Ancient Cache of Technologies.", p.error),
                    line("Yesterday", "Played 48 min. No crash.", p.success),
                ]
                .spacing(14)
            )
            .padding(18)
            .width(Fill)
            .style(move |_| surface(p.container, p.on_window, 20.0)),
            row![
                button(text("Find the crashing mod").size(14)).padding([10, 18]).style(move |_, s| pill(p.container_high, p.on_window, s)),
                button(text("Open the game's log").size(14)).padding([10, 18]).style(move |_, s| ghost(p.on_window, s)),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .max_width(760),
    )
    .padding(20)
    .width(Fill)
    .height(Fill)
    .style(move |_| container::Style { border: iced::Border { radius: 24.0.into(), ..Default::default() }, ..surface(p.container_low, p.on_window, 24.0) })
    .into()
}

fn fmt_n(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
