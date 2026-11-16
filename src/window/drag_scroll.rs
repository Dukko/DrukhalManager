//! Scrolls a list while something is dragged near its top or bottom edge,
//! since the mouse wheel doesn't reach the app during a drag.

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// Height of the band at each edge that scrolls, in pixels.
const EDGE: f64 = 64.0;
/// Fastest scroll, in pixels per frame, with the pointer at the very edge.
const MAX_STEP: f64 = 24.0;

pub(super) fn scroll_while_dragging(scroller: &gtk::ScrolledWindow) {
    let motion = gtk::DropControllerMotion::new();
    let pointer_y: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));
    let ticking = Rc::new(Cell::new(false));

    motion.connect_enter({
        let pointer_y = pointer_y.clone();
        let ticking = ticking.clone();
        glib::clone!(
            #[weak]
            scroller,
            move |motion, _, y| {
                pointer_y.set(Some(y));
                if ticking.replace(true) {
                    return;
                }
                let pointer_y = pointer_y.clone();
                let ticking = ticking.clone();
                let motion = motion.clone();
                scroller.add_tick_callback(move |scroller, _| {
                    let Some(y) = pointer_y.get().filter(|_| motion.contains_pointer()) else {
                        ticking.set(false);
                        return glib::ControlFlow::Break;
                    };
                    let h = scroller.height() as f64;
                    let step = if y < EDGE {
                        -MAX_STEP * (1.0 - y / EDGE)
                    } else if y > h - EDGE {
                        MAX_STEP * (1.0 - (h - y) / EDGE)
                    } else {
                        0.0
                    };
                    if step != 0.0 {
                        let adj = scroller.vadjustment();
                        let max = (adj.upper() - adj.page_size()).max(adj.lower());
                        adj.set_value((adj.value() + step).clamp(adj.lower(), max));
                    }
                    glib::ControlFlow::Continue
                });
            }
        )
    });
    motion.connect_motion({
        let pointer_y = pointer_y.clone();
        move |_, _, y| pointer_y.set(Some(y))
    });
    motion.connect_leave({
        let pointer_y = pointer_y.clone();
        move |_| pointer_y.set(None)
    });
    scroller.add_controller(motion);
}
