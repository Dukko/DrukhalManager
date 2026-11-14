//! The Multiplayer Check dialog: save a report of your mods to send to
//! another player, or compare theirs with yours.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::mods::archive;
use crate::multiplayer::{self, Build, Comparison, FileDiff, HashCache, ModInput, Progress, Report};

/// Files listed per mod; the rest are summarised.
const ROWS_PER_MOD: usize = 200;
const DEFAULT_NAME: &str = "multiplayer-report.json";

/// What to report: the enabled mods in load order (with their folders or
/// zips) and the game.
pub struct Request {
    pub mods: Vec<(String, Option<u64>, Option<PathBuf>)>,
    pub game_dir: Option<PathBuf>,
    pub game_version: Option<String>,
    /// The build of the game that's installed, if it was found.
    pub build: Option<Build>,
}

impl Request {
    /// Reads everything; call off the main thread.
    fn report(&self, all_game_files: bool, progress: &Progress) -> Report {
        let mods: Vec<ModInput> = self
            .mods
            .iter()
            .map(|(name, steam_id, content)| ModInput { name: name.clone(), steam_id: *steam_id, root: content.as_deref().and_then(|c| archive::files(c).ok()) })
            .collect();
        let path = HashCache::path();
        let mut cache = HashCache::load(&path);
        let mut report = multiplayer::report(self.game_dir.as_deref(), self.game_version.as_deref(), all_game_files, &mods, progress, &mut cache);
        report.build = self.build;
        // Only a speed-up, so failing to keep it doesn't matter.
        let _ = cache.save(&path);
        report
    }
}

/// The progress bar under the actions, and the actions to disable while a
/// report is being built.
#[derive(Clone)]
struct Busy {
    bar: gtk::ProgressBar,
    rows: [adw::PreferencesRow; 3],
}

impl Busy {
    /// Shows the bar following `progress` until `done` is called.
    fn start(&self, what: &str) -> (Arc<Progress>, impl FnOnce() + use<>) {
        let what = what.to_owned();
        let progress = Arc::new(Progress::default());
        let running = Rc::new(std::cell::Cell::new(true));
        self.rows.iter().for_each(|r| r.set_sensitive(false));
        self.bar.set_fraction(0.0);
        self.bar.set_text(Some(&t!("Counting files…")));
        self.bar.set_visible(true);
        let (bar, tracked, still) = (self.bar.clone(), progress.clone(), running.clone());
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if !still.get() {
                return glib::ControlFlow::Break;
            }
            match tracked.fraction() {
                None => bar.pulse(),
                Some(fraction) => {
                    bar.set_fraction(fraction);
                    let (done, total) = (tracked.done.load(Ordering::Relaxed), tracked.total.load(Ordering::Relaxed));
                    bar.set_text(Some(&tf!("{what}: {done} of {files}", what = &what, done = done.min(total), files = tnf!("{count} file", "{count} files", count = total))));
                }
            }
            glib::ControlFlow::Continue
        });
        let this = self.clone();
        let done = move || {
            running.set(false);
            this.bar.set_visible(false);
            this.rows.iter().for_each(|r| r.set_sensitive(true));
        };
        (progress, done)
    }
}

