//! Asking an AI model to merge several mods' versions of one object.
//!
//! Two backends: Claude through the Anthropic Messages API (Rust has no
//! official SDK, so this is plain HTTP), or any OpenAI-compatible
//! `/chat/completions` endpoint such as OpenRouter, Ollama or LM Studio.
//! Calls are blocking; run them off the main thread.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::source::Source;

pub const CLAUDE_MODEL: &str = "claude-opus-5-5";
pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
/// Lets the API retry a declined request on another model by itself.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Which service does the merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provider {
    Anthropic { api_key: String },
    /// `base_url` is the part before `/chat/completions`, e.g.
    /// `https://openrouter.ai/api/v1` or `http://localhost:11434/v1`.
    OpenAiCompatible { base_url: String, model: String, api_key: String },
}

impl Provider {
    /// What to show the user as doing the merge.
    pub fn model_name(&self) -> &str {
        match self {
            Provider::Anthropic { .. } => CLAUDE_MODEL,
            Provider::OpenAiCompatible { model, .. } => model,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Merged {
    /// The merged definition, ready to drop into a file.
    pub script: String,
    /// What was combined and any judgement calls, for the user to review.
    pub notes: String,
}

const SYSTEM: &str = "You are an expert Stellaris modder who merges conflicting mods.

Several mods each ship their own version of the same game object. The game keeps only one, so all but one mod's changes are silently lost. Your job is to write a single definition that keeps every mod's intended changes.

How to merge:
- When a vanilla version is provided, use it as the common base: work out what each mod changed relative to it, then apply all of those changes together.
- Without a vanilla version, treat the definitions as alternatives and combine their distinct additions.
- Combine additive changes (new conditions, modifiers, list entries, effects) rather than choosing between them. Keep trigger logic correct when combining conditions: think about whether they belong under AND, OR or NOT.
- When mods set the same value differently, prefer the mod with the higher priority (loaded later) and say so in the notes.
- Don't invent changes that no mod made, and don't drop vanilla content that no mod removed.
- Keep the original formatting style and keep useful comments. Add a short `# merged:` comment only where you made a judgement call.

Output rules:
- `script` must be valid Paradox script containing only the merged definition, plus any `@variable = value` lines it needs at the top.
- For an event, output exactly one event block, without a `namespace` line.
- For a localisation entry, output exactly one line in the form `key:0 \"text\"`, with no language header.
- `notes` is a short plain-text summary for the player: what came from which mod, and any conflict you resolved. No markdown.
- Reply with a JSON object with exactly the string fields `script` and `notes`.";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "script": {"type": "string", "description": "The merged definition"},
            "notes": {"type": "string", "description": "Short summary of what was merged"}
        },
        "required": ["script", "notes"],
        "additionalProperties": false
    })
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(600))).build().into()
}

pub fn merge(provider: &Provider, category: &str, key: &str, vanilla: Option<&Source>, sources: &[Source]) -> Result<Merged, String> {
    complete(provider, SYSTEM, &prompt(category, key, vanilla, sources))
}

const FIX_SYSTEM: &str = "You are an expert Stellaris modder repairing a mod's broken script.

The object below had unbalanced braces. A tool already made it parse by adding or removing `}` mechanically, which may have put braces in the wrong place and nested blocks wrongly. Using the indentation, the structure of similar Stellaris objects and the obvious intent, restore the nesting the author meant.

Rules:
- Change only what is needed to fix the structure. Don't change values, conditions or effects, and don't add or remove content, except removing a line that is clearly a duplicated or stray fragment (mention it in the notes).
- Keep the formatting and comments.
- `script` must be valid Paradox script containing only the repaired definition, without any `@variable` lines (those stay elsewhere in the file). For an event, output exactly one event block without a `namespace` line.
- `notes` is a short plain-text summary for the player of what you changed. No markdown.
- Reply with a JSON object with exactly the string fields `script` and `notes`.";

