//! Host desktop stylesheets: `~/.config/gtk-4.0/gtk.css` in the Flatpak,
//! and Omarchy's active theme (`colors.toml` / `light.mode`) everywhere on
//! Linux.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio::prelude::*;

use crate::platform::is_flatpak;

/// Applies the host's GTK 4 stylesheet (`~/.config/gtk-4.0/gtk.css` inside
/// the Flatpak) and Omarchy's active theme palette when present. Both are
/// watched and reloaded live when the user switches themes.
pub fn load_host_stylesheet() {
    let Some(display) = gtk::gdk::Display::default() else { return };
    load_omarchy(&display);
    if is_flatpak() {
        load_gtk_css(&display);
    }
}

/// Applies `~/.config/gtk-4.0/gtk.css` inside the Flatpak, as GTK would
/// outside the sandbox, so custom libadwaita colour schemes
/// (DankMaterialShell, matugen, Gradience…) apply.
fn load_gtk_css(display: &gtk::gdk::Display) {
    let dir = crate::platform::host_config_dir().join("gtk-4.0");
    let css = dir.join("gtk.css");
    // Nothing to do when the app's own folder has one, or the host has none.
    if gtk::glib::user_config_dir().join("gtk-4.0/gtk.css").exists() || !css.exists() {
        return;
    }
    let provider = gtk::CssProvider::new();
    let file = gtk::gio::File::for_path(&css);
    provider.load_from_file(&file);
    gtk::style_context_add_provider_for_display(display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER);

    let Ok(monitor) = gtk::gio::File::for_path(&dir).monitor_directory(gtk::gio::FileMonitorFlags::NONE, gtk::gio::Cancellable::NONE) else { return };
    let pending: Rc<RefCell<Option<gtk::glib::SourceId>>> = Default::default();
    monitor.connect_changed(move |_, _, _, _| {
        // A palette change rewrites several files at once; reload once.
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
        }
        let (provider, file, pending_ref) = (provider.clone(), file.clone(), pending.clone());
        *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            pending_ref.borrow_mut().take();
            provider.load_from_file(&file);
        }));
    });
    // Lives as long as the app.
    std::mem::forget(monitor);
}

/// Candidate directories where Omarchy keeps the active theme.
fn omarchy_theme_dirs() -> [PathBuf; 2] {
    [
        crate::platform::host_state_dir().join("omarchy/current/theme"),
        crate::platform::host_config_dir().join("omarchy/current/theme"),
    ]
}

/// Applies Omarchy's active theme (`colors.toml` + optional `light.mode`)
/// just below `STYLE_PROVIDER_PRIORITY_USER`, so an explicit `gtk-4.0/gtk.css`
/// still wins if present.
fn load_omarchy(display: &gtk::gdk::Display) {
    let dirs = omarchy_theme_dirs();
    let Some(active_dir) = dirs.iter().find(|d| d.join("colors.toml").is_file()).cloned() else {
        return;
    };
    let Some((css, is_light)) = omarchy_css_for_dir(&active_dir) else {
        return;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    gtk::style_context_add_provider_for_display(display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER - 1);
    apply_color_scheme(is_light);

    let pending: Rc<RefCell<Option<gtk::glib::SourceId>>> = Default::default();
    let schedule_reload = {
        let dirs = dirs.clone();
        let provider = provider.clone();
        let pending = pending.clone();
        move || {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let (dirs, provider, pending_ref) = (dirs.clone(), provider.clone(), pending.clone());
            *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                pending_ref.borrow_mut().take();
                if let Some((css, is_light)) = dirs.iter().find_map(|d| omarchy_css_for_dir(d)) {
                    provider.load_from_string(&css);
                    apply_color_scheme(is_light);
                }
            }));
        }
    };

    // Watch both the theme folder (in-place edits to colors.toml / light.mode)
    // and its parent (`current/` symlink swaps when switching themes).
    let mut watch_paths = Vec::new();
    for dir in &dirs {
        if dir.exists() {
            watch_paths.push(dir.clone());
        }
        if let Some(parent) = dir.parent()
            && parent.exists()
        {
            watch_paths.push(parent.to_path_buf());
        }
    }
    for path in watch_paths {
        if let Ok(monitor) = gtk::gio::File::for_path(&path).monitor_directory(gtk::gio::FileMonitorFlags::NONE, gtk::gio::Cancellable::NONE) {
            let reload = schedule_reload.clone();
            monitor.connect_changed(move |_, _, _, _| reload());
            std::mem::forget(monitor);
        }
    }
}

