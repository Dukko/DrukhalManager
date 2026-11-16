//! The objects enabled mods replace that a game update changed: the mods
//! still load their own copies, so the game's changes don't apply to them.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;

use crate::conflicts_view::Analysis;
use crate::diff_view::{Compare, Header, Pair};
use crate::merge::base_game::Behind;

pub fn present(parent: &impl IsA<gtk::Widget>, analysis: Rc<Analysis>) {
    let Some((old, new)) = analysis.behind_versions.clone() else { return };
    let navigation = adw::NavigationView::new();

    let page = adw::PreferencesPage::new();
    let intro = adw::PreferencesGroup::builder()
        .description(t!("These mods still load their own copies, so the game's changes to these objects don't apply until the mods are updated."))
        .build();
    page.add(&intro);
    let mut by_mod: BTreeMap<usize, Vec<&Behind>> = BTreeMap::new();
    for b in &analysis.behind {
        by_mod.entry(b.mod_position).or_default().push(b);
    }
    for (position, objects) in by_mod {
        let group = adw::PreferencesGroup::builder().title(glib_escape(analysis.name(position))).build();
        for b in objects {
            let row = adw::ActionRow::builder().title(&b.key).subtitle(&b.category).use_markup(false).activatable(true).build();
            if b.now.is_none() {
                row.add_suffix(&gtk::Label::builder().label(t!("Removed from the game")).css_classes(["dim-label"]).build());
            }
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let (navigation, analysis, b, old, new) = (navigation.clone(), analysis.clone(), b.clone(), old.clone(), new.clone());
            row.connect_activated(move |_| navigation.push(&detail(&analysis, &b, &old, &new)));
            group.add(&row);
        }
        page.add(&group);
    }

    navigation.add(&wrap(&t!("Changed by the Game Update"), Some(&tf!("Objects your enabled mods replace that changed between Stellaris {old} and {new}", old = &old, new = &new)), &page));
    adw::Dialog::builder().content_width(900).content_height(720).child(&navigation).build().present(Some(parent));
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

fn detail(analysis: &Analysis, b: &Behind, old: &str, new: &str) -> adw::NavigationPage {
    let pair = Pair::new(false);
    pair.show(&b.was.text, b.now.as_ref().map_or("", |n| n.text.as_str()), &Compare::Each, false);
    pair.left.set_header(&Header { title: tf!("Stellaris {version}", version = old), subtitle: b.was.file.clone(), color: None, badge: None });
    pair.right.set_header(&Header {
        title: tf!("Stellaris {version}", version = new),
        subtitle: b.now.as_ref().map_or_else(|| t!("Removed from the game"), |n| n.file.clone()),
        color: None,
        badge: None,
    });
    pair.widget.set_vexpand(true);

    let note = gtk::Label::builder()
        .label(tf!("{name} still uses its own copy, from {file}.", name = analysis.name(b.mod_position), file = &b.file))
        .css_classes(["dim-label"])
        .wrap(true)
        .xalign(0.0)
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&pair.widget);
    body.append(&note);
    wrap(&b.key, None, &body)
}
