//! Preferences: which AI provider merges conflicts.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::merge::ai::{CLAUDE_MODEL, OPENROUTER_URL, Provider};
use crate::settings::{self, Settings};

/// Edits a copy of `settings`; `on_close` receives the result when the
/// dialog closes.
pub fn present(parent: &impl IsA<gtk::Widget>, settings: Settings, on_close: impl Fn(Settings) + 'static) {
    // Remember both providers' fields while the user switches between them.
    let (mut anthropic_key, mut openai) = (String::new(), settings::openrouter());
    match &settings.provider {
        Provider::Anthropic { api_key } => anthropic_key = api_key.clone(),
        p @ Provider::OpenAiCompatible { .. } => openai = p.clone(),
    }
    let Provider::OpenAiCompatible { base_url, model, api_key: openai_key } = openai else { unreachable!() };

    let provider = adw::ComboRow::builder()
        .title("Provider")
        .model(&gtk::StringList::new(&["Claude (Anthropic API)", "OpenAI-compatible"]))
        .selected(u32::from(matches!(settings.provider, Provider::OpenAiCompatible { .. })))
        .build();

    let anthropic = adw::PreferencesGroup::builder()
        .title("Claude")
        .description(format!("Merges use {CLAUDE_MODEL}. Leave the key empty to use the ANTHROPIC_API_KEY environment variable."))
        .build();
    let anthropic_key_row = adw::PasswordEntryRow::builder().title("API Key").text(&anthropic_key).build();
    anthropic.add(&anthropic_key_row);

    let compatible = adw::PreferencesGroup::builder()
        .title("OpenAI-Compatible Endpoint")
        .description("OpenRouter, or a local server such as Ollama (http://localhost:11434/v1) or LM Studio (http://localhost:1234/v1). Use a model that's good at code; small local models often produce broken merges.")
        .build();
    let url_row = adw::EntryRow::builder().title("Base URL").text(&base_url).build();
    let model_row = adw::EntryRow::builder().title("Model ID, as the provider names it").text(&model).build();
    let key_row = adw::PasswordEntryRow::builder().title("API Key (optional for local servers)").text(&openai_key).build();
    compatible.add(&url_row);
    compatible.add(&model_row);
    compatible.add(&key_row);

    let choice = adw::PreferencesGroup::builder().title("AI Merging").description("Merging sends the conflicting definitions to the provider you choose.").build();
    choice.add(&provider);

    let sync = {
        let (anthropic, compatible) = (anthropic.clone(), compatible.clone());
        move |row: &adw::ComboRow| {
            anthropic.set_visible(row.selected() == 0);
            compatible.set_visible(row.selected() == 1);
        }
    };
    sync(&provider);
    provider.connect_selected_notify(sync);

    let page = adw::PreferencesPage::new();
    page.add(&choice);
    page.add(&anthropic);
    page.add(&compatible);
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);

    let on_close = Rc::new(RefCell::new(Some(on_close)));
    dialog.connect_closed(move |_| {
        let provider = if provider.selected() == 0 {
            Provider::Anthropic { api_key: anthropic_key_row.text().trim().to_owned() }
        } else {
            let url = url_row.text().trim().to_owned();
            Provider::OpenAiCompatible {
                base_url: if url.is_empty() { OPENROUTER_URL.to_owned() } else { url },
                model: model_row.text().trim().to_owned(),
                api_key: key_row.text().trim().to_owned(),
            }
        };
        if let Some(f) = on_close.borrow_mut().take() {
            f(Settings { provider });
        }
    });
    dialog.present(Some(parent));
}
