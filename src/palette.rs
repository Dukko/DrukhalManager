//! The desktop's colours: the palette matugen, DankMaterialShell,
//! Gradience or Omarchy set, read from their files.
//!
//! Sources, first found wins (the order the GTK version of the app used):
//!
//! 1. `~/.config/gtk-4.0/gtk.css` — the `@define-color` lines matugen,
//!    DankMaterialShell or Gradience write.
//! 2. Omarchy's active theme, `colors.toml` and `light.mode`.
//! 3. [`Palette::default`], a Material 3 dark palette.
//!
//! [`watch`] calls back with a fresh palette whenever one of them changes,
//! so a theme switch shows at once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `#rrggbb` or `#rgb`.
    pub fn hex(s: &str) -> Option<Self> {
        let hex = s.trim().strip_prefix('#')?;
        let digit = |i: usize, n: usize| u8::from_str_radix(hex.get(i..i + n)?, 16).ok();
        match hex.len() {
            6 | 8 => Some(Self::new(digit(0, 2)?, digit(2, 2)?, digit(4, 2)?)),
            3 | 4 => Some(Self::new(digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17)),
            _ => None,
        }
    }

    /// `#rrggbb`, `rgb(r, g, b)` or `rgba(r, g, b, a)` (alpha is dropped).
    pub fn css(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.starts_with('#') {
            return Self::hex(s);
        }
        let inner = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb("))?.strip_suffix(')')?;
        let mut parts = inner.split(',').map(|p| p.trim().parse::<f32>().ok());
        let mut next = || parts.next().flatten().map(|v| v.clamp(0.0, 255.0).round() as u8);
        Some(Self::new(next()?, next()?, next()?))
    }

    /// Relative luminance, 0 (black) to 1 (white).
    pub fn luminance(self) -> f32 {
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// `amount` (0–1) of `other` mixed into this colour, as CSS
    /// `color-mix(in srgb, other amount, self)`.
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
        Rgb::new(m(self.r, other.r), m(self.g, other.g), m(self.b, other.b))
    }

    /// Black or white, whichever reads on this colour.
    pub fn on(self) -> Rgb {
        if self.luminance() > 0.4 { Rgb::new(0x11, 0x11, 0x11) } else { Rgb::new(0xff, 0xff, 0xff) }
    }
}

/// Where a palette came from, for showing in the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    GtkCss(PathBuf),
    Omarchy(PathBuf),
    Builtin,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::GtkCss(p) => write!(f, "gtk.css ({})", p.display()),
            Source::Omarchy(p) => write!(f, "Omarchy ({})", p.display()),
            Source::Builtin => f.write_str("built-in"),
        }
    }
}

/// The colours a UI draws with: the source's own, plus Material 3's tonal
/// surfaces worked out from them the way the GTK app's stylesheet does.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub source: Source,
    pub light: bool,
    pub window: Rgb,
    pub on_window: Rgb,
    pub accent: Rgb,
    pub on_accent: Rgb,
    pub error: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    /// Surface containers a step up from the window, tinted with the accent.
    pub container_low: Rgb,
    pub container: Rgb,
    pub container_high: Rgb,
    /// A quiet accent fill, for selected rows and chips.
    pub secondary: Rgb,
    pub outline: Rgb,
    /// Text a step quieter than `on_window`.
    pub muted: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Self::from_core(Source::Builtin, Rgb::new(0x14, 0x13, 0x18), Rgb::new(0xe6, 0xe0, 0xe9), Rgb::new(0xd0, 0xbc, 0xff), None, None, None, None)
    }
}

impl Palette {
    /// Works out the derived colours from the source's core ones.
    #[allow(clippy::too_many_arguments)]
    pub fn from_core(source: Source, window: Rgb, on_window: Rgb, accent: Rgb, error: Option<Rgb>, success: Option<Rgb>, warning: Option<Rgb>, light: Option<bool>) -> Self {
        let light = light.unwrap_or(window.luminance() > 0.4);
        // The GTK app's `--m3-container*`: currentColor then accent mixed in.
        let tone = |fg: f32, ac: f32| window.mix(on_window, fg).mix(accent, ac);
        Self {
            source,
            light,
            window,
            on_window,
            accent,
            on_accent: accent.on(),
            error: error.unwrap_or(if light { Rgb::new(0xba, 0x1a, 0x1a) } else { Rgb::new(0xff, 0xb4, 0xab) }),
            success: success.unwrap_or(if light { Rgb::new(0x1b, 0x6e, 0x3a) } else { Rgb::new(0x8f, 0xd6, 0x9c) }),
            warning: warning.unwrap_or(if light { Rgb::new(0x7d, 0x57, 0x00) } else { Rgb::new(0xf2, 0xc0, 0x5c) }),
            container_low: tone(0.03, 0.04),
            container: tone(0.06, 0.05),
            container_high: tone(0.10, 0.06),
            secondary: window.mix(accent, 0.24),
            outline: window.mix(on_window, 0.35),
            muted: window.mix(on_window, 0.68),
        }
    }

