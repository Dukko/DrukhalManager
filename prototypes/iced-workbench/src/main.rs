//! Mockup: DrukhalManager's main window in iced, as a three-pane
//! workbench. A rail of views on the left, the load order in the middle
//! with each mod's conflicts drawn as a heat bar, and an inspector for the
//! selected mod on the right. Ctrl+K opens a command palette.
//!
//! Colours come from `drukhal-palette` (matugen's gtk.css, else Omarchy's
//! theme) and follow it live.

use std::path::PathBuf;

use drukhal_palette::{Palette, Rgb};
use drukhal_sample_data::{self as sample, Category, Mod};
use iced::widget::{button, column, container, mouse_area, opaque, row, rule, scrollable, space, stack, svg, text, text_input, toggler};
use iced::{Background, Border, Color, Element, Fill, Font, Length, Shadow, Subscription, Task, Theme, Vector, keyboard, window};

const SANS: Font = Font::with_name("Google Sans Flex");
const DISPLAY: Font = Font::with_name("DM Serif Display");
const MONO: Font = Font::with_name("Fira Code");

fn main() -> iced::Result {
    iced::application(Workbench::boot, Workbench::update, Workbench::view)
        .title("DrukhalManager — iced workbench mockup")
        .theme(Workbench::theme)
        .subscription(Workbench::subscription)
        .font(include_bytes!("../../../data/fonts/GoogleSansFlex.ttf").as_slice())
        .font(include_bytes!("../../../data/fonts/FiraCode.ttf").as_slice())
        .font(include_bytes!("../../../data/fonts/DMSerifDisplay.ttf").as_slice())
        .default_font(SANS)
        .window_size((1320.0, 820.0))
        .run()
}

/// `--palette <file>`, `--screenshot <out.png>`, `--palette-open`.
struct Args {
    palette: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    command_palette: bool,
}

fn args() -> Args {
    let mut args = Args { palette: None, screenshot: None, command_palette: false };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--palette" => args.palette = it.next().map(PathBuf::from),
            "--screenshot" => args.screenshot = it.next().map(PathBuf::from),
            "--palette-open" => args.command_palette = true,
            _ => eprintln!("unknown argument {a}"),
        }
    }
    args
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    LoadOrder,
    Conflicts,
    Collections,
    Patches,
    Launch,
}

impl View {
    const ALL: [View; 5] = [View::LoadOrder, View::Conflicts, View::Collections, View::Patches, View::Launch];

    fn label(self) -> &'static str {
        match self {
            View::LoadOrder => "Load order",
            View::Conflicts => "Conflicts",
            View::Collections => "Collections",
            View::Patches => "Patches",
            View::Launch => "Launch & logs",
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            View::LoadOrder => include_bytes!("../../../data/icons/material/view-list-symbolic.svg"),
            View::Conflicts => include_bytes!("../../../data/icons/material/dialog-warning-symbolic.svg"),
            View::Collections => include_bytes!("../../../data/icons/material/io.github.dukko.DrukhalManager-collections-symbolic.svg"),
            View::Patches => include_bytes!("../../../data/icons/material/package-x-generic-symbolic.svg"),
            View::Launch => include_bytes!("../../../data/icons/material/applications-games-symbolic.svg"),
        }
    }
}

struct Workbench {
    palette: Palette,
    fixed_palette: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    mods: Vec<Mod>,
    selected: usize,
    view: View,
    search: String,
    command: Option<String>,
}

#[derive(Debug, Clone)]
enum Message {
    Palette(Palette),
    Select(usize),
    Toggle(usize, bool),
    Move(usize, isize),
    View(View),
    Search(String),
    OpenCommands,
    Command(String),
    CloseCommands,
    Shot(window::Screenshot),
}