pub fn present(parent: &impl IsA<gtk::Widget>, request: Request) {
    let request = Arc::new(request);
    let page = adw::PreferencesPage::new();
    let intro = adw::PreferencesGroup::builder()
        .description(t!("Playing together needs the same game version, mods, load order and mod files. Save your report and send it to the other players, or open one of theirs to see what differs."))
        .build();
    let save = adw::ActionRow::builder().title(t!("Save My Report…")).subtitle(tnf!("{count} enabled mod and the game's files", "{count} enabled mods and the game's files", count = request.mods.len())).activatable(true).build();
    save.add_suffix(&gtk::Image::from_icon_name("document-save-symbolic"));
    let open = adw::ActionRow::builder().title(t!("Compare With a Report…")).subtitle(t!("Open the report another player sent you")).activatable(true).build();
    open.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    let all_files = adw::SwitchRow::builder()
        .title(t!("Include All Game Files"))
        .subtitle(t!("Also localisation, interface and graphics, to find modified or damaged game files outside the multiplayer checksum. Slow the first time."))
        .build();
    intro.add(&save);
    intro.add(&open);
    intro.add(&all_files);
    let bar = gtk::ProgressBar::builder().show_text(true).pulse_step(0.1).margin_top(12).visible(false).build();
    intro.add(&bar);
    page.add(&intro);
    // Windows players can't make a report, so say it before comparing.
    if request.build == Some(Build::Linux) {
        page.add(&adw::PreferencesGroup::builder().title(t!("Playing With Windows Players")).description(tf!("You run the native Linux build. {note}", note = multiplayer::builds_differ())).build());
    }
    let busy = Busy { bar, rows: [save.clone().upcast(), open.clone().upcast(), all_files.clone().upcast()] };

    let results: Rc<std::cell::RefCell<Vec<adw::PreferencesGroup>>> = Rc::default();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&t!("Multiplayer Check"), "")));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    let dialog = adw::Dialog::builder().content_width(760).content_height(720).child(&toolbar).build();

    let json_filter = || {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&t!("Multiplayer reports")));
        filter.add_suffix("json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        filters
    };

    save.connect_activated(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        toasts,
        #[strong]
        request,
        #[strong]
        busy,
        #[weak]
        all_files,
        move |_| {
            let all_game_files = all_files.is_active();
            let chooser = gtk::FileDialog::builder().title(t!("Save Multiplayer Report")).initial_name(DEFAULT_NAME).filters(&json_filter()).build();
            let window = dialog.root().and_downcast::<gtk::Window>();
            let (busy, request) = (busy.clone(), request.clone());
            chooser.save(window.as_ref(), None::<&gio::Cancellable>, move |file| {
                let Some(path) = file.ok().and_then(|f| f.path()) else { return };
                let (progress, done) = busy.start(&t!("Reading"));
                glib::spawn_future_local(async move {
                    let written = gio::spawn_blocking(move || std::fs::write(&path, request.report(all_game_files, &progress).to_json()).map(|()| path)).await;
                    done();
                    let message = match written {
                        Ok(Ok(path)) => tf!("Saved {name}; send it to the other players", name = path.file_name().unwrap_or_default().to_string_lossy()),
                        Ok(Err(e)) => tf!("Could not save the report: {error}", error = e),
                        Err(_) => t!("Could not save the report"),
                    };
                    toasts.add_toast(adw::Toast::builder().title(message).use_markup(false).build());
                });
            });
        }
    ));

    open.connect_activated(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        toasts,
        #[weak]
        page,
        #[strong]
        request,
        #[strong]
        results,
        #[strong]
        busy,
        move |_| {
            let chooser = gtk::FileDialog::builder().title(t!("Open Multiplayer Report")).filters(&json_filter()).build();
            let window = dialog.root().and_downcast::<gtk::Window>();
            let (busy, request, results) = (busy.clone(), request.clone(), results.clone());
            chooser.open(window.as_ref(), None::<&gio::Cancellable>, move |file| {
                let Some(path) = file.ok().and_then(|f| f.path()) else { return };
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let (progress, done) = busy.start(&t!("Comparing"));
                glib::spawn_future_local(async move {
                    let compared = gio::spawn_blocking(move || {
                        let src = std::fs::read_to_string(&path).map_err(|e| tf!("Could not read {file}: {error}", file = path.display(), error = e))?;
                        // Check theirs first, so a wrong file fails at once.
                        let theirs = Report::from_json(&src)?;
                        // Mine covers what theirs does, so nothing goes unchecked.
                        Ok::<_, String>(multiplayer::compare(&request.report(theirs.all_game_files, &progress), &theirs))
                    })
                    .await
                    .unwrap_or_else(|_| Err(t!("The comparison failed")));
                    done();
                    match compared {
                        Ok(c) => {
                            for group in results.borrow_mut().drain(..) {
                                page.remove(&group);
                            }
                            for group in render(&c, &name) {
                                page.add(&group);
                                results.borrow_mut().push(group);
                            }
                        }
                        Err(message) => toasts.add_toast(adw::Toast::builder().title(message).use_markup(false).timeout(0).build()),
                    }
                });
            });
        }
    ));

    dialog.present(Some(parent));
}

fn label_row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder().title(title).subtitle(subtitle).use_markup(false).build()
}

fn file_row(f: &FileDiff) -> adw::ActionRow {
    let what = match f {
        FileDiff::Changed(_) => t!("Different"),
        FileDiff::LineEndings(_) => t!("Only line endings differ (Windows CRLF or Linux LF)"),
        FileDiff::OnlyMine(_) => t!("Only in yours"),
        FileDiff::OnlyTheirs(_) => t!("Only in theirs"),
    };
    let checksum = multiplayer::in_checksum(f.path());
    let row = label_row(f.path(), &if checksum { tf!("{what} · in the multiplayer checksum", what = &what) } else { what });
    if checksum {
        row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
    }
    row
}

