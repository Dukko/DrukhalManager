//! Code cards for the Conflicts page: one version of an object or file per
//! card, with line numbers, light syntax colouring and the differences from
//! the card next to it highlighted. Two cards side by side are a [`Pair`],
//! aligned row by row and scrolled together.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::diff::{self, Diff, Hunk, Kind};

/// Above this many lines, syntax colouring is skipped to stay responsive.
const COLOUR_LIMIT: usize = 20_000;

/// What a card's header shows.
#[derive(Debug, Clone, Default)]
pub struct Header {
    pub title: String,
    pub subtitle: String,
    /// Index into the mod colour palette, for the dot before the title.
    pub color: Option<usize>,
    /// A small label next to the actions, with an optional style class
    /// (`success`, `accent` or `warning`).
    pub badge: Option<(String, Option<&'static str>)>,
}

/// One version, in a card.
pub struct Card {
    pub widget: gtk::Box,
    dot: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    badge: gtk::Label,
    actions: gtk::Box,
    numbers_scroll: gtk::ScrolledWindow,
    pub scroll: gtk::ScrolledWindow,
    numbers: gtk::TextView,
    pub code: gtk::TextView,
}

impl Card {
    pub fn new(editable: bool) -> Rc<Self> {
        install_css();
        let dot = gtk::Box::builder().css_classes(["mod-dot"]).valign(gtk::Align::Center).visible(false).build();
        let title = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).width_chars(12).css_classes(["heading"]).build();
        let subtitle = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).css_classes(["dim-label", "caption", "monospace"]).build();
        let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).valign(gtk::Align::Center).build();
        titles.append(&title);
        titles.append(&subtitle);
        let badge = gtk::Label::builder().css_classes(["badge"]).valign(gtk::Align::Center).visible(false).build();
        let actions = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
        let header = gtk::Box::builder().spacing(10).css_classes(["code-card-header"]).build();
        header.append(&dot);
        header.append(&titles);
        header.append(&badge);
        header.append(&actions);

        let numbers = code_view(false);
        numbers.add_css_class("line-numbers");
        numbers.set_can_target(false);
        numbers.set_justification(gtk::Justification::Right);
        numbers.set_left_margin(10);
        numbers.set_right_margin(6);
        let code = code_view(editable);
        create_tags(&numbers.buffer());
        create_tags(&code.buffer());
        let scroll = gtk::ScrolledWindow::builder().child(&code).hexpand(true).vexpand(true).build();
        // The numbers follow the code's scrolling, without a scrollbar.
        let numbers_scroll = gtk::ScrolledWindow::builder()
            .child(&numbers)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .vadjustment(&scroll.vadjustment())
            .build();
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.append(&numbers_scroll);
        body.append(&scroll);

        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card", "code-card"]).hexpand(true).vexpand(true).build();
        widget.append(&header);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&body);

        let card = Rc::new(Self { widget, dot, title, subtitle, badge, actions, numbers_scroll, scroll, numbers, code });
        if editable {
            // Line numbers keep up with edits.
            let weak = Rc::downgrade(&card);
            card.code.buffer().connect_changed(move |buffer| {
                if let Some(card) = weak.upgrade() {
                    let lines = buffer.line_count().max(1) as usize;
                    let current = card.numbers.buffer().line_count() as usize;
                    if lines != current || card.numbers.buffer().char_count() == 0 {
                        card.set_numbers(&numbers_text(lines));
                    }
                    colour_lines(buffer, 0, lines);
                }
            });
        }
        card
    }

    /// Fills the line numbers and sizes their column to fit them; a text
    /// view only measures itself after it was laid out.
    fn set_numbers(&self, text: &str) {
        self.numbers.buffer().set_text(text);
        let digits = text.lines().map(str::len).max().unwrap_or(1).max(2);
        let (width, _) = self.numbers.create_pango_layout(Some(&"0".repeat(digits))).pixel_size();
        self.numbers.set_size_request(width + self.numbers.left_margin() + self.numbers.right_margin(), -1);
    }

    pub fn set_header(&self, header: &Header) {
        self.title.set_label(&header.title);
        self.title.set_tooltip_text(Some(&header.title));
        self.subtitle.set_label(&header.subtitle);
        self.subtitle.set_tooltip_text(Some(&header.subtitle));
        self.subtitle.set_visible(!header.subtitle.is_empty());
        self.dot.set_visible(header.color.is_some());
        if let Some(c) = header.color {
            self.dot.set_css_classes(&["mod-dot", &format!("mod-color-{}", c % PALETTE)]);
        }
        match &header.badge {
            Some((text, class)) => {
                self.badge.set_label(text);
                self.badge.set_css_classes(&["badge"]);
                if let Some(class) = class {
                    self.badge.add_css_class(class);
                }
                self.badge.set_visible(true);
            }
            None => self.badge.set_visible(false),
        }
    }

    /// Replaces the header's buttons.
    pub fn set_actions(&self, buttons: &[gtk::Widget]) {
        while let Some(child) = self.actions.first_child() {
            self.actions.remove(&child);
        }
        for b in buttons {
            self.actions.append(b);
        }
    }

    /// Shows `text` as it is, with no differences marked.
    pub fn set_text(&self, text: &str) {
        let lines = text.lines().count().max(1);
        self.set_numbers(&numbers_text(lines));
        let buffer = self.code.buffer();
        buffer.set_text(text);
        if !self.code.is_editable() {
            colour_lines(&buffer, 0, lines);
        }
        buffer.place_cursor(&buffer.start_iter());
    }

    pub fn text(&self) -> String {
        let buffer = self.code.buffer();
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
    }

    /// Shows one side of `diff`: `text` is this side, `other` the other one.
    fn show_side(&self, text: &str, other: &str, diff: &Diff, side: Side) {
        let mine: Vec<&str> = text.lines().collect();
        let theirs: Vec<&str> = other.lines().collect();
        let (mut code, mut numbers) = (String::new(), String::new());
        for (i, row) in diff.rows.iter().enumerate() {
            if i > 0 {
                code.push('\n');
                numbers.push('\n');
            }
            if let Some(line) = side.of(row) {
                code.push_str(mine[line]);
                numbers.push_str(&(line + 1).to_string());
            }
        }
        let buffer = self.code.buffer();
        buffer.set_text(&code);
        self.set_numbers(&numbers);
        if diff.rows.len() <= COLOUR_LIMIT {
            colour_lines(&buffer, 0, diff.rows.len());
        }
        for (i, row) in diff.rows.iter().enumerate() {
            let tag = match (row.kind, side.of(row).is_some()) {
                (Kind::Same, _) => continue,
                (_, false) => "filler",
                (Kind::Changed, true) => "changed",
                (Kind::Removed, true) => "removed",
                (Kind::Added, true) => "added",
            };
            tag_line(&buffer, i, tag);
            tag_line(&self.numbers.buffer(), i, tag);
            if row.kind == Kind::Changed
                && let (Some(l), Some(r)) = (row.left, row.right)
            {
                let (ranges_l, ranges_r) = diff::words(if side == Side::Left { mine[l] } else { theirs[l] }, if side == Side::Left { theirs[r] } else { mine[r] });
                for range in if side == Side::Left { ranges_l } else { ranges_r } {
                    if let (Some(start), Some(end)) = (buffer.iter_at_line_index(i as i32, range.start as i32), buffer.iter_at_line_index(i as i32, range.end as i32)) {
                        buffer.apply_tag_by_name("word", &start, &end);
                    }
                }
            }
        }
        buffer.place_cursor(&buffer.start_iter());
    }

    /// Highlights `rows` as the difference being looked at.
    fn mark_current(&self, rows: Option<std::ops::Range<usize>>) {
        for buffer in [self.code.buffer(), self.numbers.buffer()] {
            buffer.remove_tag_by_name("current", &buffer.start_iter(), &buffer.end_iter());
            if let Some(rows) = &rows {
                for i in rows.clone() {
                    tag_line(&buffer, i, "current");
                }
            }
        }
    }

    fn scroll_to_row(&self, row: usize) {
        let buffer = self.code.buffer();
        if let Some(mut iter) = buffer.iter_at_line(row as i32) {
            self.code.scroll_to_iter(&mut iter, 0.0, true, 0.0, 0.3);
        }
    }

    /// Lets this card scroll on its own again after sharing a pair's
    /// scrolling.
    fn unlink(&self) {
        let own = gtk::Adjustment::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        self.scroll.set_vadjustment(Some(&own));
        self.numbers_scroll.set_vadjustment(Some(&own));
    }

    fn link_to(&self, other: &Card) {
        let shared = other.scroll.vadjustment();
        self.scroll.set_vadjustment(Some(&shared));
        self.numbers_scroll.set_vadjustment(Some(&shared));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

impl Side {
    fn of(self, row: &diff::Row) -> Option<usize> {
        match self {
            Side::Left => row.left,
            Side::Right => row.right,
        }
    }
}

/// How a pair's two cards are compared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Compare {
    /// With each other, aligned row by row.
    #[default]
    Each,
    /// Each with this text (the base game's version), so each card marks
    /// what its mod changed. The cards aren't aligned or scrolled together.
    Base(String),
}

