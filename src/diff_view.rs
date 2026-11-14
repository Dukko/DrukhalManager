//! Code cards for the Conflicts page: one version of an object or file per
//! card, with line numbers, light syntax colouring and the differences from
//! the card next to it highlighted. Two cards side by side are a [`Pair`],
//! aligned row by row and scrolled together.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::diff::{self, Diff, Hunk, Kind};

/// Unchanged lines kept around each difference when the rest is folded.
const FOLD_CONTEXT: usize = 3;
/// The fewest lines worth folding: the fold's own bubble takes room.
const FOLD_MIN: usize = 4;
/// Space around a fold bubble, above and below together, in pixels.
const FOLD_ROOM: i32 = 12;

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

/// What clicking a fold bubble does now.
type OnClick = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
type Bubble = (gtk::Button, OnClick);

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
    /// Whether the card keeps its own line numbers and colours up to date
    /// while edited. A pair refreshes its editable card itself.
    standalone: Cell<bool>,
    /// The bubbles standing in for folded lines, each with where its room
    /// is: above (or else below) a line, and how far into that room it sits.
    folds: RefCell<Vec<(gtk::Button, usize, bool, i32)>>,
    /// Every bubble made so far, with what clicking it does. A text view
    /// can't give its overlays back, so they're hidden and used again.
    bubbles: RefCell<Vec<Bubble>>,
}

