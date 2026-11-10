//! Asking an AI model to merge several mods' versions of one object.
//!
//! Two backends: Claude through the Anthropic Messages API (Rust has no
//! official SDK, so this is plain HTTP), or any OpenAI-compatible
//! `/chat/completions` endpoint such as OpenRouter, Ollama or LM Studio.
//! Calls are blocking; run them off the main thread.

use std::io::BufRead;
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

/// What the model is doing, reported while a request streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// A piece of the model's reasoning (summarized, for Claude).
    Thinking(String),
    /// A piece of the final answer.
    Output(String),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Merged {
    /// The merged definition, ready to drop into a file.
    pub script: String,
    /// What was combined and any judgement calls, for the user to review.
    pub notes: String,
    /// Tokens the request used, when the provider reports them.
    #[serde(skip)]
    pub usage: Option<Usage>,
}

/// Tokens billed for one request. Output includes the model's thinking.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    /// What the provider charged in dollars, when it says (OpenRouter does).
    pub cost: Option<f64>,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.cost = match (self.cost, other.cost) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
    }
}

/// Claude Opus 5.5's price in US dollars per million input and output tokens.
const CLAUDE_PRICE: (f64, f64) = (4.0, 20.0);

/// What a request is expected to use before it's sent.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Estimate {
    pub input: u64,
    /// Without thinking: just the merged script and notes.
    pub output_low: u64,
    /// With room for the model's reasoning.
    pub output_high: u64,
}

impl std::ops::AddAssign for Estimate {
    fn add_assign(&mut self, other: Self) {
        self.input += other.input;
        self.output_low += other.output_low;
        self.output_high += other.output_high;
    }
}

/// Roughly how many tokens text costs; Paradox script tokenizes densely.
fn tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(3)
}

/// A rough guess at what merging one object will use.
pub fn estimate(category: &str, key: &str, vanilla: Option<&Source>, sources: &[Source]) -> Estimate {
    let input = tokens(SYSTEM) + tokens(&prompt(category, key, vanilla, sources));
    let largest = sources.iter().chain(vanilla).map(|s| tokens(&s.text)).max().unwrap_or(0);
    // The merged script is about as long as the longest version, plus notes
    // and JSON escaping. Thinking at high effort varies a lot.
    let output_low = largest + largest / 5 + 150;
    let output_high = output_low + 1500 + input;
    Estimate { input, output_low, output_high }
}

/// Dollars per million input and output tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

impl Price {
    pub fn cost(&self, input: u64, output: u64) -> f64 {
        (input as f64 * self.input + output as f64 * self.output) / 1_000_000.0
    }
}

/// The model's price, when it can be found: fixed for Claude, and looked up
/// from the `/models` list for OpenAI-compatible services that publish
/// prices there (OpenRouter). Blocking; run it off the main thread.
pub fn price(provider: &Provider) -> Option<Price> {
    match provider {
        Provider::Anthropic { .. } => Some(Price { input: CLAUDE_PRICE.0, output: CLAUDE_PRICE.1 }),
        Provider::OpenAiCompatible { base_url, model, api_key } => {
            let url = format!("{}/models", base_url.trim_end_matches('/'));
            let mut request = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(20))).build().new_agent().get(&url);
            if !api_key.is_empty() {
                request = request.header("authorization", &format!("Bearer {api_key}"));
            }
            let mut response = request.call().ok()?;
            if response.status() != 200 {
                return None;
            }
            let list: Value = response.body_mut().read_json().ok()?;
            listed_price(&list, model)
        }
    }
}

/// Reads a model's price from an OpenRouter-style model list, which gives
/// dollars per token as strings.
fn listed_price(list: &Value, model: &str) -> Option<Price> {
    let entry = list["data"].as_array()?.iter().find(|m| m["id"].as_str() == Some(model))?;
    let per_million = |field: &str| -> Option<f64> {
        let v = &entry["pricing"][field];
        let per_token = v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64())?;
        Some(per_token * 1_000_000.0)
    };
    Some(Price { input: per_million("prompt")?, output: per_million("completion")? })
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

