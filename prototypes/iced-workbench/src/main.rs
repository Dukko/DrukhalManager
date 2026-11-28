//! Mockup: DrukhalManager's main window in iced, as two separate modes.
//!
//! - **Mods** is for everyone: the load order, playsets and collections,
//!   and launching. It says nothing about conflicts.
//! - **Conflicts** is for those who want it: the conflict queue, the
//!   versions side by side, and merging.
//!
//! The switch between them in the header is the only place they meet.
//! Colours come from `drukhal-palette` (matugen's gtk.css, else Omarchy's
//! theme) and follow it live. Ctrl+K opens the current mode's commands.

mod conflicts;
mod mods;
mod style;

use std::path::PathBuf;

use drukhal_palette::Palette;
use iced::widget::{button, column, container, mouse_area, opaque, row, space, stack, text, text_input};
use iced::{Background, Color, Element, Fill, Shadow, Subscription, Task, Theme, Vector, keyboard, window};

use style::{DISPLAY, MONO, SANS, c, ghost, icon, icons, pill, surface};

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("DrukhalManager — iced mockup")
        .theme(App::theme)
        .subscription(App::subscription)
        .font(include_bytes!("../../../data/fonts/GoogleSansFlex.ttf").as_slice())
        .font(include_bytes!("../../../data/fonts/FiraCode.ttf").as_slice())
        .font(include_bytes!("../../../data/fonts/DMSerifDisplay.ttf").as_slice())
        .default_font(SANS)
        .window_size((1320.0, 820.0))
        .run()
}

/// `--palette <file>`, `--screenshot <out.png>`, `--mode mods|conflicts`,
/// `--commands` (opens the command palette), and for screenshots
/// `--show menu|added|base|ignore|drag` (a context menu open, a block
/// added, the comparison with the base game, the ignore sheet, a drag
/// under way).
struct Args {
    palette: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    mode: Mode,
    commands: bool,
    show: Option<String>,
}

fn args() -> Args {
    let mut args = Args { palette: None, screenshot: None, mode: Mode::Mods, commands: false, show: None };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--palette" => args.palette = it.next().map(PathBuf::from),
            "--screenshot" => args.screenshot = it.next().map(PathBuf::from),
            "--mode" => args.mode = if it.next().as_deref() == Some("conflicts") { Mode::Conflicts } else { Mode::Mods },
            "--commands" => args.commands = true,
            "--show" => args.show = it.next(),
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
    mods: mods::State,
    conflicts: conflicts::State,
    command: Option<String>,
    /// Where the mouse is, for the context menu and the dragged row.
    cursor: iced::Point,
}

#[derive(Debug, Clone)]
enum Message {
    Cursor(iced::Point),
    /// The left button came up, wherever it was.
    Released,
    /// The right button went down, after the cursor moved there.
    RightPressed,
    /// A key nothing else took, for the current mode's shortcuts.
    Key(keyboard::Key, keyboard::Modifiers),
    Escape,
    Palette(Palette),
    Mode(Mode),
    Mods(mods::Message),
    Conflicts(conflicts::Message),
    OpenCommands,
    Command(String),
    RunCommand(Command),
    CloseCommands,
    Shot(window::Screenshot),
}

/// A command palette entry.
#[derive(Debug, Clone, Copy)]
struct Command {
    name: &'static str,
    keys: &'static str,
    /// Switches mode when run; the others do nothing in the mockup.
    goes_to: Option<Mode>,
}

const MOD_COMMANDS: &[Command] = &[
    Command { name: "Launch Stellaris", keys: "Ctrl Enter", goes_to: None },
    Command { name: "Switch playset…", keys: "Ctrl P", goes_to: None },
    Command { name: "Sort by category", keys: "", goes_to: None },
    Command { name: "Turn every mod off", keys: "", goes_to: None },
    Command { name: "Find the crashing mod…", keys: "", goes_to: None },
    Command { name: "Freeze this playset as a collection", keys: "", goes_to: None },
    Command { name: "Check for mod updates", keys: "", goes_to: None },
    Command { name: "Go to Conflicts", keys: "Ctrl 2", goes_to: Some(Mode::Conflicts) },
];

const CONFLICT_COMMANDS: &[Command] = &[
    Command { name: "Rescan", keys: "Ctrl R", goes_to: None },
    Command { name: "Merge every simple conflict", keys: "", goes_to: None },
    Command { name: "Next conflict", keys: "J", goes_to: None },
    Command { name: "Previous conflict", keys: "K", goes_to: None },
    Command { name: "Look up an object…", keys: "Ctrl L", goes_to: None },
    Command { name: "Edit ignore rules", keys: "", goes_to: None },
    Command { name: "Go to Mods", keys: "Ctrl 1", goes_to: Some(Mode::Mods) },
];