impl Workbench {
    fn boot() -> (Self, Task<Message>) {
        let args = args();
        let palette = match &args.palette {
            Some(p) => Palette::load(p).unwrap_or_default(),
            None => Palette::detect(),
        };
        let shot = match &args.screenshot {
            // Give the fonts and first layout a moment, then grab the frame.
            Some(_) => Task::future(async { std::thread::sleep(std::time::Duration::from_millis(1200)) })
                .then(|_| window::latest())
                .and_then(window::screenshot)
                .map(Message::Shot),
            None => Task::none(),
        };
        let state = Self {
            palette,
            fixed_palette: args.palette,
            screenshot: args.screenshot,
            mods: sample::mods(),
            selected: 3,
            view: View::LoadOrder,
            search: String::new(),
            command: args.command_palette.then(String::new),
        };
        (state, shot)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Palette(p) => self.palette = p,
            Message::Select(i) => self.selected = i,
            Message::Toggle(i, on) => self.mods[i].enabled = on,
            Message::Move(i, by) => {
                let to = i as isize + by;
                if to >= 0 && (to as usize) < self.mods.len() {
                    self.mods.swap(i, to as usize);
                    self.selected = to as usize;
                }
            }
            Message::View(v) => self.view = v,
            Message::Search(s) => self.search = s,
            Message::OpenCommands => self.command = Some(String::new()),
            Message::Command(s) => self.command = Some(s),
            Message::CloseCommands => self.command = None,
            Message::Shot(shot) => {
                if let Some(path) = self.screenshot.take() {
                    let saved = image::save_buffer(&path, &shot.rgba, shot.size.width, shot.size.height, image::ColorType::Rgba8);
                    match saved {
                        Ok(()) => println!("saved {}", path.display()),
                        Err(e) => eprintln!("could not save {}: {e}", path.display()),
                    }
                    return iced::exit();
                }
            }
        }
        Task::none()
    }

    /// The palette's changes, from a watcher thread, and Ctrl+K / Escape.
    fn subscription(&self) -> Subscription<Message> {
        let palette = Subscription::run_with(self.fixed_palette.clone(), |fixed| {
            let fixed = fixed.clone();
            iced::stream::channel(4, async move |mut out: iced::futures::channel::mpsc::Sender<Message>| {
                let (tx, rx) = std::sync::mpsc::channel();
                let Ok(_watcher) = drukhal_palette::watch(fixed, move |p| drop(tx.send(p))) else { return };
                loop {
                    // The watcher thread blocks; hand its results over.
                    let next = rx.try_recv();
                    match next {
                        Ok(p) => {
                            let _ = out.try_send(Message::Palette(p));
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            })
        });
        let keys = keyboard::listen().filter_map(|event| match event {
            keyboard::Event::KeyPressed { key: keyboard::Key::Character(c), modifiers, .. } if modifiers.command() && c.as_str() == "k" => Some(Message::OpenCommands),
            keyboard::Event::KeyPressed { key: keyboard::Key::Named(keyboard::key::Named::Escape), .. } => Some(Message::CloseCommands),
            _ => None,
        });
        Subscription::batch([palette, keys])
    }

    fn theme(&self) -> Theme {
        let p = &self.palette;
        Theme::custom(
            "Desktop",
            iced::theme::Palette { background: c(p.window), text: c(p.on_window), primary: c(p.accent), success: c(p.success), warning: c(p.warning), danger: c(p.error) },
        )
    }

    fn view(&self) -> Element<'_, Message> {
        let p = &self.palette;
        let body = row![self.rail(), self.load_order(), self.inspector()].spacing(12).height(Fill);
        let main = container(column![self.header(), body, self.status()].spacing(12).padding(12)).width(Fill).height(Fill).style(move |_| surface(p.window, p.on_window, 0.0));
        match &self.command {
            Some(query) => stack![main, self.commands(query)].into(),
            None => main.into(),
        }
    }

    fn header(&self) -> Element<'_, Message> {
        let p = &self.palette;
        let playset = button(row![text("Playset").size(13).color(c(p.muted)), text("Grand Campaign").size(14), icon(include_bytes!("../../../data/icons/material/pan-down-symbolic.svg"), 16, p.on_window)].spacing(8).align_y(iced::Center))
            .padding([8, 16])
            .style(move |_, s| pill(p.container_high, p.on_window, s));
        let search = text_input("Search mods, objects, files…", &self.search)
            .on_input(Message::Search)
            .padding([10, 16])
            .width(380)
            .style(move |_, _| text_input::Style {
                background: Background::Color(c(p.container)),
                border: Border { radius: 999.0.into(), width: 0.0, color: Color::TRANSPARENT },
                icon: c(p.muted),
                placeholder: c(p.muted),
                value: c(p.on_window),
                selection: c(p.secondary),
            });
        let kbd = container(text("Ctrl K").size(12).font(MONO).color(c(p.muted))).padding([4, 8]).style(move |_| outlined(p.outline, 6.0));
        let launch = button(row![icon(include_bytes!("../../../data/icons/material/media-playback-start-symbolic.svg"), 18, p.on_accent), text("Launch Stellaris").size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center))
            .padding([10, 20])
            .style(move |_, s| pill(p.accent, p.on_accent, s));
        row![
            text("Drukhal").font(DISPLAY).size(30),
            space().width(16),
            playset,
            space().width(Fill),
            search,
            kbd,
            space().width(Fill),
            button(text("Scan")).padding([10, 18]).style(move |_, s| pill(p.secondary, p.on_window, s)),
            launch
        ]
        .spacing(10)
        .align_y(iced::Center)
        .into()
    }

    fn rail(&self) -> Element<'_, Message> {
        let p = &self.palette;
        let conflicts: u32 = self.mods.iter().filter(|m| m.enabled).map(|m| m.loses).sum();
        let items = View::ALL.iter().map(|&v| {
            let selected = v == self.view;
            let badge: Element<'_, Message> = match v {
                View::Conflicts => container(text(conflicts.to_string()).size(12).color(c(p.on_accent))).padding([2, 8]).style(move |_| surface(p.accent, p.on_accent, 999.0)).into(),
                View::Patches => container(text("2").size(12)).padding([2, 8]).style(move |_| surface(p.warning.mix(p.window, 0.6), p.on_window, 999.0)).into(),
                _ => space().into(),
            };
            let fg = if selected { p.on_window } else { p.muted };
            button(row![icon(v.icon(), 20, fg), text(v.label()).size(14).color(c(fg)), space().width(Fill), badge].spacing(12).align_y(iced::Center))
                .width(Fill)
                .padding([10, 14])
                .on_press(Message::View(v))
                .style(move |_, s| {
                    let bg = if selected { p.secondary } else { p.container_low };
                    pill(bg, p.on_window, s)
                })
                .into()
        });
        let theme_card = container(
            column![
                text("Colours").size(12).color(c(p.muted)),
                text(source_name(&p.source)).size(13),
                row([p.accent, p.secondary, p.container_high, p.success, p.warning, p.error].map(|col| container(space()).width(18).height(18).style(move |_| surface(col, col, 999.0)).into())).spacing(4),
            ]
            .spacing(6),
        )
        .padding(14)
        .width(Fill)
        .style(move |_| surface(p.container, p.on_window, 16.0));
        container(column![column(items).spacing(4), space().height(Fill), theme_card].spacing(8))
            .width(230)
            .height(Fill)
            .padding(8)
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }

    fn load_order(&self) -> Element<'_, Message> {
        let p = &self.palette;
        let query = self.search.to_lowercase();
        let mut list = column![].spacing(2);
        let max = self.mods.iter().map(|m| m.wins + m.loses).max().unwrap_or(1).max(1);
        for category in Category::ALL {
            let in_category: Vec<usize> = (0..self.mods.len()).filter(|&i| self.mods[i].category == category && self.mods[i].name.to_lowercase().contains(&query)).collect();
            if in_category.is_empty() {
                continue;
            }
            list = list.push(container(text(category.label().to_uppercase()).size(11).color(c(p.accent))).padding([12, 16]));
            for i in in_category {
                list = list.push(self.mod_row(i, max));
            }
        }
        let head = row![
            text("Load order").size(20),
            space().width(Fill),
            chip("Sorted by category", p),
            chip("11 mods · 10 on", p),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .padding([4, 8]);
        container(column![head, scrollable(list).height(Fill)].spacing(8))
            .width(Length::FillPortion(3))
            .height(Fill)
            .padding(12)
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }

    fn mod_row(&self, i: usize, max: u32) -> Element<'_, Message> {
        let p = &self.palette;
        let m = &self.mods[i];
        let selected = i == self.selected;
        let fg = if m.enabled { p.on_window } else { p.muted };
        let heat = {
            // Wins in the accent, losses in the error colour, out of the most any mod has.
            let total = 140.0;
            let w = total * m.wins as f32 / max as f32;
            let l = total * m.loses as f32 / max as f32;
            row![
                container(space()).width(w.max(0.0)).height(6).style(move |_| surface(p.success, p.success, 3.0)),
                container(space()).width(l.max(0.0)).height(6).style(move |_| surface(p.error, p.error, 3.0)),
                container(space()).width((total - w - l).max(0.0)).height(6).style(move |_| surface(p.container_high, p.container_high, 3.0)),
            ]
            .spacing(2)
        };
        let warn: Element<'_, Message> = if m.outdated {
            container(text(format!("for {}", m.version)).size(11).color(c(p.warning))).padding([2, 8]).style(move |_| outlined(p.warning, 999.0)).into()
        } else {
            space().into()
        };
        let content = row![
            icon(include_bytes!("../../../data/icons/material/list-drag-handle-symbolic.svg"), 18, p.muted),
            text(format!("{:>2}", i + 1)).font(MONO).size(13).color(c(p.muted)),
            column![text(m.name).size(15).color(c(fg)), text(m.author).size(12).color(c(p.muted))].width(Fill),
            warn,
            column![heat, text(format!("wins {} · loses {}", m.wins, m.loses)).size(11).color(c(p.muted))].spacing(4).width(150),
            toggler(m.enabled).on_toggle(move |on| Message::Toggle(i, on)).size(20),
        ]
        .spacing(14)
        .align_y(iced::Center);
        button(content)
            .width(Fill)
            .padding([10, 16])
            .on_press(Message::Select(i))
            .style(move |_, s| {
                let bg = if selected { p.secondary } else { p.container_low };
                let mut st = pill(bg, p.on_window, s);
                st.border.radius = 16.0.into();
                st
            })
            .into()
    }

    fn inspector(&self) -> Element<'_, Message> {
        let p = &self.palette;
        let m = &self.mods[self.selected];
        let stat = |n: String, label: &'static str| column![text(n).size(22), text(label).size(12).color(c(p.muted))].spacing(2).width(Fill);
        let conflicts = sample::conflicts();
        let mine: Vec<_> = conflicts.iter().filter(|cf| cf.versions.iter().any(|v| v.mod_name == m.name)).collect();
        let mut list = column![].spacing(6);
        for cf in &mine {
            let wins = cf.versions[cf.winner].mod_name == m.name;
            let (label, col) = if cf.simple.is_some() { ("same", p.muted) } else if wins { ("wins", p.success) } else { ("loses", p.error) };
            list = list.push(
                container(
                    row![
                        column![text(cf.key).font(MONO).size(13), text(cf.category).size(11).color(c(p.muted))].width(Fill),
                        container(text(label).size(11).color(c(col))).padding([2, 10]).style(move |_| outlined(col, 999.0)),
                    ]
                    .align_y(iced::Center),
                )
                .padding([10, 14])
                .style(move |_| surface(p.container_high, p.on_window, 14.0)),
            );
        }
        if mine.is_empty() {
            list = list.push(text("No conflicts with the other enabled mods.").size(13).color(c(p.muted)));
        }
        let actions = row![
            button(row![icon(include_bytes!("../../../data/icons/material/io.github.dukko.DrukhalManager-ai-symbolic.svg"), 16, p.on_accent), text("Merge with AI").size(13).color(c(p.on_accent))].spacing(6).align_y(iced::Center))
                .padding([8, 16])
                .style(move |_, s| pill(p.accent, p.on_accent, s)),
            button(text("Move up").size(13)).padding([8, 14]).on_press(Message::Move(self.selected, -1)).style(move |_, s| pill(p.container_high, p.on_window, s)),
            button(text("Move down").size(13)).padding([8, 14]).on_press(Message::Move(self.selected, 1)).style(move |_, s| pill(p.container_high, p.on_window, s)),
        ]
        .spacing(8);
        container(
            column![
                text(m.category.label().to_uppercase()).size(11).color(c(p.accent)),
                text(m.name).font(DISPLAY).size(28),
                text(format!("by {} · supports {}", m.author, m.version)).size(13).color(c(p.muted)),
                rule::horizontal(1).style(move |_| rule::Style { color: c(p.outline.mix(p.window, 0.5)), radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true }),
                row![stat(fmt_n(m.files), "files"), stat(fmt_n(m.objects), "objects"), stat(m.wins.to_string(), "wins"), stat(m.loses.to_string(), "loses")],
                text("Conflicts").size(16),
                scrollable(list).height(Fill),
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

    fn status(&self) -> Element<'_, Message> {
        let p = &self.palette;
        let on = self.mods.iter().filter(|m| m.enabled).count();
        text(format!("Scanned in 2.3 s · {on} mods on · Stellaris 4.1.2 · colours from {}", p.source)).size(12).color(c(p.muted)).into()
    }

    fn commands<'a>(&'a self, query: &'a str) -> Element<'a, Message> {
        let p = &self.palette;
        let all = [
            ("Scan for conflicts", "Ctrl R"),
            ("Launch Stellaris", "Ctrl Enter"),
            ("Merge every simple conflict", ""),
            ("Sort by category", ""),
            ("Find the crashing mod…", ""),
            ("Look up an object…", "Ctrl L"),
            ("Freeze this playset as a collection", ""),
            ("Preferences", "Ctrl ,"),
        ];
        let q = query.to_lowercase();
        let matches = all.iter().filter(|(name, _)| name.to_lowercase().contains(&q));
        let list = column(matches.enumerate().map(|(n, (name, keys))| {
            let first = n == 0;
            container(row![text(*name).size(14), space().width(Fill), text(*keys).size(12).font(MONO).color(c(p.muted))])
                .padding([10, 16])
                .width(Fill)
                .style(move |_| surface(if first { p.secondary } else { p.container_high }, p.on_window, 12.0))
                .into()
        }))
        .spacing(2);
        let card = container(
            column![
                text_input("Type a command", query).on_input(Message::Command).padding([12, 16]).size(16).style(move |_, _| text_input::Style {
                    background: Background::Color(c(p.container)),
                    border: Border { radius: 14.0.into(), width: 2.0, color: c(p.accent) },
                    icon: c(p.muted),
                    placeholder: c(p.muted),
                    value: c(p.on_window),
                    selection: c(p.secondary),
                }),
                list
            ]
            .spacing(10),
        )
        .padding(14)
        .width(560)
        .style(move |_| container::Style { shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.45), offset: Vector::new(0.0, 12.0), blur_radius: 40.0 }, ..surface(p.container_high, p.on_window, 24.0) });
        // A scrim; clicking it closes the palette.
        let scrim = mouse_area(container(space()).width(Fill).height(Fill).style(|_| container::Style { background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))), ..Default::default() })).on_press(Message::CloseCommands);
        stack![scrim, container(opaque(card)).width(Fill).padding(iced::Padding { top: 110.0, ..iced::Padding::ZERO }).center_x(Fill)].into()
    }
}

