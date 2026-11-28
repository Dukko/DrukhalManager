//! The Mods mode: what's installed, what's on and in what order,
//! collections, and launching the game. Nothing here mentions conflicts;
//! that's the Conflicts mode's job, so players who never merge never
//! meet them.

use drukhal_core::mods::checks::{Issue, Severity};
use drukhal_core::mods::{self, Source};
use iced::widget::{button, column, container, mouse_area, row, rule, scrollable, space, text, text_input, toggler, tooltip};
use iced::{Element, Fill, Length, mouse};

use crate::library::Library;
use crate::palette::{Palette, Rgb};
use crate::style::{DISPLAY, MONO, alpha, c, chip, field, filled_chip, ghost, icon, icons, pill, surface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    LoadOrder,
    Collections,
    Launch,
}

impl View {
    const ALL: [View; 3] = [View::LoadOrder, View::Collections, View::Launch];

    fn label(self) -> String {
        match self {
            View::LoadOrder => t!("Load order"),
            View::Collections => t!("Collections"),
            View::Launch => t!("Launch"),
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

/// A mod being dragged in the load order, by load position.
#[derive(Debug, Clone, Copy)]
struct Drag {
    from: usize,
    /// The row under the cursor, and whether the drop goes below it.
    over: Option<(usize, bool)>,
}

/// About how tall a load order row is, to tell its top half from its bottom.
const ROW_HEIGHT: f32 = 58.0;

pub struct State {
    /// The selected mod, by index into the library's mods.
    selected: Option<usize>,
    view: View,
    search: String,
    drag: Option<Drag>,
}

#[derive(Debug, Clone)]
pub enum Message {
    View(View),
    Select(usize),
    Toggle(usize, bool),
    /// Move the mod at a load position by some places.
    MoveBy(usize, isize),
    Search(String),
    /// Pressed on a row's handle, by load position.
    DragStart(usize),
    /// The cursor moved over a row while dragging, `y` from its top.
    DragOver(usize, f32),
    /// The mouse button came up anywhere; the app sends this.
    DragEnd,
    DragCancel,
    Save,
    Undo,
    Redo,
    Launch { resume: bool },
    SwitchCollection(String),
    NewCollection,
    OpenFolder(usize),
    OpenWorkshop(u64),
}

/// What happened, for the app's status line: a message, and whether it's
/// an error.
pub type Notice = Option<(String, bool)>;

impl State {
    pub fn new(library: &Library) -> Self {
        Self { selected: library.state.enabled.first().copied().or((!library.state.mods.is_empty()).then_some(0)), view: View::LoadOrder, search: String::new(), drag: None }
    }

    pub fn update(&mut self, library: &mut Library, message: Message) -> Notice {
        let done = |r: Result<String, String>| match r {
            Ok(s) if s.is_empty() => None,
            Ok(s) => Some((s, false)),
            Err(e) => Some((e, true)),
        };
        let notice = match message {
            Message::View(v) => {
                self.view = v;
                None
            }
            Message::Select(i) => {
                self.selected = Some(i);
                None
            }
            Message::Toggle(i, on) => {
                library.state.set_enabled(i, on);
                self.selected = Some(i);
                None
            }
            Message::MoveBy(pos, by) => {
                let to = pos as isize + by;
                if to >= 0 {
                    library.state.move_to(pos, to as usize);
                }
                None
            }
            Message::Search(s) => {
                self.search = s;
                None
            }
            Message::DragStart(pos) => {
                self.selected = library.state.enabled.get(pos).copied();
                self.drag = Some(Drag { from: pos, over: None });
                None
            }
            Message::DragOver(pos, y) => {
                if let Some(drag) = &mut self.drag {
                    drag.over = Some((pos, y > ROW_HEIGHT / 2.0));
                }
                None
            }
            Message::DragEnd => {
                if let Some(Drag { from, over: Some((target, below)) }) = self.drag.take() {
                    library.state.move_enabled(from, if below { target + 1 } else { target });
                }
                None
            }
            Message::DragCancel => {
                self.drag = None;
                None
            }
            Message::Save => done(library.save()),
            Message::Undo => {
                library.state.undo();
                None
            }
            Message::Redo => {
                library.state.redo();
                None
            }
            Message::Launch { resume } => done(library.launch(resume)),
            Message::SwitchCollection(name) => done(library.switch_collection(&name)),
            Message::NewCollection => done(library.new_collection()),
            Message::OpenFolder(i) => {
                let folder = library.mod_at(i).content.clone();
                match folder {
                    Some(dir) => done(drukhal_core::platform::open(&dir.to_string_lossy()).map(|_| String::new()).map_err(|e| tf!("Could not open the folder: {error}", error = e))),
                    None => Some((t!("This mod's files weren't found"), true)),
                }
            }
            Message::OpenWorkshop(id) => done(drukhal_core::platform::open(&format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}")).map(|_| String::new()).map_err(|e| tf!("Could not open the Workshop page: {error}", error = e))),
        };
        library.recheck();
        notice
    }

    /// The name of the mod being dragged, for the app to draw by the cursor.
    pub fn dragging<'a>(&self, library: &'a Library) -> Option<&'a str> {
        let i = *library.state.enabled.get(self.drag?.from)?;
        Some(name(library, i))
    }

    pub fn header_actions<'a>(&'a self, library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
        let collection = button(row![text(t!("Collection")).size(13).color(c(p.muted)), text(&library.collections.active).size(14)].spacing(8).align_y(iced::Center))
            .padding([8, 16])
            .on_press(Message::View(View::Collections))
            .style(move |_, s| pill(p.container_high, p.on_window, s));
        let search = text_input(&t!("Search mods"), &self.search).on_input(Message::Search).padding([10, 16]).width(260).style(move |_, _| field(p, 999.0, false));
        let save = button(text(if library.state.dirty { t!("Save") } else { t!("Saved") }).size(14))
            .padding([10, 18])
            .on_press_maybe(library.state.dirty.then_some(Message::Save))
            .style(move |_, s| pill(p.secondary, p.on_window, s));
        let launch = button(row![icon(icons::PLAY, 18, p.on_accent), text(t!("Launch Stellaris")).size(14).color(c(p.on_accent))].spacing(8).align_y(iced::Center))
            .padding([10, 20])
            .on_press_maybe(library.game.is_some().then_some(Message::Launch { resume: false }))
            .style(move |_, s| pill(p.accent, p.on_accent, s));
        row![collection, search, save, launch].spacing(10).align_y(iced::Center).into()
    }

    pub fn status(&self, library: &Library) -> String {
        let game = library.game_version().map_or_else(|| t!("Stellaris not found"), |v| tf!("Stellaris {version}", version = v));
        tf!("{installed} mods installed · {on} in the load order · {game}", installed = library.state.mods.len(), on = library.state.enabled.len(), game = game)
    }

    pub fn view<'a>(&'a self, library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
        let main: Element<'a, Message> = match self.view {
            View::LoadOrder if library.state.mods.is_empty() => empty(library, p),
            View::LoadOrder => row![self.load_order(library, p), self.inspector(library, p)].spacing(12).into(),
            View::Collections => collections(library, p),
            View::Launch => launch(library, p),
        };
        row![self.rail(library, p), main].spacing(12).height(Fill).into()
    }

    fn rail<'a>(&'a self, library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
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
        let history = row![
            button(text(t!("Undo")).size(13)).padding([8, 14]).on_press_maybe(library.state.can_undo().then_some(Message::Undo)).style(move |_, s| pill(p.container, p.on_window, s)),
            button(text(t!("Redo")).size(13)).padding([8, 14]).on_press_maybe(library.state.can_redo().then_some(Message::Redo)).style(move |_, s| pill(p.container, p.on_window, s)),
        ]
        .spacing(6);
        let colours = container(
            column![
                text(t!("Colours")).size(12).color(c(p.muted)),
                text(crate::source_name(&p.source)).size(13),
                row([p.accent, p.secondary, p.container_high, p.success, p.warning, p.error].map(|col| container(space()).width(18).height(18).style(move |_| surface(col, col, 999.0)).into())).spacing(4),
            ]
            .spacing(6),
        )
        .padding(14)
        .width(Fill)
        .style(move |_| surface(p.container, p.on_window, 16.0));
        container(column![column(items).spacing(4), space().height(Fill), history, colours].spacing(8))
            .width(220)
            .height(Fill)
            .padding(8)
            .style(move |_| surface(p.container_low, p.on_window, 24.0))
            .into()
    }

    fn load_order<'a>(&'a self, library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
        let query = self.search.trim().to_lowercase();
        let matches = |i: usize| query.is_empty() || name(library, i).to_lowercase().contains(&query);
        let mut list = column![].spacing(2);
        list = list.push(section(p, tf!("In the load order ({count})", count = library.state.enabled.len())));
        for (pos, &i) in library.state.enabled.iter().enumerate() {
            if !matches(i) {
                continue;
            }
            let over = self.drag.and_then(|d| d.over).filter(|(t, _)| *t == pos);
            list = list.push(drop_line(p, over.is_some_and(|(_, below)| !below)));
            list = list.push(self.mod_row(library, p, i, Some(pos)));
            list = list.push(drop_line(p, over.is_some_and(|(_, below)| below)));
        }
        let disabled = library.state.disabled_in(library.settings.disabled_order, &library.settings.aliases);
        list = list.push(section(p, tf!("Not in the load order ({count})", count = disabled.len())));
        for i in disabled.into_iter().filter(|&i| matches(i)) {
            list = list.push(self.mod_row(library, p, i, None));
        }
        let head = row![text(t!("Load order")).size(20), space().width(Fill), text(if library.state.dirty { t!("Unsaved changes · Ctrl+S") } else { String::new() }).size(12).color(c(p.warning))]
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

    /// A mod's row; `pos` is its load position when it's in the load order.
    fn mod_row<'a>(&'a self, library: &'a Library, p: &'a Palette, i: usize, pos: Option<usize>) -> Element<'a, Message> {
        let m = library.mod_at(i);
        let selected = self.selected == Some(i);
        let enabled = pos.is_some();
        let fg = if enabled { p.on_window } else { p.muted };
        let issues = library.issues.get(&i).map(Vec::as_slice).unwrap_or_default();
        let mut flags = row![].spacing(6);
        if let Some(issue) = issues.iter().find(|i| i.requirement.is_some()) {
            flags = flags.push(hint(chip(t!("needs a mod"), p.error), &issue.message, p));
        } else if let Some(issue) = issues.iter().find(|i| i.severity == Severity::Warning) {
            flags = flags.push(hint(chip(t!("check"), p.warning), &issue.message, p));
        }
        // Made for another game version: worth a glance, not an alarm.
        let supported = m.descriptor.game_version();
        if let Some(game) = library.game_version()
            && !supported.is_empty()
            && !drukhal_core::mods::checks::version_matches(supported, game)
        {
            flags = flags.push(chip(tf!("for {version}", version = supported), p.warning));
        }
        if m.error.is_some() || m.content.is_none() {
            flags = flags.push(chip(t!("files missing"), p.error));
        }
        // The handle starts a drag; the button around the row only selects.
        // Not while searching: hidden rows would make a drop land unseen.
        let draggable = pos.is_some() && self.search.trim().is_empty();
        let handle: Element<'a, Message> = match pos {
            Some(pos) if draggable => mouse_area(container(icon(icons::DRAG, 18, p.muted)).padding([8, 4])).on_press(Message::DragStart(pos)).interaction(mouse::Interaction::Grab).into(),
            _ => container(icon(icons::DRAG, 18, if enabled { p.outline } else { p.window })).padding([8, 4]).into(),
        };
        let position = pos.map_or_else(|| "  ".to_owned(), |pos| format!("{:>3}", pos + 1));
        let subtitle = match (m.source, m.descriptor.remote_file_id) {
            (Source::Workshop, Some(id)) => tf!("Workshop · {id}", id = id),
            (Source::Workshop, None) => t!("Workshop"),
            (Source::Local, _) => t!("Local mod"),
        };
        let content = row![
            handle,
            text(position).font(MONO).size(13).color(c(p.muted)),
            column![text(name(library, i)).size(15).color(c(fg)), text(subtitle).size(12).color(c(p.muted))].width(Fill),
            flags,
            text(m.descriptor.game_version().to_owned()).font(MONO).size(12).color(c(p.muted)),
            toggler(enabled).on_toggle(move |on| Message::Toggle(i, on)).size(20),
        ]
        .spacing(14)
        .align_y(iced::Center);
        let dragged = self.drag.is_some_and(|d| Some(d.from) == pos);
        let row_button = button(content).width(Fill).padding([10, 16]).on_press(Message::Select(i)).style(move |_, s| {
            let mut st = if selected { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) };
            st.border.radius = 16.0.into();
            if dragged {
                // Where it was: an outline, so the gap reads as its old place.
                st.background = None;
                st.text_color = alpha(p.on_window, 0.4);
                st.border = iced::Border { radius: 16.0.into(), width: 1.5, color: c(p.outline) };
            }
            st
        });
        match pos {
            Some(pos) if self.drag.is_some() => mouse_area(row_button).on_move(move |pt| Message::DragOver(pos, pt.y)).interaction(mouse::Interaction::Grabbing).into(),
            _ => row_button.into(),
        }
    }

    fn inspector<'a>(&'a self, library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
        let Some(i) = self.selected.filter(|&i| i < library.state.mods.len()) else {
            return container(text(t!("Pick a mod to see it here.")).size(14).color(c(p.muted))).padding(20).width(Length::FillPortion(2)).height(Fill).style(move |_| surface(p.container, p.on_window, 24.0)).into();
        };
        let m = library.mod_at(i);
        let pos = library.state.enabled.iter().position(|&e| e == i);
        let fact = |value: String, label: String| column![text(value).size(20), text(label).size(12).color(c(p.muted))].spacing(2).width(Fill);
        let issues = library.issues.get(&i).map(Vec::as_slice).unwrap_or_default();
        let mut problems = column![].spacing(6);
        for issue in issues {
            problems = problems.push(issue_card(issue, p));
        }
        if issues.is_empty() {
            problems = problems.push(text(t!("Nothing to look at.")).size(13).color(c(p.muted)));
        }
        let note: Element<'a, Message> = match library.settings.notes.get(&m.id) {
            Some(note) if !note.is_empty() => container(text(note.as_str()).size(13)).padding([10, 14]).width(Fill).style(move |_| surface(p.container_high, p.on_window, 14.0)).into(),
            _ => space().into(),
        };
        let quiet = |label: String, glyph: &'static [u8], msg: Option<Message>| {
            button(row![icon(glyph, 16, p.on_window), text(label).size(13)].spacing(6).align_y(iced::Center)).padding([8, 14]).on_press_maybe(msg).style(move |_, s| pill(p.container_high, p.on_window, s))
        };
        let actions = row![
            quiet(t!("Up"), icons::UP, pos.filter(|&p| p > 0).map(|pos| Message::MoveBy(pos, -1))),
            quiet(t!("Down"), icons::DOWN_ARROW, pos.filter(|&p| p + 1 < library.state.enabled.len()).map(|pos| Message::MoveBy(pos, 1))),
            space().width(Fill),
            quiet(t!("Folder"), icons::FOLDER, m.content.is_some().then_some(Message::OpenFolder(i))),
            quiet(t!("Workshop"), icons::LINK, m.descriptor.remote_file_id.map(Message::OpenWorkshop)),
        ]
        .spacing(8);
        let tags = m.descriptor.tags.join(" · ");
        container(
            column![
                text(if m.source == Source::Workshop { t!("WORKSHOP") } else { t!("LOCAL") }).size(11).color(c(p.accent)),
                text(name(library, i)).font(DISPLAY).size(28),
                text(if tags.is_empty() { m.id.clone() } else { tags }).size(13).color(c(p.muted)),
                note,
                rule::horizontal(1).style(move |_| rule::Style { color: c(p.outline.mix(p.window, 0.5)), radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true }),
                row![
                    fact(m.descriptor.game_version().to_owned(), t!("game version")),
                    fact(if m.descriptor.version.is_empty() { "–".to_owned() } else { m.descriptor.version.clone() }, t!("mod version")),
                    fact(pos.map_or_else(|| "–".to_owned(), |pos| format!("#{}", pos + 1)), t!("in load order")),
                ],
                text(t!("To look at")).size(16),
                scrollable(problems).height(Fill),
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

/// The name a mod goes by: the player's alias for it, else its own.
fn name(library: &Library, i: usize) -> &str {
    let m = library.mod_at(i);
    library.settings.aliases.get(&m.id).map_or_else(|| mods::display_name(m), String::as_str)
}

fn section<'a>(p: &'a Palette, label: String) -> Element<'a, Message> {
    container(text(label.to_uppercase()).size(11).color(c(p.accent))).padding([12, 16]).into()
}

