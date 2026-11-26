//! Effects, triggers and scopes that enabled mods use but the game doesn't
//! have, and events they fire that nothing defines. The game skips them without saying so, so the features they belong
//! to quietly stop working. Each object can be fixed with AI and saved to
//! the merged patch.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::conflicts_view::{Analysis, MergeHooks};
use crate::diff_view::{Compare, Header, Pair};
use crate::merge::ai::{self, Checked};
use crate::merge::patch::{Print, VANILLA};
use crate::merge::script_check::{self, Ctx, Finding, Problem};
use crate::merge::source::{self, Source};
use crate::merge_dialog;
use crate::thinking_view::{self, ThinkingView};

/// The problems found in one object of one mod.
#[derive(Clone)]
struct Object {
    position: usize,
    category: String,
    key: String,
    file: String,
    findings: Vec<Finding>,
}

pub fn present(parent: &impl IsA<gtk::Widget>, analysis: Rc<Analysis>, hooks: Rc<MergeHooks>) {
    let Some(script) = &analysis.script else { return };
    let navigation = adw::NavigationView::new();

    let mut intro = t!("These scripts use effects, triggers or scopes this version of Stellaris doesn't have, or fire events nothing defines. The game skips them without saying so, so the features they belong to quietly stop working.");
    if script.docs_outdated {
        intro.push_str("\n\n");
        intro.push_str(&t!("The game's script documentation is from before its last update. Start Stellaris once to refresh it, then rescan."));
    }
    let page = adw::PreferencesPage::new();
    page.add(&adw::PreferencesGroup::builder().description(&intro).build());

    let mut objects: BTreeMap<(usize, String, String, String), Object> = BTreeMap::new();
    let mut loose: BTreeMap<usize, Vec<&Finding>> = BTreeMap::new();
    let mut boms: BTreeMap<usize, Vec<Finding>> = BTreeMap::new();
    let mut less: BTreeMap<usize, Vec<Finding>> = BTreeMap::new();
    let mut elsewhere: BTreeMap<usize, Vec<Finding>> = BTreeMap::new();
    for finding in &script.findings {
        match &finding.object {
            Some((category, key)) => {
                let id = (finding.mod_position, category.clone(), key.clone(), finding.file.clone());
                objects.entry(id).or_insert_with(|| Object { position: finding.mod_position, category: category.clone(), key: key.clone(), file: finding.file.clone(), findings: Vec::new() }).findings.push(finding.clone());
            }
            None if finding.problem == Problem::Bom => boms.entry(finding.mod_position).or_default().push(finding.clone()),
            None if finding.problem == Problem::Unsure => less.entry(finding.mod_position).or_default().push(finding.clone()),
            None if finding.problem == Problem::Elsewhere => elsewhere.entry(finding.mod_position).or_default().push(finding.clone()),
            None => loose.entry(finding.mod_position).or_default().push(finding),
        }
    }
    // Objects with only uncertain findings have nothing to fix.
    objects.retain(|_, object| {
        let certain = object.findings.iter().any(|f| !matches!(f.problem, Problem::Unsure | Problem::Elsewhere));
        if !certain {
            for finding in &object.findings {
                let list = if finding.problem == Problem::Elsewhere { &mut elsewhere } else { &mut less };
                list.entry(object.position).or_default().push(finding.clone());
            }
        }
        certain
    });
    let mut positions: Vec<usize> = objects.values().map(|o| o.position).chain(loose.keys().copied()).chain(boms.keys().copied()).chain(less.keys().copied()).chain(elsewhere.keys().copied()).collect();
    positions.sort_unstable();
    positions.dedup();
    // Objects nothing calls have no effect in game, so they come last.
    let unused = |o: &Object| o.findings.iter().all(|f| f.unused);
    let live = |position: usize| loose.contains_key(&position) || boms.contains_key(&position) || objects.values().any(|o| o.position == position && !unused(o));
    positions.sort_by_key(|&position| !live(position));
    for position in positions {
        let group = adw::PreferencesGroup::builder().title(glib_escape(analysis.name(position))).build();
        for object in objects.values().filter(|o| o.position == position && !unused(o)) {
            group.add(&object_row(&navigation, &analysis, &hooks, object));
        }
        for finding in boms.get(&position).into_iter().flatten() {
            group.add(&bom_row(&analysis, &hooks, finding));
        }
        for finding in loose.get(&position).into_iter().flatten() {
            let row = adw::ActionRow::builder().title(format!("{}:{}", finding.file, finding.line)).subtitle(describe(finding)).use_markup(false).build();
            group.add(&row);
        }
        let dead: Vec<&Object> = objects.values().filter(|o| o.position == position && unused(o)).collect();
        if !dead.is_empty() {
            let expander = adw::ExpanderRow::builder().title(t!("Not used")).subtitle(t!("Nothing in the game or your enabled mods calls these, so they have no effect. Fixing them is optional.")).use_markup(false).build();
            for object in dead {
                expander.add_row(&object_row(&navigation, &analysis, &hooks, object));
            }
            group.add(&expander);
        }
        if let Some(findings) = elsewhere.get(&position) {
            let expander = adw::ExpanderRow::builder()
                .title(t!("Probably from another mod"))
                .subtitle(t!("Names that look like another mod's triggers or effects, and no installed mod defines them. The mod may expect a companion mod you don't have."))
                .use_markup(false)
                .build();
            for finding in findings {
                expander.add_row(&adw::ActionRow::builder().title(format!("{}:{}", finding.file, finding.line)).subtitle(describe(finding)).use_markup(false).build());
            }
            group.add(&expander);
        }
        if let Some(findings) = less.get(&position) {
            let expander = adw::ExpanderRow::builder().title(t!("Less certain")).subtitle(t!("Settings this kind of block doesn't take in the base game. They may be fine.")).use_markup(false).build();
            for finding in findings {
                expander.add_row(&adw::ActionRow::builder().title(format!("{}:{}", finding.file, finding.line)).subtitle(describe(finding)).use_markup(false).build());
            }
            group.add(&expander);
        }
        page.add(&group);
    }

    navigation.add(&wrap(&t!("Script Problems"), None, &page));
    adw::Dialog::builder().content_width(900).content_height(720).child(&navigation).build().present(Some(parent));
}