/// Two cards side by side, with the differences between them.
#[allow(clippy::type_complexity)]
pub struct Pair {
    pub widget: gtk::Box,
    pub left: Rc<Card>,
    pub right: Rc<Card>,
    texts: RefCell<(String, String)>,
    diff: RefCell<Diff>,
    current: Cell<Option<usize>>,
    /// Whether the cards are compared with the base game instead.
    base: Cell<bool>,
    /// Called when the difference count or the current one changes, with
    /// whether the cards are compared with the base game.
    on_change: RefCell<Vec<Box<dyn Fn(usize, Option<usize>, bool)>>>,
}

impl Pair {
    pub fn new() -> Rc<Self> {
        let (left, right) = (Card::new(false), Card::new(false));
        let widget = gtk::Box::builder().spacing(12).homogeneous(true).vexpand(true).build();
        widget.append(&left.widget);
        widget.append(&right.widget);
        Rc::new(Self { widget, left, right, texts: RefCell::default(), diff: RefCell::default(), current: Cell::new(None), base: Cell::new(false), on_change: RefCell::default() })
    }

    pub fn connect_change(&self, f: impl Fn(usize, Option<usize>, bool) + 'static) {
        self.on_change.borrow_mut().push(Box::new(f));
    }

    /// Shows `left` and `right`, compared as `compare` says.
    pub fn show(&self, left: &str, right: &str, compare: &Compare, ignore_whitespace: bool) {
        let mut diff = diff::lines(left, right, ignore_whitespace);
        self.base.set(matches!(compare, Compare::Base(_)));
        match compare {
            Compare::Each => {
                self.right.link_to(&self.left);
                self.left.show_side(left, right, &diff, Side::Left);
                self.right.show_side(right, left, &diff, Side::Right);
            }
            Compare::Base(base) => {
                self.left.unlink();
                self.right.unlink();
                let l = diff::lines(base, left, ignore_whitespace);
                let r = diff::lines(base, right, ignore_whitespace);
                self.left.show_changes(left, base, &l);
                self.right.show_changes(right, base, &r);
                // The rows shown aren't the pair's diff, so there are no
                // differences to step through.
                diff = Diff::default();
            }
        }
        self.texts.replace((left.to_owned(), right.to_owned()));
        self.diff.replace(diff);
        self.current.set(None);
        self.left.mark_current(None);
        self.right.mark_current(None);
        self.notify();
    }