fn drop_line<'a>(p: &'a Palette, at: bool) -> Element<'a, Message> {
    if at { container(space()).width(Fill).height(3).style(move |_| surface(p.accent, p.accent, 2.0)).into() } else { space().height(0).into() }
}

fn hint<'a>(content: Element<'a, Message>, message: &'a str, p: &'a Palette) -> Element<'a, Message> {
    tooltip(content, container(text(message).size(12)).padding([6, 10]).max_width(360).style(move |_| surface(p.container_high, p.on_window, 8.0)), tooltip::Position::Top).into()
}

fn issue_card<'a>(issue: &'a Issue, p: &'a Palette) -> Element<'a, Message> {
    let (glyph, col): (&'static [u8], Rgb) = match (issue.requirement.is_some(), issue.severity) {
        (true, _) => (icons::WARNING, p.error),
        (false, Severity::Warning) => (icons::WARNING, p.warning),
        (false, Severity::Info) => (icons::INFO, p.muted),
    };
    container(row![icon(glyph, 18, col), text(issue.message.as_str()).size(13)].spacing(12).align_y(iced::Center))
        .padding([10, 14])
        .width(Fill)
        .style(move |_| surface(p.container_high, p.on_window, 14.0))
        .into()
}

fn empty<'a>(library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
    let why = match &library.error {
        Some(e) => tf!("The mods couldn't be read: {error}", error = e),
        None => tf!("No mods were found in {folder}. Subscribe to some on the Workshop, or set where the game keeps its data in Preferences.", folder = library.user_dir.display()),
    };
    container(column![text(t!("No mods yet")).size(20), text(why).size(13).color(c(p.muted))].spacing(8).max_width(640)).padding(20).width(Fill).height(Fill).style(move |_| surface(p.container_low, p.on_window, 24.0)).into()
}