impl App {
    fn boot() -> (Self, Task<Message>) {
        let args = args();
        let palette = match &args.palette {
            Some(p) => Palette::load(p).unwrap_or_default(),
            None => Palette::detect(),
        };
        let shot = match &args.screenshot {
            // Give the fonts and first layout a moment, then grab the frame.
            Some(_) => Task::future(async { std::thread::sleep(std::time::Duration::from_millis(1200)) }).then(|_| window::latest()).and_then(window::screenshot).map(Message::Shot),
            None => Task::none(),
        };
        let mut app = Self {
            palette,
            fixed_palette: args.palette,
            screenshot: args.screenshot,
            mode: args.mode,
            mods: mods::State::new(),
            conflicts: conflicts::State::new(),
            command: args.commands.then(String::new),
            cursor: iced::Point::ORIGIN,
        };
        match args.show.as_deref() {
            Some("menu") => app.conflicts.demo_menu(iced::Point::new(330.0, 370.0)),
            Some("added") => app.conflicts.demo_added(),
            Some("base") => app.conflicts.demo_base(),
            Some("ignore") => app.conflicts.demo_ignore(),
            Some("drag") => {
                app.mods.demo_drag();
                app.cursor = iced::Point::new(560.0, 300.0);
            }
            _ => {}
        }
        (app, shot)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Cursor(at) => self.cursor = at,
            Message::Released => self.mods.update(mods::Message::DragEnd),
            // A click can arrive in the same batch as the move before it,
            // after the menu opened at the old position: place it again.
            Message::RightPressed => self.conflicts.place_menu(self.cursor),
            Message::Key(key, modifiers) => {
                if self.mode == Mode::Conflicts && self.command.is_none() && !self.conflicts.has_sheet() {
                    use conflicts::{Message as M, Op};
                    let message = match key.as_ref() {
                        keyboard::Key::Character("z") if modifiers.command() => Some(M::Undo),
                        keyboard::Key::Character("s") if modifiers.command() => Some(M::SaveMerge),
                        _ if modifiers.command() || modifiers.alt() => None,
                        keyboard::Key::Character("]") => Some(M::StepBlock(1)),
                        keyboard::Key::Character("[") => Some(M::StepBlock(-1)),
                        keyboard::Key::Character("a") => Some(M::ApplyFocused(Op::Above)),
                        keyboard::Key::Character("b") => Some(M::ApplyFocused(Op::Below)),
                        keyboard::Key::Character("r") => Some(M::ApplyFocused(Op::Replace)),
                        keyboard::Key::Character("m") => Some(M::ApplyFocused(Op::Ai)),
                        keyboard::Key::Character("j") => Some(M::Step(1)),
                        keyboard::Key::Character("k") => Some(M::Step(-1)),
                        _ => None,
                    };
                    if let Some(message) = message {
                        self.conflicts.update(message);
                    }
                }
            }
            Message::Escape => {
                if self.command.is_some() {
                    self.command = None;
                } else {
                    self.mods.update(mods::Message::DragCancel);
                    self.conflicts.update(conflicts::Message::CloseMenu);
                    self.conflicts.update(conflicts::Message::CloseIgnore);
                }
            }
            Message::Palette(p) => self.palette = p,
            Message::Mode(m) => {
                self.mode = m;
                self.command = None;
            }
            Message::Mods(m) => self.mods.update(m),
            Message::Conflicts(m) => {
                let opens_menu = matches!(m, conflicts::Message::OpenMenu(_));
                self.conflicts.update(m);
                if opens_menu {
                    self.conflicts.place_menu(self.cursor);
                }
            }
            Message::OpenCommands => self.command = Some(String::new()),
            Message::Command(s) => self.command = Some(s),
            Message::RunCommand(cmd) => {
                self.command = None;
                if let Some(m) = cmd.goes_to {
                    self.mode = m;
                }
            }
            Message::CloseCommands => self.command = None,
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

    /// The palette's changes, from a watcher thread, and the shortcuts.
    fn subscription(&self) -> Subscription<Message> {
        let palette = Subscription::run_with(self.fixed_palette.clone(), |fixed| {
            let fixed = fixed.clone();
            iced::stream::channel(4, async move |mut out: iced::futures::channel::mpsc::Sender<Message>| {
                let (tx, rx) = std::sync::mpsc::channel();
                let Ok(_watcher) = drukhal_palette::watch(fixed, move |p| drop(tx.send(p))) else { return };
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
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Right)) => Some(Message::RightPressed),
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
            Mode::Mods => self.mods.view(p).map(Message::Mods),
            Mode::Conflicts => self.conflicts.view(p).map(Message::Conflicts),
        };
        let status = match self.mode {
            Mode::Mods => self.mods.status(p),
            Mode::Conflicts => self.conflicts.status(p),
        };
        let main = container(column![self.header(), container(body).height(Fill), text(status).size(12).color(c(p.muted))].spacing(12).padding(12))
            .width(Fill)
            .height(Fill)
            .style(move |_| surface(p.window, p.on_window, 0.0));
        let mut layers = stack![main];
        if self.mode == Mode::Conflicts
            && let Some(menu) = self.conflicts.overlay(p)
        {
            layers = layers.push(menu.map(Message::Conflicts));
        }
        if self.mode == Mode::Mods
            && let Some(name) = self.mods.dragging()
        {
            // The dragged mod follows the cursor.
            let ghost = container(row![icon(icons::DRAG, 16, p.on_window), text(name).size(14)].spacing(8).align_y(iced::Center))
                .padding([8, 14])
                // An outline, not a shadow: the software renderer leaves a
                // shadow's trail behind a moving widget.
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
        let segment = |mode: Mode, label: &'static str, glyph: &'static [u8]| {
            let on = self.mode == mode;
            let fg = if on { p.on_window } else { p.muted };
            button(row![icon(glyph, 18, fg), text(label).size(14).color(c(fg))].spacing(8).align_y(iced::Center))
                .padding([8, 18])
                .on_press(Message::Mode(mode))
                .style(move |_, s| if on { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) })
        };
        let switch = container(row![segment(Mode::Mods, "Mods", icons::LIST), segment(Mode::Conflicts, "Conflicts", icons::WARNING)].spacing(4))
            .padding(4)
            .style(move |_| surface(p.container_low, p.on_window, 999.0));
        let actions: Element<'_, Message> = match self.mode {
            Mode::Mods => self.mods.header_actions(p).map(Message::Mods),
            Mode::Conflicts => self.conflicts.header_actions(p).map(Message::Conflicts),
        };
        let kbd = button(text("Ctrl K").size(12).font(MONO).color(c(p.muted))).padding([4, 8]).on_press(Message::OpenCommands).style(move |_, s| ghost(p.muted, s));
        row![text("Drukhal").font(DISPLAY).size(30), space().width(12), switch, space().width(Fill), actions, kbd].spacing(10).align_y(iced::Center).into()
    }

    fn commands<'a>(&'a self, query: &'a str) -> Element<'a, Message> {
        let p = &self.palette;
        let all = match self.mode {
            Mode::Mods => MOD_COMMANDS,
            Mode::Conflicts => CONFLICT_COMMANDS,
        };
        let q = query.to_lowercase();
        let matches: Vec<Command> = all.iter().copied().filter(|cmd| cmd.name.to_lowercase().contains(&q)).collect();
        let first = matches.first().copied();
        let list = column(matches.into_iter().enumerate().map(|(n, cmd)| {
            let top = n == 0;
            button(row![text(cmd.name).size(14), space().width(Fill), text(cmd.keys).size(12).font(MONO).color(c(p.muted))])
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
        let scope = match self.mode {
            Mode::Mods => "Mods",
            Mode::Conflicts => "Conflicts",
        };
        let mut input = text_input(&format!("{scope}: type a command"), query).on_input(Message::Command).padding([12, 16]).size(16).style(move |_, _| style::field(p, 14.0, true));
        if let Some(cmd) = first {
            input = input.on_submit(Message::RunCommand(cmd));
        }
        let card = container(column![input, list].spacing(10)).padding(14).width(560).style(move |_| container::Style {
            shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.45), offset: Vector::new(0.0, 12.0), blur_radius: 40.0 },
            ..surface(p.container_high, p.on_window, 24.0)
        });
        // A scrim; clicking it closes the palette.
        let scrim = mouse_area(container(space()).width(Fill).height(Fill).style(|_| container::Style { background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.5))), ..Default::default() })).on_press(Message::CloseCommands);
        stack![scrim, container(opaque(card)).width(Fill).padding(iced::Padding { top: 110.0, ..iced::Padding::ZERO }).center_x(Fill)].into()
    }
}

/// Where the colours came from, shortly.
pub fn source_name(source: &drukhal_palette::Source) -> String {
    match source {
        drukhal_palette::Source::GtkCss(_) => "matugen · gtk.css".to_owned(),
        drukhal_palette::Source::Omarchy(path) => {
            let theme = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().replace('-', " ")).unwrap_or_default();
            format!("Omarchy · {theme}")
        }
        drukhal_palette::Source::Builtin => "Built-in Material".to_owned(),
    }
}
