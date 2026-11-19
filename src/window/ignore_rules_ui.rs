//! The ignore rules editor, opened from the Conflicts page: the rules as a
//! list in words, each with how many conflicts it decides, and as text
//! behind "Edit as Text". Also the rule builder, which makes a "won by"
//! rule from a folder and a mod picked from lists.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use super::Window;
use crate::conflicts_view::{Analysis, file_folder};
use crate::merge::ignore_rules::{self, Rules, Shown};
use crate::settings;

/// The most conflicts the builder lists; the count covers all of them.
const PREVIEW_ROWS: usize = 60;

impl Window {
    pub(super) fn edit_ignore_rules(self: &Rc<Self>, analysis: &Rc<Analysis>) {
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_text(&self.settings.borrow().ignore_rules);
        let text = gtk::TextView::builder()
            .buffer(&buffer)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(10)
            .bottom_margin(10)
            .left_margin(12)
            .right_margin(12)
            .build();
        // A hint while it's empty, as an entry's placeholder.
        let placeholder = gtk::Label::builder()
            .label(t!("# For example:\ngfx/**\nlocalisation/*\n!gfx/portraits/**"))
            .xalign(0.0)
            .yalign(0.0)
            .margin_top(10)
            .margin_start(12)
            .can_target(false)
            .css_classes(["dim-label", "monospace"])
            .build();
        let overlay = gtk::Overlay::builder().child(&text).build();
        overlay.add_overlay(&placeholder);
        let editor = gtk::ScrolledWindow::builder()
            .child(&overlay)
            .min_content_height(160)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["card"])
            .build();
        let text_help = gtk::Label::builder().label(t!("One rule per line. A file override matches by its path, an object conflict by its folder and name (common/buildings/building_lab), or by its files when they all match.\n* matches within a folder, ** across folders, and a folder alone matches everything in it. Add | won by Mod A to apply a rule only while Mod A's version is the one the game uses, or | Mod A | Mod B to apply it only when exactly those mods conflict. Start a line with ! to show conflicts again, or # for a comment. Later lines win.")).wrap(true).xalign(0.0).css_classes(["dim-label"]).build();
        let text_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
        text_box.append(&text_help);
        text_box.append(&crate::resize_grip::resizable(&editor));
        let text_revealer = gtk::Revealer::builder().child(&text_box).reveal_child(false).build();