    /// The desktop's palette: the first of the sources that has one.
    pub fn detect() -> Self {
        let css = config_dir().join("gtk-4.0/gtk.css");
        if let Some(p) = std::fs::read_to_string(&css).ok().and_then(|s| Self::from_gtk_css(&s, Source::GtkCss(css.clone()))) {
            return p;
        }
        omarchy_dirs().iter().find_map(|dir| Self::from_omarchy_dir(dir)).unwrap_or_default()
    }

    /// The palette in a given `gtk.css` or `colors.toml`, or the folder
    /// holding one.
    pub fn load(path: &Path) -> Option<Self> {
        if path.is_dir() {
            return Self::from_omarchy_dir(path);
        }
        let text = std::fs::read_to_string(path).ok()?;
        if path.extension().is_some_and(|e| e == "toml") {
            let light = path.parent().is_some_and(|d| d.join("light.mode").exists());
            Self::from_omarchy(&text, light, Source::Omarchy(path.to_owned()))
        } else {
            Self::from_gtk_css(&text, Source::GtkCss(path.to_owned()))
        }
    }

    /// From libadwaita's named colours in a stylesheet, as matugen's GTK
    /// template writes them: `@define-color accent_bg_color #d0bcff;`.
    /// `None` without a window background and foreground.
    pub fn from_gtk_css(css: &str, source: Source) -> Option<Self> {
        let named = define_colors(css);
        let get = |names: &[&str]| names.iter().find_map(|n| named.get(*n).copied());
        let window = get(&["window_bg_color", "view_bg_color"])?;
        let on_window = get(&["window_fg_color", "view_fg_color"])?;
        let accent = get(&["accent_bg_color", "accent_color"]).unwrap_or(Rgb::new(0x35, 0x84, 0xe4));
        let error = get(&["error_color", "destructive_color", "error_bg_color"]);
        let success = get(&["success_color", "success_bg_color"]);
        let warning = get(&["warning_color", "warning_bg_color"]);
        Some(Self::from_core(source, window, on_window, accent, error, success, warning, None))
    }

    /// From an Omarchy theme folder: `colors.toml` and `light.mode`.
    pub fn from_omarchy_dir(dir: &Path) -> Option<Self> {
        let path = dir.join("colors.toml");
        let toml = std::fs::read_to_string(&path).ok()?;
        Self::from_omarchy(&toml, dir.join("light.mode").exists(), Source::Omarchy(path))
    }

    /// From an Omarchy `colors.toml`, read the way the GTK app reads it:
    /// `background`, `foreground`, `accent` (else `color4`), and `color1`,
    /// `color2`, `color3` for error, success and warning.
    pub fn from_omarchy(toml: &str, light_marker: bool, source: Source) -> Option<Self> {
        let colors = flat_toml(toml);
        let get = |k: &str| colors.get(k).and_then(|v| Rgb::hex(v));
        let window = get("background")?;
        let on_window = get("foreground")?;
        let accent = get("accent").or_else(|| get("color4")).unwrap_or(Rgb::new(0x35, 0x84, 0xe4));
        let light = if light_marker {
            Some(true)
        } else {
            match colors.get("mode").map(|m| m.to_ascii_lowercase()).as_deref() {
                Some("light") => Some(true),
                Some("dark") => Some(false),
                _ => None,
            }
        };
        Some(Self::from_core(source, window, on_window, accent, get("color1"), get("color2"), get("color3"), light))
    }
}

/// `@define-color name value;` lines, following `@other` references.
fn define_colors(css: &str) -> HashMap<String, Rgb> {
    let mut raw: HashMap<&str, &str> = HashMap::new();
    for statement in css.split(';') {
        let Some((_, rest)) = statement.split_once("@define-color") else { continue };
        if let Some((name, value)) = rest.trim().split_once(char::is_whitespace) {
            raw.insert(name, value.trim());
        }
    }
    fn resolve<'a>(raw: &HashMap<&'a str, &'a str>, mut value: &'a str) -> Option<Rgb> {
        // A few hops at most; a loop just gives up.
        for _ in 0..8 {
            match value.strip_prefix('@') {
                Some(name) => value = raw.get(name)?,
                None => return Rgb::css(value),
            }
        }
        None
    }
    raw.iter().filter_map(|(name, value)| Some((name.to_string(), resolve(&raw, value)?))).collect()
}

