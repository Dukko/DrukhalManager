// Release builds on Windows open without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! DrukhalManager's interface, in iced. Everything it shows and changes
//! comes from `drukhal-core`; this is the window around it.
//!
//! Two modes that don't overlap: **Mods** (the load order, collections,
//! launching) is for everyone; **Conflicts** is for those who merge. The
//! switch in the header is the only place they meet. Colours follow the
//! desktop (matugen's gtk.css, else Omarchy's theme), live.

#[macro_use]
extern crate drukhal_core;

mod library;
mod mods_view;
mod palette;
mod style;

use std::path::PathBuf;

use drukhal_core::{diagnostics, platform, update};
use iced::widget::{button, column, container, mouse_area, opaque, row, space, stack, text, text_input};
use iced::{Background, Color, Element, Fill, Shadow, Subscription, Task, Theme, Vector, keyboard, window};

use library::Library;
use palette::Palette;
use style::{DISPLAY, MONO, SANS, c, ghost, icon, icons, pill, surface};

fn main() -> iced::Result {
    // Before any UI, so the first string the app shows is translated too.
    drukhal_core::i18n::init();
    diagnostics::log!("app", "DrukhalManager {} started ({})", update::CURRENT, if platform::is_flatpak() { "Flatpak" } else { "native" });
    iced::application(App::boot, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .font(include_bytes!("../data/fonts/GoogleSansFlex.ttf").as_slice())
        .font(include_bytes!("../data/fonts/FiraCode.ttf").as_slice())
        .font(include_bytes!("../data/fonts/DMSerifDisplay.ttf").as_slice())
        .default_font(SANS)
        .window(window::Settings { size: iced::Size::new(1320.0, 820.0), min_size: Some(iced::Size::new(960.0, 600.0)), platform_specific: platform_window(), ..Default::default() })
        .run()
}

/// On Linux, the app id names the window for the desktop, which finds the
/// icon and the .desktop file by it.
fn platform_window() -> window::settings::PlatformSpecific {
    #[cfg(target_os = "linux")]
    return window::settings::PlatformSpecific { application_id: drukhal_core::APP_ID.to_owned(), ..Default::default() };
    #[cfg(not(target_os = "linux"))]
    window::settings::PlatformSpecific::default()
}

/// `--palette <file>` (a `gtk.css`, `colors.toml` or Omarchy theme folder
/// instead of the desktop's), `--screenshot <out.png>` (saves the first
/// frame and quits), `--mode mods|conflicts`.
struct Args {
    palette: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    mode: Mode,
}

fn args() -> Args {
    let mut args = Args { palette: None, screenshot: None, mode: Mode::Mods };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--palette" => args.palette = it.next().map(PathBuf::from),
            "--screenshot" => args.screenshot = it.next().map(PathBuf::from),
            "--mode" => args.mode = if it.next().as_deref() == Some("conflicts") { Mode::Conflicts } else { Mode::Mods },
            _ => eprintln!("unknown argument {a}"),
        }
    }
    args
}

/// The two halves of the app. Nothing of one shows in the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mods,
    Conflicts,
}

struct App {
    palette: Palette,
    fixed_palette: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    mode: Mode,
    library: Library,
    mods: mods_view::State,
    /// The last thing that happened, and whether it went wrong.
    notice: Option<(String, bool)>,
    command: Option<String>,
    /// Where the mouse is, for the dragged row.
    cursor: iced::Point,
}

#[derive(Debug, Clone)]
enum Message {
    Palette(Palette),
    Mode(Mode),
    Mods(mods_view::Message),
    Cursor(iced::Point),
    /// The left button came up, wherever it was.
    Released,
    Escape,
    /// A key nothing else took.
    Key(keyboard::Key, keyboard::Modifiers),
    OpenCommands,
    Command(String),
    RunCommand(Command),
    CloseCommands,
    DismissNotice,
    Shot(window::Screenshot),
}