/// A row for a script file that starts with a byte-order mark, with a button
/// that saves a copy without it to the merged patch.
fn bom_row(analysis: &Rc<Analysis>, hooks: &Rc<MergeHooks>, finding: &Finding) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(&finding.file).subtitle(describe(finding)).use_markup(false).build();
    let already = (hooks.is_file_merged)(&finding.file);
    let fixed = gtk::Label::builder().label(t!("Fixed in the merged patch")).css_classes(["success"]).visible(already).valign(gtk::Align::Center).build();
    let save = gtk::Button::builder().label(t!("Save Fixed Copy to Patch")).valign(gtk::Align::Center).visible(!already).build();
    row.add_suffix(&fixed);
    row.add_suffix(&save);
    let (analysis, hooks, finding) = (analysis.clone(), hooks.clone(), finding.clone());
    save.connect_clicked(move |button| {
        let position = finding.mod_position;
        let Some(root) = analysis.roots.get(position).and_then(Option::as_ref) else { return };
        let saved = script_check::fix_bom(root, &finding.file, &analysis.ids[position], analysis.name(position), hooks.game_dir.as_deref()).and_then(|fix| (hooks.save_file)(&finding.file, &fix.contents, fix.prints));
        match saved {
            Ok(()) => {
                hooks.saved();
                button.set_visible(false);
                fixed.set_visible(true);
                (hooks.toast)(&tf!("Saved a copy of {file} without the byte-order mark to the merged patch", file = &finding.file));
            }
            Err(message) => (hooks.toast)(&message),
        }
    });
    row
}