impl Card {
    pub fn new(editable: bool) -> Rc<Self> {
        install_css();
        let dot = gtk::Box::builder().css_classes(["mod-dot"]).valign(gtk::Align::Center).visible(false).build();
        let title = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).width_chars(12).css_classes(["heading"]).build();
        let subtitle = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).css_classes(["dim-label", "caption", "monospace"]).build();
        // Whether the version can be changed here, next to its title.
        let (icon, text, tooltip) = if editable {
            ("document-edit-symbolic", "Editable", "You can change this version: type in it, paste into it or copy from it")
        } else {
            ("changes-prevent-symbolic", "View only", "This version can't be changed here; select text to copy it")
        };
        let mode = gtk::Box::builder().spacing(4).valign(gtk::Align::Center).tooltip_text(tooltip).css_classes(["card-mode", if editable { "editable" } else { "view-only" }]).build();
        mode.append(&gtk::Image::from_icon_name(icon));
        mode.append(&gtk::Label::new(Some(text)));
        let title_row = gtk::Box::builder().spacing(8).build();
        title_row.append(&title);
        title_row.append(&mode);
        let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).valign(gtk::Align::Center).build();
        titles.append(&title_row);
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
        if editable {
            widget.add_css_class("editable");
        }
        widget.append(&header);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&body);

        let card = Rc::new(Self { widget, dot, title, subtitle, badge, actions, numbers_scroll, scroll, numbers, code, standalone: Cell::new(true), folds: RefCell::default(), bubbles: RefCell::default() });
        // Fold bubbles follow the text as it's laid out: a long text is
        // measured bit by bit, which shows as its scrolling range changing.
        // The adjustment is replaced when cards link their scrolling.
        let watch = {
            let weak = Rc::downgrade(&card);
            move |code: &gtk::TextView| {
                let Some(adjustment) = code.vadjustment() else { return };
                let weak = weak.clone();
                adjustment.connect_changed(move |_| {
                    if let Some(card) = weak.upgrade() {
                        card.place_folds();
                    }
                });
            }
        };
        watch(&card.code);
        card.code.connect_vadjustment_notify(watch);
        if editable {
            // Line numbers keep up with edits.
            let weak = Rc::downgrade(&card);
            card.code.buffer().connect_changed(move |buffer| {
                if let Some(card) = weak.upgrade() {
                    card.place_folds();
                }
                if let Some(card) = weak.upgrade()
                    && card.standalone.get()
                {
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
        buffer_text(&self.code.buffer())
    }

    /// Puts `text` in the card, unless `keep_text` says to mark what's
    /// already there (the user's edits), then numbers and colours it and
    /// clears the old marks.
    fn fill(&self, text: &str, keep_text: bool) {
        let buffer = self.code.buffer();
        if !keep_text && set_if_changed(&buffer, &text.lines().collect::<Vec<_>>().join("\n")) {
            buffer.place_cursor(&buffer.start_iter());
        }
        let lines = text.lines().count().max(buffer.line_count() as usize).max(1);
        let numbers = numbers_text(lines);
        if buffer_text(&self.numbers.buffer()) != numbers {
            self.set_numbers(&numbers);
        }
        clear_marks(&buffer);
        clear_marks(&self.numbers.buffer());
        for (fold, ..) in self.folds.take() {
            fold.set_visible(false);
        }
        if lines <= COLOUR_LIMIT {
            colour_lines(&buffer, 0, lines);
        }
    }

    /// Shows one side of `diff`: `text` is this side, `other` the other
    /// one. Rows where only the other side has a line become space above
    /// the next line, so the buffer only ever holds this side's text.
    fn show_side(&self, text: &str, other: &str, diff: &Diff, side: Side, keep_text: bool) {
        self.fill(text, keep_text);
        let mine: Vec<&str> = text.lines().collect();
        let theirs: Vec<&str> = other.lines().collect();
        let buffer = self.code.buffer();
        let mut missing = 0;
        let mut gaps = Vec::new();
        for row in &diff.rows {
            let Some(line) = side.of(row) else {
                missing += 1;
                continue;
            };
            if missing > 0 {
                gaps.push((line, true, missing));
                missing = 0;
            }
            let tag = match row.kind {
                Kind::Same => continue,
                Kind::Changed => "changed",
                Kind::Removed => "removed",
                Kind::Added => "added",
            };
            tag_line(&buffer, line, tag);
            tag_line(&self.numbers.buffer(), line, tag);
            if row.kind == Kind::Changed
                && let (Some(l), Some(r)) = (row.left, row.right)
            {
                let (ranges_l, ranges_r) = diff::words(if side == Side::Left { mine[l] } else { theirs[l] }, if side == Side::Left { theirs[r] } else { mine[r] });
                for range in if side == Side::Left { ranges_l } else { ranges_r } {
                    if let (Some(start), Some(end)) = (buffer.iter_at_line_index(line as i32, range.start as i32), buffer.iter_at_line_index(line as i32, range.end as i32)) {
                        buffer.apply_tag_by_name("word", &start, &end);
                    }
                }
            }
        }
        if missing > 0
            && let Some(last) = mine.len().checked_sub(1)
        {
            gaps.push((last, false, missing));
        }
        for (line, above, rows) in gaps {
            self.add_gap(line, above, rows * self.line_height());
        }
    }

    /// Shows `text` with the lines that differ from `base` marked.
    fn show_changes(&self, text: &str, base: &str, diff: &Diff, keep_text: bool) {
        self.fill(text, keep_text);
        let buffer = self.code.buffer();
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
    }

    /// The height of a line of code.
    fn line_height(&self) -> i32 {
        self.code.create_pango_layout(Some("X")).pixel_size().1.max(1)
    }

    /// Leaves `px` of room above (or below) `line`: for lines the other
    /// card has and this one doesn't, or for a fold bubble.
    fn add_gap(&self, line: usize, above: bool, px: i32) {
        let name = format!("gap-{}-{px}", if above { "above" } else { "below" });
        for buffer in [self.code.buffer(), self.numbers.buffer()] {
            let table = buffer.tag_table();
            if table.lookup(&name).is_none() {
                let tag = gtk::TextTag::new(Some(&name));
                if above {
                    tag.set_pixels_above_lines(px);
                } else {
                    tag.set_pixels_below_lines(px);
                }
                table.add(&tag);
            }
            tag_line(&buffer, line, &name);
        }
    }

    /// Hides `lines`, which are the same on both cards, behind a bubble
    /// that says how many there are and calls `expand` when clicked. The
    /// bubble gets room of its own, `FOLD_ROOM` taller than a line of
    /// `height`, so it can't cover text.
    fn fold(&self, lines: std::ops::Range<usize>, height: i32, expand: impl Fn() + 'static) {
        for buffer in [self.code.buffer(), self.numbers.buffer()] {
            let Some(start) = buffer.iter_at_line(lines.start as i32) else { return };
            let end = buffer.iter_at_line(lines.end as i32).unwrap_or_else(|| buffer.end_iter());
            buffer.apply_tag_by_name("folded", &start, &end);
        }
        // The room is above the line after the fold, or at the end of the
        // text below the line before it.
        let (line, above) = if lines.end < self.code.buffer().line_count() as usize { (lines.end, true) } else { (lines.start.saturating_sub(1), false) };
        self.add_gap(line, above, height + FOLD_ROOM);
        let n = lines.len();
        let free = self.bubbles.borrow().iter().find(|(b, _)| !b.is_visible()).cloned();
        let (bubble, on_click) = free.unwrap_or_else(|| {
            let bubble = gtk::Button::builder()
                .tooltip_text(t!("Show these lines"))
                .css_classes(["fold-bubble"])
                .cursor(&gtk::gdk::Cursor::from_name("pointer", None).expect("the pointer cursor exists"))
                .build();
            let on_click = OnClick::default();
            bubble.connect_clicked({
                let on_click = on_click.clone();
                move |_| {
                    // Taken out first: expanding hides and reuses bubbles.
                    let expand = on_click.borrow().clone();
                    if let Some(expand) = expand {
                        expand();
                    }
                }
            });
            self.code.add_overlay(&bubble, 0, 0);
            self.bubbles.borrow_mut().push((bubble.clone(), on_click.clone()));
            (bubble, on_click)
        });
        bubble.set_label(&format!("⋯  {n} unchanged {}", if n == 1 { "line" } else { "lines" }));
        bubble.set_height_request(height);
        bubble.set_visible(true);
        on_click.replace(Some(Rc::new(expand)));
        self.folds.borrow_mut().push((bubble, line, above, FOLD_ROOM / 2));
        self.place_folds();
    }

    /// Puts each fold bubble in its room, going by where the text view
    /// really laid the lines out.
    fn place_folds(&self) {
        let buffer = self.code.buffer();
        for (bubble, line, above, inset) in &*self.folds.borrow() {
            let Some(iter) = buffer.iter_at_line(*line as i32) else { continue };
            let (y, height) = self.code.line_yrange(&iter);
            // The room is the top of the line's space, or the bottom.
            let room = bubble.height_request() + 2 * inset;
            let top = if *above { y } else { y + height - room };
            self.code.move_overlay(bubble, 0, top + inset);
        }
    }

    /// Highlights `lines` as the difference being looked at.
    fn mark_current(&self, lines: Option<std::ops::Range<usize>>) {
        for buffer in [self.code.buffer(), self.numbers.buffer()] {
            buffer.remove_tag_by_name("current", &buffer.start_iter(), &buffer.end_iter());
            for i in lines.clone().unwrap_or_default() {
                tag_line(&buffer, i, "current");
            }
        }
    }

    fn scroll_to_line(&self, line: usize) {
        let buffer = self.code.buffer();
        if let Some(mut iter) = buffer.iter_at_line(line as i32) {
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

/// How long typing pauses before an edited card is compared again.
const REFRESH_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// Two cards side by side, with the differences between them. The right
/// card can be made editable; its differences update as it's edited.
#[allow(clippy::type_complexity)]
pub struct Pair {
    pub widget: gtk::Box,
    pub left: Rc<Card>,
    pub right: Rc<Card>,
    texts: RefCell<(String, String)>,
    diff: RefCell<Diff>,
    current: Cell<Option<usize>>,
    compare: RefCell<Compare>,
    ignore_whitespace: Cell<bool>,
    /// Whether runs of unchanged lines are folded away.
    fold: Cell<bool>,
    /// Folds the user opened, by their first line on the left.
    unfolded: RefCell<std::collections::BTreeSet<usize>>,
    me: std::rc::Weak<Pair>,
    /// Whether the cards share their scrolling.
    linked: Cell<bool>,
    /// Set while the pair fills the cards, so that isn't taken for an edit.
    filling: Cell<bool>,
    refresh: RefCell<Option<glib::SourceId>>,
    /// Called when the difference count or the current one changes, with
    /// whether the cards are compared with the base game.
    on_change: RefCell<Vec<Box<dyn Fn(usize, Option<usize>, bool)>>>,
    /// Called with the right card's text as it's edited.
    on_edit: RefCell<Vec<Box<dyn Fn(&str)>>>,
}

impl Pair {
    pub fn new(right_editable: bool) -> Rc<Self> {
        let (left, right) = (Card::new(false), Card::new(right_editable));
        right.standalone.set(false);
        let widget = gtk::Box::builder().spacing(12).homogeneous(true).vexpand(true).build();
        widget.append(&left.widget);
        widget.append(&right.widget);
        let pair = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            fold: Cell::new(false),
            unfolded: RefCell::default(),
            widget,
            left,
            right,
            texts: RefCell::default(),
            diff: RefCell::default(),
            current: Cell::new(None),
            compare: RefCell::default(),
            ignore_whitespace: Cell::new(false),
            linked: Cell::new(false),
            filling: Cell::new(false),
            refresh: RefCell::default(),
            on_change: RefCell::default(),
            on_edit: RefCell::default(),
        });
        if right_editable {
            let weak = Rc::downgrade(&pair);
            pair.right.code.buffer().connect_changed(move |buffer| {
                let Some(pair) = weak.upgrade() else { return };
                if pair.filling.get() {
                    return;
                }
                let text = buffer_text(buffer);
                for f in &*pair.on_edit.borrow() {
                    f(&text);
                }
                // Compares again once typing pauses.
                if let Some(id) = pair.refresh.take() {
                    id.remove();
                }
                let weak = Rc::downgrade(&pair);
                let id = glib::timeout_add_local_once(REFRESH_DELAY, move || {
                    if let Some(pair) = weak.upgrade() {
                        pair.refresh.take();
                        pair.texts.borrow_mut().1 = pair.right.text();
                        pair.update(true);
                    }
                });
                pair.refresh.replace(Some(id));
            });
            pair.add_put_in_menu();
        }
        pair
    }

    /// Adds Put This In to the view-only card's right-click menu: it
    /// copies the difference under the pointer into the editable card.
    fn add_put_in_menu(self: &Rc<Self>) {
        let action = gio::SimpleAction::new("put-in", None);
        action.set_enabled(false);
        // The difference that was right-clicked, by its place in the list.
        let clicked: Rc<Cell<Option<usize>>> = Rc::default();
        let weak = Rc::downgrade(self);
        action.connect_activate({
            let clicked = clicked.clone();
            move |_, _| {
                if let (Some(pair), Some(hunk)) = (weak.upgrade(), clicked.get()) {
                    pair.put_in(hunk);
                }
            }
        });
        let group = gio::SimpleActionGroup::new();
        group.add_action(&action);
        self.left.code.insert_action_group("diff", Some(&group));
        let menu = gio::Menu::new();
        menu.append(Some("Put This In"), Some("diff.put-in"));
        self.left.code.set_extra_menu(Some(&menu));

        // Before the text view opens its menu, note what's under the pointer.
        let click = gtk::GestureClick::builder().button(gtk::gdk::BUTTON_SECONDARY).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, x, y| {
            let Some(pair) = weak.upgrade() else { return };
            // The differences must match what's in the editor right now.
            if let Some(id) = pair.refresh.take() {
                id.remove();
                pair.texts.borrow_mut().1 = pair.right.text();
                pair.update(true);
            }
            let code = &pair.left.code;
            let (_, by) = code.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            let (iter, _) = code.line_at_y(by);
            let line = iter.line() as usize;
            let hunk = pair.diff.borrow().hunks.iter().position(|h| h.left.contains(&line));
            clicked.set(hunk);
            action.set_enabled(hunk.is_some());
        });
        self.left.code.add_controller(click);
    }

    /// Replaces the editable card's side of difference `index` with the
    /// view-only card's lines, as one edit that can be undone.
    fn put_in(&self, index: usize) {
        let Some(hunk) = self.diff.borrow().hunks.get(index).cloned() else { return };
        let left = self.texts.borrow().0.clone();
        let lines: Vec<&str> = left.lines().collect();
        let Some(theirs) = lines.get(hunk.left.clone()) else { return };
        let text = theirs.join("\n");
        let buffer = self.right.code.buffer();
        buffer.begin_user_action();
        if hunk.right.is_empty() {
            // Lines only the left has go in before the line that follows.
            match buffer.iter_at_line(hunk.right.start as i32).filter(|_| hunk.right.start < buffer.line_count() as usize) {
                Some(mut at) => buffer.insert(&mut at, &format!("{text}\n")),
                None => buffer.insert(&mut buffer.end_iter(), &format!("\n{text}")),
            }
        } else if let (Some(mut start), Some(mut end)) = (buffer.iter_at_line(hunk.right.start as i32), buffer.iter_at_line(hunk.right.end as i32 - 1)) {
            if !end.ends_line() {
                end.forward_to_line_end();
            }
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &text);
        }
        buffer.end_user_action();
        // Compare again now, not after the typing pause.
        if let Some(id) = self.refresh.take() {
            id.remove();
        }
        self.texts.borrow_mut().1 = self.right.text();
        self.update(true);
    }

    pub fn connect_change(&self, f: impl Fn(usize, Option<usize>, bool) + 'static) {
        self.on_change.borrow_mut().push(Box::new(f));
    }

    /// Calls `f` with the right card's text whenever it's edited.
    pub fn connect_edit(&self, f: impl Fn(&str) + 'static) {
        self.on_edit.borrow_mut().push(Box::new(f));
    }

    /// Whether to fold runs of unchanged lines, from the next `show` on.
    /// Only cards compared with each other fold.
    pub fn set_fold(&self, fold: bool) {
        self.fold.set(fold);
    }

    /// Shows `left` and `right`, compared as `compare` says.
    pub fn show(&self, left: &str, right: &str, compare: &Compare, ignore_whitespace: bool) {
        self.unfolded.borrow_mut().clear();
        self.texts.replace((left.to_owned(), right.to_owned()));
        self.compare.replace(compare.clone());
        self.ignore_whitespace.set(ignore_whitespace);
        self.update(false);
    }

    /// Compares the texts again and marks them, keeping the right card's
    /// text (and its cursor and undo history) when `keep_right`.
    fn update(&self, keep_right: bool) {
        let (left, right) = self.texts.borrow().clone();
        let compare = self.compare.borrow().clone();
        let whitespace = self.ignore_whitespace.get();
        self.filling.set(true);
        let diff = match &compare {
            Compare::Each => {
                if !self.linked.replace(true) {
                    self.right.link_to(&self.left);
                }
                let diff = diff::lines(&left, &right, whitespace);
                self.left.show_side(&left, &right, &diff, Side::Left, false);
                self.right.show_side(&right, &left, &diff, Side::Right, keep_right);
                if self.fold.get() {
                    self.fold_unchanged(&diff);
                }
                diff
            }
            Compare::Base(base) => {
                if self.linked.replace(false) {
                    self.left.unlink();
                    self.right.unlink();
                }
                self.left.show_changes(&left, base, &diff::lines(base, &left, whitespace), false);
                self.right.show_changes(&right, base, &diff::lines(base, &right, whitespace), keep_right);
                // The rows shown aren't the pair's diff, so there are no
                // differences to step through.
                Diff::default()
            }
        };
        self.filling.set(false);
        self.diff.replace(diff);
        self.current.set(None);
        self.notify();
    }

    /// Folds the runs of unchanged rows on both cards, but for the ones
    /// the user opened.
    fn fold_unchanged(&self, diff: &Diff) {
        let height = self.left.line_height();
        for rows in diff::folds(diff, FOLD_CONTEXT, FOLD_MIN) {
            let (Some(left), Some(right)) = (diff.rows[rows.start].left, diff.rows[rows.start].right) else { continue };
            if self.unfolded.borrow().contains(&left) {
                continue;
            }
            for (card, first) in [(&self.left, left), (&self.right, right)] {
                let me = self.me.clone();
                card.fold(first..first + rows.len(), height, move || {
                    if let Some(pair) = me.upgrade() {
                        pair.unfolded.borrow_mut().insert(left);
                        // With what's in the editor now, not what was last compared.
                        pair.texts.borrow_mut().1 = pair.right.text();
                        pair.update(true);
                    }
                });
            }
        }
        // Once more when the cards have been laid out.
        let (left, right) = (Rc::downgrade(&self.left), Rc::downgrade(&self.right));
        glib::idle_add_local_once(move || {
            for card in [left.upgrade(), right.upgrade()].into_iter().flatten() {
                card.place_folds();
            }
        });
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
        let hunk = self.diff.borrow().hunks[next].clone();
        self.left.mark_current(Some(hunk.left.clone()));
        self.right.mark_current(Some(hunk.right.clone()));
        // Defer, so a freshly filled view is laid out first.
        let (left, right) = (Rc::downgrade(&self.left), Rc::downgrade(&self.right));
        glib::idle_add_local_once(move || {
            if let Some(l) = left.upgrade() {
                l.scroll_to_line(hunk.left.start);
            }
            if let Some(r) = right.upgrade() {
                r.scroll_to_line(hunk.right.start);
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
        let base = matches!(*self.compare.borrow(), Compare::Base(_));
        for f in &*self.on_change.borrow() {
            f(self.count(), self.current.get(), base);
        }
    }
}

/// The buffer's whole text, folded lines too.
fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string()
}

/// Replaces the buffer's text if it differs, and says whether it did.
fn set_if_changed(buffer: &gtk::TextBuffer, text: &str) -> bool {
    if buffer_text(buffer) == text {
        return false;
    }
    buffer.set_text(text);
    true
}

/// Removes the difference marks and gaps, keeping the syntax colours.
fn clear_marks(buffer: &gtk::TextBuffer) {
    let (start, end) = buffer.bounds();
    for name in ["filler", "added", "removed", "changed", "current", "word", "folded"] {
        buffer.remove_tag_by_name(name, &start, &end);
    }
    let mut gaps = Vec::new();
    buffer.tag_table().foreach(|tag| {
        if tag.name().is_some_and(|n| n.starts_with("gap-")) {
            gaps.push(tag.clone());
        }
    });
    for tag in gaps {
        buffer.remove_tag(&tag, &start, &end);
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
    tag("folded", &[("invisible", true.to_value())]);
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
.fold-bubble { min-height: 0; padding: 0 12px; border-radius: 999px; font-size: smaller; font-weight: normal; background: alpha(currentColor, 0.09); box-shadow: none; }
.fold-bubble:hover { background: alpha(currentColor, 0.16); }
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
.conflict-split .sidebar-pane, .conflict-split .sidebar-pane:backdrop { background-color: transparent; box-shadow: none; border: none; }
.conflict-sidebar { border-radius: 12px; background-color: var(--sidebar-bg-color); color: var(--sidebar-fg-color); box-shadow: 0 0 0 1px var(--sidebar-border-color); }
.conflict-sidebar:backdrop { background-color: var(--sidebar-backdrop-color); }
.conflict-sidebar .navigation-sidebar { background: transparent; }
.conflict-list row.folder { padding-top: 14px; }
.conflict-list row.folder label { font-size: 0.85em; font-weight: bold; }
.conflict-list row.conflict { padding: 6px 8px; border-radius: 8px; margin: 1px 6px; }
.conflict-title { font-size: 1.6em; font-weight: 800; }
.card-mode { font-size: 0.8em; font-weight: bold; border-radius: 99px; padding: 1px 8px 1px 6px; }
.card-mode image { -gtk-icon-size: 12px; }
.card-mode.view-only { background: alpha(currentColor, 0.08); color: alpha(currentColor, 0.65); }
.card-mode.editable { background: alpha(@accent_bg_color, 0.2); color: @accent_color; }
.code-card.editable { box-shadow: inset 0 0 0 1px alpha(@accent_bg_color, 0.55); }
.code-card.editable .code-view { caret-color: @accent_color; }
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
