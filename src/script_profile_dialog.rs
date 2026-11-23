//! The script performance dialog: the game's own script profile, with the
//! time credited to the enabled mod whose object each item belongs to.

use std::rc::Rc;

use adw::prelude::*;

use crate::conflicts_view::Analysis;
use crate::paradox::script_profile::{self, Entry, Profile};

/// Items listed under each mod.
const ITEMS_PER_MOD: usize = 15;

/// `when` is how long ago the profile was recorded; `entries` is empty when
/// there is no profile yet.
pub fn present(parent: &impl IsA<gtk::Widget>, analysis: Rc<Analysis>, entries: &[Entry], when: Option<String>) {
    let page = adw::PreferencesPage::new();
    if entries.is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("power-profile-performance-symbolic")
            .title(t!("No Script Profile Yet"))
            .description(t!("Open the console in Stellaris and type script_profiler. Play for a while, then type it again. The game then writes a profile of how long each script took."))
            .build();
        let group = adw::PreferencesGroup::new();
        group.add(&empty);
        page.add(&group);
    } else {
        let profile = script_profile::attribute(entries, &analysis.report);
        fill(&page, &analysis, &profile, when);
    }

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&t!("Script Performance"), &t!("How long the game spent in each mod's scripts"))));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    adw::Dialog::builder().content_width(720).content_height(640).child(&toolbar).build().present(Some(parent));
}

fn fill(page: &adw::PreferencesPage, analysis: &Analysis, profile: &Profile, when: Option<String>) {
    let percent = |ms: f64| ms * 100.0 / profile.total_ms.max(f64::EPSILON);
    let in_mods = profile.total_ms - profile.unmatched_ms;
    let seconds = format!("{:.1}", profile.total_ms / 1000.0);
    let share = format!("{:.0}", percent(in_mods));
    let description = match when {
        Some(when) => tf!("{seconds} s profiled, {share}% of it in your mods, recorded {when}", seconds = seconds, share = share, when = when),
        None => tf!("{seconds} s profiled, {share}% of it in your mods", seconds = seconds, share = share),
    };
    let summary = adw::PreferencesGroup::builder().description(description).build();
    let note = gtk::Label::builder()
        .label(t!("A mod that replaces the game's own scripts is credited with their time too, so a high number isn't always the mod's fault."))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .margin_bottom(6)
        .build();
    summary.add(&note);
    summary.add(&adw::ActionRow::builder().title(t!("The base game")).subtitle(tf!("{ms} ms · {share}%", ms = format!("{:.0}", profile.unmatched_ms), share = format!("{:.0}", percent(profile.unmatched_ms)))).build());
    page.add(&summary);

    let mods = adw::PreferencesGroup::builder().title(t!("Mods")).build();
    for m in &profile.mods {
        let row = adw::ExpanderRow::builder()
            .title(analysis.name(m.position))
            .subtitle(tf!("{ms} ms · {share}%", ms = format!("{:.0}", m.ms), share = format!("{:.1}", percent(m.ms))))
            .use_markup(false)
            .build();
        for item in m.items.iter().take(ITEMS_PER_MOD) {
            let detail = tf!("{ms} ms · {hits} · {category}", ms = format!("{:.1}", item.entry.ms), hits = tnf!("{count} hit", "{count} hits", count = item.entry.hits), category = &item.category);
            row.add_row(&adw::ActionRow::builder().title(&item.entry.name).subtitle(detail).use_markup(false).title_lines(2).build());
        }
        if m.items.len() > ITEMS_PER_MOD {
            row.add_row(&adw::ActionRow::builder().title(tnf!("…and {count} more item", "…and {count} more items", count = m.items.len() - ITEMS_PER_MOD)).css_classes(["dim-label"]).build());
        }
        mods.add(&row);
    }
    page.add(&mods);
}