/// A row that opens the page for fixing one object.
fn object_row(navigation: &adw::NavigationView, analysis: &Rc<Analysis>, hooks: &Rc<MergeHooks>, object: &Object) -> adw::ActionRow {
    let problems = tnf!("{count} problem", "{count} problems", count = object.findings.len());
    let row = adw::ActionRow::builder().title(&object.key).subtitle(tf!("{category} · {problems}", category = &object.category, problems = problems)).use_markup(false).activatable(true).build();
    // Shown once the object is in the patch, including after a save
    // from the page this row opens.
    let fixed = gtk::Label::builder().label(t!("Fixed in the merged patch")).css_classes(["success"]).visible((hooks.is_merged)(&object.category, &object.key)).build();
    row.add_suffix(&fixed);
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let (navigation, analysis, hooks, object) = (navigation.clone(), analysis.clone(), hooks.clone(), object.clone());
    row.connect_activated(move |_| {
        let fixed = fixed.clone();
        navigation.push(&detail(&analysis, &hooks, &navigation, &object, Rc::new(move || fixed.set_visible(true))));
    });
    row
}

/// What a finding says, for the player.
fn describe(f: &Finding) -> String {
    match (f.problem, f.ctx) {
        (Problem::Unknown, Ctx::Effect) => tf!("Line {line}: {name} isn't an effect this game has", line = f.line, name = &f.name),
        (Problem::Unknown, Ctx::Trigger) => tf!("Line {line}: {name} isn't a trigger this game has", line = f.line, name = &f.name),
        (Problem::WrongKind, Ctx::Effect) => tf!("Line {line}: {name} is a trigger, used where an effect belongs", line = f.line, name = &f.name),
        (Problem::WrongKind, Ctx::Trigger) => tf!("Line {line}: {name} is an effect, used where a trigger belongs", line = f.line, name = &f.name),
        (Problem::Elsewhere, _) => tf!("Line {line}: {name} looks like it comes from another mod, and no installed mod defines it", line = f.line, name = &f.name),
        (Problem::Unsure, _) => tf!("Line {line}: {name} isn't a setting this kind of block takes in the base game", line = f.line, name = &f.name),
        (Problem::Capitalisation, _) => tf!("Line {line}: {name} has the wrong capitalisation", line = f.line, name = &f.name),
        (Problem::Bom, _) => t!("Starts with a byte-order mark (BOM); the game can't read the first entry and misreads the rest"),
        (Problem::NoEvent, _) => tf!("Line {line}: fires the event {name}, which neither the game nor an enabled mod has, so nothing happens", line = f.line, name = &f.name),
    }
}

/// What the model is told is wrong, one sentence per distinct name. Not
/// translated, and without line numbers, which only mean something in the
/// file.
fn problems_for_model(findings: &[Finding]) -> Vec<String> {
    let mut counts: Vec<(&Finding, usize)> = Vec::new();
    for f in findings {
        match counts.iter_mut().find(|(c, _)| c.name == f.name && c.ctx == f.ctx && c.problem == f.problem) {
            Some((_, n)) => *n += 1,
            None => counts.push((f, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(f, n)| {
            let (kind, other) = match f.ctx {
                Ctx::Effect => ("an effect", "a trigger"),
                Ctx::Trigger => ("a trigger", "an effect"),
            };
            let what = match f.problem {
                Problem::Bom => "the file starts with a byte-order mark".to_owned(),
                Problem::Unknown => format!("`{}` isn't {kind} the current game has", f.name),
                Problem::WrongKind => format!("`{}` is {other}, used where {kind} belongs", f.name),
                Problem::Elsewhere => format!("`{}` may come from another mod; leave it", f.name),
                Problem::Unsure => format!("`{}` isn't a setting this kind of block takes in the base game (it may be fine)", f.name),
                Problem::Capitalisation => format!("`{}` has the wrong capitalisation; the game's names are lowercase", f.name),
                Problem::NoEvent => format!(
                    "it fires the event `{}`, which neither the game nor any enabled mod defines, so nothing happens. Its namespace defines: {}. Fire the event that was meant if one clearly fits, otherwise remove the call",
                    f.name,
                    f.siblings.join(", ")
                ),
            };
            if n > 1 { format!("{what} (used {n} times)") } else { what }
        })
        .collect()
}

/// Group titles are markup; mod names can hold `&` and `<`.
fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

/// A page with its own header bar.
fn wrap(title: &str, subtitle: Option<&str>, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, subtitle.unwrap_or_default())));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder().title(title).child(&toolbar).build()
}

