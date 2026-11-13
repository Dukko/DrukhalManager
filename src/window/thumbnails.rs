//! Mod thumbnails: the descriptor's `picture` (usually `thumbnail.png`),
//! shown beside each mod in the load order. Workshop mods without one
//! use their Workshop preview, downloaded once and kept in the cache.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::Window;
use crate::merge::patch;
use crate::mods::{Mod, archive};
use crate::paradox::workshop;

/// Stored at this size, enough for the inspector's thumbnail and for
/// the list's on high-density screens.
const STORED: i32 = 80;

const CSS: &str = "
.mod-thumbnail { border-radius: 6px; }
.mod-thumbnail.placeholder { background: alpha(currentColor, 0.08); }
.mod-thumbnail.patch { background: alpha(@accent_bg_color, 0.15); }
";

/// Decoded thumbnails by file, so rebuilding the list doesn't read them
/// again. `None`: missing or unreadable.
#[derive(Default)]
pub(super) struct Cache {
    loaded: HashMap<PathBuf, Option<gdk::Texture>>,
    /// Being read, with the images waiting for each.
    pending: HashMap<PathBuf, Vec<glib::WeakRef<gtk::Image>>>,
    /// Workshop items whose preview was asked for since the list was
    /// last reloaded, so a missing one isn't asked for again.
    asked: HashSet<u64>,
    /// Waiting to be asked for, in one request.
    queued: Vec<u64>,
}

/// Where a Workshop item's downloaded preview is kept.
fn previews_dir() -> PathBuf {
    crate::platform::cache_dir().join("previews")
}

pub(super) fn add_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// Where a mod's thumbnail is: the picture its descriptor names, or else
/// `thumbnail.png`, which Steam and the launcher show when it names none.
/// Zipped mods use their unpacked copy.
fn picture_path(m: &Mod) -> Option<PathBuf> {
    let content = m.content.as_deref()?;
    let dir = if content.is_file() { archive::files(content).ok()? } else { content.to_path_buf() };
    let named = m.descriptor.picture.as_deref().map(str::trim).filter(|p| !p.is_empty());
    named.into_iter().chain(["thumbnail.png", "thumbnail.jpg", "thumbnail.jpeg"]).find_map(|name| find(&dir, name))
}

/// `name` in `dir`, ignoring case: mods made on Windows often name a
/// `Thumbnail.png` as `thumbnail.png`.
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    let wanted = Path::new(name);
    let folder = dir.join(wanted.parent().unwrap_or(Path::new("")));
    let file = wanted.file_name()?;
    std::fs::read_dir(folder).ok()?.flatten().find(|e| e.file_name().eq_ignore_ascii_case(file) && e.path().is_file()).map(|e| e.path())
}

impl Window {
    /// A thumbnail for the mod, `size` pixels square, filled in once the
    /// image is read.
    pub(super) fn thumbnail_sized(self: &Rc<Self>, m: &Mod, size: i32) -> gtk::Image {
        let picture = gtk::Image::builder()
            .pixel_size(size)
            .width_request(size)
            .height_request(size)
            .valign(gtk::Align::Center)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["mod-thumbnail", "placeholder"])
            .build();
        // The merged patch is this app's, so it wears the app's icon.
        if patch::is_patch(&m.id) {
            picture.set_icon_name(Some(crate::APP_ID));
            picture.set_css_classes(&["mod-thumbnail", "patch"]);
            picture.set_tooltip_text(Some("Made by Drukhal Manager from your merges"));
            return picture;
        }
        let path = match picture_path(m) {
            Some(path) => path,
            None => {
                let Some(id) = m.descriptor.remote_file_id else { return picture };
                let path = previews_dir().join(id.to_string());
                if !path.is_file() {
                    self.download_preview(id, path, &picture);
                    return picture;
                }
                path
            }
        };
        let mut cache = self.thumbnails.borrow_mut();
        if let Some(texture) = cache.loaded.get(&path) {
            show(&picture, texture.as_ref());
            return picture;
        }
        let waiting = cache.pending.entry(path.clone()).or_default();
        waiting.push(picture.downgrade());
        if waiting.len() > 1 {
            return picture; // already being read
        }
        drop(cache);
        self.read_thumbnail(path);
        picture
    }

    /// The Workshop preview of item `id`, which needn't be installed,
    /// `size` pixels square.
    pub(super) fn workshop_thumbnail(self: &Rc<Self>, id: u64, size: i32) -> gtk::Image {
        let picture = gtk::Image::builder()
            .pixel_size(size)
            .width_request(size)
            .height_request(size)
            .valign(gtk::Align::Center)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["mod-thumbnail", "placeholder"])
            .build();
        let path = previews_dir().join(id.to_string());
        if !path.is_file() {
            self.download_preview(id, path, &picture);
            return picture;
        }
        let mut cache = self.thumbnails.borrow_mut();
        if let Some(texture) = cache.loaded.get(&path) {
            show(&picture, texture.as_ref());
            return picture;
        }
        let waiting = cache.pending.entry(path.clone()).or_default();
        waiting.push(picture.downgrade());
        if waiting.len() == 1 {
            drop(cache);
            self.read_thumbnail(path);
        }
        picture
    }

    /// Reads `path` and shows it in the images waiting for it.
    fn read_thumbnail(self: &Rc<Self>, path: PathBuf) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let file = path.clone();
            let texture = gio::spawn_blocking(move || load(&file)).await.ok().flatten();
            let Some(ui) = weak.upgrade() else { return };
            let mut cache = ui.thumbnails.borrow_mut();
            for picture in cache.pending.remove(&path).unwrap_or_default() {
                if let Some(picture) = picture.upgrade() {
                    show(&picture, texture.as_ref());
                }
            }
            cache.loaded.insert(path, texture);
        });
    }

    /// Fetches Workshop item `id`'s preview into `path` and shows it in
    /// `picture`. Items asked for together go to Steam in one request.
    fn download_preview(self: &Rc<Self>, id: u64, path: PathBuf, picture: &gtk::Image) {
        let mut cache = self.thumbnails.borrow_mut();
        let new = cache.asked.insert(id);
        if !new && !cache.pending.contains_key(&path) {
            return; // asked for before, and Steam had none
        }
        cache.pending.entry(path).or_default().push(picture.downgrade());
        if !new {
            return; // on its way
        }
        cache.queued.push(id);
        if cache.queued.len() > 1 {
            return; // a request is about to go
        }
        drop(cache);

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // Let the rest of the list queue its items first.
            glib::timeout_future(std::time::Duration::from_millis(100)).await;
            let Some(ui) = weak.upgrade() else { return };
            let ids = std::mem::take(&mut ui.thumbnails.borrow_mut().queued);
            drop(ui);
            let dir = previews_dir();
            let saved = gio::spawn_blocking({
                let (ids, dir) = (ids.clone(), dir.clone());
                move || workshop::download_previews(&ids, &dir)
            })
            .await
            .unwrap_or_default();
            let Some(ui) = weak.upgrade() else { return };
            for id in ids {
                let path = dir.join(id.to_string());
                if saved.contains(&id) {
                    ui.read_thumbnail(path);
                } else {
                    ui.thumbnails.borrow_mut().pending.remove(&path);
                }
            }
        });
    }
}

