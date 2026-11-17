//! The app's Material 3 look: its fonts, its icons and the stylesheet that
//! shapes libadwaita's widgets.
//!
//! Colours still come from libadwaita's palette (and from the host's
//! `gtk.css` when a tool like DankMaterialShell or matugen writes one), so
//! the app follows the desktop's accent and light or dark mode. This only
//! changes shapes, type and icons.
//!
//! Fonts (SIL Open Font License 1.1, `data/fonts/*-OFL.txt`):
//! Google Sans Flex for the interface, Fira Code for code and DM Serif
//! Display for a few large titles. Icons: Material Symbols Rounded (Apache
//! License 2.0, `data/icons/material/LICENSE`). See tools/ to rebuild them.

mod icons;

use std::path::{Path, PathBuf};

use gtk::pango::prelude::*;
use gtk::{gdk, prelude::*};
use sha2::{Digest, Sha256};

const FONTS: &[(&str, &[u8])] = &[
    ("GoogleSansFlex.ttf", include_bytes!("../data/fonts/GoogleSansFlex.ttf")),
    ("FiraCode.ttf", include_bytes!("../data/fonts/FiraCode.ttf")),
    ("DMSerifDisplay.ttf", include_bytes!("../data/fonts/DMSerifDisplay.ttf")),
];

/// The icon theme holding the Material icons; it falls back to Adwaita
/// for any icon it doesn't have.
const ICON_THEME: &str = "DrukhalMaterial";

const INDEX_THEME: &str = "[Icon Theme]
Name=DrukhalMaterial
Comment=Material Symbols Rounded, for DrukhalManager
Inherits=Adwaita,hicolor
Directories=scalable/actions

[scalable/actions]
Size=16
MinSize=8
MaxSize=512
Type=Scalable
Context=Actions
";

/// Applies the fonts, icons and stylesheet. Before the first window.
pub fn install() {
    let Some(display) = gdk::Display::default() else { return };
    match unpack() {
        Ok(dir) => {
            add_fonts(&dir.join("fonts"));
            let theme = gtk::IconTheme::for_display(&display);
            // Ours first, so its icons replace the system theme's.
            let mut paths = vec![dir.join("icons")];
            paths.extend(theme.search_path());
            let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            theme.set_search_path(&paths);
            use_icon_theme();
        }
        Err(e) => crate::diagnostics::log!("theme", "Could not unpack the fonts and icons: {e}"),
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    // Above the app's other stylesheets, below the user's own gtk.css.
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 10);
}

/// Switches to the Material icons, and back again whenever the desktop
/// changes the icon theme.
fn use_icon_theme() {
    let Some(settings) = gtk::Settings::default() else { return };
    settings.set_gtk_icon_theme_name(Some(ICON_THEME));
    settings.connect_gtk_icon_theme_name_notify(|settings| {
        if settings.gtk_icon_theme_name().as_deref() != Some(ICON_THEME) {
            settings.set_gtk_icon_theme_name(Some(ICON_THEME));
        }
    });
}

/// Writes the fonts and icon theme to the cache, once per version of them.
/// Pango and GTK load both from files.
fn unpack() -> std::io::Result<PathBuf> {
    let mut hash = Sha256::new();
    for (name, bytes) in FONTS {
        hash.update(name.as_bytes());
        hash.update(bytes);
    }
    for (name, svg) in icons::ICONS {
        hash.update(name.as_bytes());
        hash.update(svg.as_bytes());
    }
    hash.update(INDEX_THEME.as_bytes());
    let id: String = hash.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect();
    let root = crate::platform::cache_dir().join("theme");
    let dir = root.join(&id);
    if !dir.join("done").exists() {
        let fonts = dir.join("fonts");
        std::fs::create_dir_all(&fonts)?;
        for (name, bytes) in FONTS {
            std::fs::write(fonts.join(name), bytes)?;
        }
        let theme = dir.join("icons").join(ICON_THEME);
        let actions = theme.join("scalable/actions");
        std::fs::create_dir_all(&actions)?;
        std::fs::write(theme.join("index.theme"), INDEX_THEME)?;
        for (name, svg) in icons::ICONS {
            std::fs::write(actions.join(format!("{name}.svg")), svg)?;
        }
        std::fs::write(dir.join("done"), "")?;
    }
    // Older versions' copies.
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if entry.file_name() != id.as_str() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(dir)
}

fn add_fonts(dir: &Path) {
    // GTK draws every widget's text with the default font map.
    let map = gtk::Label::new(None).pango_context().font_map();
    let Some(map) = map else { return };
    for (name, _) in FONTS {
        if let Err(e) = map.add_font_file(dir.join(name)) {
            crate::diagnostics::log!("theme", "Could not load the font {name}: {e}");
        }
    }
}

const CSS: &str = include_str!("theme/material.css");
