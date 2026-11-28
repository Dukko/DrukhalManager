//! Settings: the AI provider that merges (its key goes to the system
//! keyring, as before), and where the game and its data are when they
//! aren't found on their own. Opens over either mode.

use std::path::PathBuf;

use drukhal_core::merge::ai::{self, Provider};
use drukhal_core::settings::{self, Settings};
use iced::widget::{button, column, container, mouse_area, opaque, row, scrollable, space, stack, text, text_input};
use iced::{Background, Color, Element, Fill, Shadow, Vector};

use crate::palette::Palette;
use crate::style::{MONO, c, field, ghost, pill, surface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Claude,
    OpenRouter,
    /// Any OpenAI-compatible server, local ones included.
    Other,
}

/// The settings being edited, as text until saved.
#[derive(Debug, Clone)]
pub struct Draft {
    kind: Kind,
    api_key: String,
    /// Whether a key is already saved in the keyring for the provider.
    key_saved: bool,
    model: String,
    base_url: String,
    data_dir: String,
    executable: String,
    launch_args: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Kind(Kind),
    ApiKey(String),
    Model(String),
    BaseUrl(String),
    DataDir(String),
    Executable(String),
    LaunchArgs(String),
    Save,
    Close,
}

/// What saving did: the settings to use, and whether the game's folders
/// changed, so the mods must be read again.
pub struct Saved {
    pub settings: Settings,
    pub reload: bool,
    pub notice: String,
}

impl Draft {
    pub fn new(s: &Settings) -> Self {
        let (kind, api_key, model, base_url) = match &s.provider {
            Provider::Anthropic { api_key, model } => (Kind::Claude, api_key.clone(), model.clone(), String::new()),
            Provider::OpenAiCompatible { base_url, model, api_key } if base_url.contains("openrouter.ai") => (Kind::OpenRouter, api_key.clone(), model.clone(), base_url.clone()),
            Provider::OpenAiCompatible { base_url, model, api_key } => (Kind::Other, api_key.clone(), model.clone(), base_url.clone()),
        };
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        Self { kind, api_key, key_saved: s.key_in_keyring, model, base_url, data_dir: path(&s.game.data_dir), executable: path(&s.game.executable), launch_args: s.game.launch_args.clone() }
    }

    /// Applies a message; `Some` when the sheet should close, with what
    /// was saved (`None` inside for Cancel).
    pub fn update(&mut self, current: &Settings, message: Message) -> Option<Result<Option<Saved>, String>> {
        match message {
            Message::Kind(k) => {
                if k != self.kind {
                    self.kind = k;
                    self.api_key.clear();
                    self.key_saved = false;
                    match k {
                        Kind::Claude => self.model = ai::builtin_claude_models().first().map(|m| m.id.clone()).unwrap_or_default(),
                        Kind::OpenRouter => {
                            if let Provider::OpenAiCompatible { base_url, .. } = settings::openrouter() {
                                self.base_url = base_url;
                            }
                            self.model.clear();
                        }
                        Kind::Other => {
                            self.base_url = "http://localhost:11434/v1".to_owned();
                            self.model.clear();
                        }
                    }
                }
            }
            Message::ApiKey(s) => self.api_key = s,
            Message::Model(s) => self.model = s,
            Message::BaseUrl(s) => self.base_url = s,
            Message::DataDir(s) => self.data_dir = s,
            Message::Executable(s) => self.executable = s,
            Message::LaunchArgs(s) => self.launch_args = s,
            Message::Close => return Some(Ok(None)),
            Message::Save => return Some(self.save(current).map(Some)),
        }
        None
    }

    fn save(&self, current: &Settings) -> Result<Saved, String> {
        let mut s = current.clone();
        let key = self.api_key.trim().to_owned();
        s.provider = match self.kind {
            Kind::Claude => Provider::Anthropic { api_key: String::new(), model: self.model.trim().to_owned() },
            Kind::OpenRouter | Kind::Other => Provider::OpenAiCompatible { base_url: self.base_url.trim().to_owned(), model: self.model.trim().to_owned(), api_key: String::new() },
        };
        let mut notice = t!("Settings saved");
        if !key.is_empty() {
            // The keyring keeps the key; without one, the settings file does.
            match drukhal_core::secrets::set(drukhal_core::secrets::kind(&s.provider), &key) {
                Ok(()) => s.key_in_keyring = true,
                Err(e) => {
                    *settings::api_key_mut(&mut s.provider) = key;
                    s.key_in_keyring = false;
                    notice = tf!("Settings saved. The keyring couldn't be reached ({error}), so the key is in the settings file, readable only by you.", error = e);
                }
            }
        } else if !self.key_saved {
            s.key_in_keyring = false;
        }
        let path = |t: &str| (!t.trim().is_empty()).then(|| PathBuf::from(t.trim()));
        s.game.data_dir = path(&self.data_dir);
        s.game.executable = path(&self.executable);
        s.game.launch_args = self.launch_args.trim().to_owned();
        s.save(&settings::path()).map_err(|e| tf!("Could not save settings: {error}", error = e))?;
        let reload = s.game.data_dir != current.game.data_dir || s.game.executable != current.game.executable;
        Ok(Saved { settings: s, reload, notice })
    }