pub fn merge(provider: &Provider, category: &str, key: &str, vanilla: Option<&Source>, sources: &[Source], on: &mut dyn FnMut(Progress)) -> Result<Merged, String> {
    complete(provider, SYSTEM, &prompt(category, key, vanilla, sources), on)
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
pub fn fix(provider: &Provider, category: &str, key: &str, text: &str, problems: &[String], mod_name: &str, on: &mut dyn FnMut(Progress)) -> Result<Merged, String> {
    let prompt = format!(
        "Repair `{key}` in `{category}` from the mod \"{}\".\n\nWhat the tool changed in this file:\n{}\n\n<object>\n{text}\n</object>\n",
        mod_name.replace('"', "'"),
        problems.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
    );
    complete(provider, FIX_SYSTEM, &prompt, on)
}

fn complete(provider: &Provider, system: &str, prompt: &str, on: &mut dyn FnMut(Progress)) -> Result<Merged, String> {
    match provider {
        Provider::Anthropic { api_key } => merge_anthropic(api_key, system, prompt, on),
        Provider::OpenAiCompatible { base_url, model, api_key } => merge_openai(base_url, model, api_key, system, prompt, on),
    }
}

fn merge_anthropic(api_key: &str, system: &str, prompt: &str, on: &mut dyn FnMut(Progress)) -> Result<Merged, String> {
    let body = json!({
        "model": CLAUDE_MODEL,
        "max_tokens": 64000,
        "stream": true,
        "fallbacks": "default",
        // Claude Opus 5.5 always thinks; ask for readable summaries of it.
        "thinking": {"type": "adaptive", "display": "summarized"},
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
    if status != 200 {
        let reply: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        return Err(http_error("The Claude API", status.as_u16(), &reply));
    }
    anthropic_stream(std::io::BufReader::new(response.into_body().into_reader()), on)
}

/// Reads Server-Sent Events, calling `f` with each event's data.
fn read_sse(reader: impl BufRead, mut f: impl FnMut(&str) -> Result<(), String>) -> Result<(), String> {
    let mut data = String::new();
    for line in reader.lines() {
        let line = line.map_err(|e| format!("The connection broke off: {e}"))?;
        if line.is_empty() {
            if !data.is_empty() {
                f(&data)?;
                data.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if !data.is_empty() {
        f(&data)?;
    }
    Ok(())
}

fn anthropic_stream(reader: impl BufRead, on: &mut dyn FnMut(Progress)) -> Result<Merged, String> {
    let mut text = String::new();
    let mut stop_reason = None;
    let mut usage = Usage::default();
    let count = |u: &Value, field: &str| u[field].as_u64().unwrap_or(0);
    read_sse(reader, |data| {
        let event: Value = serde_json::from_str(data).map_err(|e| format!("Unreadable event from the Claude API: {e}"))?;
        match event["type"].as_str() {
            Some("message_start") => {
                let u = &event["message"]["usage"];
                usage.input = count(u, "input_tokens") + count(u, "cache_creation_input_tokens") + count(u, "cache_read_input_tokens");
                usage.output = count(u, "output_tokens");
                usage.cost = Some(Price { input: CLAUDE_PRICE.0, output: CLAUDE_PRICE.1 }.cost(usage.input, usage.output));
            }
            Some("content_block_delta") => match event["delta"]["type"].as_str() {
                Some("thinking_delta") => on(Progress::Thinking(event["delta"]["thinking"].as_str().unwrap_or_default().to_owned())),
                Some("text_delta") => {
                    let delta = event["delta"]["text"].as_str().unwrap_or_default();
                    text.push_str(delta);
                    on(Progress::Output(delta.to_owned()));
                }
                _ => {}
            },
            Some("message_delta") => {
                stop_reason = event["delta"]["stop_reason"].as_str().map(str::to_owned);
                // Cumulative, so the last one is the total.
                if let Some(output) = event["usage"]["output_tokens"].as_u64() {
                    usage.output = output;
                    usage.cost = Some(Price { input: CLAUDE_PRICE.0, output: CLAUDE_PRICE.1 }.cost(usage.input, usage.output));
                }
            }
            Some("error") => return Err(format!("The Claude API reported an error: {}", event["error"]["message"].as_str().unwrap_or("unknown"))),
            _ => {}
        }
        Ok(())
    })?;
    match stop_reason.as_deref() {
        Some("refusal") => Err("Claude declined to merge this object.".into()),
        Some("max_tokens") => Err("The merged result was too long and got cut off.".into()),
        _ => parse_merged(&text).map(|m| Merged { usage: Some(usage), ..m }),
    }
}

fn merge_openai(base_url: &str, model: &str, api_key: &str, system: &str, prompt: &str, on: &mut dyn FnMut(Progress)) -> Result<Merged, String> {
    if model.trim().is_empty() {
        return Err("Choose a model for the OpenAI-compatible provider in Preferences.".into());
    }
    let body = json!({
        "model": model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}],
        "stream": true,
        // Asks for a final chunk with the token counts.
        "stream_options": {"include_usage": true},
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
    if status != 200 {
        let reply: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        return Err(http_error("The AI provider", status.as_u16(), &reply));
    }
    openai_stream(std::io::BufReader::new(response.into_body().into_reader()), on)
}

/// Splits a reply into reasoning and answer when the model writes its
/// reasoning inline as `<think>…</think>`, as many local models do. While
/// the opening tag may still be arriving, nothing counts as answer yet.
fn split_think(raw: &str) -> (&str, &str) {
    let trimmed = raw.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<think>") {
        return match rest.split_once("</think>") {
            Some((thinking, answer)) => (thinking, answer),
            None => (rest, ""),
        };
    }
    if "<think>".starts_with(trimmed) { ("", "") } else { ("", raw) }
}

fn openai_stream(mut reader: impl BufRead, on: &mut dyn FnMut(Progress)) -> Result<Merged, String> {
    // Some servers ignore `stream` and send a single JSON reply.
    let first = reader.fill_buf().map_err(|e| format!("The connection broke off: {e}"))?.iter().copied().find(|b| !b.is_ascii_whitespace());
    if first == Some(b'{') {
        let mut body = String::new();
        reader.read_to_string(&mut body).map_err(|e| e.to_string())?;
        let reply: Value = serde_json::from_str(&body).map_err(|e| format!("Unreadable reply from the AI provider: {e}"))?;
        let message = &reply["choices"][0]["message"];
        if let Some(reasoning) = message["reasoning"].as_str().or(message["reasoning_content"].as_str()) {
            on(Progress::Thinking(reasoning.to_owned()));
        }
        return parse_openai(200, &reply);
    }

    let (mut raw, mut thinking_sent, mut answer_sent) = (String::new(), 0, 0);
    let mut finish = None;
    let mut usage = None;
    read_sse(reader, |data| {
        if data.trim() == "[DONE]" {
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(data).map_err(|e| format!("Unreadable event from the AI provider: {e}"))?;
        if let Some(message) = chunk.pointer("/error/message").and_then(Value::as_str) {
            return Err(format!("The AI provider reported an error: {message}"));
        }
        if chunk["usage"].is_object() {
            usage = openai_usage(&chunk["usage"]);
        }
        let choice = &chunk["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            finish = Some(reason.to_owned());
        }
        let delta = &choice["delta"];
        // OpenRouter sends `reasoning`; DeepSeek-style servers `reasoning_content`.
        if let Some(reasoning) = delta["reasoning"].as_str().or(delta["reasoning_content"].as_str()).filter(|r| !r.is_empty()) {
            on(Progress::Thinking(reasoning.to_owned()));
        }
        if let Some(content) = delta["content"].as_str() {
            raw.push_str(content);
            let (thinking, answer) = split_think(&raw);
            if thinking.len() > thinking_sent {
                on(Progress::Thinking(thinking[thinking_sent..].to_owned()));
                thinking_sent = thinking.len();
            }
            if answer.len() > answer_sent {
                on(Progress::Output(answer[answer_sent..].to_owned()));
                answer_sent = answer.len();
            }
        }
        Ok(())
    })?;
    match finish.as_deref() {
        Some("length") => Err("The merged result was too long and got cut off.".into()),
        Some("content_filter") => Err("The model declined to merge this object.".into()),
        _ => parse_merged(split_think(&raw).1).map(|m| Merged { usage, ..m }),
    }
}

fn openai_usage(u: &Value) -> Option<Usage> {
    Some(Usage { input: u["prompt_tokens"].as_u64()?, output: u["completion_tokens"].as_u64().unwrap_or(0), cost: u["cost"].as_f64() })
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
    parse_merged(text).map(|m| Merged { usage: openai_usage(&reply["usage"]), ..m })
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

    fn collect(f: impl FnOnce(&mut dyn FnMut(Progress)) -> Result<Merged, String>) -> (Result<Merged, String>, String, String) {
        let (mut thinking, mut output) = (String::new(), String::new());
        let result = f(&mut |p| match p {
            Progress::Thinking(t) => thinking.push_str(&t),
            Progress::Output(o) => output.push_str(&o),
        });
        (result, thinking, output)
    }

    #[test]
    fn streams_anthropic_thinking_and_answer() {
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":20,\"output_tokens\":1}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Comparing the \"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"two versions.\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"x\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"{\\\"script\\\":\\\"x = {}\\\",\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"\\\"notes\\\":\\\"n\\\"}\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":12}}\n\n";
        let (result, thinking, output) = collect(|on| anthropic_stream(sse.as_bytes(), on));
        let merged = result.unwrap();
        assert_eq!((merged.script.as_str(), merged.notes.as_str()), ("x = {}", "n"));
        let usage = merged.usage.unwrap();
        assert_eq!((usage.input, usage.output), (30, 12));
        assert!((usage.cost.unwrap() - (30.0 * 4.0 + 12.0 * 20.0) / 1e6).abs() < 1e-12);
        assert_eq!(thinking, "Comparing the two versions.");
        assert!(output.starts_with("{\"script\""));

        let refusal = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\n";
        assert!(collect(|on| anthropic_stream(refusal.as_bytes(), on)).0.unwrap_err().contains("declined"));
        let error = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        assert!(collect(|on| anthropic_stream(error.as_bytes(), on)).0.unwrap_err().contains("Overloaded"));
    }

    #[test]
    fn streams_openai_reasoning_in_every_style() {
        let chunk = |delta: Value| format!("data: {}\n\n", json!({"choices": [{"delta": delta}]}));
        // OpenRouter's separate `reasoning` field.
        let usage = format!("data: {}\n\n", json!({"choices": [], "usage": {"prompt_tokens": 7, "completion_tokens": 3, "cost": 0.0001}}));
        let sse = [chunk(json!({"reasoning": "Hmm. "})), chunk(json!({"content": "{\"script\":\"a = {}\",\"notes\":\"\"}"})), usage, "data: [DONE]\n\n".into()].concat();
        let (result, thinking, _) = collect(|on| openai_stream(sse.as_bytes(), on));
        let merged = result.unwrap();
        assert_eq!((merged.script.as_str(), thinking.as_str()), ("a = {}", "Hmm. "));
        assert_eq!(merged.usage, Some(Usage { input: 7, output: 3, cost: Some(0.0001) }));
        // Inline <think> tags, split across chunks.
        let sse = [chunk(json!({"content": "<thi"})), chunk(json!({"content": "nk>Let me"})), chunk(json!({"content": " see</think>{\"script\":\"b = {}\","})), chunk(json!({"content": "\"notes\":\"\"}"}))].concat();
        let (result, thinking, output) = collect(|on| openai_stream(sse.as_bytes(), on));
        assert_eq!(result.unwrap().script, "b = {}");
        assert_eq!(thinking, "Let me see");
        assert!(output.starts_with('{'));
        // A server that ignores `stream`.
        let plain = json!({"choices": [{"finish_reason": "stop", "message": {"reasoning_content": "R", "content": "{\"script\":\"c = {}\",\"notes\":\"\"}"}}]}).to_string();
        let (result, thinking, _) = collect(|on| openai_stream(plain.as_bytes(), on));
        assert_eq!((result.unwrap().script, thinking.as_str()), ("c = {}".to_owned(), "R"));
    }

    #[test]
    fn parses_openai_replies() {
        let fenced = json!({"choices": [{"finish_reason": "stop", "message": {"content": "Here you go:\n```json\n{\"script\": \"x = {}\", \"notes\": \"n\"}\n```"}}]});
        let merged = parse_openai(200, &fenced).unwrap();
        assert_eq!((merged.script.as_str(), merged.notes.as_str(), merged.usage), ("x = {}", "n", None));
        assert!(parse_openai(200, &json!({"choices": [{"finish_reason": "length", "message": {"content": "{"}}]})).unwrap_err().contains("cut off"));
        assert!(parse_openai(401, &json!({"error": "No auth credentials found"})).unwrap_err().contains("No auth credentials"));
        assert!(parse_openai(200, &json!({"choices": [{"message": {"content": "sorry"}}]})).is_err());
    }

    #[test]
    fn estimates_tokens_and_reads_prices() {
        let a = Source { label: "A".into(), file: "a.txt".into(), text: "x = { a = yes }".repeat(100) };
        let e = estimate("common/buildings", "x", None, &[a.clone(), a]);
        assert!(e.input > 1000 && e.output_low > 500 && e.output_high > e.output_low);
        assert_eq!(price(&Provider::Anthropic { api_key: "k".into() }).unwrap().cost(1_000_000, 100_000), 6.0);
        let list = json!({"data": [{"id": "a/b", "pricing": {"prompt": "0.0000003", "completion": "0.0000012"}}, {"id": "free", "pricing": {}}]});
        let p = listed_price(&list, "a/b").unwrap();
        assert!((p.input - 0.3).abs() < 1e-9 && (p.output - 1.2).abs() < 1e-9);
        assert_eq!(listed_price(&list, "free"), None);
        assert_eq!(listed_price(&list, "missing"), None);
    }

    /// Sends one small merge with the saved provider and key. Costs real
    /// money, so it only runs on request:
    /// `cargo test live_merge -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_merge_with_saved_provider() {
        let path = crate::settings::path();
        let provider = crate::settings::Settings::load(&path).with_keyring(&path).ready_provider().expect("no AI provider set up");
        println!("model: {}", provider.model_name());
        println!("price: {:?}", price(&provider));
        let v = Source { label: "Vanilla".into(), file: "common/buildings/00_buildings.txt".into(), text: "building_lab = {\n\tcost = { minerals = 100 }\n\tupkeep = { energy = 2 }\n}".into() };
        let a = Source { label: "Cheap Labs".into(), file: "common/buildings/a.txt".into(), text: "building_lab = {\n\tcost = { minerals = 50 }\n\tupkeep = { energy = 2 }\n}".into() };
        let b = Source { label: "Lab Jobs".into(), file: "common/buildings/b.txt".into(), text: "building_lab = {\n\tcost = { minerals = 100 }\n\tupkeep = { energy = 2 }\n\tplanet_modifier = { job_researcher_add = 1 }\n}".into() };
        let e = estimate("common/buildings", "building_lab", Some(&v), &[a.clone(), b.clone()]);
        println!("estimate: {e:?}");
        let merged = merge(&provider, "common/buildings", "building_lab", Some(&v), &[a, b], &mut |_| {}).expect("merge failed");
        println!("script:\n{}\nnotes: {}\nusage: {:?}", merged.script, merged.notes, merged.usage);
        assert!(merged.script.contains("minerals = 50") && merged.script.contains("job_researcher_add"));
    }

    #[test]
    fn provider_settings_round_trip() {
        let p = Provider::OpenAiCompatible { base_url: OPENROUTER_URL.into(), model: "some/model".into(), api_key: "k".into() };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"kind\":\"open_ai_compatible\""));
        assert_eq!(serde_json::from_str::<Provider>(&json).unwrap(), p);
    }
}
