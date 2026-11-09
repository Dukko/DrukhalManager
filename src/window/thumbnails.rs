//! Mod thumbnails: the descriptor's `picture` (usually `thumbnail.png`),
//! shown beside each mod in the load order.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::Window;
use crate::mods::{Mod, archive};

const SIZE: i32 = 40;
/// Stored at twice the size shown, for high-density screens.
const STORED: i32 = SIZE * 2;

const CSS: &str = "
.mod-thumbnail { border-radius: 6px; }
.mod-thumbnail.placeholder { background: alpha(currentColor, 0.08); }
";

/// Decoded thumbnails by file, so rebuilding the list doesn't read them
/// again. `None`: missing or unreadable.
#[derive(Default)]
pub(super) struct Cache {
    loaded: HashMap<PathBuf, Option<gdk::Texture>>,
    /// Being read, with the images waiting for each.
    pending: HashMap<PathBuf, Vec<glib::WeakRef<gtk::Image>>>,
}

pub(super) fn add_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// Where a mod's thumbnail is, if its descriptor names one. Zipped mods
/// use their unpacked copy.
fn picture_path(m: &Mod) -> Option<PathBuf> {
    let name = m.descriptor.picture.as_deref().filter(|p| !p.trim().is_empty())?;
    let content = m.content.as_deref()?;
    let dir = if content.is_file() { archive::files(content).ok()? } else { content.to_path_buf() };
    Some(dir.join(name.trim()))
}

impl Window {
    /// A square thumbnail for the mod, filled in once the image is read.
    pub(super) fn thumbnail(self: &Rc<Self>, m: &Mod) -> gtk::Image {
        let picture = gtk::Image::builder()
            .pixel_size(SIZE)
            .width_request(SIZE)
            .height_request(SIZE)
            .valign(gtk::Align::Center)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["mod-thumbnail", "placeholder"])
            .build();
        let Some(path) = picture_path(m) else { return picture };
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
        picture
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
        picture.set_pixel_size(SIZE);
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