/// Reads the image as a square of `STORED` pixels, cropped from the centre
/// like a cover photo.
fn load(path: &std::path::Path) -> Option<gdk::Texture> {
    use gtk::gdk_pixbuf::Pixbuf;
    let (_, width, height) = Pixbuf::file_info(path)?;
    let short = width.min(height).max(1);
    let scale = |n: i32| (i64::from(n) * i64::from(STORED) / i64::from(short)).max(i64::from(STORED)) as i32;
    let (w, h) = (scale(width), scale(height));
    let pixbuf = Pixbuf::from_file_at_scale(path, w, h, false).ok()?;
    // A copy, so its rows are its own rather than a window on the original.
    let square = pixbuf.new_subpixbuf((w - STORED) / 2, (h - STORED) / 2, STORED, STORED).copy()?;
    let format = if square.has_alpha() { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
    let bytes = square.read_pixel_bytes();
    Some(gdk::MemoryTexture::new(STORED, STORED, format, &bytes, square.rowstride() as usize).upcast())
}

fn show(picture: &gtk::Image, texture: Option<&gdk::Texture>) {
    if let Some(texture) = texture {
        picture.set_paintable(Some(texture));
        // The size it was made at.
        picture.set_pixel_size(picture.width_request());
        picture.remove_css_class("placeholder");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 300×120 PNG in three vertical bands: orange, white, green.
    fn banded_png(path: &std::path::Path) {
        use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
        let pixbuf = Pixbuf::new(Colorspace::Rgb, false, 8, 300, 120).unwrap();
        for x in 0..300 {
            let (r, g, b) = if x < 100 { (230, 80, 40) } else if x < 200 { (250, 250, 250) } else { (40, 160, 60) };
            for y in 0..120 {
                pixbuf.put_pixel(x, y, r, g, b, 255);
            }
        }
        pixbuf.savev(path, "png", &[]).unwrap();
    }

    #[test]
    fn finds_thumbnails_the_descriptor_doesnt_name() {
        let dir = std::env::temp_dir().join(format!("drukhal-thumb-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("gfx")).unwrap();
        std::fs::write(dir.join("Thumbnail.PNG"), b"").unwrap();
        std::fs::write(dir.join("gfx/Cover.png"), b"").unwrap();
        // Windows finds it by the name asked for, so compare ignoring case.
        let found = |name: &str, expected: &str| find(&dir, name).is_some_and(|p| p.to_string_lossy().eq_ignore_ascii_case(&dir.join(expected).to_string_lossy()));
        assert!(found("thumbnail.png", "Thumbnail.PNG"), "ignores case");
        assert!(found("gfx/cover.png", "gfx/Cover.png"));
        assert_eq!(find(&dir, "missing.png"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn crops_thumbnails_to_a_centred_square() {
        let path = std::env::temp_dir().join(format!("drukhal-thumb-{}.png", std::process::id()));
        banded_png(&path);
        let texture = load(&path).unwrap();
        assert_eq!((texture.width(), texture.height()), (STORED, STORED));

        let mut downloader = gdk::TextureDownloader::new(&texture);
        downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (pixels, stride) = downloader.download_bytes();
        let at = |x: usize| &pixels[(STORED as usize / 2) * stride + x * 4..][..3];
        // Scaled to 200×80, the middle 80 columns are kept: a sliver of
        // orange, white in the middle, a sliver of green.
        assert!(at(0)[0] > 200 && at(0)[2] < 80, "left edge orange: {:?}", at(0));
        assert!(at(40).iter().all(|&c| c > 240), "centre white: {:?}", at(40));
        assert!(at(79)[1] > 140 && at(79)[0] < 80, "right edge green: {:?}", at(79));

        assert!(load(&path.with_extension("missing.png")).is_none());
        std::fs::remove_file(path).unwrap();
    }
}