fn apply_color_scheme(is_light: bool) {
    let scheme = if is_light { adw::ColorScheme::ForceLight } else { adw::ColorScheme::ForceDark };
    adw::StyleManager::default().set_color_scheme(scheme);
}

/// Reads `colors.toml` and `light.mode` from `dir` and builds a libadwaita
/// stylesheet overriding the palette, returning `(css, is_light)`.
fn omarchy_css_for_dir(dir: &Path) -> Option<(String, bool)> {
    let toml = std::fs::read_to_string(dir.join("colors.toml")).ok()?;
    let light_marker = dir.join("light.mode").exists();
    omarchy_css_from_toml(&toml, light_marker)
}

fn omarchy_css_from_toml(toml: &str, light_marker: bool) -> Option<(String, bool)> {
    let colors = parse_colors_toml(toml);
    let (bg, bg_rgb) = parse_hex(colors.get("background")?)?;
    let (fg, _) = parse_hex(colors.get("foreground")?)?;
    let (accent, accent_rgb) = colors.get("accent").or_else(|| colors.get("color4")).and_then(|s| parse_hex(s)).unwrap_or_else(|| ("#3584e4".to_owned(), (0x35, 0x84, 0xe4)));
    let (red, red_rgb) = colors.get("color1").and_then(|s| parse_hex(s)).unwrap_or_else(|| ("#e01b24".to_owned(), (0xe0, 0x1b, 0x24)));
    let (green, green_rgb) = colors.get("color2").and_then(|s| parse_hex(s)).unwrap_or_else(|| ("#2ec27e".to_owned(), (0x2e, 0xc2, 0x7e)));
    let (yellow, yellow_rgb) = colors.get("color3").and_then(|s| parse_hex(s)).unwrap_or_else(|| ("#e5a50a".to_owned(), (0xe5, 0xa5, 0x0a)));

    let is_light = if light_marker {
        true
    } else {
        match colors.get("mode").map(|m| m.trim().to_ascii_lowercase()).as_deref() {
            Some("light") => true,
            Some("dark") => false,
            _ => luminance(bg_rgb) > 0.5,
        }
    };

    let accent_fg = contrasting_fg(accent_rgb);
    let red_fg = contrasting_fg(red_rgb);
    let green_fg = contrasting_fg(green_rgb);
    let yellow_fg = contrasting_fg(yellow_rgb);

    let view_bg = bg.as_str();
    let sidebar_bg = format!("color-mix(in srgb, {fg} 3%, {bg})");
    let card_bg = format!("color-mix(in srgb, {fg} 5%, {bg})");
    let dialog_bg = format!("color-mix(in srgb, {fg} 6%, {bg})");
    let popover_bg = format!("color-mix(in srgb, {fg} 7%, {bg})");
    let headerbar_border = format!("color-mix(in srgb, {fg} 10%, transparent)");

    let css = format!(
        "@define-color accent_color {accent};
@define-color accent_bg_color {accent};
@define-color accent_fg_color {accent_fg};
@define-color window_bg_color {bg};
@define-color window_fg_color {fg};
@define-color view_bg_color {view_bg};
@define-color view_fg_color {fg};
@define-color headerbar_bg_color {bg};
@define-color headerbar_fg_color {fg};
@define-color headerbar_border_color {headerbar_border};
@define-color sidebar_bg_color {sidebar_bg};
@define-color sidebar_fg_color {fg};
@define-color card_bg_color {card_bg};
@define-color card_fg_color {fg};
@define-color popover_bg_color {popover_bg};
@define-color popover_fg_color {fg};
@define-color dialog_bg_color {dialog_bg};
@define-color dialog_fg_color {fg};
@define-color destructive_bg_color {red};
@define-color destructive_fg_color {red_fg};
@define-color destructive_color {red};
@define-color success_bg_color {green};
@define-color success_fg_color {green_fg};
@define-color success_color {green};
@define-color warning_bg_color {yellow};
@define-color warning_fg_color {yellow_fg};
@define-color warning_color {yellow};
@define-color error_bg_color {red};
@define-color error_fg_color {red_fg};
@define-color error_color {red};

:root {{
  --accent-color: {accent};
  --accent-bg-color: {accent};
  --accent-fg-color: {accent_fg};
  --window-bg-color: {bg};
  --window-fg-color: {fg};
  --view-bg-color: {view_bg};
  --view-fg-color: {fg};
  --headerbar-bg-color: {bg};
  --headerbar-fg-color: {fg};
  --headerbar-border-color: {headerbar_border};
  --sidebar-bg-color: {sidebar_bg};
  --sidebar-fg-color: {fg};
  --card-bg-color: {card_bg};
  --card-fg-color: {fg};
  --popover-bg-color: {popover_bg};
  --popover-fg-color: {fg};
  --dialog-bg-color: {dialog_bg};
  --dialog-fg-color: {fg};
  --destructive-bg-color: {red};
  --destructive-fg-color: {red_fg};
  --destructive-color: {red};
  --success-bg-color: {green};
  --success-fg-color: {green_fg};
  --success-color: {green};
  --warning-bg-color: {yellow};
  --warning-fg-color: {yellow_fg};
  --warning-color: {yellow};
  --error-bg-color: {red};
  --error-fg-color: {red_fg};
  --error-color: {red};
}}
"
    );

    Some((css, is_light))
}