        let summary = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["heading"]).build();
        let add = gtk::Button::builder().child(&adw::ButtonContent::builder().icon_name("list-add-symbolic").label(t!("_Add Rule…")).use_underline(true).build()).tooltip_text(t!("Hide the conflicts in a folder that one mod wins")).build();
        let as_text = gtk::ToggleButton::builder().use_underline(true).label(t!("Edit as _Text")).build();
        let bar = gtk::Box::builder().spacing(6).build();
        bar.append(&summary);
        bar.append(&add);
        bar.append(&as_text);
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(6)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        // Irony's Default mode: a patch or submod is meant to override the
        // mods it depends on.
        let dependency_count = analysis.report.files.iter().filter(|c| c.builds_on_dependencies(&analysis.depends_on)).count()
            + analysis.report.objects.iter().filter(|c| c.builds_on_dependencies(&analysis.depends_on)).count();
        let hide_dependencies = adw::SwitchRow::builder()
            .title(t!("Hide Conflicts With Dependencies"))
            .subtitle(tnf!(
                "Patches and submods are meant to override the mods they depend on. This hides {count} conflict.",
                "Patches and submods are meant to override the mods they depend on. This hides {count} conflicts.",
                count = dependency_count
            ))
            .active(!self.settings.borrow().show_dependency_conflicts)
            .build();
        let options = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        options.append(&hide_dependencies);
        body.append(&options);
        body.append(&bar);
        body.append(&list);
        body.append(&text_revealer);
        let scroll = gtk::ScrolledWindow::builder().child(&body).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();

        let cancel = gtk::Button::with_mnemonic(&t!("_Cancel"));
        let save = gtk::Button::builder().use_underline(true).label(t!("_Save")).css_classes(["suggested-action"]).build();
        let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroll));
        let dialog = adw::Dialog::builder().title(t!("Ignore Rules")).content_width(680).content_height(620).child(&toolbar).build();

        {
            let revealer = text_revealer.clone();
            let text = text.clone();
            as_text.connect_toggled(move |b| {
                revealer.set_reveal_child(b.is_active());
                if b.is_active() {
                    text.grab_focus();
                }
            });
        }
        let update = {
            let (analysis, buffer, placeholder) = (analysis.clone(), buffer.clone(), placeholder.clone());
            move || {
                let source = buffer_text(&buffer);
                placeholder.set_visible(source.is_empty());
                show_rules(&analysis, &buffer, &source, &summary, &list);
            }
        };
        update();
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        buffer.connect_changed(move |_| {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let (pending_ref, update) = (pending.clone(), update.clone());
            *pending.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_millis(200), move || {
                pending_ref.borrow_mut().take();
                update();
            }));
        });
        {
            let (analysis, buffer, dialog_ref) = (analysis.clone(), buffer.clone(), dialog.downgrade());
            add.connect_clicked(move |_| {
                let Some(dialog) = dialog_ref.upgrade() else { return };
                let buffer = buffer.clone();
                present_builder(&dialog, &analysis, None, None, move |line| append_line(&buffer, &line));
            });
        }

        {
            let dialog = dialog.downgrade();
            cancel.connect_clicked(move |_| {
                if let Some(d) = dialog.upgrade() {
                    d.close();
                }
            });
        }
        let (weak, dialog_ref) = (Rc::downgrade(self), dialog.downgrade());
        save.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let rules = buffer_text(&buffer).trim_end().to_owned();
            {
                let mut settings = ui.settings.borrow_mut();
                settings.ignore_rules = rules;
                settings.show_dependency_conflicts = !hide_dependencies.is_active();
                if let Err(e) = settings.save(&settings::path()) {
                    ui.add_toast(adw::Toast::builder().title(tf!("Could not save the ignore rules: {error}", error = e)).timeout(0).build());
                    return;
                }
            }
            if let Some(d) = dialog_ref.upgrade() {
                d.close();
            }
            ui.refresh();
            ui.render_conflicts();
        });
        dialog.present(Some(&self.window));
    }

    /// Opens the rule builder from the Conflicts page, starting from
    /// `folder` and the mod at scanned `position`; an added rule is saved
    /// straight away.
    pub(super) fn add_ignore_rule(self: &Rc<Self>, analysis: &Rc<Analysis>, folder: Option<&str>, position: Option<usize>) {
        let weak = Rc::downgrade(self);
        present_builder(&self.window, analysis, folder, position, move |line| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut settings = ui.settings.borrow_mut();
                let previous = std::mem::take(&mut settings.ignore_rules);
                settings.ignore_rules = with_line(&previous, &line);
                if let Err(e) = settings.save(&settings::path()) {
                    settings.ignore_rules = previous;
                    drop(settings);
                    ui.add_toast(adw::Toast::builder().title(tf!("Could not save the ignore rules: {error}", error = e)).timeout(0).build());
                    return;
                }
            }
            ui.add_toast(adw::Toast::new(&t!("Rule added")));
            ui.refresh();
            ui.render_conflicts();
        });
    }
}

fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
}

/// `rules` with `line` added at the end, on a line of its own.
fn with_line(rules: &str, line: &str) -> String {
    let rules = rules.trim_end();
    if rules.is_empty() { line.to_owned() } else { format!("{rules}\n{line}") }
}

fn append_line(buffer: &gtk::TextBuffer, line: &str) {
    let text = with_line(&buffer_text(buffer), line);
    buffer.set_text(&text);
}

/// Removes line `line` (from 0) from `buffer`.
fn remove_line(buffer: &gtk::TextBuffer, line: usize) {
    let Some(mut start) = buffer.iter_at_line(line as i32) else { return };
    let mut end = start;
    if !end.forward_line() {
        // The last line: take the line break before it instead.
        start.backward_char();
        end = buffer.end_iter();
    }
    buffer.delete(&mut start, &mut end);
}

