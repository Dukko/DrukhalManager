//! Material 3 styling over iced: colours from the desktop's palette,
//! pill buttons, tonal surfaces, chips and icons.

use drukhal_palette::{Palette, Rgb};
use iced::widget::{button, container, svg, text, text_input};
use iced::{Background, Border, Color, Element, Font, Shadow};

pub const SANS: Font = Font::with_name("Google Sans Flex");
pub const DISPLAY: Font = Font::with_name("DM Serif Display");
/// Fira Code's file is a variable font whose default instance is Light,
/// and iced matches a named font's weight exactly, so ask for Light.
pub const MONO: Font = Font { weight: iced::font::Weight::Light, ..Font::with_name("Fira Code") };

pub fn c(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.r, rgb.g, rgb.b)
}

pub fn alpha(rgb: Rgb, a: f32) -> Color {
    Color { a, ..c(rgb) }
}

pub fn surface(bg: Rgb, fg: Rgb, radius: f32) -> container::Style {
    container::Style { background: Some(Background::Color(c(bg))), text_color: Some(c(fg)), border: Border { radius: radius.into(), ..Border::default() }, ..container::Style::default() }
}

pub fn outlined(col: Rgb, radius: f32) -> container::Style {
    container::Style { border: Border { radius: radius.into(), width: 1.0, color: c(col) }, ..container::Style::default() }
}

/// A pill button: hover and press add a state layer of the text colour.
pub fn pill(bg: Rgb, fg: Rgb, status: button::Status) -> button::Style {
    let layer = match status {
        button::Status::Hovered => 0.08,
        button::Status::Pressed => 0.12,
        _ => 0.0,
    };
    button::Style { background: Some(Background::Color(c(bg.mix(fg, layer)))), text_color: c(fg), border: Border { radius: 999.0.into(), ..Border::default() }, shadow: Shadow::default(), snap: true }
}

/// The same, with a transparent resting state, for quiet buttons.
pub fn ghost(fg: Rgb, status: button::Status) -> button::Style {
    let a = match status {
        button::Status::Hovered => 0.08,
        button::Status::Pressed => 0.12,
        _ => 0.0,
    };
    button::Style { background: Some(Background::Color(alpha(fg, a))), text_color: c(fg), border: Border { radius: 999.0.into(), ..Border::default() }, shadow: Shadow::default(), snap: true }
}

pub fn field(p: &Palette, radius: f32, focus_ring: bool) -> text_input::Style {
    text_input::Style {
        background: Background::Color(c(p.container)),
        border: Border { radius: radius.into(), width: if focus_ring { 2.0 } else { 0.0 }, color: c(p.accent) },
        icon: c(p.muted),
        placeholder: c(p.muted),
        value: c(p.on_window),
        selection: c(p.secondary),
    }
}

pub fn chip<'a, M: 'a>(label: impl text::IntoFragment<'a>, tone: Rgb) -> Element<'a, M> {
    container(text(label).size(11).color(c(tone))).padding([3, 10]).style(move |_| outlined(tone, 999.0)).into()
}

pub fn filled_chip<'a, M: 'a>(label: impl text::IntoFragment<'a>, bg: Rgb, fg: Rgb) -> Element<'a, M> {
    container(text(label).size(11).color(c(fg))).padding([3, 10]).style(move |_| surface(bg, fg, 999.0)).into()
}

pub fn icon<'a, M: 'a>(bytes: &'static [u8], size: u16, col: Rgb) -> Element<'a, M> {
    svg(svg::Handle::from_memory(bytes)).width(size as f32).height(size as f32).style(move |_, _| svg::Style { color: Some(c(col)) }).into()
}

/// The Material icons the app ships, by the name the GTK app uses.
pub mod icons {
    macro_rules! icons {
        ($($name:ident = $file:literal),* $(,)?) => {
            $(pub const $name: &[u8] = include_bytes!(concat!("../../../data/icons/material/", $file, "-symbolic.svg"));)*
        };
    }
    icons! {
        LIST = "view-list",
        COLLECTIONS = "io.github.dukko.DrukhalManager-collections",
        GAMES = "applications-games",
        WARNING = "dialog-warning",
        PATCH = "package-x-generic",
        HIDE = "view-conceal",
        DOWN = "pan-down",
        PLAY = "media-playback-start",
        DRAG = "list-drag-handle",
        AI = "io.github.dukko.DrukhalManager-ai",
        INFO = "dialog-information",
        OK = "emblem-ok",
        LINK = "adw-external-link",
        FOLDER = "document-open",
        UP = "go-up",
        DOWN_ARROW = "go-down",
        REFRESH = "view-refresh",
        NEXT = "go-next",
        PREVIOUS = "go-previous",
        UPDATE = "software-update-available",
    }
}
