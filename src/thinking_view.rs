//! A collapsible box that streams the AI model's reasoning while it works.
//! Collapsed, its header shows a spinner and the latest thought; expanded,
//! the full reasoning, following along as it arrives.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::merge::ai::Progress;

pub struct ThinkingView {
    pub widget: gtk::Expander,
    title: gtk::Label,
    spinner: adw::Spinner,
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    /// Words of reasoning received, across all runs.
    words: Cell<usize>,
    /// Characters of answer received in the current run.
    output: Cell<usize>,
    /// Whether the current run has sent any reasoning yet.
    reasoned: Cell<bool>,
}

impl ThinkingView {
    pub fn new() -> Rc<Self> {
        let spinner = adw::Spinner::new();
        let title = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).hexpand(true).build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(&spinner);
        header.append(&title);

        let buffer = gtk::TextBuffer::new(None);
        buffer.create_tag(Some("heading"), &[("weight", &700i32.to_value())]);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .css_classes(["dim-label"])
            .build();
        let scroll = gtk::ScrolledWindow::builder().child(&view).hscrollbar_policy(gtk::PolicyType::Never).min_content_height(80).max_content_height(260).propagate_natural_height(true).css_classes(["card"]).margin_top(6).build();
        let widget = gtk::Expander::builder().label_widget(&header).child(&crate::resize_grip::resizable(&scroll)).visible(false).build();
        widget.set_tooltip_text(Some("What the model is thinking. Click to expand."));
        Rc::new(Self { widget, title, spinner, view, buffer, words: Cell::new(0), output: Cell::new(0), reasoned: Cell::new(false) })
    }

    fn begin(&self, heading: Option<&str>) {
        self.widget.set_visible(true);
        self.spinner.set_visible(true);
        self.title.set_label("Waiting for the model…");
        self.output.set(0);
        self.reasoned.set(false);
        if let Some(heading) = heading {
            let mut end = self.buffer.end_iter();
            if end.offset() > 0 {
                self.buffer.insert(&mut end, "\n\n");
            }
            self.buffer.insert_with_tags_by_name(&mut end, heading, &["heading"]);
            self.buffer.insert(&mut end, "\n");
        }
    }

    fn push(&self, progress: Progress) {
        match progress {
            Progress::Thinking(text) => {
                self.reasoned.set(true);
                self.words.set(self.words.get() + text.split_whitespace().count());
                let mut end = self.buffer.end_iter();
                self.buffer.insert(&mut end, &text);
                self.view.scroll_to_iter(&mut self.buffer.end_iter(), 0.0, false, 0.0, 1.0);
                // The header previews the latest line of reasoning.
                let all = self.buffer.text(&self.buffer.start_iter(), &self.buffer.end_iter(), false);
                let latest = all.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
                self.title.set_label(&format!("Thinking: {latest}"));
            }
            Progress::Output(text) => {
                self.output.set(self.output.get() + text.chars().count());
                self.title.set_label(&format!("Writing the result… {} characters", self.output.get()));
            }
        }
    }

    fn end(&self) {
        self.spinner.set_visible(false);
        let words = self.words.get();
        self.title.set_label(&match (words, self.reasoned.get()) {
            (0, _) => "This model didn't share its reasoning".to_owned(),
            (w, _) => format!("Reasoning ({w} words)"),
        });
        if words == 0 {
            self.widget.set_expanded(false);
        }
    }
}

/// Runs `job` off the main thread, streaming its progress into `view`.
/// `heading` labels this run in the reasoning, for dialogs that run several.
pub async fn run<T: Send + 'static>(view: &Rc<ThinkingView>, heading: Option<&str>, job: impl FnOnce(&mut dyn FnMut(Progress)) -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = async_channel::unbounded();
    view.begin(heading);
    let handle = gio::spawn_blocking(move || job(&mut |p| drop(tx.send_blocking(p))));
    // The channel closes when the job finishes and drops its sender.
    while let Ok(progress) = rx.recv().await {
        view.push(progress);
    }
    let result = handle.await.ok();
    view.end();
    result
}