/// Asks the provider to restore the intended nesting of a damaged object.
pub fn fix(provider: &Provider, category: &str, key: &str, text: &str, problems: &[String], mod_name: &str) -> Result<Merged, String> {
    let prompt = format!(
        "Repair `{key}` in `{category}` from the mod \"{}\".\n\nWhat the tool changed in this file:\n{}\n\n<object>\n{text}\n</object>\n",
        mod_name.replace('"', "'"),
        problems.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
    );
    complete(provider, FIX_SYSTEM, &prompt)
}

fn complete(provider: &Provider, system: &str, prompt: &str) -> Result<Merged, String> {
    match provider {
        Provider::Anthropic { api_key } => merge_anthropic(api_key, system, prompt),
        Provider::OpenAiCompatible { base_url, model, api_key } => merge_openai(base_url, model, api_key, system, prompt),
    }
}

fn merge_anthropic(api_key: &str, system: &str, prompt: &str) -> Result<Merged, String> {
    let body = json!({
        "model": CLAUDE_MODEL,
        "max_tokens": 16000,
        "fallbacks": "default",
        "output_config": {
            "effort": "high",
            "format": {"type": "json_schema", "schema": schema()}
        },
        "system": system,
        "messages": [{"role": "user", "content": prompt}]
    });

    let mut response = agent()
        .post(ANTHROPIC_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .send_json(&body)
        .map_err(|e| format!("Could not reach the Claude API: {e}"))?;
    let status = response.status();
    let reply: Value = response.body_mut().read_json().map_err(|e| format!("Unreadable reply from the Claude API ({status}): {e}"))?;
    parse_anthropic(status.as_u16(), &reply)
}

fn merge_openai(base_url: &str, model: &str, api_key: &str, system: &str, prompt: &str) -> Result<Merged, String> {
    if model.trim().is_empty() {
        return Err("Choose a model for the OpenAI-compatible provider in Preferences.".into());
    }
    let body = json!({
        "model": model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}],
        // Servers that don't support structured output usually ignore this;
        // the prompt asks for JSON either way.
        "response_format": {"type": "json_schema", "json_schema": {"name": "merge", "strict": true, "schema": schema()}}
    });
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut request = agent().post(&url).header("content-type", "application/json");
    if !api_key.is_empty() {
        request = request.header("authorization", &format!("Bearer {api_key}"));
    }
    let mut response = request.send_json(&body).map_err(|e| format!("Could not reach {url}: {e}"))?;
    let status = response.status();
    let reply: Value = response.body_mut().read_json().map_err(|e| format!("Unreadable reply from {url} ({status}): {e}"))?;
    parse_openai(status.as_u16(), &reply)
}

fn prompt(category: &str, key: &str, vanilla: Option<&Source>, sources: &[Source]) -> String {
    let kind = match category {
        "events" => "event",
        "localisation" => "localisation entry",
        _ => "object",
    };
    let mut out = format!("Merge the {kind} `{key}` from `{category}`.\n\n");
    match vanilla {
        Some(v) => out.push_str(&format!("<vanilla file=\"{}\">\n{}\n</vanilla>\n\n", v.file, v.text)),
        None => out.push_str("The base game does not define this, or it could not be found.\n\n"),
    }
    out.push_str("Mod versions, from lowest to highest priority:\n\n");
    for (i, s) in sources.iter().enumerate() {
        out.push_str(&format!("<mod priority=\"{}\" name=\"{}\" file=\"{}\">\n{}\n</mod>\n\n", i + 1, s.label.replace('"', "'"), s.file, s.text));
    }
    out
}

fn http_error(service: &str, status: u16, reply: &Value) -> String {
    // OpenAI-style servers sometimes put a bare string in `error`.
    let message = reply.pointer("/error/message").or_else(|| reply.get("error")).and_then(Value::as_str).unwrap_or("unknown error");
    match status {
        401 | 403 => format!("{service} rejected the API key ({message})"),
        429 => format!("Rate limited by {service}, try again shortly ({message})"),
        _ => format!("{service} error {status}: {message}"),
    }
}