/// Ties `value`'s lifetime to `widget`'s, so a pair keeps refreshing its
/// comparison while its widget is on screen.
fn keep_alive<T: 'static>(widget: &impl IsA<gtk::Widget>, value: Rc<T>) {
    let cell = std::cell::RefCell::new(Some(value));
    widget.connect_destroy(move |_| drop(cell.take()));
}

fn dim(text: String) -> gtk::Label {
    gtk::Label::builder().label(text).css_classes(["dim-label"]).wrap(true).xalign(0.0).margin_top(6).margin_bottom(6).margin_start(12).margin_end(12).selectable(true).build()
}

fn error_label() -> gtk::Label {
    gtk::Label::builder().css_classes(["error"]).wrap(true).xalign(0.0).margin_bottom(6).margin_start(12).margin_end(12).selectable(true).visible(false).build()
}

fn show_error(label: &gtk::Label, message: &str) {
    label.set_label(message);
    label.set_visible(true);
}

/// The mod's copy of the object as it is on disk now.
fn read_mod_copy(analysis: &Analysis, object: &Object) -> Option<String> {
    let root = analysis.roots.get(object.position)?.as_ref()?;
    let bytes = std::fs::read(root.join(&object.file)).ok()?;
    source::extract(&object.category, &object.key, String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}'))
}

/// What the page needs once the player asks for a fix.
struct Fix {
    analysis: Rc<Analysis>,
    hooks: Rc<MergeHooks>,
    navigation: adw::NavigationView,
    object: Object,
    mod_text: String,
    /// Holds the object, and the review once there is a result.
    stack: gtk::Stack,
    on_saved: Rc<dyn Fn()>,
}

fn detail(analysis: &Rc<Analysis>, hooks: &Rc<MergeHooks>, navigation: &adw::NavigationView, object: &Object, on_saved: Rc<dyn Fn()>) -> adw::NavigationPage {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let lines = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for finding in &object.findings {
        lines.append(&dim(describe(finding)));
    }
    body.append(&gtk::ScrolledWindow::builder().child(&lines).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).max_content_height(140).build());

    let stack = gtk::Stack::new();
    stack.add_named(&body, Some("object"));

    let Some(mod_text) = read_mod_copy(analysis, object) else {
        body.append(&dim(t!("The mod's copy couldn't be read, so it can't be fixed here.")));
        return wrap(&object.key, Some(analysis.name(object.position)), &stack);
    };
    let view = gtk::TextView::builder().buffer(&gtk::TextBuffer::new(None)).editable(false).cursor_visible(false).monospace(true).top_margin(8).bottom_margin(8).left_margin(10).right_margin(10).build();
    view.buffer().set_text(&mod_text);
    let scroll = gtk::ScrolledWindow::builder().child(&view).vexpand(true).margin_start(12).margin_end(12).css_classes(["card"]).build();
    body.append(&scroll);

    let fix = Rc::new(Fix { analysis: analysis.clone(), hooks: hooks.clone(), navigation: navigation.clone(), object: object.clone(), mod_text, stack: stack.clone(), on_saved });
    let error = error_label();
    error.set_margin_top(6);
    let thinking = ThinkingView::new();
    let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).margin_top(12).margin_start(12).margin_end(12).margin_bottom(12).build();
    let edit = gtk::Button::builder().label(t!("Edit by Hand")).build();
    let fix_button = gtk::Button::builder().label(t!("Fix with AI")).css_classes(["suggested-action"]).build();
    let stop_button = gtk::Button::builder().label(t!("Stop")).visible(false).build();
    buttons.append(&edit);
    buttons.append(&fix_button);
    buttons.append(&stop_button);
    body.append(&error);
    body.append(&buttons);
    let thinking_box = gtk::Box::builder().margin_start(12).margin_end(12).margin_bottom(12).build();
    thinking_box.append(&thinking.widget);
    thinking.widget.set_hexpand(true);
    body.append(&thinking_box);
    let edit_ref = fix.clone();
    edit.connect_clicked(move |_| edit_ref.edit());
    let button_ref = fix_button.clone();
    let stop_ref = stop_button.clone();
    let edit_btn = edit.clone();
    let thinking_stop = thinking.clone();
    stop_button.connect_clicked(move |_| {
        thinking_stop.stop();
    });
    let thinking_destroy = thinking.clone();
    body.connect_destroy(move |_| {
        thinking_destroy.stop();
    });
    fix_button.connect_clicked(move |_| fix.start(&button_ref, &stop_ref, &edit_btn, &error, &thinking));
    wrap(&object.key, Some(analysis.name(object.position)), &stack)
}

