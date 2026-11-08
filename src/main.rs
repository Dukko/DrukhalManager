mod merge;
mod mods;
mod paradox;

use adw::prelude::*;

use paradox::steam;

const APP_ID: &str = "io.github.dukkokun.DrukhalManager";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    let home = gtk::glib::home_dir();
    let user_dir = steam::stellaris_user_dir(&home);
    let workshop_dirs = steam::workshop_dirs(&home, steam::STELLARIS_APP_ID);

    let title = adw::WindowTitle::new("DrukhalManager", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();

    let body: gtk::Widget = match mods::discover(&user_dir, &workshop_dirs) {
        Ok(found) if !found.is_empty() => {
            let enabled = found.iter().filter(|m| m.load_position.is_some()).count();
            title.set_subtitle(&format!("{enabled} of {} mods enabled", found.len()));
            mod_list(&found).upcast()
        }
        Ok(_) => status_page("No Mods Found", &format!("No descriptors in {}", user_dir.join("mod").display())),
        Err(e) => status_page("Could Not Read Mods", &format!("{}: {e}", user_dir.display())),
    };

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("DrukhalManager")
        .default_width(960)
        .default_height(640)
        .content(&toolbar)
        .build();
    window.present();
}

fn mod_list(found: &[mods::Mod]) -> gtk::ScrolledWindow {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();

    for m in found {
        let d = &m.descriptor;
        let name = if d.name.is_empty() { m.id.as_str() } else { d.name.as_str() };
        let source = match m.source {
            mods::Source::Workshop => "Workshop",
            mods::Source::Local => "Local",
        };
        let mut subtitle = vec![source.to_owned()];
        if !d.game_version().is_empty() {
            subtitle.push(format!("Stellaris {}", d.game_version()));
        }
        if !d.tags.is_empty() {
            subtitle.push(d.tags.join(", "));
        }

        let row = adw::ActionRow::builder().title(name).subtitle(subtitle.join(" · ")).use_markup(false).build();
        if let Some(content) = &m.content {
            row.set_tooltip_text(Some(&content.display().to_string()));
        }

        let position = gtk::Label::builder()
            .label(m.load_position.map(|p| (p + 1).to_string()).unwrap_or_default())
            .width_chars(3)
            .xalign(1.0)
            .css_classes(["dim-label", "numeric"])
            .build();
        row.add_prefix(&position);

        if let Some(error) = &m.error {
            let icon = gtk::Image::builder().icon_name("dialog-warning-symbolic").tooltip_text(error).css_classes(["warning"]).build();
            row.add_suffix(&icon);
        }

        let toggle = gtk::Switch::builder().active(m.load_position.is_some()).valign(gtk::Align::Center).sensitive(false).build();
        row.add_suffix(&toggle);
        list.append(&row);
    }

    let clamp = adw::Clamp::builder().maximum_size(900).child(&list).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
    gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build()
}

fn status_page(title: &str, description: &str) -> gtk::Widget {
    adw::StatusPage::builder()
        .icon_name("applications-games-symbolic")
        .title(title)
        .description(description)
        .build()
        .upcast()
}