fn parse_anthropic(status: u16, reply: &Value) -> Result<Merged, String> {
    if status != 200 {
        return Err(http_error("The Claude API", status, reply));
    }
    match reply["stop_reason"].as_str() {
        Some("refusal") => return Err("Claude declined to merge this object.".into()),
        Some("max_tokens") => return Err("The merged result was too long and got cut off.".into()),
        _ => {}
    }
    let text = reply["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect::<String>();
    parse_merged(&text)
}

fn parse_openai(status: u16, reply: &Value) -> Result<Merged, String> {
    if status != 200 {
        return Err(http_error("The AI provider", status, reply));
    }
    let choice = &reply["choices"][0];
    match choice["finish_reason"].as_str() {
        Some("length") => return Err("The merged result was too long and got cut off.".into()),
        Some("content_filter") => return Err("The model declined to merge this object.".into()),
        _ => {}
    }
    if let Some(refusal) = choice.pointer("/message/refusal").and_then(Value::as_str) {
        return Err(format!("The model declined to merge this object: {refusal}"));
    }
    let text = choice.pointer("/message/content").and_then(Value::as_str).ok_or("The AI provider sent an empty reply.")?;
    parse_merged(text)
}

/// Reads the `{script, notes}` object, tolerating the code fences and
/// surrounding chatter that models without structured output tend to add.
fn parse_merged(text: &str) -> Result<Merged, String> {
    if let Ok(merged) = serde_json::from_str(text.trim()) {
        return Ok(merged);
    }
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Err("The model's reply did not contain the merged result.".into());
    };
    serde_json::from_str(&text[start..=end]).map_err(|e| format!("The model's reply was not in the expected format: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prompt_in_priority_order() {
        let v = Source { label: "Vanilla".into(), file: "common/buildings/00_buildings.txt".into(), text: "building_lab = {}".into() };
        let a = Source { label: "Mod \"A\"".into(), file: "common/buildings/a.txt".into(), text: "building_lab = { a = yes }".into() };
        let b = Source { label: "B".into(), file: "common/buildings/b.txt".into(), text: "building_lab = { b = yes }".into() };
        let p = prompt("common/buildings", "building_lab", Some(&v), &[a, b]);
        assert!(p.contains("<vanilla file=\"common/buildings/00_buildings.txt\">\nbuilding_lab = {}\n</vanilla>"));
        assert!(p.find("priority=\"1\" name=\"Mod 'A'\"").unwrap() < p.find("priority=\"2\" name=\"B\"").unwrap());
    }

    #[test]
    fn parses_anthropic_replies() {
        let ok = json!({"stop_reason": "end_turn", "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": "{\"script\":\"x = {}\",\"notes\":\"n\"}"}]});
        assert_eq!(parse_anthropic(200, &ok).unwrap(), Merged { script: "x = {}".into(), notes: "n".into() });
        assert!(parse_anthropic(200, &json!({"stop_reason": "refusal", "content": []})).unwrap_err().contains("declined"));
        assert!(parse_anthropic(401, &json!({"error": {"message": "invalid x-api-key"}})).unwrap_err().contains("rejected"));
        assert!(parse_anthropic(500, &json!({"error": {"message": "overloaded"}})).unwrap_err().contains("overloaded"));
    }

    #[test]
    fn parses_openai_replies() {
        let fenced = json!({"choices": [{"finish_reason": "stop", "message": {"content": "Here you go:\n```json\n{\"script\": \"x = {}\", \"notes\": \"n\"}\n```"}}]});
        assert_eq!(parse_openai(200, &fenced).unwrap(), Merged { script: "x = {}".into(), notes: "n".into() });
        assert!(parse_openai(200, &json!({"choices": [{"finish_reason": "length", "message": {"content": "{"}}]})).unwrap_err().contains("cut off"));
        assert!(parse_openai(401, &json!({"error": "No auth credentials found"})).unwrap_err().contains("No auth credentials"));
        assert!(parse_openai(200, &json!({"choices": [{"message": {"content": "sorry"}}]})).is_err());
    }

    #[test]
    fn provider_settings_round_trip() {
        let p = Provider::OpenAiCompatible { base_url: OPENROUTER_URL.into(), model: "some/model".into(), api_key: "k".into() };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"kind\":\"open_ai_compatible\""));
        assert_eq!(serde_json::from_str::<Provider>(&json).unwrap(), p);
    }
}