    pub fn view<'a>(&'a self, p: &'a Palette) -> Element<'a, Message> {
        let segment = |label: String, kind: Kind| {
            let on = self.kind == kind;
            button(text(label).size(13)).padding([7, 14]).on_press(Message::Kind(kind)).style(move |_, s| if on { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) })
        };
        let label = |s: String| text(s).size(12).color(c(p.muted));
        let input = |placeholder: String, value: &'a str, on: fn(String) -> Message| text_input(&placeholder, value).on_input(on).padding([9, 14]).style(move |_, _| field(p, 12.0, false));
        let key_hint = if self.key_saved && self.api_key.is_empty() { t!("A key is saved in the keyring; type a new one to replace it") } else { t!("API key") };
        let mut ai_fields = column![
            container(row![segment(t!("Claude"), Kind::Claude), segment(t!("OpenRouter"), Kind::OpenRouter), segment(t!("Other"), Kind::Other)].spacing(4)).padding(4).style(move |_| surface(p.container, p.on_window, 999.0)),
            label(t!("API key")),
            input(key_hint, &self.api_key, Message::ApiKey).secure(true),
        ]
        .spacing(8);
        match self.kind {
            Kind::Claude => {
                let models = row(ai::builtin_claude_models().into_iter().map(|m| {
                    let on = m.id == self.model;
                    button(text(m.name).size(12)).padding([6, 12]).on_press(Message::Model(m.id)).style(move |_, s| if on { pill(p.secondary, p.on_window, s) } else { ghost(p.on_window, s) }).into()
                }))
                .spacing(4)
                .wrap();
                ai_fields = ai_fields.push(label(t!("Model"))).push(models);
            }
            Kind::OpenRouter => {
                ai_fields = ai_fields.push(label(t!("Model, as OpenRouter names it"))).push(input("anthropic/claude-sonnet-5.5".to_owned(), &self.model, Message::Model));
            }
            Kind::Other => {
                ai_fields = ai_fields
                    .push(label(t!("Server, the part before /chat/completions")))
                    .push(input("http://localhost:11434/v1".to_owned(), &self.base_url, Message::BaseUrl))
                    .push(label(t!("Model")))
                    .push(input("qwen3".to_owned(), &self.model, Message::Model));
            }
        }
        let game = column![
            label(t!("Game data folder (empty: found on its own)")),
            input("~/.local/share/Paradox Interactive/Stellaris".to_owned(), &self.data_dir, Message::DataDir),
            label(t!("Game executable (empty: found in the Steam libraries)")),
            input(t!("Path to stellaris"), &self.executable, Message::Executable),
            label(t!("Launch options")),
            input("-debug".to_owned(), &self.launch_args, Message::LaunchArgs).font(MONO),
        ]
        .spacing(8);
        let card = container(
            column![
                text(t!("Settings")).size(22),
                scrollable(column![text(t!("AI merging")).size(16), ai_fields, space().height(8), text(t!("The game")).size(16), game].spacing(10)).height(iced::Length::Shrink),
                row![
                    space().width(Fill),
                    button(text(t!("Cancel")).size(14)).padding([10, 18]).on_press(Message::Close).style(move |_, s| ghost(p.on_window, s)),
                    button(text(t!("Save")).size(14).color(c(p.on_accent))).padding([10, 18]).on_press(Message::Save).style(move |_, s| pill(p.accent, p.on_accent, s)),
                ]
                .spacing(8),
            ]
            .spacing(14),
        )
        .padding(24)
        .width(620)
        .max_height(720)
        .style(move |_| container::Style { shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.45), offset: Vector::new(0.0, 12.0), blur_radius: 40.0 }, ..surface(p.container_high, p.on_window, 28.0) });
        let scrim = mouse_area(container(space()).width(Fill).height(Fill).style(|_| container::Style { background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.5))), ..Default::default() })).on_press(Message::Close);
        stack![scrim, container(opaque(card)).center(Fill)].into()
    }
}