/// Flat `key = "value"` pairs.
fn flat_toml(src: &str) -> HashMap<String, String> {
    src.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with('[') {
                return None;
            }
            let (key, rest) = line.split_once('=')?;
            let rest = rest.trim();
            let value = match rest.chars().next()? {
                q @ ('"' | '\'') => rest[1..].split(q).next()?,
                _ => rest.split('#').next()?.trim(),
            };
            Some((key.trim().to_owned(), value.to_owned()))
        })
        .collect()
}

// The host's folders, also from inside the Flatpak's sandbox. Windows has
// neither, so it keeps the built-in palette.
#[cfg(unix)]
fn config_dir() -> PathBuf {
    drukhal_core::platform::host_config_dir()
}

#[cfg(unix)]
fn omarchy_dirs() -> [PathBuf; 2] {
    [drukhal_core::platform::host_state_dir().join("omarchy/current/theme"), config_dir().join("omarchy/current/theme")]
}

#[cfg(windows)]
fn config_dir() -> PathBuf {
    PathBuf::new()
}

#[cfg(windows)]
fn omarchy_dirs() -> [PathBuf; 2] {
    [PathBuf::new(), PathBuf::new()]
}

/// The folders whose changes can change the palette: the gtk-4.0 folder,
/// and Omarchy's theme folders with their parents, since switching themes
/// swaps the `current` symlink.
fn watched(fixed: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = match fixed {
        Some(path) => vec![path.parent().unwrap_or(path).to_owned()],
        None => {
            let mut dirs = vec![config_dir().join("gtk-4.0")];
            for d in omarchy_dirs() {
                dirs.extend(d.parent().map(Path::to_owned));
                dirs.push(d);
            }
            dirs
        }
    };
    dirs.retain(|d| d.exists());
    dirs.dedup();
    dirs
}

/// Calls `changed` with the palette again (from `fixed`, or the desktop's)
/// each time its files change, on a thread of its own, for as long as the
/// returned watcher lives. Bursts of writes (a theme switch rewrites
/// several files) give one call.
pub fn watch(fixed: Option<PathBuf>, changed: impl Fn(Palette) + Send + 'static) -> notify::Result<impl Drop> {
    use notify::Watcher;
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            let _ = tx.send(());
        }
    })?;
    for dir in watched(fixed.as_deref()) {
        watcher.watch(&dir, notify::RecursiveMode::NonRecursive)?;
    }
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
            changed(match &fixed {
                Some(path) => Palette::load(path).unwrap_or_default(),
                None => Palette::detect(),
            });
        }
    });
    Ok(Guard(Some(watcher)))
}

struct Guard(Option<notify::RecommendedWatcher>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_matugen_gtk_css() {
        let css = "/* matugen */\n@define-color accent_color #d0bcff;\n@define-color accent_bg_color @accent_color;\n@define-color window_bg_color #141218;\n@define-color window_fg_color rgb(230, 224, 233);\n@define-color error_color #ffb4ab;\nwindow { color: red; }";
        let p = Palette::from_gtk_css(css, Source::Builtin).unwrap();
        assert_eq!(p.accent, Rgb::new(0xd0, 0xbc, 0xff));
        assert_eq!(p.on_window, Rgb::new(230, 224, 233));
        assert_eq!(p.error, Rgb::new(0xff, 0xb4, 0xab));
        assert!(!p.light);
        assert!(Palette::from_gtk_css("@define-color accent_color #fff;", Source::Builtin).is_none());
    }

    #[test]
    fn reads_omarchy_colors_toml() {
        let toml = "# Tokyo Night\naccent = \"#7aa2f7\"\nbackground = \"#1a1b26\"\nforeground = \"#c0caf5\"\ncolor1 = \"#f7768e\"\n";
        let p = Palette::from_omarchy(toml, false, Source::Builtin).unwrap();
        assert_eq!(p.window, Rgb::new(0x1a, 0x1b, 0x26));
        assert_eq!(p.error, Rgb::new(0xf7, 0x76, 0x8e));
        assert!(!p.light);
        assert!(Palette::from_omarchy(toml, true, Source::Builtin).unwrap().light, "light.mode wins");
    }

    #[test]
    fn watch_reloads_on_change() {
        let dir = std::env::temp_dir().join(format!("drukhal-palette-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("colors.toml");
        std::fs::write(&file, "background = \"#1a1b26\"\nforeground = \"#c0caf5\"\naccent = \"#7aa2f7\"\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let _watcher = watch(Some(file.clone()), move |p| drop(tx.send(p))).unwrap();
        std::fs::write(&file, "background = \"#eff1f5\"\nforeground = \"#4c4f69\"\naccent = \"#1e66f5\"\n").unwrap();
        let p = rx.recv_timeout(Duration::from_secs(5)).expect("a reload");
        assert_eq!(p.accent, Rgb::new(0x1e, 0x66, 0xf5));
        assert!(p.light);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
