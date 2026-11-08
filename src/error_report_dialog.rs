//! The game errors dialog: the last session's `error.log`, grouped by the
//! mod each error comes from, with the merged patch first.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::gio;

use crate::conflicts_view::plural;
use crate::paradox::error_log::{Group, Report};

/// Rows shown per group; the rest are summarised.
const ROWS_PER_GROUP: usize = 200;

/// `roots` maps descriptor ids to mod folders, for opening the files
/// errors mention.
pub fn present(parent: &impl IsA<gtk::Widget>, report: &Report, log: PathBuf, modified: Option<String>, roots: std::collections::HashMap<String, PathBuf>) {
    let page = adw::PreferencesPage::new();
    let mut description = format!("{} in error.log", plural(report.lines, "line", "lines"));
    if let Some(when) = modified {
        description.push_str(&format!(", written {when}"));
    }
    description.push_str(&format!(". {} come from your mods.", plural(report.from_mods(), "error", "errors")));
    let summary = adw::PreferencesGroup::builder().description(description).build();
    let open_log = gtk::Button::builder().label("Open error.log").css_classes(["flat"]).valign(gtk::Align::Center).build();
    open_log.connect_clicked(move |b| open(b, &log));
    summary.set_header_suffix(Some(&open_log));
    page.add(&summary);

    let groups = adw::PreferencesGroup::new();
    for g in &report.groups {
        groups.add(&group_row(g, roots.get(g.mod_id.as_deref().unwrap_or_default()).cloned()));
    }
    page.add(&groups);
    if report.groups.is_empty() {
        let empty = adw::StatusPage::builder().icon_name("emblem-ok-symbolic").title("No Errors").description("The game didn't log any errors last time").build();
        summary.set_description(None);
        groups.add(&empty);
    }

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Game Errors", "From the last time Stellaris ran")));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    adw::Dialog::builder().content_width(900).content_height(720).child(&toolbar).build().present(Some(parent));
}

fn group_row(g: &Group, root: Option<PathBuf>) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title(&g.label)
        .subtitle(format!("{} · {}", plural(g.total(), "error", "errors"), plural(g.entries.len(), "kind", "kinds")))
        .use_markup(false)
        .build();
    if g.is_patch {
        row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").tooltip_text("Errors from your merges and fixes").css_classes(["warning"]).build());
        row.set_subtitle(&format!("{} · from your merges and fixes", plural(g.total(), "error", "errors")));
    }
    for e in g.entries.iter().take(ROWS_PER_GROUP) {
        let mut subtitle = Vec::new();
        if e.count > 1 {
            subtitle.push(format!("×{}", e.count));
        }
        if let Some(file) = &e.file {
            subtitle.push(file.clone());
        }
        let entry = adw::ActionRow::builder().title(e.message.trim()).subtitle(subtitle.join(" · ")).use_markup(false).title_lines(3).build();
        if let (Some(root), Some(file)) = (&root, &e.file)
            && root.join(file).is_file()
        {
            let path = root.join(file);
            let button = gtk::Button::builder().icon_name("document-open-symbolic").tooltip_text("Open this file").valign(gtk::Align::Center).css_classes(["flat"]).build();
            button.connect_clicked(move |b| open(b, &path));
            entry.add_suffix(&button);
        }
        row.add_row(&entry);
    }
    if g.entries.len() > ROWS_PER_GROUP {
        row.add_row(&adw::ActionRow::builder().title(format!("…and {} more kinds", g.entries.len() - ROWS_PER_GROUP)).css_classes(["dim-label"]).build());
    }
    row
}

fn open(widget: &impl IsA<gtk::Widget>, path: &std::path::Path) {
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
}