fn files_expander(title: &str, files: &[FileDiff]) -> adw::ExpanderRow {
    let in_checksum = files.iter().filter(|f| multiplayer::in_checksum(f.path())).count();
    let mut subtitle = tnf!("{count} file differs", "{count} files differ", count = files.len());
    subtitle.push_str(&if in_checksum > 0 {
        tf!(" · {count} in the multiplayer checksum, so the game won't start together", count = in_checksum)
    } else {
        t!(" · none in the multiplayer checksum (graphics, sound…), so it may still work")
    });
    let row = adw::ExpanderRow::builder().title(title).subtitle(subtitle).use_markup(false).build();
    if in_checksum > 0 {
        row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
    }
    for f in files.iter().take(ROWS_PER_MOD) {
        row.add_row(&file_row(f));
    }
    if files.len() > ROWS_PER_MOD {
        row.add_row(&label_row(&tf!("…and {count} more", count = files.len() - ROWS_PER_MOD), ""));
    }
    row
}

fn render(c: &Comparison, theirs: &str) -> Vec<adw::PreferencesGroup> {
    let group = |title: &str, description: &str| adw::PreferencesGroup::builder().title(title).description(description).build();
    if c.matches() {
        let g = group(&tf!("Compared With {name}", name = theirs), "");
        g.add(&adw::StatusPage::builder().icon_name("emblem-ok-symbolic").title(t!("Everything Matches")).description(t!("Same game version, mods, load order and mod files")).build());
        return vec![g];
    }
    let mut out = vec![group(&tf!("Compared With {name}", name = theirs), &t!("What differs between your setup and theirs. Fix these so you all have the same."))];

    if c.game_version.is_some() || c.builds.is_some() || !c.game_files.is_empty() {
        let differing = if c.builds.is_some() { multiplayer::builds_differ() } else { String::new() };
        let g = group(&t!("Game"), &differing);
        if let Some((mine, theirs)) = &c.game_version {
            g.add(&label_row(&t!("Different game version"), &tf!("Yours: {mine} · Theirs: {theirs}", mine = mine, theirs = theirs)));
        }
        if let Some((mine, theirs)) = c.builds {
            g.add(&label_row(&t!("Different game build"), &tf!("Yours: {mine} · Theirs: {theirs}", mine = mine.describe(), theirs = theirs.describe())));
        }
        if !c.game_files.is_empty() {
            g.add(&files_expander(&t!("Game files"), &c.game_files));
        }
        out.push(g);
    }
    if !c.only_mine.is_empty() {
        let g = group(&t!("Only You Have"), &t!("Enabled for you but not for them"));
        c.only_mine.iter().for_each(|m| g.add(&label_row(m, "")));
        out.push(g);
    }
    if !c.only_theirs.is_empty() {
        let g = group(&t!("Only They Have"), &t!("Enabled for them but not for you"));
        c.only_theirs.iter().for_each(|m| g.add(&label_row(m, "")));
        out.push(g);
    }
    if !c.order.is_empty() {
        let g = group(&t!("Different Load Order"), &t!("The mods you both have load in a different order, which changes which files win"));
        for (name, mine, theirs) in c.order.iter().filter(|(_, a, b)| a != b) {
            g.add(&label_row(name, &tf!("Yours: {mine} · Theirs: {theirs}", mine = mine, theirs = theirs)));
        }
        out.push(g);
    }
    if !c.mods.is_empty() {
        let g = group(&t!("Different Mod Files"), &t!("Usually a different version of the mod: resubscribe or update so you have the same"));
        c.mods.iter().for_each(|m| g.add(&files_expander(&m.name, &m.files)));
        out.push(g);
    }
    if !c.case.is_empty() {
        let g = group(
            &t!("Files the Linux Build May Skip"),
            &t!("The native Linux build reads file and folder names exactly, unlike Windows, so it may skip these files: they're in a game folder whose name isn't lowercase, such as Common or common/Traits, or named like another file but for case. Its checksum then doesn't match. The mod's author can rename them, or whoever runs the native Linux build can switch to the Windows build through Proton."),
        );
        for (name, files) in &c.case {
            let row = adw::ExpanderRow::builder().title(name).subtitle(tnf!("{count} file", "{count} files", count = files.len())).use_markup(false).build();
            files.iter().take(ROWS_PER_MOD).for_each(|f| row.add_row(&label_row(f, "")));
            if files.len() > ROWS_PER_MOD {
                row.add_row(&label_row(&tf!("…and {count} more", count = files.len() - ROWS_PER_MOD), ""));
            }
            g.add(&row);
        }
        out.push(g);
    }
    out
}
