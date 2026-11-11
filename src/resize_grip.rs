//! A handle under a scrolled box that drags it taller or shorter.

use std::cell::Cell;

use adw::prelude::*;
use gtk::glib;

/// The shortest a resized box can get.
const MIN_HEIGHT: i32 = 60;

/// Wraps `scroll` with a grip below it. Dragging the grip sets the box's
/// height, replacing the limits it was built with.
pub fn resizable(scroll: &gtk::ScrolledWindow) -> gtk::Box {
    install_css();
    let grip = gtk::Box::builder()
        .height_request(12)
        .halign(gtk::Align::Fill)
        .cursor(&gtk::gdk::Cursor::from_name("row-resize", None).expect("a standard cursor"))
        .tooltip_text("Drag to resize")
        .css_classes(["resize-grip"])
        .build();
    let pill = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).css_classes(["resize-grip-pill"]).build();
    grip.append(&pill);

    let drag = gtk::GestureDrag::new();
    let start = std::rc::Rc::new(Cell::new(0));
    drag.connect_drag_begin(glib::clone!(
        #[weak]
        scroll,
        #[strong]
        start,
        move |_, _, _| start.set(scroll.height())
    ));
    drag.connect_drag_update(glib::clone!(
        #[weak]
        scroll,
        #[strong]
        start,
        move |_, _, dy| set_height(&scroll, (start.get() + dy.round() as i32).max(MIN_HEIGHT))
    ));
    grip.add_controller(drag);

    let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
    wrapper.append(scroll);
    wrapper.append(&grip);
    wrapper
}

/// Fixes `scroll`'s height, in the order GTK accepts: the maximum may never
/// be below the minimum.
fn set_height(scroll: &gtk::ScrolledWindow, height: i32) {
    if height > scroll.max_content_height() {
        scroll.set_max_content_height(height);
        scroll.set_min_content_height(height);
    } else {
        scroll.set_min_content_height(height);
        scroll.set_max_content_height(height);
    }
}

thread_local! {
    static CSS_INSTALLED: Cell<bool> = const { Cell::new(false) };
}

fn install_css() {
    if CSS_INSTALLED.with(|c| c.replace(true)) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

const CSS: &str = r#"
.resize-grip-pill { min-width: 36px; min-height: 4px; border-radius: 99px; background: alpha(currentColor, 0.2); }
.resize-grip:hover .resize-grip-pill { background: alpha(currentColor, 0.45); }
"#;