fn collections<'a>(library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
    let installed: std::collections::HashSet<&str> = library.state.mods.iter().map(|m| m.id.as_str()).collect();
    let cards = column(library.collections.list.iter().map(|coll| {
        let active = coll.name == library.collections.active;
        let missing = coll.mods.iter().filter(|id| !installed.contains(id.as_str())).count();
        let detail = if missing == 0 {
            tnf!("{count} mod", "{count} mods", count = coll.mods.len())
        } else {
            tnf!("{count} mods, {missing} not installed", "{count} mods, {missing} not installed", count = coll.mods.len(), missing = missing)
        };
        let badge: Element<'a, Message> = if active { filled_chip(t!("Playing"), p.accent, p.on_accent) } else { space().into() };
        button(column![row![text(coll.name.as_str()).size(18), space().width(Fill), badge].align_y(iced::Center), text(detail).size(13).color(c(p.muted))].spacing(6))
            .padding(18)
            .width(Fill)
            .on_press_maybe((!active).then(|| Message::SwitchCollection(coll.name.clone())))
            .style(move |_, s| {
                let mut st = pill(p.container, p.on_window, s);
                st.border = iced::Border { radius: 20.0.into(), width: if active { 2.0 } else { 0.0 }, color: c(p.accent) };
                st
            })
            .into()
    }))
    .spacing(10);
    container(
        column![
            row![text(t!("Collections")).size(20), space().width(Fill), button(row![icon(icons::ADD, 16, p.on_window), text(t!("New from this load order")).size(13)].spacing(6).align_y(iced::Center)).padding([8, 14]).on_press(Message::NewCollection).style(move |_, s| pill(p.container_high, p.on_window, s))].align_y(iced::Center),
            text(t!("A collection is a load order you can switch to. Switching saves the current one first, and sets the Paradox Launcher's playset too.")).size(13).color(c(p.muted)),
            scrollable(cards).height(Fill),
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

fn launch<'a>(library: &'a Library, p: &'a Palette) -> Element<'a, Message> {
    let game: Element<'a, Message> = match &library.game {
        Some(game) => column![
            text(game.version.as_deref().map_or_else(|| t!("Stellaris"), |v| tf!("Stellaris {version}", version = v))).size(18),
            text(game.dir.display().to_string()).font(MONO).size(12).color(c(p.muted)),
        ]
        .spacing(4)
        .into(),
        None => text(t!("Stellaris wasn't found. Set where it is in Preferences.")).size(14).color(c(p.error)).into(),
    };
    let start = |label: String, resume: bool, primary: bool| {
        button(text(label).size(14).color(c(if primary { p.on_accent } else { p.on_window })))
            .padding([10, 18])
            .on_press_maybe(library.game.is_some().then_some(Message::Launch { resume }))
            .style(move |_, s| if primary { pill(p.accent, p.on_accent, s) } else { pill(p.container_high, p.on_window, s) })
    };
    let how = if drukhal_core::platform::is_flatpak() { t!("In the Flatpak, the game starts through Steam with this load order as the Paradox Launcher's playset.") } else { t!("The game starts directly with this load order and your launch options.") };
    container(
        column![
            text(t!("Launch")).size(20),
            container(game).padding(18).width(Fill).style(move |_| surface(p.container, p.on_window, 20.0)),
            row![start(t!("Launch Stellaris"), false, true), start(t!("Continue last save"), true, false)].spacing(8),
            text(how).size(13).color(c(p.muted)),
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