/// Parses flat `key = "value"` pairs from an Omarchy `colors.toml`.
fn parse_colors_toml(src: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for raw_line in src.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else { continue };
        let key = key.trim();
        let rest = rest.trim();
        let value = if let Some(inner) = rest.strip_prefix('"') {
            inner.split('"').next().unwrap_or("")
        } else if let Some(inner) = rest.strip_prefix('\'') {
            inner.split('\'').next().unwrap_or("")
        } else {
            rest.split('#').next().unwrap_or("").trim()
        };
        if !key.is_empty() && !value.is_empty() {
            map.insert(key.to_owned(), value.to_owned());
        }
    }
    map
}

/// Parses `#RRGGBB` (or `#RGB`) into normalized `#rrggbb` and `(r, g, b)`.
fn parse_hex(s: &str) -> Option<(String, (u8, u8, u8))> {
    let hex = s.trim().strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((format!("#{r:02x}{g:02x}{b:02x}"), (r, g, b)))
        }
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            Some((format!("#{r:02x}{g:02x}{b:02x}"), (r, g, b)))
        }
        _ => None,
    }
}

fn luminance((r, g, b): (u8, u8, u8)) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

fn contrasting_fg(rgb: (u8, u8, u8)) -> &'static str {
    if luminance(rgb) > 0.5 { "#111111" } else { "#ffffff" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_dark_omarchy_css_from_colors_toml() {
        let toml = r##"
# Tokyo Night
accent = "#7aa2f7"
background = "#1a1b26"
foreground = "#c0caf5"
color1 = "#f7768e"
color2 = "#9ece6a"
color3 = "#e0af68"
"##;
        let (css, is_light) = omarchy_css_from_toml(toml, false).unwrap();
        assert!(!is_light);
        assert!(css.contains("@define-color window_bg_color #1a1b26;"));
        assert!(css.contains("@define-color window_fg_color #c0caf5;"));
        assert!(css.contains("@define-color accent_bg_color #7aa2f7;"));
        assert!(css.contains("--window-bg-color: #1a1b26;"));
        assert!(css.contains("--accent-bg-color: #7aa2f7;"));
        assert!(css.contains("--destructive-color: #f7768e;"));
        assert!(css.contains("--success-color: #9ece6a;"));
        assert!(css.contains("--warning-color: #e0af68;"));
    }

    #[test]
    fn respects_light_mode_marker_and_mode_field() {
        let toml = r##"
mode = "light"
accent = "#1e66f5"
background = "#eff1f5"
foreground = "#4c4f69"
"##;
        let (css, is_light) = omarchy_css_from_toml(toml, false).unwrap();
        assert!(is_light);
        assert!(css.contains("@define-color accent_fg_color #ffffff;"));

        let dark_bg_with_marker = r##"
accent = "#7aa2f7"
background = "#1a1b26"
foreground = "#c0caf5"
"##;
        let (_, is_light) = omarchy_css_from_toml(dark_bg_with_marker, true).unwrap();
        assert!(is_light);
    }

    #[test]
    fn returns_none_without_valid_background_and_foreground() {
        assert!(omarchy_css_from_toml("accent = \"#7aa2f7\"", false).is_none());
        assert!(omarchy_css_from_toml("background = \"not-a-hex\"\nforeground = \"#ffffff\"", false).is_none());
    }
}