    /// Number of differences between the two cards.
    pub fn count(&self) -> usize {
        self.diff.borrow().hunks.len()
    }

    /// Moves to the next (`forward`) or previous difference.
    pub fn step(&self, forward: bool) {
        let n = self.count();
        if n == 0 {
            return;
        }
        let next = match (self.current.get(), forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        self.current.set(Some(next));
        let rows = self.diff.borrow().hunks[next].rows.clone();
        self.left.mark_current(Some(rows.clone()));
        self.right.mark_current(Some(rows.clone()));
        // Defer, so a freshly filled view is laid out first.
        let (left, right) = (Rc::downgrade(&self.left), Rc::downgrade(&self.right));
        glib::idle_add_local_once(move || {
            if let Some(l) = left.upgrade() {
                l.scroll_to_row(rows.start);
            }
            if let Some(r) = right.upgrade() {
                r.scroll_to_row(rows.start);
            }
        });
        self.notify();
    }

    /// The difference being looked at.
    pub fn current(&self) -> Option<Hunk> {
        self.current.get().and_then(|i| self.diff.borrow().hunks.get(i).cloned())
    }

    /// The left text with the current difference taken from the right.
    pub fn take_current(&self) -> Option<String> {
        let hunk = self.current()?;
        let (left, right) = &*self.texts.borrow();
        Some(diff::take(left, right, &hunk))
    }

    fn notify(&self) {
        for f in &*self.on_change.borrow() {
            f(self.count(), self.current.get(), self.base.get());
        }
    }
}

impl Card {
    /// Shows `text` with the lines that differ from `base` marked.
    fn show_changes(&self, text: &str, base: &str, diff: &Diff) {
        let lines = text.lines().count().max(1);
        self.set_numbers(&numbers_text(lines));
        let buffer = self.code.buffer();
        buffer.set_text(text);
        if lines <= COLOUR_LIMIT {
            colour_lines(&buffer, 0, lines);
        }
        let base_lines: Vec<&str> = base.lines().collect();
        let mine: Vec<&str> = text.lines().collect();
        for row in &diff.rows {
            let Some(line) = row.right else { continue };
            let tag = match row.kind {
                Kind::Same => continue,
                Kind::Changed => "changed",
                _ => "added",
            };
            tag_line(&buffer, line, tag);
            tag_line(&self.numbers.buffer(), line, tag);
            if let (Kind::Changed, Some(b)) = (row.kind, row.left) {
                for range in diff::words(base_lines[b], mine[line]).1 {
                    if let (Some(start), Some(end)) = (buffer.iter_at_line_index(line as i32, range.start as i32), buffer.iter_at_line_index(line as i32, range.end as i32)) {
                        buffer.apply_tag_by_name("word", &start, &end);
                    }
                }
            }
        }
        buffer.place_cursor(&buffer.start_iter());
    }
}

fn code_view(editable: bool) -> gtk::TextView {
    gtk::TextView::builder()
        .monospace(true)
        .editable(editable)
        .cursor_visible(editable)
        .wrap_mode(gtk::WrapMode::None)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(12)
        .css_classes(["code-view"])
        .build()
}

fn numbers_text(lines: usize) -> String {
    (1..=lines).map(|n| n.to_string()).collect::<Vec<_>>().join("\n")
}

fn tag_line(buffer: &gtk::TextBuffer, line: usize, tag: &str) {
    let Some(start) = buffer.iter_at_line(line as i32) else { return };
    let mut end = start;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    // Paragraph backgrounds need at least the line itself tagged; an empty
    // line is tagged through its newline.
    if start == end {
        end.forward_char();
    }
    buffer.apply_tag_by_name(tag, &start, &end);
}

/// The tags every card buffer uses. Later tags take priority.
fn create_tags(buffer: &gtk::TextBuffer) {
    let dark = adw::StyleManager::default().is_dark();
    let pick = |light: &str, dark_colour: &str| if dark { dark_colour.to_owned() } else { light.to_owned() };
    let table = buffer.tag_table();
    let tag = |name: &str, props: &[(&str, glib::Value)]| {
        let t = gtk::TextTag::new(Some(name));
        for (k, v) in props {
            t.set_property_from_value(k, v);
        }
        table.add(&t);
    };
    tag("key", &[("foreground", pick("#1c71d8", "#78aeed").to_value())]);
    tag("number", &[("foreground", pick("#c64600", "#ffa348").to_value())]);
    tag("string", &[("foreground", pick("#26a269", "#8ff0a4").to_value())]);
    tag("comment", &[("foreground", pick("#77767b", "#9a9996").to_value())]);
    tag("filler", &[("paragraph-background", pick("rgba(128,128,128,0.10)", "rgba(255,255,255,0.05)").to_value())]);
    tag("added", &[("paragraph-background", pick("rgba(46,194,126,0.18)", "rgba(46,194,126,0.16)").to_value())]);
    tag("removed", &[("paragraph-background", pick("rgba(224,27,36,0.13)", "rgba(224,27,36,0.18)").to_value())]);
    tag("changed", &[("paragraph-background", pick("rgba(245,194,17,0.24)", "rgba(245,194,17,0.14)").to_value())]);
    tag("current", &[("paragraph-background", pick("rgba(53,132,228,0.22)", "rgba(53,132,228,0.30)").to_value())]);
    tag("word", &[("background", pick("rgba(230,97,0,0.28)", "rgba(255,163,72,0.30)").to_value())]);
}

/// Colours keys, numbers, strings and comments in lines `from..to`.
fn colour_lines(buffer: &gtk::TextBuffer, from: usize, to: usize) {
    for name in ["key", "number", "string", "comment"] {
        buffer.remove_tag_by_name(name, &buffer.start_iter(), &buffer.end_iter());
    }
    if to.saturating_sub(from) > COLOUR_LIMIT {
        return;
    }
    for line in from..to {
        let Some(start) = buffer.iter_at_line(line as i32) else { break };
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let text = buffer.text(&start, &end, false);
        for (range, name) in highlight(&text) {
            if let (Some(a), Some(b)) = (buffer.iter_at_line_index(line as i32, range.start as i32), buffer.iter_at_line_index(line as i32, range.end as i32)) {
                buffer.apply_tag_by_name(name, &a, &b);
            }
        }
    }
}

/// Byte ranges of one line of Paradox script to colour, with the tag name.
fn highlight(line: &str) -> Vec<(std::ops::Range<usize>, &'static str)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'#' {
            out.push((i..bytes.len(), "comment"));
            break;
        }
        if c == b'"' {
            let end = line[i + 1..].find('"').map_or(bytes.len(), |p| i + 2 + p);
            out.push((i..end, "string"));
            i = end;
            continue;
        }
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'@' || b == b':' || b == b'-' || b >= 0x80;
        if word(c) {
            let start = i;
            while i < bytes.len() && word(bytes[i]) {
                i += 1;
            }
            let token = &line[start..i];
            let rest = line[i..].trim_start();
            let is_number = token.trim_start_matches('-').parse::<f64>().is_ok();
            if is_number {
                out.push((start..i, "number"));
            } else if rest.starts_with('=') || rest.starts_with('<') || rest.starts_with('>') || rest.starts_with("!=") {
                out.push((start..i, "key"));
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Mod colours, used by index in load order.
const PALETTE: usize = 8;

thread_local! {
    static CSS_INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Adds the Conflicts page's styles to the display, once.
pub fn install_css() {
    if CSS_INSTALLED.with(|c| c.replace(true)) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

const CSS: &str = r#"
.code-card { padding: 0; }
.code-card-header { padding: 8px 10px 8px 14px; min-height: 40px; }
.code-view, .code-view text, .line-numbers, .line-numbers text { background: transparent; }
.line-numbers text { color: alpha(currentColor, 0.45); }
.mod-dot { min-width: 10px; min-height: 10px; border-radius: 99px; }
.mod-color-0 { background: #3584e4; }
.mod-color-1 { background: #2ec27e; }
.mod-color-2 { background: #e66100; }
.mod-color-3 { background: #9141ac; }
.mod-color-4 { background: #e01b24; }
.mod-color-5 { background: #f5c211; }
.mod-color-6 { background: #1c9fa0; }
.mod-color-7 { background: #986a44; }
.badge { font-size: 0.8em; font-weight: bold; border-radius: 6px; padding: 2px 8px; background: alpha(currentColor, 0.08); color: alpha(currentColor, 0.7); }
.badge.success { background: alpha(@success_color, 0.18); color: @success_color; }
.badge.accent { background: alpha(@accent_bg_color, 0.18); color: @accent_color; }
.badge.warning { background: alpha(@warning_color, 0.2); color: @warning_color; }
.chip { border-radius: 99px; padding: 3px 12px 3px 10px; background: alpha(currentColor, 0.07); }
.chip.done { background: alpha(@success_color, 0.14); }
.chip.current { background: alpha(@accent_bg_color, 0.22); font-weight: bold; }
.chip.later { opacity: 0.75; }
button.chip { min-height: 0; }
.status-dot { min-width: 14px; min-height: 14px; border-radius: 99px; border: 2px solid alpha(currentColor, 0.4); }
.status-dot.progress { border-color: @accent_color; border-right-color: alpha(currentColor, 0.15); border-bottom-color: alpha(currentColor, 0.15); }
.conflict-list row.folder { padding-top: 14px; }
.conflict-list row.folder label { font-size: 0.85em; font-weight: bold; }
.conflict-list row.conflict { padding: 6px 8px; border-radius: 8px; margin: 1px 6px; }
.conflict-title { font-size: 1.6em; font-weight: 800; }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_keys_numbers_strings_and_comments() {
        let line = "\tcost = { energy = 100 } # cheap";
        let tags: Vec<(&str, &str)> = highlight(line).into_iter().map(|(r, t)| (&line[r], t)).collect();
        assert_eq!(tags, [("cost", "key"), ("energy", "key"), ("100", "number"), ("# cheap", "comment")]);
        let line = "name = \"Lab # 1\" size > -5.5";
        let tags: Vec<(&str, &str)> = highlight(line).into_iter().map(|(r, t)| (&line[r], t)).collect();
        assert_eq!(tags, [("name", "key"), ("\"Lab # 1\"", "string"), ("size", "key"), ("-5.5", "number")]);
    }

    #[test]
    fn numbers_lines() {
        assert_eq!(numbers_text(3), "1\n2\n3");
    }
}
