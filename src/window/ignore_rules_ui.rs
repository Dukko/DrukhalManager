//! The ignore rules editor, opened from the Conflicts page: the rules as
//! text, with a live preview of how many conflicts each one hides.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use super::Window;
use crate::conflicts_view::{Analysis, plural};
use crate::merge::ignore_rules::Rules;
use crate::settings;

const HELP: &str = "One rule per line. A file override matches by its path, an object conflict by its folder and name (common/buildings/building_lab), or by its files when they all match.\n\
* matches within a folder, ** across folders, and a folder alone matches everything in it. Add | Mod A | Mod B to apply a rule only when exactly those mods conflict. Start a line with ! to show conflicts again, or # for a comment. Later lines win.";

const PLACEHOLDER: &str = "# For example:\ngfx/**\nlocalisation/*\n!gfx/portraits/**";

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
            .label(PLACEHOLDER)
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

        let summary = gtk::Label::builder().xalign(0.0).css_classes(["heading"]).build();
        let preview = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        let preview_scroll = gtk::ScrolledWindow::builder()
            .child(&preview)
            .min_content_height(120)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

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
            .title("Hide Conflicts With Dependencies")
            .subtitle(format!(
                "Patches and submods are meant to override the mods they depend on. This hides {}.",
                plural(dependency_count, "conflict", "conflicts")
            ))
            .active(!self.settings.borrow().show_dependency_conflicts)
            .build();
        let options = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        options.append(&hide_dependencies);
        body.append(&options);
        body.append(&gtk::Label::builder().label(HELP).wrap(true).xalign(0.0).css_classes(["dim-label"]).build());
        body.append(&crate::resize_grip::resizable(&editor));
        body.append(&summary);
        body.append(&preview_scroll);

        let cancel = gtk::Button::with_mnemonic("_Cancel");
        let save = gtk::Button::builder().use_underline(true).label("_Save").css_classes(["suggested-action"]).build();
        let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        let dialog = adw::Dialog::builder().title("Ignore Rules").content_width(680).content_height(620).child(&toolbar).build();

        let update = {
            let (analysis, buffer, placeholder) = (analysis.clone(), buffer.clone(), placeholder.clone());
            move || {
                let source = buffer_text(&buffer);
                placeholder.set_visible(source.is_empty());
                show_preview(&analysis, &source, &summary, &preview);
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
                    ui.toasts.add_toast(adw::Toast::builder().title(format!("Could not save the ignore rules: {e}")).timeout(0).build());
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
        text.grab_focus();
    }
}

fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
}

/// Fills `list` with each rule and how many conflicts it decides, and
/// `summary` with the total hidden.
fn show_preview(analysis: &Analysis, source: &str, summary: &gtk::Label, list: &gtk::ListBox) {
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
    summary.set_label(&format!("Hides {hidden} of {}", plural(total, "conflict", "conflicts")));

    let lines: Vec<&str> = source.lines().collect();
    for (line, hides) in rules.lines() {
        let n = decided.get(&line).copied().unwrap_or(0);
        let effect = match (n, hides) {
            (0, _) => "Matches no conflicts".to_owned(),
            (n, true) => format!("Hides {}", plural(n, "conflict", "conflicts")),
            (n, false) => format!("Shows {} again", plural(n, "conflict", "conflicts")),
        };
        let row = adw::ActionRow::builder()
            .title(lines.get(line).map_or("", |l| l.trim()))
            .subtitle(effect)
            .use_markup(false)
            .build();
        row.add_prefix(&gtk::Label::builder().label((line + 1).to_string()).width_chars(3).xalign(1.0).css_classes(["dim-label", "numeric"]).build());
        if n == 0 {
            row.add_css_class("dim-label");
        }
        list.append(&row);
    }
    list.set_visible(!rules.is_empty());
}
