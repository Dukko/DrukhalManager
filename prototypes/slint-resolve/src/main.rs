//! Mockup: DrukhalManager's conflicts in Slint, conflicts first. The UI is
//! in `ui/resolve.slint`; this fills it with sample conflicts and the
//! desktop's colours, and keeps the colours current.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use drukhal_palette::{Palette, Rgb, Source};
use slint::{Color, ModelRc, SharedString, VecModel};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let (fixed, screenshot) = args();
    let app = AppWindow::new()?;
    let palette = match &fixed {
        Some(p) => Palette::load(p).unwrap_or_default(),
        None => Palette::detect(),
    };
    apply(&app, &palette);

    let conflicts = Rc::new(drukhal_sample_data::conflicts());
    let queue: Vec<ConflictItem> = conflicts
        .iter()
        .map(|c| ConflictItem {
            key: c.key.into(),
            category: c.category.into(),
            mods: ModelRc::new(VecModel::from(c.versions.iter().map(|v| SharedString::from(initials(v.mod_name))).collect::<Vec<_>>())),
            status: if c.simple.is_some() { "simple" } else if c.key.starts_with("l_") { "merged" } else { "merge" }.into(),
            winner: c.versions[c.winner].mod_name.into(),
        })
        .collect();
    app.set_queue(ModelRc::new(VecModel::from(queue)));
    show(&app, &conflicts, 0);
    {
        let weak = app.as_weak();
        let conflicts = conflicts.clone();
        app.on_select(move |i| {
            if let Some(app) = weak.upgrade() {
                show(&app, &conflicts, i as usize);
            }
        });
    }
    app.on_act(|what| println!("mockup: {what}"));

    // Live colours: the watcher thread hands each new palette to the UI thread.
    let weak = app.as_weak();
    let _watcher = drukhal_palette::watch(fixed, move |p| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                apply(&app, &p);
            }
        });
    });

    if let Some(path) = screenshot {
        let weak = app.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1200), move || {
            let Some(app) = weak.upgrade() else { return };
            match app.window().take_snapshot() {
                Ok(shot) => match image::save_buffer(&path, shot.as_bytes(), shot.width(), shot.height(), image::ColorType::Rgba8) {
                    Ok(()) => println!("saved {}", path.display()),
                    Err(e) => eprintln!("could not save {}: {e}", path.display()),
                },
                Err(e) => eprintln!("no snapshot: {e}"),
            }
            let _ = slint::quit_event_loop();
        });
    }
    app.run()
}

/// `--palette <file>`, `--screenshot <out.png>`.
fn args() -> (Option<PathBuf>, Option<PathBuf>) {
    let (mut palette, mut screenshot) = (None, None);
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--palette" => palette = it.next().map(PathBuf::from),
            "--screenshot" => screenshot = it.next().map(PathBuf::from),
            _ => eprintln!("unknown argument {a}"),
        }
    }
    (palette, screenshot)
}

fn apply(app: &AppWindow, p: &Palette) {
    let g = app.global::<Colours>();
    let c = |rgb: Rgb| Color::from_rgb_u8(rgb.r, rgb.g, rgb.b);
    g.set_light(p.light);
    g.set_window(c(p.window));
    g.set_on_window(c(p.on_window));
    g.set_accent(c(p.accent));
    g.set_on_accent(c(p.on_accent));
    g.set_error(c(p.error));
    g.set_success(c(p.success));
    g.set_warning(c(p.warning));
    g.set_container_low(c(p.container_low));
    g.set_container(c(p.container));
    g.set_container_high(c(p.container_high));
    g.set_secondary(c(p.secondary));
    g.set_outline(c(p.outline));
    g.set_muted(c(p.muted));
    g.set_source_name(
        match &p.source {
            Source::GtkCss(_) => "matugen (gtk.css)".to_owned(),
            Source::Omarchy(path) => format!("Omarchy ({})", path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            Source::Builtin => "built-in".to_owned(),
        }
        .into(),
    );
}

/// Shows conflict `i`: its versions with the lines the others don't have
/// marked, and why the game uses the one it does.
fn show(app: &AppWindow, conflicts: &[drukhal_sample_data::Conflict], i: usize) {
    let Some(c) = conflicts.get(i) else { return };
    app.set_selected(i as i32);
    app.set_key(c.key.into());
    app.set_category(c.category.into());
    let winner = c.versions[c.winner].mod_name;
    app.set_why(match c.simple {
        Some(simple) => format!("{simple}, so there's nothing to merge. In game now: {winner}'s version."),
        None => format!("In game now: {winner}'s version, because {}.", c.why),
    }
    .into());
    let versions: Vec<Version> = c
        .versions
        .iter()
        .enumerate()
        .map(|(n, v)| {
            // A line is marked when no other version has it.
            let others: HashSet<&str> = c.versions.iter().enumerate().filter(|(m, _)| *m != n).flat_map(|(_, o)| o.text.lines().map(str::trim)).collect();
            let lines: Vec<Line> = v
                .text
                .lines()
                .enumerate()
                .map(|(k, l)| Line { number: k as i32 + 1, text: l.replace('\t', "    ").into(), changed: !others.contains(l.trim()) })
                .collect();
            Version {
                mod_name: v.mod_name.into(),
                file: v.file.into(),
                position: position(v.mod_name),
                wins: n == c.winner,
                lines: ModelRc::new(VecModel::from(lines)),
            }
        })
        .collect();
    app.set_versions(ModelRc::new(VecModel::from(versions)));
}

/// The mod's place in the sample load order, from 1.
fn position(name: &str) -> i32 {
    drukhal_sample_data::mods().iter().position(|m| m.name == name).map_or(0, |p| p as i32 + 1)
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).filter(|c| c.is_alphanumeric()).take(2).collect()
}
