//! The guided crash hunt: turns off part of the mods, has the user launch
//! and say whether it crashed, and narrows down to the mod that does it.
//! It runs in a temporary collection; the user's comes back at the end.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::{Window, display_name};
use crate::conflicts_view::plural;
use crate::mods::checks;
use crate::mods::crash_hunt::{Hunt, Step};

/// A hunt in progress. The hunt's numbers are positions in `ids`, so a
/// reload in between doesn't confuse it.
pub(super) struct Running {
    hunt: Hunt,
    ids: Vec<String>,
    names: Vec<String>,
    /// The collection to go back to, and its load order.
    collection: String,
    mods: Vec<String>,
    /// The temporary collection's name.
    temporary: String,
}

impl Window {
    pub(super) fn hunting(&self) -> bool {
        self.crash_hunt.borrow().is_some()
    }

    /// The menu item: explains and starts a hunt, or offers to stop one.
    pub(super) fn crash_hunt_menu(self: &Rc<Self>) {
        if self.hunting() {
            let dialog = adw::AlertDialog::new(Some("Stop Looking for the Crashing Mod?"), Some("Your collection and load order come back as they were."));
            dialog.add_responses(&[("cancel", "_Keep Going"), ("stop", "_Stop")]);
            dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
            dialog.set_close_response("cancel");
            let weak = Rc::downgrade(self);
            dialog.connect_response(Some("stop"), move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.finish_hunt(None);
                }
            });
            return dialog.present(Some(&self.window));
        }
        if self.game.is_none() {
            return self.toasts.add_toast(adw::Toast::new("Stellaris wasn't found, so it can't be launched for the tests"));
        }
        let count = self.state.borrow().enabled.len();
        if count == 0 {
            return self.toasts.add_toast(adw::Toast::new("No mods are enabled"));
        }
        let launches = Hunt::new((0..count).collect(), HashMap::new(), &[]).estimate();
        let dialog = adw::AlertDialog::builder()
            .heading("Find the Crashing Mod")
            .body(format!(
                "DrukhalManager turns off part of your {count} mods at a time. Each time, launch the game, do what made it crash, and say whether it crashed. That takes about {launches} launches.\n\nStart a new game for each test, or load a save you don't mind losing, and don't save: a save loaded with mods turned off can break. Mods that others need stay on with them. Your collection and load order come back at the end."
            ))
            .build();
        dialog.add_responses(&[("cancel", "_Cancel"), ("start", "_Start")]);
        dialog.set_response_appearance("start", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("start"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(Some("start"), move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.start_hunt();
            }
        });
        dialog.present(Some(&self.window));
    }

    fn start_hunt(self: &Rc<Self>) {
        if self.state.borrow().dirty && !self.save() {
            return;
        }
        // Mods the last error log blames are tested first.
        let blamed: Vec<String> = self.error_report().groups.into_iter().filter(|g| g.total() > 0).filter_map(|g| g.mod_id).collect();
        let (ids, names, needs, first) = {
            let state = self.state.borrow();
            let position: HashMap<usize, usize> = state.enabled.iter().enumerate().map(|(p, &i)| (i, p)).collect();
            let ids: Vec<String> = state.enabled.iter().map(|&i| state.mods[i].id.clone()).collect();
            let names = state.enabled.iter().map(|&i| display_name(&state.mods[i]).to_owned()).collect();
            let needs = state.enabled.iter().enumerate().map(|(p, &i)| (p, checks::dependencies_of(&state.mods, i).iter().filter_map(|d| position.get(d).copied()).collect())).collect();
            let first: Vec<usize> = ids.iter().enumerate().filter(|(_, id)| blamed.contains(id)).map(|(p, _)| p).collect();
            (ids, names, needs, first)
        };
        let (collection, mods) = {
            let collections = self.collections.borrow();
            (collections.active.clone(), collections.active().mods.clone())
        };
        let temporary = {
            let mut collections = self.collections.borrow_mut();
            let name = collections.add("Crash Hunt", ids.clone());
            collections.active = name.clone();
            name
        };
        self.save_collections();
        self.rebuild_collection_menu();
        let hunt = Hunt::new((0..ids.len()).collect(), needs, &first);
        let step = hunt.step();
        self.crash_hunt.replace(Some(Running { hunt, ids, names, collection, mods, temporary }));
        self.next_hunt_step(step);
    }

    /// Sets up the next test, or ends the hunt with its result.
    fn next_hunt_step(self: &Rc<Self>, step: Step) {
        let Step::Test(on) = step else { return self.finish_hunt(Some(step)) };
        let (ids, banner) = {
            let running = self.crash_hunt.borrow();
            let Some(r) = running.as_ref() else { return };
            let ids: Vec<String> = on.iter().map(|&p| r.ids[p].clone()).collect();
            let what = if r.hunt.launches == 0 {
                format!("Test 1: all {} mods, to see the crash happen", r.ids.len())
            } else {
                format!("Test {}: {} of {} mods on, {} left to check", r.hunt.launches + 1, on.len(), r.ids.len(), plural(r.hunt.suspects(), "suspect", "suspects"))
            };
            (ids, format!("{what}. Launch, try to make it crash, then quit."))
        };
        self.state.borrow_mut().set_enabled_ids(&ids);
        self.save();
        self.hunt_banner.set_title(&banner);
        self.hunt_banner.set_revealed(true);
    }

    /// After a test session: asks whether it crashed.
    pub(super) fn ask_hunt_result(self: &Rc<Self>, detected: bool) {
        let body = if detected { "Stellaris left a crash report, so it looks like it did." } else { "Answer “Yes” if it crashed, froze, or showed the problem you're looking for." };
        let dialog = adw::AlertDialog::new(Some("Did Stellaris Crash?"), Some(body));
        dialog.add_responses(&[("stop", "_Stop Looking"), ("no", "_No"), ("yes", "_Yes")]);
        dialog.set_response_appearance("yes", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some(if detected { "yes" } else { "no" }));
        // Closing it leaves the test to run again.
        dialog.set_close_response("again");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            match response {
                "yes" | "no" => {
                    let step = ui.crash_hunt.borrow_mut().as_mut().map(|r| r.hunt.answer(response == "yes"));
                    if let Some(step) = step {
                        ui.next_hunt_step(step);
                    }
                }
                "stop" => ui.finish_hunt(None),
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Puts the user's collection back, then shows what was found.
    fn finish_hunt(self: &Rc<Self>, result: Option<Step>) {
        let Some(r) = self.crash_hunt.take() else { return };
        self.hunt_banner.set_revealed(false);
        {
            let mut collections = self.collections.borrow_mut();
            collections.active = r.collection.clone();
            collections.remove(&r.temporary);
        }
        self.save_collections();
        self.rebuild_collection_menu();
        self.apply_collection_mods(&r.mods);
        let launches = plural(r.hunt.launches, "launch", "launches");
        let (heading, body) = match &result {
            None => return self.toasts.add_toast(adw::Toast::new("Stopped; your load order is back")),
            Some(Step::Found(p)) => (
                format!("Found It: {}", r.names[*p]),
                format!("After {launches}: Stellaris crashed with “{}” on and didn't without it. It may only crash together with another of your mods, but turning it off avoids the crash.\n\nYour load order is back as it was.", r.names[*p]),
            ),
            Some(Step::Combination(left)) => (
                "No Single Mod".to_owned(),
                format!(
                    "After {launches}: the crash takes several of these together, which can't be tested apart:\n{}\n\nYour load order is back as it was.",
                    left.iter().map(|&p| format!("• {}", r.names[p])).collect::<Vec<_>>().join("\n")
                ),
            ),
            Some(Step::NoCrash | Step::Test(_)) => (
                "It Didn't Crash".to_owned(),
                "Stellaris didn't crash with all the mods on, so there was nothing to narrow down. Try again when the crash comes back.\n\nYour load order is back as it was.".to_owned(),
            ),
        };
        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        dialog.add_response("close", "_Close");
        let culprit = if let Some(Step::Found(p)) = result { self.state.borrow().mods.iter().position(|m| m.id == r.ids[p]) } else { None };
        if let Some(index) = culprit {
            if self.state.borrow().mods[index].descriptor.remote_file_id.is_some() {
                dialog.add_response("workshop", "Open _Workshop Page");
            }
            dialog.add_response("disable", "_Turn It Off");
            dialog.set_response_appearance("disable", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("disable"));
        }
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let (Some(ui), Some(index)) = (weak.upgrade(), culprit) else { return };
            match response {
                "workshop" => ui.open_workshop(index),
                "disable" => {
                    ui.state.borrow_mut().set_enabled(index, false);
                    ui.refresh();
                    ui.offer_undo(&format!("Turned off {}", glib::markup_escape_text(display_name(&ui.state.borrow().mods[index]))));
                }
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }
}