impl Fix {
    fn start(self: &Rc<Self>, button: &gtk::Button, stop_button: &gtk::Button, edit: &gtk::Button, error: &gtk::Label, thinking: &Rc<ThinkingView>) {
        let Some(provider) = (self.hooks.provider)() else {
            (self.hooks.toast)(&t!("Set up an AI provider in Preferences first"));
            (self.hooks.open_preferences)();
            return;
        };
        let Some(script) = &self.analysis.script else { return };
        let (context, known, docs, game_dir) = (script.context.clone(), script.known.clone(), script.docs.clone(), script.game_dir.clone());
        let mod_copy = Source { label: self.analysis.name(self.object.position).to_owned(), file: self.object.file.clone(), text: self.mod_text.clone() };
        let (category, key, file) = (self.object.category.clone(), self.object.key.clone(), self.object.file.clone());
        let problems = problems_for_model(&self.object.findings);
        // Names that may come from a mod that isn't installed, which the model leaves.
        let leave: std::collections::HashSet<String> = script.findings.iter().filter(|f| f.problem == Problem::Elsewhere).map(|f| f.name.clone()).collect();
        // For missing events: each namespace's events that do exist.
        let namespaces: std::collections::HashMap<String, Vec<String>> = self
            .object
            .findings
            .iter()
            .filter(|f| f.problem == Problem::NoEvent)
            .filter_map(|f| Some((f.name.rsplit_once('.')?.0.to_owned(), f.siblings.clone())))
            .collect();
        let mut names: Vec<String> = self.object.findings.iter().map(|f| f.name.clone()).collect();
        names.sort();
        names.dedup();
        button.set_visible(false);
        stop_button.set_visible(true);
        edit.set_sensitive(false);
        error.set_visible(false);
        let (fix, button, stop_button, edit, error, thinking) = (self.clone(), button.clone(), stop_button.clone(), edit.clone(), error.clone(), thinking.clone());
        glib::spawn_future_local(async move {
            let result = thinking_view::run(&thinking, None, move |on| {
                // The game's own version of the object, when it defines one.
                let game = source::vanilla(&game_dir, &category, &key);
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let reference = docs.reference(&names, 6000);
                // The result has to parse, define the object, and use only
                // names the game has, by the same rules as the scan.
                let check = |text: &str| {
                    let script = merge_dialog::validate(&category, &key, text)?;
                    // Uncertain names don't make a fix fail.
                    let remaining: Vec<_> = script_check::check_text(&context, &known, &file, &script).into_iter().filter(|(_, name, _, problem)| *problem != Problem::Unsure && !leave.contains(name)).collect();
                    // Events the namespace doesn't have, kept or newly made up.
                    let mut events: Vec<String> = script_check::fired_events(&file, &script)
                        .into_iter()
                        .filter(|id| id.rsplit_once('.').and_then(|(ns, _)| namespaces.get(ns)).is_some_and(|ids| !ids.contains(id)))
                        .collect();
                    events.sort();
                    events.dedup();
                    if !events.is_empty() {
                        return Err(format!("it still fires events that don't exist: {}", events.join(", ")));
                    }
                    if remaining.is_empty() {
                        return Ok(script);
                    }
                    let mut words: Vec<String> = remaining.into_iter().map(|(_, name, _, _)| name).collect();
                    words.sort();
                    words.dedup();
                    Err(format!("it still uses names the current game doesn't have: {}", words.join(", ")))
                };
                ai::fix_script_checked(&provider, &category, &key, &mod_copy, game.as_ref(), &problems, &reference, &check, on).map(|checked| (checked, game))
            })
            .await;
            stop_button.set_visible(false);
            button.set_visible(true);
            button.set_sensitive(true);
            edit.set_sensitive(true);
            match result {
                Some(Ok((checked, game))) => fix.review(checked, game),
                Some(Err(message)) => show_error(&error, &message),
                None => {
                    if thinking.is_stopped() {
                        show_error(&error, &t!("Fix stopped"));
                    } else {
                        show_error(&error, &t!("The fix failed unexpectedly"));
                    }
                }
            }
        });
    }