/// What a rule applies to, in words.
fn condition(shown: &Shown) -> String {
    match (&shown.winner, shown.mods.is_empty()) {
        (Some(winner), true) => tf!("Where {mod_name} wins", mod_name = winner),
        (Some(winner), false) => tf!("Where {mod_name} wins, and only {mods} conflict", mod_name = winner, mods = shown.mods.join(", ")),
        (None, false) => tf!("Only when exactly {mods} conflict", mods = shown.mods.join(", ")),
        (None, true) => t!("Everything that matches"),
    }
}

/// Fills `list` with each rule in words and how many conflicts it decides,
/// and `summary` with the total hidden.
fn show_rules(analysis: &Analysis, buffer: &gtk::TextBuffer, source: &str, summary: &gtk::Label, list: &gtk::ListBox) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }
    let rules = Rules::parse(source);
    let report = &analysis.report;
    let mut decided: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut hidden = 0;
    let decisions = report
        .files
        .iter()
        .map(|c| rules.deciding_file(c, &analysis.names))
        .chain(report.objects.iter().map(|c| rules.deciding_object(c, &analysis.names)));
    for (line, hides) in decisions.flatten() {
        *decided.entry(line).or_default() += 1;
        hidden += usize::from(hides);
    }
    let total = report.files.len() + report.objects.len();
    summary.set_label(&tf!("Hides {hidden} of {total}", hidden = hidden, total = tnf!("{count} conflict", "{count} conflicts", count = total)));

    for shown in rules.shown() {
        let n = decided.get(&shown.line).copied().unwrap_or(0);
        let effect = match (n, shown.hides) {
            (0, _) => t!("matches no conflicts"),
            (n, true) => tnf!("hides {count} conflict", "hides {count} conflicts", count = n),
            (n, false) => tnf!("shows {count} conflict again", "shows {count} conflicts again", count = n),
        };
        let row = adw::ActionRow::builder()
            .title(&shown.pattern)
            .subtitle(tf!("{condition} · {effect}", condition = condition(shown), effect = effect))
            .use_markup(false)
            .build();
        let (icon, tooltip) = if shown.hides { ("view-conceal-symbolic", t!("Hides conflicts")) } else { ("view-reveal-symbolic", t!("Shows conflicts again")) };
        row.add_prefix(&gtk::Image::builder().icon_name(icon).tooltip_text(tooltip).build());
        row.add_prefix(&gtk::Label::builder().label((shown.line + 1).to_string()).width_chars(3).xalign(1.0).css_classes(["dim-label", "numeric"]).build());
        let delete = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text(t!("Remove this rule")).valign(gtk::Align::Center).css_classes(["flat"]).build();
        let (buffer, line) = (buffer.clone(), shown.line);
        delete.connect_clicked(move |_| remove_line(&buffer, line));
        row.add_suffix(&delete);
        if n == 0 {
            row.add_css_class("dim-label");
        }
        list.append(&row);
    }
    if rules.is_empty() {
        list.append(&adw::ActionRow::builder().title(t!("No rules yet")).subtitle(t!("Add a rule to hide the conflicts in a folder that one mod wins, such as a merger that's meant to override them.")).build());
    }
}

/// The folders that have conflicts, sorted: object folders and the folders
/// of overridden files, without the root.
fn conflict_folders(analysis: &Analysis) -> Vec<String> {
    let report = &analysis.report;
    let mut folders: Vec<String> = report.objects.iter().map(|c| c.category.clone()).chain(report.files.iter().map(|c| file_folder(&c.path).to_owned())).filter(|f| !f.is_empty()).collect();
    folders.sort_unstable();
    folders.dedup();
    folders
}

/// A conflict as the builder lists it: its name, the winning mod's
/// position, and every mod in it.
struct Listed {
    name: String,
    winner: Option<usize>,
    mods: Vec<usize>,
}