/// A command palette entry: what it does, as a message to run.
#[derive(Debug, Clone)]
struct Command {
    name: String,
    keys: &'static str,
    run: Box<Message>,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let args = args();
        let palette = match &args.palette {
            Some(p) => Palette::load(p).unwrap_or_default(),
            None => Palette::detect(),
        };
        let shot = match &args.screenshot {
            // Give the fonts and first layout a moment, then grab the frame.
            Some(_) => Task::future(async { tokio::time::sleep(std::time::Duration::from_millis(1200)).await }).then(|_| window::latest()).and_then(window::screenshot).map(Message::Shot),
            None => Task::none(),
        };
        let library = Library::load();
        let mods = mods_view::State::new(&library);
        let app = Self { palette, fixed_palette: args.palette, screenshot: args.screenshot, mode: args.mode, library, mods, notice: None, command: None, cursor: iced::Point::ORIGIN };
        (app, shot)
    }

    fn title(&self) -> String {
        if self.library.state.dirty { format!("DrukhalManager — {} •", self.library.collections.active) } else { format!("DrukhalManager — {}", self.library.collections.active) }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Palette(p) => self.palette = p,
            Message::Mode(m) => {
                self.mode = m;
                self.command = None;
            }
            Message::Mods(m) => {
                if let Some(notice) = self.mods.update(&mut self.library, m) {
                    self.notice = Some(notice);
                }
            }
            Message::Cursor(at) => self.cursor = at,
            Message::Released => return self.update(Message::Mods(mods_view::Message::DragEnd)),
            Message::Escape => {
                if self.command.is_some() {
                    self.command = None;
                } else {
                    return self.update(Message::Mods(mods_view::Message::DragCancel));
                }
            }
            Message::Key(key, modifiers) => {
                if self.command.is_none() && self.mode == Mode::Mods && modifiers.command() {
                    let message = match key.as_ref() {
                        keyboard::Key::Character("s") => Some(mods_view::Message::Save),
                        keyboard::Key::Character("z") if modifiers.shift() => Some(mods_view::Message::Redo),
                        keyboard::Key::Character("z") => Some(mods_view::Message::Undo),
                        keyboard::Key::Named(keyboard::key::Named::Enter) => Some(mods_view::Message::Launch { resume: false }),
                        _ => None,
                    };
                    if let Some(message) = message {
                        return self.update(Message::Mods(message));
                    }
                }
            }
            Message::OpenCommands => self.command = Some(String::new()),
            Message::Command(s) => self.command = Some(s),
            Message::RunCommand(cmd) => {
                self.command = None;
                return self.update(*cmd.run);
            }
            Message::CloseCommands => self.command = None,
            Message::DismissNotice => self.notice = None,
            Message::Shot(shot) => {
                if let Some(path) = self.screenshot.take() {
                    match image::save_buffer(&path, &shot.rgba, shot.size.width, shot.size.height, image::ColorType::Rgba8) {
                        Ok(()) => println!("saved {}", path.display()),
                        Err(e) => eprintln!("could not save {}: {e}", path.display()),
                    }
                    return iced::exit();
                }
            }
        }
        Task::none()
    }

    /// The palette's changes, from a watcher thread, the shortcuts, and the
    /// mouse for dragging.
    fn subscription(&self) -> Subscription<Message> {
        let palette = Subscription::run_with(self.fixed_palette.clone(), |fixed| {
            let fixed = fixed.clone();
            iced::stream::channel(4, async move |mut out: iced::futures::channel::mpsc::Sender<Message>| {
                let (tx, rx) = std::sync::mpsc::channel();
                let Ok(_watcher) = palette::watch(fixed, move |p| drop(tx.send(p))) else { return };
                loop {
                    match rx.try_recv() {
                        Ok(p) => drop(out.try_send(Message::Palette(p))),
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            })
        });
        let keys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, modifiers, .. } = event else { return None };
            match key.as_ref() {
                keyboard::Key::Character("k") if modifiers.command() => Some(Message::OpenCommands),
                keyboard::Key::Character("1") if modifiers.command() => Some(Message::Mode(Mode::Mods)),
                keyboard::Key::Character("2") if modifiers.command() => Some(Message::Mode(Mode::Conflicts)),
                keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Escape),
                _ => Some(Message::Key(key.clone(), modifiers)),
            }
        });
        let mouse = iced::event::listen_with(|event, _, _| match event {
            iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => Some(Message::Cursor(position)),
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => Some(Message::Released),
            _ => None,
        });
        Subscription::batch([palette, keys, mouse])
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
        let body: Element<'_, Message> = match self.mode {
            Mode::Mods => self.mods.view(&self.library, p).map(Message::Mods),
            Mode::Conflicts => conflicts_placeholder(p),
        };
        let status: Element<'_, Message> = match &self.notice {
            Some((message, error)) => row![
                text(message.as_str()).size(12).color(c(if *error { p.error } else { p.on_window })),
                button(text(t!("Dismiss")).size(12)).padding([2, 8]).on_press(Message::DismissNotice).style(move |_, s| ghost(p.muted, s)),
            ]
            .spacing(8)
            .align_y(iced::Center)
            .into(),
            None => text(match self.mode {
                Mode::Mods => self.mods.status(&self.library),
                Mode::Conflicts => String::new(),
            })
            .size(12)
            .color(c(p.muted))
            .into(),
        };
        let main = container(column![self.header(), container(body).height(Fill), status].spacing(12).padding(12)).width(Fill).height(Fill).style(move |_| surface(p.window, p.on_window, 0.0));
        let mut layers = stack![main];
        if self.mode == Mode::Mods
            && let Some(name) = self.mods.dragging(&self.library)
        {
            // The dragged mod follows the cursor. An outline, not a shadow:
            // the software renderer leaves a shadow's trail behind.
            let ghost = container(row![icon(icons::DRAG, 16, p.on_window), text(name).size(14)].spacing(8).align_y(iced::Center))
                .padding([8, 14])
                .style(move |_| container::Style { border: iced::Border { radius: 14.0.into(), width: 1.5, color: c(p.accent) }, ..surface(p.secondary, p.on_window, 14.0) });
            layers = layers.push(iced::widget::pin(ghost).x(self.cursor.x + 12.0).y(self.cursor.y - 18.0).width(Fill).height(Fill));
        }
        if let Some(query) = &self.command {
            layers = layers.push(self.commands(query));
        }
        layers.into()
    }

    fn header(&self) -> Element<'_, Message> {
        let p = &self.palette;
        // The switch between the two halves. No counts on it: someone who
        // only manages mods shouldn't be told about conflicts.
        let segment = |mode: Mode, label: String, glyph: &'static [u8]| {
            let on = self.mode == mode;
            let fg = if on { p.on_window } else { p.muted };
            button(row![icon(glyph, 18, fg), text(label).size(14).color(c(fg))].spacing(8).align_y(iced::Center))
                .padding([8, 18])
                .on_press(Message::Mode(mode))
                .style(move |_, s| if on { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) })
        };
        let switch = container(row![segment(Mode::Mods, t!("Mods"), icons::LIST), segment(Mode::Conflicts, t!("Conflicts"), icons::WARNING)].spacing(4))
            .padding(4)
            .style(move |_| surface(p.container_low, p.on_window, 999.0));
        let actions: Element<'_, Message> = match self.mode {
            Mode::Mods => self.mods.header_actions(&self.library, p).map(Message::Mods),
            Mode::Conflicts => space().into(),
        };
        let kbd = button(text("Ctrl K").size(12).font(MONO).color(c(p.muted))).padding([4, 8]).on_press(Message::OpenCommands).style(move |_, s| ghost(p.muted, s));
        row![text("Drukhal").font(DISPLAY).size(30), space().width(12), switch, space().width(Fill), actions, kbd].spacing(10).align_y(iced::Center).into()
    }

    /// The current mode's commands, plus going to the other mode.
    fn command_list(&self) -> Vec<Command> {
        let mods = |name: String, keys: &'static str, m: mods_view::Message| Command { name, keys, run: Box::new(Message::Mods(m)) };
        match self.mode {
            Mode::Mods => {
                let mut list = vec![
                    mods(t!("Launch Stellaris"), "Ctrl Enter", mods_view::Message::Launch { resume: false }),
                    mods(t!("Continue last save"), "", mods_view::Message::Launch { resume: true }),
                    mods(t!("Save load order"), "Ctrl S", mods_view::Message::Save),
                    mods(t!("Undo"), "Ctrl Z", mods_view::Message::Undo),
                    mods(t!("Redo"), "Ctrl Shift Z", mods_view::Message::Redo),
                    mods(t!("New collection from this load order"), "", mods_view::Message::NewCollection),
                ];
                for coll in &self.library.collections.list {
                    if coll.name != self.library.collections.active {
                        list.push(mods(tf!("Switch to {name}", name = coll.name.as_str()), "", mods_view::Message::SwitchCollection(coll.name.clone())));
                    }
                }
                list.push(Command { name: t!("Go to Conflicts"), keys: "Ctrl 2", run: Box::new(Message::Mode(Mode::Conflicts)) });
                list
            }
            Mode::Conflicts => vec![Command { name: t!("Go to Mods"), keys: "Ctrl 1", run: Box::new(Message::Mode(Mode::Mods)) }],
        }
    }

    fn commands<'a>(&'a self, query: &'a str) -> Element<'a, Message> {
        let p = &self.palette;
        let q = query.to_lowercase();
        let matches: Vec<Command> = self.command_list().into_iter().filter(|cmd| cmd.name.to_lowercase().contains(&q)).collect();
        let first = matches.first().cloned();
        let list = column(matches.into_iter().enumerate().map(|(n, cmd)| {
            let top = n == 0;
            button(row![text(cmd.name.clone()).size(14), space().width(Fill), text(cmd.keys).size(12).font(MONO).color(c(p.muted))])
                .padding([10, 16])
                .width(Fill)
                .on_press(Message::RunCommand(cmd))
                .style(move |_, s| {
                    let mut st = pill(if top { p.secondary } else { p.container_high }, p.on_window, s);
                    st.border.radius = 12.0.into();
                    st
                })
                .into()
        }))
        .spacing(2);
        let mut input = text_input(&t!("Type a command"), query).on_input(Message::Command).padding([12, 16]).size(16).style(move |_, _| style::field(p, 14.0, true));
        if let Some(cmd) = first {
            input = input.on_submit(Message::RunCommand(cmd));
        }
        let card = container(column![input, iced::widget::scrollable(list).height(iced::Length::Shrink)].spacing(10)).padding(14).width(560).max_height(520).style(move |_| container::Style {
            shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.45), offset: Vector::new(0.0, 12.0), blur_radius: 40.0 },
            ..surface(p.container_high, p.on_window, 24.0)
        });
        // A scrim; clicking it closes the palette.
        let scrim = mouse_area(container(space()).width(Fill).height(Fill).style(|_| container::Style { background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.5))), ..Default::default() })).on_press(Message::CloseCommands);
        stack![scrim, container(opaque(card)).width(Fill).padding(iced::Padding { top: 110.0, ..iced::Padding::ZERO }).center_x(Fill)].into()
    }
}

/// Conflicts mode, until its port: what it will hold.
fn conflicts_placeholder<'a>(p: &'a Palette) -> Element<'a, Message> {
    container(
        column![
            text(t!("Conflicts")).size(20),
            text(t!("Conflict scans and merging are being rebuilt for the new interface. The mockup in prototypes/iced-workbench shows where they're going: the queue, the versions side by side, adding blocks above or below, and comparing with the base game.")).size(13).color(c(p.muted)),
        ]
        .spacing(8)
        .max_width(680),
    )
    .padding(20)
    .width(Fill)
    .height(Fill)
    .style(move |_| surface(p.container_low, p.on_window, 24.0))
    .into()
}

/// Where the colours came from, shortly.
pub fn source_name(source: &palette::Source) -> String {
    match source {
        palette::Source::GtkCss(_) => "matugen · gtk.css".to_owned(),
        palette::Source::Omarchy(path) => {
            let theme = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().replace('-', " ")).unwrap_or_default();
            format!("Omarchy · {theme}")
        }
        palette::Source::Builtin => t!("Built-in Material"),
    }
}