    /// Shows the result next to the mod's copy, to edit and save.
    fn review(self: &Rc<Self>, checked: Checked, game: Option<Source>) {
        let (script, problem) = match checked.script {
            Ok(script) => (script, None),
            // Kept even when still rejected, to fix by hand.
            Err(problem) => (checked.merged.script, Some(problem)),
        };
        self.show_review(script, Some(checked.merged.notes), problem, game);
    }

    /// Edits the mod's copy by hand, without asking the model.
    fn edit(self: &Rc<Self>) {
        let game = self.analysis.script.as_ref().and_then(|script| source::vanilla(&script.game_dir, &self.object.category, &self.object.key));
        self.show_review(self.mod_text.clone(), Some(t!("Change the right-hand copy, then save it to the merged patch. Rescan to see what's left.")), None, game);
    }

    /// `script` next to the mod's copy, to edit and save. `notes` is what
    /// the model said, or how to start by hand; `problem` is why a model's
    /// result was still rejected.
    fn show_review(self: &Rc<Self>, script: String, notes: Option<String>, problem: Option<String>, game: Option<Source>) {
        let pair = Pair::new(true);
        pair.show(&self.mod_text, &script, &Compare::Each, false);
        pair.left.set_header(&Header { title: self.analysis.name(self.object.position).to_owned(), subtitle: self.object.file.clone(), color: None, badge: None });
        pair.right.set_header(&Header { title: t!("Fixed Copy"), subtitle: String::new(), color: None, badge: None });
        pair.widget.set_vexpand(true);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&pair.widget);
        if let Some(notes) = notes.filter(|notes| !notes.trim().is_empty()) {
            body.append(&dim(notes));
        }
        let error = error_label();
        if let Some(problem) = problem {
            show_error(&error, &tf!("The model's result still has a problem after a correction: {problem}. Fix it by hand, or undo and try again.", problem = problem));
        }
        body.append(&error);
        let discard = gtk::Button::builder().label(t!("Discard")).build();
        let save = gtk::Button::builder().label(t!("Save to Merged Patch")).css_classes(["suggested-action"]).build();
        let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).margin_start(12).margin_end(12).margin_bottom(12).build();
        buttons.append(&discard);
        buttons.append(&save);
        body.append(&buttons);

        let stack = self.stack.clone();
        discard.connect_clicked(move |_| stack.set_visible_child_name("object"));
        let (fix, weak, error) = (self.clone(), Rc::downgrade(&pair), error.clone());
        save.connect_clicked(move |_| {
            if let Some(pair) = weak.upgrade() {
                fix.save(&pair.right.text(), game.as_ref(), &error);
            }
        });
        keep_alive(&body, pair);

        if let Some(old) = self.stack.child_by_name("review") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&body, Some("review"));
        self.stack.set_visible_child_name("review");
    }

    fn save(&self, text: &str, game: Option<&Source>, error: &gtk::Label) {
        let (category, key) = (&self.object.category, &self.object.key);
        let script = match merge_dialog::validate(category, key, text) {
            Ok(script) => script,
            Err(message) => return show_error(error, &message),
        };
        // Fingerprints of the sources, so the patch flags this copy as
        // outdated if the game or the mod changes again.
        let position = self.object.position;
        let mut prints = Vec::new();
        if let Some(game) = game {
            prints.push(Print::new(VANILLA, "Vanilla", &game.file, &game.text));
        }
        prints.push(Print::new(&self.analysis.ids[position], self.analysis.name(position), &self.object.file, &self.mod_text));
        match (self.hooks.save_object)(category, key, &script, prints) {
            Ok(()) => {
                self.hooks.saved();
                (self.on_saved)();
                (self.hooks.toast)(&tf!("Saved the fixed {key} to the merged patch", key = key));
                self.navigation.pop();
            }
            Err(message) => show_error(error, &message),
        }
    }
}