/// The conflicts a rule for `folder` alone matches, whoever wins.
fn in_folder(analysis: &Analysis, folder: &str) -> Vec<Listed> {
    let rules = Rules::parse(folder);
    let report = &analysis.report;
    let objects = report.objects.iter().filter(|c| rules.hiding_object(c, &analysis.names).is_some()).map(|c| Listed {
        name: c.key.clone(),
        winner: c.winner().map(|(i, _)| c.definitions[i].mod_position),
        mods: c.definitions.iter().map(|d| d.mod_position).collect(),
    });
    let files = report.files.iter().filter(|c| rules.hiding_file(c, &analysis.names).is_some()).map(|c| Listed {
        name: c.path.rsplit('/').next().unwrap_or(&c.path).to_owned(),
        winner: c.mods.last().copied(),
        mods: c.mods.clone(),
    });
    objects.chain(files).collect()
}

/// Opens the rule builder over `parent`: pick a folder and a mod, see
/// which conflicts the rule hides and which stay, and add it. `on_add`
/// gets the new rule's line.
pub(super) fn present_builder(parent: &impl IsA<gtk::Widget>, analysis: &Rc<Analysis>, folder: Option<&str>, position: Option<usize>, on_add: impl Fn(String) + 'static) {
    let folders = conflict_folders(analysis);
    if folders.is_empty() {
        return;
    }
    let folder_row = adw::ComboRow::builder().title(t!("Folder")).model(&gtk::StringList::new(&folders.iter().map(String::as_str).collect::<Vec<_>>())).enable_search(true).build();
    folder_row.set_expression(Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")));
    let start = folder.and_then(|f| folders.iter().position(|x| x.eq_ignore_ascii_case(f.trim_matches('/')))).unwrap_or(0);
    folder_row.set_selected(start as u32);
    let mod_row = adw::ComboRow::builder().title(t!("Mod")).subtitle(t!("The rule hides a conflict while this mod's version is the one the game uses")).enable_search(true).build();
    mod_row.set_expression(Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")));
    let choice = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    choice.append(&folder_row);
    choice.append(&mod_row);

    let rule_label = gtk::Label::builder().xalign(0.0).selectable(true).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).css_classes(["monospace", "dim-label"]).build();
    let hides_label = gtk::Label::builder().xalign(0.0).css_classes(["heading"]).build();
    let hides = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let stays_label = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["heading"]).build();
    let stays_help = gtk::Label::builder().label(t!("Another mod's version is used here, so the rule leaves these shown.")).xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
    let stays = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let stays_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    stays_box.append(&stays_label);
    stays_box.append(&stays_help);
    stays_box.append(&stays);

    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(6).margin_bottom(18).margin_start(18).margin_end(18).build();
    body.append(&choice);
    body.append(&rule_label);
    body.append(&hides_label);
    body.append(&hides);
    body.append(&stays_box);
    let scroll = gtk::ScrolledWindow::builder().child(&body).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();

    let cancel = gtk::Button::with_mnemonic(&t!("_Cancel"));
    let add = gtk::Button::builder().use_underline(true).label(t!("_Add Rule")).css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
    header.pack_start(&cancel);
    header.pack_end(&add);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroll));
    let dialog = adw::Dialog::builder().title(t!("Add Ignore Rule")).content_width(600).content_height(640).child(&toolbar).build();

    // The mods offered for the picked folder, by scanned position, and
    // the rule they make.
    let mods: Rc<RefCell<Vec<usize>>> = Rc::default();
    let line: Rc<RefCell<Option<String>>> = Rc::default();
    let preferred = Rc::new(std::cell::Cell::new(position));

    let show = {
        let (analysis, folders, folder_row, mod_row, mods, line, add) = (analysis.clone(), folders.clone(), folder_row.clone(), mod_row.clone(), mods.clone(), line.clone(), add.clone());
        move || {
            let folder = &folders[(folder_row.selected() as usize).min(folders.len() - 1)];
            let listed = in_folder(&analysis, folder);
            let Some(&picked) = mods.borrow().get(mod_row.selected() as usize) else {
                *line.borrow_mut() = None;
                add.set_sensitive(false);
                return;
            };
            let rule = ignore_rules::won_by(folder, analysis.name(picked));
            rule_label.set_label(&rule);
            *line.borrow_mut() = Some(rule);
            add.set_sensitive(true);

            for list in [&hides, &stays] {
                while let Some(row) = list.first_child() {
                    list.remove(&row);
                }
            }
            let others = |c: &Listed| c.mods.iter().filter(|&&p| p != picked).map(|&p| analysis.name(p)).collect::<Vec<_>>().join(", ");
            let won: Vec<&Listed> = listed.iter().filter(|c| c.winner == Some(picked)).collect();
            let lost: Vec<&Listed> = listed.iter().filter(|c| c.winner != Some(picked) && c.mods.contains(&picked)).collect();
            hides_label.set_label(&tnf!("Hides {count} conflict", "Hides {count} conflicts", count = won.len()));
            for c in won.iter().take(PREVIEW_ROWS) {
                hides.append(&adw::ActionRow::builder().title(&c.name).subtitle(tf!("Wins over {mods}", mods = others(c))).use_markup(false).build());
            }
            if won.len() > PREVIEW_ROWS {
                hides.append(&adw::ActionRow::builder().title(tf!("And {count} more", count = won.len() - PREVIEW_ROWS)).css_classes(["dim-label"]).build());
            }
            stays_box.set_visible(!lost.is_empty());
            stays_label.set_label(&tnf!("Still Shown: {count} Conflict", "Still Shown: {count} Conflicts", count = lost.len()));
            for c in lost.iter().take(PREVIEW_ROWS) {
                let winner = c.winner.map_or("", |w| analysis.name(w));
                stays.append(&adw::ActionRow::builder().title(&c.name).subtitle(tf!("{mod_name} wins", mod_name = winner)).use_markup(false).build());
            }
            if lost.len() > PREVIEW_ROWS {
                stays.append(&adw::ActionRow::builder().title(tf!("And {count} more", count = lost.len() - PREVIEW_ROWS)).css_classes(["dim-label"]).build());
            }
        }
    };
    // The mods that win something in the folder, most wins first.
    let fill_mods = {
        let (analysis, folders, folder_row, mod_row, mods, preferred, show) = (analysis.clone(), folders.clone(), folder_row.clone(), mod_row.clone(), mods.clone(), preferred.clone(), show.clone());
        move || {
            let folder = &folders[(folder_row.selected() as usize).min(folders.len() - 1)];
            let mut wins: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
            for c in in_folder(&analysis, folder) {
                if let Some(w) = c.winner {
                    *wins.entry(w).or_default() += 1;
                }
            }
            let mut ranked: Vec<(usize, usize)> = wins.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let labels: Vec<String> = ranked.iter().map(|&(p, n)| tnf!("{mod_name} (wins {count})", "{mod_name} (wins {count})", count = n, mod_name = analysis.name(p))).collect();
            *mods.borrow_mut() = ranked.iter().map(|&(p, _)| p).collect();
            mod_row.set_model(Some(&gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>())));
            let start = preferred.get().and_then(|p| mods.borrow().iter().position(|&m| m == p)).unwrap_or(0);
            mod_row.set_selected(start as u32);
            show();
        }
    };
    fill_mods();
    {
        let fill_mods = fill_mods.clone();
        folder_row.connect_selected_notify(move |_| fill_mods());
    }
    {
        let (mods, preferred, show) = (mods.clone(), preferred.clone(), show.clone());
        mod_row.connect_selected_notify(move |row| {
            if let Some(&p) = mods.borrow().get(row.selected() as usize) {
                preferred.set(Some(p));
            }
            show();
        });
    }

    {
        let dialog = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = dialog.upgrade() {
                d.close();
            }
        });
    }
    let dialog_ref = dialog.downgrade();
    add.connect_clicked(move |_| {
        let Some(rule) = line.borrow().clone() else { return };
        if let Some(d) = dialog_ref.upgrade() {
            d.close();
        }
        on_add(rule);
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_lines_after_the_last_rule() {
        assert_eq!(with_line("", "a"), "a");
        assert_eq!(with_line("x\n\n", "a"), "x\na");
    }
}