fn c(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.r, rgb.g, rgb.b)
}

fn surface(bg: Rgb, fg: Rgb, radius: f32) -> container::Style {
    container::Style { background: Some(Background::Color(c(bg))), text_color: Some(c(fg)), border: Border { radius: radius.into(), ..Border::default() }, ..container::Style::default() }
}

fn outlined(col: Rgb, radius: f32) -> container::Style {
    container::Style { border: Border { radius: radius.into(), width: 1.0, color: c(col) }, ..container::Style::default() }
}

/// A Material pill button: hover and press add a state layer of the text colour.
fn pill(bg: Rgb, fg: Rgb, status: button::Status) -> button::Style {
    let layer = match status {
        button::Status::Hovered => 0.08,
        button::Status::Pressed => 0.12,
        _ => 0.0,
    };
    button::Style { background: Some(Background::Color(c(bg.mix(fg, layer)))), text_color: c(fg), border: Border { radius: 999.0.into(), ..Border::default() }, shadow: Shadow::default(), snap: true }
}

fn chip<'a>(label: &'a str, p: &'a Palette) -> Element<'a, Message> {
    container(text(label).size(12).color(c(p.muted))).padding([4, 12]).style(move |_| outlined(p.outline, 8.0)).into()
}

fn icon<'a>(bytes: &'static [u8], size: u16, col: Rgb) -> Element<'a, Message> {
    svg(svg::Handle::from_memory(bytes)).width(size as f32).height(size as f32).style(move |_, _| svg::Style { color: Some(c(col)) }).into()
}

fn source_name(source: &drukhal_palette::Source) -> String {
    match source {
        drukhal_palette::Source::GtkCss(_) => "matugen · gtk.css".to_owned(),
        drukhal_palette::Source::Omarchy(path) => {
            let theme = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().replace('-', " ")).unwrap_or_default();
            format!("Omarchy · {theme}")
        }
        drukhal_palette::Source::Builtin => "Built-in Material".to_owned(),
    }
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
