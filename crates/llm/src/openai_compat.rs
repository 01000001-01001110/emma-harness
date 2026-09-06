//! The OpenAI chat-completions wire format, once, for every host that speaks it.
//!
//! # Why one file and not two
//!
//! OpenRouter's API is `POST {base}/chat/completions` with a bearer token, an
//! OpenAI-shaped request body and an OpenAI-shaped SSE stream. So is OpenAI's.
//! The differences between them are four values, not four behaviours: the base
//! URL, the environment variable that overrides the stored key, the default
//! model, and the spelling of the output cap (`max_tokens` against
//! `max_completion_tokens`, which the newer OpenAI reasoning models require and
//! OpenRouter does not accept). Those four live in [`Wire`], which is a const
//! per host; everything below it is shared.
//!
//! Two copies of an SSE assembler that must agree on tool-call indexing is the
//! failure this avoids. `ollama.rs` is a separate file for the opposite reason:
//! its `/api/chat` differs in the parts an agent loop depends on, and no amount
//! of parameterising a base URL reaches that.
//!
//! # No hardcoded model roster
//!
//! The model id is passed through verbatim. OpenRouter carries hundreds of
//! models, renames them, and publishes stealth models under nicknames that
//! change without notice; a list compiled here would be wrong within the week
//! and would refuse a model that works. `models::limits` is deliberately not
//! consulted either: that table is Anthropic ceilings, and applying it to a
//! model it has never heard of would clamp a request for a reason that does not
//! exist here.
//!
//! # What this deliberately does not do
//!
//! **No cache breakpoints.** `cache_control` is an Anthropic construct. Both
//! hosts here cache automatically or not at all, so [`Caching`](crate::Caching)
//! is ignored rather than faked, and nothing writes a marker that would be a
//! 400 on arrival.
//!
//! **No effort levels.** `reasoning_effort` exists on some models and is a 400
//! on others, and this client has no way to tell which model a passthrough id
//! names. `Effort` is dropped rather than guessed at, exactly as `ollama.rs`
//! drops it. That is a real capability gap and it is stated here rather than
//! papered over with a value that happens to work on the models we tried.
//!
//! # Usage, mapped honestly
//!
//! `Usage::input_tokens` in this crate means the *uncached remainder*, and
//! OpenAI's `prompt_tokens` means the whole prompt including whatever was
//! served from cache. Copying one into the other would over-count every cached
//! call by the size of the cache hit. So `cache_read_input_tokens` takes
//! `prompt_tokens_details.cached_tokens` and `input_tokens` takes the
//! difference, which makes `billable_input_tokens()` equal `prompt_tokens`,
//! which is what the bill says. `cache_creation_input_tokens` stays zero
//! because neither host reports a cache *write* and inventing one would corrupt
//! any cost figure folded from it. `context_window` stays zero: no host here
//! says what window the call ran under.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::retry::{retry_after_seconds, Retry};
use crate::{
    redact, trim_body, ApiKey, AssistantTurn, Content, ContentBlock, Event, LlmError, Message,
    Mode, Provider, ProviderKind, Request, Role, TextBlock, ThinkingBlock, ToolCall, Usage,
};

/// Everything that differs between two hosts speaking the same wire format.
///
/// A struct of values rather than a trait, because none of these is a
/// behaviour: a trait here would be five methods that all return a constant and
/// one implementation per host to keep in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    /// The registry name, as `emma set-provider` takes it.
    pub name: &'static str,
    /// The chat-completions endpoint, in full.
    pub base_url: &'static str,
    /// The environment variable that outranks the stored key.
    pub env_var: &'static str,
    /// What runs when the caller names no model. A real id, not a family.
    pub default_model: &'static str,
    /// `max_tokens` or `max_completion_tokens`. The newer OpenAI reasoning
    /// models reject the first outright; OpenRouter does not implement the
    /// second. There is no spelling that works on both, which is why this is a
    /// field.
    pub max_tokens_field: &'static str,
    /// The spelling of the sampling temperature, or `None` for a host that
    /// will not take one at all.
    ///
    /// A field for the same reason `max_tokens_field` is one: the newer OpenAI
    /// reasoning models accept only their own fixed temperature and answer a
    /// 400 to any other value, so on that host there is no number that is safe
    /// to send and the knob is dropped rather than clamped. OpenRouter carries
    /// models that do take it, so it keeps the parameter.
    pub temperature_field: Option<&'static str>,
}

/// OpenRouter: one key, every model it carries, by id.
pub const OPENROUTER: Wire = Wire {
    name: "openrouter",
    base_url: "https://openrouter.ai/api/v1/chat/completions",
    env_var: "OPENROUTER_API_KEY",
    // A concrete id rather than a family alias, because a family alias moves
    // under us and the run that changed would look like a run that did not.
    default_model: "anthropic/claude-opus-4.1",
    max_tokens_field: "max_tokens",
    temperature_field: Some("temperature"),
};

/// OpenAI directly, for a key that is already scoped to them.
pub const OPENAI: Wire = Wire {
    name: "openai",
    base_url: "https://api.openai.com/v1/chat/completions",
    env_var: "OPENAI_API_KEY",
    default_model: "gpt-5",
    max_tokens_field: "max_completion_tokens",
    temperature_field: None,
};

/// Long enough for a reasoning model to think before the first byte in batch
/// mode, and still bounded so a wedged connection cannot hold the loop.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

// region: Rendering a request
// ---------------------------------------------------------------------------
// Rendering a request
//
// Emma's four-part `Request` into the one JSON body this format takes. The one
// shape that does not survive a one-to-one mapping is a tool result, which
// becomes its own message rather than a block inside a turn.
// ---------------------------------------------------------------------------

/// Emma's tool definitions (rendered once, in Anthropic's shape) into this one.
///
/// Only the envelope moves: the schema is JSON Schema on both sides.
fn tools_to_openai(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let parameters = t
                .get("input_schema")
                .or_else(|| t.get("parameters"))
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            json!({
                "type": "function",
                "function": {
                    "name": t.get("name").cloned().unwrap_or(Value::Null),
                    "description": t.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": parameters,
                }
            })
        })
        .collect()
}

/// Emma's messages into this format's.
///
/// ⚠ A TOOL RESULT BECOMES ITS OWN MESSAGE. In Anthropic's shape a result is a
/// `tool_result` block inside a user turn; here it is a `role: "tool"` message
/// whose `content` is a plain string, emitted in place so that it still follows
/// the call it answers.
///
/// **Pictures are not carried, because this crate has no shape that holds
/// one.** `ToolResult` has no `images` field in this tree yet — it arrives with
/// `content::ToolImage`, which is a later package — so there is nothing here to
/// drop and nothing to send. When it lands, the picture cannot ride inside the
/// tool message either: this API rejects content parts on that role, so an
/// `image_url` part hung there is a 400. It has to follow as a user message
/// carrying the part, named, which is the compromise `ollama.rs` makes for the
/// same lack of a shape that could carry it. Stated here rather than left as an
/// absence, because an image quietly missing from a request is the failure that
/// looks like the model ignoring it.
fn messages_to_openai(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        match &m.content {
            Content::Text(t) => out.push(json!({"role": role, "content": t})),
            Content::Blocks(blocks) => {
                let mut text = String::new();
                let mut calls = Vec::new();
                for b in blocks {
                    match b {
                        ContentBlock::Text(t) => text.push_str(&t.text),
                        // Not echoed back. There is no signed-thinking contract
                        // to satisfy on this format, and replaying a previous
                        // turn's reasoning as input has no defined meaning.
                        ContentBlock::Thinking(_) | ContentBlock::RedactedThinking(_) => {}
                        ContentBlock::ToolUse(c) => {
                            // ⚠ `arguments` IS A STRING HERE. Nesting the
                            // object is accepted by some gateways and rejected
                            // by the real API, which is the worst kind of
                            // difference: it works until it is pointed at
                            // production.
                            calls.push(json!({
                                "id": c.id,
                                "type": "function",
                                "function": {
                                    "name": c.name,
                                    "arguments": c.input.to_string(),
                                }
                            }));
                        }
                        ContentBlock::ToolResult(r) => {
                            // Emitted immediately so ordering survives: a
                            // result must follow the call it answers.
                            out.push(json!({
                                "role": "tool",
                                "tool_call_id": r.tool_use_id,
                                "content": r.content,
                            }));
                        }
                        // A block this client could not model. It was kept
                        // whole for the provider that produced it, and this is
                        // not that provider, so there is nothing honest to send.
                        ContentBlock::Passthrough(_) => {}
                    }
                }
                if !text.is_empty() || !calls.is_empty() {
                    let mut msg = Map::new();
                    msg.insert("role".into(), json!(role));
                    msg.insert("content".into(), json!(text));
                    if !calls.is_empty() {
                        msg.insert("tool_calls".into(), Value::Array(calls));
                    }
                    out.push(Value::Object(msg));
                }
            }
        }
    }
    out
}

// endregion: Rendering a request

// region: Reading a reply
// ---------------------------------------------------------------------------
// Reading a reply
//
// One decoder for the batch body and one assembler for the stream, meeting at
// `turn_from_parts` so the two cannot drift apart about what a turn contains.
// ---------------------------------------------------------------------------

/// A tool call whose arguments did not parse is a fault, not an empty call.
///
/// The empty-string case is not that case: a call to a tool that takes no
/// arguments legitimately carries `""`. Truncation leaves a fragment behind,
/// and a fragment does not parse.
fn parse_arguments(raw: &str) -> Result<Value, LlmError> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(raw)
        .map_err(|e| LlmError::Protocol(format!("tool arguments were not valid JSON: {e}")))
}

/// The pieces of a turn, however they arrived.
#[derive(Default)]
struct Parts {
    thinking: String,
    text: String,
    /// Keyed by the index this format assigns each call, so a streamed turn is
    /// rebuilt in the order it was sent regardless of frame arrival.
    calls: BTreeMap<usize, PartialCall>,
    finish_reason: String,
    usage: Usage,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// ⚠ `finish_reason` IS NOT `stop_reason`. Emma branches on `"tool_use"` to
/// decide whether the loop continues, and a host that emitted tool calls while
/// reporting `"stop"` would end the goal mid-plan. The content decides, and the
/// reported reason is only consulted when there are no calls.
fn stop_reason_of(finish: &str, has_calls: bool) -> String {
    if has_calls {
        return "tool_use".to_string();
    }
    match finish {
        "length" => "max_tokens".to_string(),
        "tool_calls" => "tool_use".to_string(),
        // An empty reason means the host never said, which is not the same as
        // "it ended normally"; both are reported as an ordinary end because
        // nothing downstream can act on the difference.
        "" | "stop" => "end_turn".to_string(),
        other => other.to_string(),
    }
}

impl Parts {
    fn finish(self) -> Result<AssistantTurn, LlmError> {
        let mut content = Vec::new();
        if !self.thinking.is_empty() {
            content.push(ContentBlock::Thinking(ThinkingBlock {
                thinking: self.thinking,
                // No signature: nothing on this format signs reasoning, and a
                // fabricated one would be echoed back as a claim we cannot make.
                signature: None,
                extra: Map::new(),
            }));
        }
        if !self.text.is_empty() {
            content.push(ContentBlock::Text(TextBlock {
                text: self.text,
                extra: Map::new(),
            }));
        }
        let has_calls = !self.calls.is_empty();
        for (index, call) in self.calls {
            content.push(ContentBlock::ToolUse(ToolCall {
                // ⚠ AN ID IS SYNTHESISED WHEN THE HOST OMITS ONE. Emma pairs a
                // result to its call by id, and some gateways send tool calls
                // with no id at all. Stable within a turn, which is the only
                // place the pairing is read.
                id: if call.id.is_empty() {
                    format!("call-{index}-{}", call.name)
                } else {
                    call.id
                },
                input: parse_arguments(&call.arguments)?,
                name: call.name,
                extra: Map::new(),
            }));
        }
        Ok(AssistantTurn {
            stop_reason: stop_reason_of(&self.finish_reason, has_calls),
            content,
            usage: self.usage,
        })
    }
}

/// `usage`, mapped so `billable_input_tokens()` equals what the host billed.
/// See the module doc for why `input_tokens` is not `prompt_tokens`.
fn usage_from(usage: &Value) -> Usage {
    let prompt = usage
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cached = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        // A host that reports more cached than prompt tokens is reporting
        // something this mapping cannot represent; clamping keeps the
        // uncached remainder from going negative and lying in the other
        // direction.
        .min(prompt);
    Usage {
        input_tokens: prompt - cached,
        output_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        // Neither host reports a cache write. Zero means "nothing written",
        // and here it also means "never reported"; inventing a number would
        // corrupt any cost figure computed from it.
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cached,
        // Neither host runs a server-side tool on Emma's behalf on this
        // endpoint — a `:online` OpenRouter id searches, but nothing in the
        // `usage` object counts it — so zero is the truth rather than a
        // placeholder. `ProviderKind::web_search` answers false for both for
        // the same reason.
        server_tool_use: Default::default(),
        // No host here says what window the call ran under.
        context_window: 0,
    }
}

/// Reasoning text, under either of the two names hosts use for it.
///
/// OpenRouter normalises to `reasoning`; several upstreams pass
/// `reasoning_content` through unchanged. Both are the same thing and reading
/// only one drops a whole thinking block on half the fleet.
fn reasoning_of(v: &Value) -> Option<&str> {
    v.get("reasoning")
        .or_else(|| v.get("reasoning_content"))
        .and_then(Value::as_str)
}

/// Batch mode: the whole completion arrived at once.
fn turn_from_completion(body: &Value) -> Result<AssistantTurn, LlmError> {
    // An error body with a 200 status is a real shape on gateway hosts, and
    // reading it as a completion produces an empty turn the loop treats as the
    // model having nothing to say.
    if let Some(message) = body.pointer("/error/message").and_then(Value::as_str) {
        return Err(LlmError::Api {
            status: 200,
            message: message.to_string(),
        });
    }
    let choice = body
        .pointer("/choices/0")
        .ok_or_else(|| LlmError::Protocol("the response carried no choices".into()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| LlmError::Protocol("the first choice carried no message".into()))?;

    let mut parts = Parts {
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        ..Parts::default()
    };
    if let Some(r) = reasoning_of(message) {
        parts.thinking.push_str(r);
    }
    if let Some(t) = message.get("content").and_then(Value::as_str) {
        parts.text.push_str(t);
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (i, call) in calls.iter().enumerate() {
            let f = call.get("function").unwrap_or(call);
            parts.calls.insert(
                call.get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(i as u64) as usize,
                PartialCall {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: f
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    // ⚠ A STRING, NOT AN OBJECT, per the format. A gateway that
                    // nests the object anyway is accepted rather than refused:
                    // the intent is unambiguous and the alternative is a
                    // correct call rejected for its envelope.
                    arguments: match f.get("arguments") {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    },
                },
            );
        }
    }
    if let Some(u) = body.get("usage") {
        parts.usage = usage_from(u);
    }
    parts.finish()
}

/// Streaming: one `data:` frame, folded into the turn under assembly.
///
/// Every field is additive, because a chunk carries fragments and never a
/// replacement: text and reasoning concatenate, and a tool call's `arguments`
/// concatenate under the index the host assigned it. Overwriting instead of
/// appending is how a streamed tool call arrives as its last fragment only.
async fn apply_chunk(
    parts: &mut Parts,
    chunk: &Value,
    events: Option<&mpsc::Sender<Event>>,
) -> Result<(), LlmError> {
    if let Some(message) = chunk.pointer("/error/message").and_then(Value::as_str) {
        return Err(LlmError::Api {
            status: 200,
            message: message.to_string(),
        });
    }
    if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
        parts.usage = usage_from(u);
    }
    let Some(choice) = chunk.pointer("/choices/0") else {
        return Ok(());
    };
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        parts.finish_reason = reason.to_string();
    }
    let Some(delta) = choice.get("delta") else {
        return Ok(());
    };
    if let Some(r) = reasoning_of(delta) {
        parts.thinking.push_str(r);
    }
    if let Some(t) = delta.get("content").and_then(Value::as_str) {
        if !t.is_empty() {
            parts.text.push_str(t);
            notify(events, Event::TextDelta(t.to_string())).await;
        }
    }
    for call in delta
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let entry = parts.calls.entry(index).or_default();
        let fresh = entry.name.is_empty();
        if let Some(id) = call.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                entry.id = id.to_string();
            }
        }
        if let Some(f) = call.get("function") {
            if let Some(name) = f.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    entry.name.push_str(name);
                }
            }
            if let Some(args) = f.get("arguments").and_then(Value::as_str) {
                entry.arguments.push_str(args);
            }
        }
        if fresh && !entry.name.is_empty() {
            notify(
                events,
                Event::ToolUseStarted {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                },
            )
            .await;
        }
    }
    Ok(())
}

async fn notify(events: Option<&mpsc::Sender<Event>>, event: Event) {
    if let Some(tx) = events {
        // Best effort: a closed receiver means the turn is being cancelled,
        // which the caller already knows about.
        let _ = tx.send(event).await;
    }
}

// endregion: Reading a reply

// region: The provider
// ---------------------------------------------------------------------------
// The provider
//
// The key, the model, the host's four values, and the path a call takes: render
// the body, send it, classify a non-success status, decide whether to send it
// again.
// ---------------------------------------------------------------------------

pub struct OpenAiCompatProvider {
    http: reqwest::Client,
    key: ApiKey,
    model: String,
    base_url: String,
    wire: Wire,
    retry: Retry,
}

// Hand-written so no future field can print the key by being added.
impl std::fmt::Debug for OpenAiCompatProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatProvider")
            .field("provider", &self.wire.name)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatProvider {
    pub fn new(wire: Wire, key: ApiKey, model: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("reqwest client"),
            key,
            model: model.unwrap_or_else(|| wire.default_model.to_string()),
            // Deliberately not read from the environment, for the reason
            // `anthropic.rs` gives: an env var that redirects where the key is
            // sent is a credential-exfiltration switch, and Emma runs inside
            // repositories with whatever env the shell carries. A custom
            // endpoint is a settings-file decision, not an ambient one, and
            // until that shape exists this is explicit or nothing.
            base_url: wire.base_url.to_string(),
            wire,
            retry: Retry::default(),
        }
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    /// Point at something other than the host's own API: a corporate gateway,
    /// or a stub in a test. Explicit by construction, never ambient.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn scrub(&self, text: impl AsRef<str>) -> String {
        redact(text.as_ref(), self.key.expose())
    }

    fn render(&self, req: &Request, mode: Mode) -> Value {
        let mut messages = Vec::new();
        if !req.instructions.is_empty() {
            messages.push(json!({"role": "system", "content": req.instructions}));
        }
        messages.extend(messages_to_openai(&req.history));
        messages.extend(messages_to_openai(&req.query));

        let mut body = json!({
            "model": self.model,
            "messages": messages,
        });
        // Inserted rather than written into the literal because the *key* is
        // the thing that varies between hosts; see `Wire::max_tokens_field`.
        body[self.wire.max_tokens_field] = json!(req.max_tokens);
        // Two conditions, and both must hold: the caller asked for a
        // temperature, and this host takes one. Neither is a clamp, because a
        // host that rejects the parameter rejects every value of it.
        if let (Some(field), Some(temperature)) = (self.wire.temperature_field, req.temperature) {
            body[field] = json!(temperature);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(tools_to_openai(&req.tools));
        }
        if mode == Mode::Stream {
            body["stream"] = json!(true);
            // Without this the final chunk carries no `usage` and every
            // streamed turn is recorded as having cost nothing, which is worse
            // than an approximate number because it silently loosens every cap
            // folded from it.
            body["stream_options"] = json!({"include_usage": true});
        }
        body
    }

    async fn classify(&self, resp: reqwest::Response) -> LlmError {
        let status = resp.status();
        let retry_after = retry_after_seconds(
            resp.headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
        );
        let body = resp.text().await.unwrap_or_default();
        let message = self.scrub(trim_body(&api_message(&body)));

        match status.as_u16() {
            401 => LlmError::Unauthorized {
                fix: self.key_fix(),
                message,
            },
            403 => LlmError::Forbidden { message },
            400 | 404 | 413 | 422 => LlmError::BadRequest { message },
            429 => LlmError::RateLimited {
                retry_after,
                retry_hint: match retry_after {
                    Some(d) => format!("; the API asked for {}s", d.as_secs()),
                    None => "; no retry-after was given".to_string(),
                },
                message,
            },
            code if code >= 500 => LlmError::Unavailable {
                status: code,
                message,
            },
            code => LlmError::Api {
                status: code,
                message,
            },
        }
    }

    /// The sentence a 401 carries: this provider's variable and this provider's
    /// command, never Anthropic's. A message naming the wrong environment
    /// variable sends the user to check a key that was never involved.
    fn key_fix(&self) -> String {
        format!(
            "Check {}, or run `emma set-provider {}` to store a working one.",
            self.wire.env_var, self.wire.name
        )
    }

    async fn execute(
        &self,
        body: Value,
        mode: Mode,
        events: Option<&mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        let mut attempt: u32 = 1;
        loop {
            let sent = self
                .http
                .post(&self.base_url)
                .header("authorization", format!("Bearer {}", self.key.expose()))
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await;

            let failure = match sent {
                Ok(resp) if resp.status().is_success() => {
                    // Past this point bytes may already have reached the user,
                    // so a mid-stream failure is surfaced rather than retried:
                    // a retry would replay text the terminal has printed.
                    return match mode {
                        Mode::Batch => self.read_batch(resp).await,
                        Mode::Stream => self.read_stream(resp, events).await,
                    };
                }
                Ok(resp) => self.classify(resp).await,
                Err(e) => LlmError::Transport(self.scrub(e.to_string())),
            };

            let Some(delay) = self.retry.delay_for(&failure, attempt) else {
                return Err(failure);
            };
            notify(
                events,
                Event::Retrying {
                    attempt,
                    max_attempts: self.retry.max_attempts,
                    delay,
                    reason: failure.to_string(),
                },
            )
            .await;
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    async fn read_batch(&self, resp: reqwest::Response) -> Result<AssistantTurn, LlmError> {
        let raw = resp
            .text()
            .await
            .map_err(|e| LlmError::Transport(self.scrub(e.to_string())))?;
        let body: Value = serde_json::from_str(&raw)
            .map_err(|e| LlmError::Protocol(self.scrub(format!("{e}: {}", trim_body(&raw)))))?;
        turn_from_completion(&body)
    }

    async fn read_stream(
        &self,
        resp: reqwest::Response,
        events: Option<&mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut parts = Parts::default();

        while let Some(item) = stream.next().await {
            let bytes = item.map_err(|e| LlmError::Transport(self.scrub(e.to_string())))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(idx) = buf.find("\n\n") {
                let frame = buf[..idx].to_string();
                buf.drain(..idx + 2);
                let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) else {
                    continue;
                };
                // The terminator is not JSON and never was; parsing it would
                // fail on every stream this format produces.
                if data.trim() == "[DONE]" {
                    continue;
                }
                let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                apply_chunk(&mut parts, &chunk, events).await?;
            }
        }
        parts.finish()
    }
}

/// The host's own error prose, or the whole body when it is not shaped the way
/// the format documents. "request failed" is never enough to act on.
fn api_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string())
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn send(
        &self,
        request: Request,
        mode: Mode,
        events: Option<mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        let body = self.render(&request, mode);
        self.execute(body, mode, events.as_ref()).await
    }
}

// endregion: The provider

// region: Identity, for the registry
// ---------------------------------------------------------------------------
// Identity, for the registry
//
// One `ProviderKind` per host, both built from the same `Wire`, so adding a
// third host that speaks this format is a const and two lines.
// ---------------------------------------------------------------------------

/// A registry entry for one host speaking this wire format.
pub struct OpenAiCompatKind(pub Wire);

impl ProviderKind for OpenAiCompatKind {
    fn name(&self) -> &'static str {
        self.0.name
    }

    fn env_var(&self) -> &'static str {
        self.0.env_var
    }

    fn default_model(&self) -> &'static str {
        self.0.default_model
    }

    fn build(&self, key: ApiKey, model: Option<String>) -> Arc<dyn Provider> {
        Arc::new(OpenAiCompatProvider::new(self.0, key, model))
    }
}

pub static OPENROUTER_KIND: OpenAiCompatKind = OpenAiCompatKind(OPENROUTER);
pub static OPENAI_KIND: OpenAiCompatKind = OpenAiCompatKind(OPENAI);

// endregion: Identity, for the registry

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Every one drives the shared loopback stub in `crate::stub` against a scripted
// reply. Nothing here reaches a real host: the claims worth pinning are about
// the bytes this file writes and reads, and a live endpoint would test
// OpenRouter's uptime instead. The key in every fixture is deliberately not
// key-shaped for either host, so nothing in this file could be mistaken for a
// credential by a scanner or by a reader.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::{stub, Reply, Stub};
    use crate::{Caching, Effort, ToolResult};

    const TEST_KEY: &str = "not-a-key-loopback-fixture";

    fn provider(s: &Stub, wire: Wire) -> OpenAiCompatProvider {
        OpenAiCompatProvider::new(wire, ApiKey::new(TEST_KEY), None)
            .with_base_url(s.url.clone())
            // Backoff is exercised by `retry`'s own unit tests; here it would
            // only make the suite slow.
            .with_retry(Retry {
                max_attempts: 3,
                base: Duration::from_millis(1),
                cap: Duration::from_millis(5),
            })
    }

    fn request() -> Request {
        Request::new("You are Emma.", vec![json!({"name": "Read"})])
    }

    async fn run(
        s: &Stub,
        req: Request,
        mode: Mode,
    ) -> (Result<AssistantTurn, LlmError>, Vec<Event>) {
        let (tx, mut rx) = mpsc::channel::<Event>(256);
        let out = provider(s, OPENROUTER).send(req, mode, Some(tx)).await;
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        (out, events)
    }

    /// One assistant turn: reasoning, text, and two tool calls. The batch and
    /// SSE fixtures are the same turn in the two wire forms, which is what
    /// makes comparing their parsed results mean anything.
    fn batch_body() -> String {
        json!({
            "id": "gen-1",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "reasoning": "check both files",
                    "content": "Reading them now.",
                    "tool_calls": [
                        {"index": 0, "id": "call_01", "type": "function",
                         "function": {"name": "Read", "arguments": "{\"path\":\"a.rs\"}"}},
                        {"index": 1, "id": "call_02", "type": "function",
                         "function": {"name": "Read", "arguments": "{\"path\":\"b.rs\"}"}}
                    ]
                }
            }],
            "usage": {
                "prompt_tokens": 32_241,
                "completion_tokens": 17,
                "prompt_tokens_details": {"cached_tokens": 29_000}
            }
        })
        .to_string()
    }

    fn sse_body() -> String {
        [
            r#"{"choices":[{"delta":{"role":"assistant","reasoning":"check both "}}]}"#,
            r#"{"choices":[{"delta":{"reasoning":"files"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Reading "}}]}"#,
            r#"{"choices":[{"delta":{"content":"them now."}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_01","function":{"name":"Read","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_02","function":{"name":"Read","arguments":"{\"path\":\"b.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":32241,"completion_tokens":17,"prompt_tokens_details":{"cached_tokens":29000}}}"#,
            "[DONE]",
        ]
        .iter()
        .map(|d| format!("data: {d}\n\n"))
        .collect()
    }

    // -- the wire, out ------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn the_model_id_is_passed_through_verbatim_however_odd_it_looks() {
        // The whole point of having no roster: a stealth model published under
        // a nickname must reach the host unedited, because a list compiled here
        // could only refuse it.
        let s = stub(vec![Reply::json(batch_body())]).await;
        let p = OpenAiCompatProvider::new(
            OPENROUTER,
            ApiKey::new(TEST_KEY),
            Some("openrouter/sonoma-sky-alpha".into()),
        )
        .with_base_url(s.url.clone());
        assert_eq!(p.model_id(), "openrouter/sonoma-sky-alpha");
        p.send(request(), Mode::Batch, None).await.unwrap();
        assert_eq!(s.last()["model"], "openrouter/sonoma-sky-alpha");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_set_temperature_reaches_a_host_that_takes_one() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let mut req = request();
        req.temperature = Some(0.9);
        provider(&s, OPENROUTER)
            .send(req, Mode::Batch, None)
            .await
            .unwrap();
        assert_eq!(s.last()["temperature"], 0.9);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_set_temperature_is_dropped_for_a_host_that_rejects_the_parameter() {
        // Not clamped to the value OpenAI's reasoning models accept, because a
        // clamp would report a number the caller did not ask for as though it
        // had been honoured. The knob simply does not exist on that host.
        let s = stub(vec![Reply::json(batch_body())]).await;
        let mut req = request();
        req.temperature = Some(0.9);
        provider(&s, OPENAI)
            .send(req, Mode::Batch, None)
            .await
            .unwrap();
        let sent = s.last();
        assert!(sent.get("temperature").is_none(), "{sent}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unset_temperature_is_absent_from_both_hosts() {
        for wire in [OPENROUTER, OPENAI] {
            let s = stub(vec![Reply::json(batch_body())]).await;
            provider(&s, wire)
                .send(request(), Mode::Batch, None)
                .await
                .unwrap();
            let sent = s.last();
            assert!(sent.get("temperature").is_none(), "{} {sent}", wire.name);
            assert_ne!(sent["temperature"], 0.0, "{}", wire.name);
            // The cap still goes out under this host's own spelling: it has no
            // absent case to be honest about.
            assert_eq!(sent[wire.max_tokens_field], 32_000, "{}", wire.name);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_two_hosts_differ_only_in_the_output_cap_spelling() {
        // The one wire difference between them, pinned in both directions:
        // OpenAI's newer reasoning models reject `max_tokens` and OpenRouter
        // does not implement `max_completion_tokens`.
        for (wire, expected, absent) in [
            (OPENROUTER, "max_tokens", "max_completion_tokens"),
            (OPENAI, "max_completion_tokens", "max_tokens"),
        ] {
            let s = stub(vec![Reply::json(batch_body())]).await;
            provider(&s, wire)
                .send(request(), Mode::Batch, None)
                .await
                .unwrap();
            let sent = s.last();
            assert_eq!(sent[expected], 32_000, "{}", wire.name);
            assert!(sent.get(absent).is_none(), "{}: {sent}", wire.name);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn instructions_lead_the_messages_and_tools_use_the_function_envelope() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let mut req = Request::new(
            "You are Emma.",
            vec![json!({
                "name": "Read",
                "description": "Read a file",
                "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}
            })],
        );
        req.query = vec![Message::user("hello")];
        provider(&s, OPENROUTER)
            .send(req, Mode::Batch, None)
            .await
            .unwrap();

        let sent = s.last();
        assert_eq!(sent["messages"][0]["role"], "system");
        assert_eq!(sent["messages"][0]["content"], "You are Emma.");
        assert_eq!(sent["messages"][1]["role"], "user");
        assert_eq!(sent["tools"][0]["type"], "function");
        assert_eq!(sent["tools"][0]["function"]["name"], "Read");
        assert_eq!(
            sent["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        // `input_schema` is Anthropic's spelling and is a 400 here.
        assert!(sent["tools"][0]["function"].get("input_schema").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_anthropic_specific_reaches_the_wire() {
        // Caching is on by default in a `Request`, and a `cache_control` marker
        // is a 400 on both hosts. Effort is dropped for the reason in the module
        // doc: there is no value that is safe on an arbitrary passthrough id.
        let s = stub(vec![Reply::json(batch_body())]).await;
        let mut req = request();
        req.caching = Caching::On;
        req.effort = Effort::XHigh;
        provider(&s, OPENROUTER)
            .send(req, Mode::Batch, None)
            .await
            .unwrap();
        let raw = s.last().to_string();
        assert!(!raw.contains("cache_control"), "{raw}");
        assert!(!raw.contains("reasoning_effort"), "{raw}");
        assert!(!raw.contains("output_config"), "{raw}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_tool_result_becomes_its_own_message_and_arguments_are_a_string() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let mut req = request();
        req.query = vec![Message {
            role: Role::Assistant,
            content: Content::Blocks(vec![
                ContentBlock::ToolUse(ToolCall {
                    id: "call_01".into(),
                    name: "Read".into(),
                    input: json!({"path": "a.rs"}),
                    extra: Map::new(),
                }),
                ContentBlock::ToolResult(ToolResult::ok("call_01", "file contents")),
            ]),
        }];
        provider(&s, OPENROUTER)
            .send(req, Mode::Batch, None)
            .await
            .unwrap();

        let msgs = s.last()["messages"].clone();
        let tool = msgs
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool")
            .expect("a tool message");
        assert_eq!(tool["tool_call_id"], "call_01");
        assert_eq!(tool["content"], "file contents");

        let assistant = msgs
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("an assistant message");
        // The defect this pins: nesting the object is accepted by some gateways
        // and rejected by the real API, so it works until it is pointed at
        // production.
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            json!("{\"path\":\"a.rs\"}")
        );
    }

    // -- the wire, back -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn streaming_assembles_the_same_turn_as_batch() {
        let b = stub(vec![Reply::json(batch_body())]).await;
        let (batch, _) = run(&b, request(), Mode::Batch).await;
        let s = stub(vec![Reply::sse(sse_body())]).await;
        let (streamed, events) = run(&s, request(), Mode::Stream).await;

        let (batch, streamed) = (batch.unwrap(), streamed.unwrap());
        assert_eq!(batch.content, streamed.content);
        assert_eq!(batch.stop_reason, streamed.stop_reason);
        assert_eq!(batch.usage, streamed.usage);

        assert_eq!(batch.text(), "Reading them now.");
        assert_eq!(batch.tool_calls().len(), 2);
        assert_eq!(batch.tool_calls()[0].input, json!({"path": "a.rs"}));
        assert_eq!(batch.tool_calls()[1].id, "call_02");

        // Streaming is only worth having if the deltas arrive.
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                Event::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Reading them now.");
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::ToolUseStarted { name, .. } if name == "Read")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streaming_asks_for_usage_or_every_streamed_turn_costs_nothing() {
        let s = stub(vec![Reply::sse(sse_body())]).await;
        run(&s, request(), Mode::Stream).await.0.unwrap();
        assert_eq!(s.last()["stream"], true);
        assert_eq!(s.last()["stream_options"]["include_usage"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn usage_maps_so_the_billable_input_is_what_the_host_billed() {
        // `input_tokens` here means the uncached remainder and `prompt_tokens`
        // there means the whole prompt. Copying one into the other over-counts
        // every cached call by the size of the hit.
        let s = stub(vec![Reply::json(batch_body())]).await;
        let usage = run(&s, request(), Mode::Batch).await.0.unwrap().usage;
        assert_eq!(usage.cache_read_input_tokens, 29_000);
        assert_eq!(usage.input_tokens, 32_241 - 29_000);
        assert_eq!(usage.billable_input_tokens(), 32_241);
        assert_eq!(usage.output_tokens, 17);
        // Not reported by either host, and therefore not invented.
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.context_window, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_that_reports_no_cache_detail_still_parses() {
        let s = stub(vec![Reply::json(
            json!({"choices": [{"finish_reason": "stop",
                                "message": {"content": "hi"}}],
                   "usage": {"prompt_tokens": 10, "completion_tokens": 2}})
            .to_string(),
        )])
        .await;
        let turn = run(&s, request(), Mode::Batch).await.0.unwrap();
        assert_eq!(turn.usage.input_tokens, 10);
        assert_eq!(turn.usage.cache_read_input_tokens, 0);
        assert_eq!(turn.stop_reason, "end_turn");
        assert_eq!(turn.text(), "hi");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_calls_reported_as_a_plain_stop_are_still_a_tool_turn() {
        // Emma branches on "tool_use" to decide whether the loop continues, so
        // a host that emits calls while saying "stop" would end the goal
        // mid-plan. The content decides.
        let s = stub(vec![Reply::json(
            json!({"choices": [{"finish_reason": "stop", "message": {
                "content": "",
                "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                                "function": {"name": "Read", "arguments": ""}}]}}]})
            .to_string(),
        )])
        .await;
        let turn = run(&s, request(), Mode::Batch).await.0.unwrap();
        assert_eq!(turn.stop_reason, "tool_use");
        // No arguments at all is a call to a tool that takes none, not a
        // truncated one.
        assert_eq!(turn.tool_calls()[0].input, json!({}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_truncated_tool_argument_is_a_fault_not_an_empty_call() {
        // Invoking a tool with `{}` because the JSON was cut off would run the
        // wrong action with default arguments.
        let s = stub(vec![Reply::json(
            json!({"choices": [{"finish_reason": "tool_calls", "message": {
                "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                                "function": {"name": "Read",
                                             "arguments": "{\"path\":\"a.r"}}]}}]})
            .to_string(),
        )])
        .await;
        let err = run(&s, request(), Mode::Batch).await.0.unwrap_err();
        assert!(matches!(err, LlmError::Protocol(_)), "{err:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_gateway_that_nests_the_arguments_object_is_still_understood() {
        // Not the documented shape, and unambiguous. Refusing it would fail a
        // correct call for its envelope.
        let s = stub(vec![Reply::json(
            json!({"choices": [{"finish_reason": "tool_calls", "message": {
                "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                                "function": {"name": "Read",
                                             "arguments": {"path": "a.rs"}}}]}}]})
            .to_string(),
        )])
        .await;
        let turn = run(&s, request(), Mode::Batch).await.0.unwrap();
        assert_eq!(turn.tool_calls()[0].input, json!({"path": "a.rs"}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_tool_call_with_no_id_gets_a_synthesised_one() {
        // Emma pairs a result to its call by id, and some gateways send none.
        let s = stub(vec![Reply::json(
            json!({"choices": [{"finish_reason": "tool_calls", "message": {
                "tool_calls": [{"index": 0, "type": "function",
                                "function": {"name": "Read", "arguments": "{}"}}]}}]})
            .to_string(),
        )])
        .await;
        let turn = run(&s, request(), Mode::Batch).await.0.unwrap();
        assert!(!turn.tool_calls()[0].id.is_empty());
        assert!(turn.tool_calls()[0].id.contains("Read"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reasoning_content_is_read_under_either_name() {
        for field in ["reasoning", "reasoning_content"] {
            let s = stub(vec![Reply::json(
                json!({"choices": [{"finish_reason": "stop",
                                    "message": {field: "thought", "content": "hi"}}]})
                .to_string(),
            )])
            .await;
            let turn = run(&s, request(), Mode::Batch).await.0.unwrap();
            assert!(
                matches!(&turn.content[0], ContentBlock::Thinking(t) if t.thinking == "thought"),
                "{field}: {:?}",
                turn.content
            );
            // Nothing on this format signs reasoning, so no signature is minted.
            assert!(matches!(&turn.content[0], ContentBlock::Thinking(t) if t.signature.is_none()));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stream_terminator_is_not_parsed_as_a_chunk() {
        // `[DONE]` is not JSON and never was. The fixture ends with one.
        let s = stub(vec![Reply::sse(sse_body())]).await;
        assert!(run(&s, request(), Mode::Stream).await.0.is_ok());
    }

    // -- failures -----------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_401_names_this_providers_variable_and_this_providers_command() {
        // The defect this pins: the sentence used to name `ANTHROPIC_API_KEY`
        // unconditionally, which sends the user to check a key that was never
        // involved in the request that failed.
        let s = stub(vec![Reply::error(
            401,
            r#"{"error":{"message":"No auth credentials found"}}"#,
        )])
        .await;
        let msg = run(&s, request(), Mode::Batch)
            .await
            .0
            .unwrap_err()
            .to_string();
        assert!(msg.contains("OPENROUTER_API_KEY"), "{msg}");
        assert!(msg.contains("emma set-provider openrouter"), "{msg}");
        assert!(!msg.contains("ANTHROPIC_API_KEY"), "{msg}");

        let s = stub(vec![Reply::error(401, "{}")]).await;
        let msg = provider(&s, OPENAI)
            .send(request(), Mode::Batch, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(msg.contains("OPENAI_API_KEY"), "{msg}");
        assert!(msg.contains("emma set-provider openai"), "{msg}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_403_is_terminal_and_a_5xx_is_retried() {
        let s = stub(vec![Reply::error(403, r#"{"error":{"message":"nope"}}"#)]).await;
        let err = run(&s, request(), Mode::Batch).await.0.unwrap_err();
        assert!(matches!(err, LlmError::Forbidden { .. }), "{err:?}");
        assert!(!err.retryable());

        let s = stub(vec![
            Reply::error(503, r#"{"error":{"message":"upstream"}}"#),
            Reply::json(batch_body()),
        ])
        .await;
        let (turn, events) = run(&s, request(), Mode::Batch).await;
        assert!(
            turn.is_ok(),
            "a 503 ended the turn instead of being retried"
        );
        assert_eq!(s.requests().len(), 2);
        // A retry the user cannot see is indistinguishable from a hang.
        assert!(events.iter().any(|e| matches!(e, Event::Retrying { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_429_waits_the_time_the_host_asked_for_and_says_so() {
        let s = stub(vec![
            Reply::error(429, r#"{"error":{"message":"slow down"}}"#)
                .with_header("retry-after", "2"),
            Reply::error(429, r#"{"error":{"message":"slow down"}}"#),
            Reply::error(429, r#"{"error":{"message":"slow down"}}"#),
        ])
        .await;
        let (turn, events) = run(&s, request(), Mode::Batch).await;
        let err = turn.unwrap_err();
        assert!(matches!(err, LlmError::RateLimited { .. }), "{err:?}");
        // The header is read off the response that carried it, which is the
        // first one; the last 429 sent none and is what the caller finally
        // sees. The hint has to reach the user on the attempt it belongs to.
        let reasons: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::Retrying { reason, .. } => Some(reason.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            reasons
                .first()
                .is_some_and(|r| r.contains("the API asked for 2s")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .last()
                .is_some_and(|r| r.contains("no retry-after was given")),
            "{reasons:?}"
        );
        // The delay itself is clamped by the test policy's 5ms cap, which keeps
        // the suite fast; that a `retry-after` outranks the curve is `retry`'s
        // own unit test.
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_400_is_surfaced_with_what_the_host_objected_to_and_is_not_retried() {
        let s = stub(vec![Reply::error(
            400,
            r#"{"error":{"message":"model not found: nonesuch/v9"}}"#,
        )])
        .await;
        let err = run(&s, request(), Mode::Batch).await.0.unwrap_err();
        assert!(err.to_string().contains("nonesuch/v9"), "{err}");
        assert!(!err.retryable());
        assert_eq!(s.requests().len(), 1, "a 400 was sent again");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_error_body_carrying_a_200_is_an_error_not_an_empty_turn() {
        // A real shape on gateway hosts. Read as a completion it produces a
        // turn with nothing in it, which the loop treats as the model having
        // nothing to say.
        let s = stub(vec![Reply::json(
            r#"{"error":{"message":"upstream refused","code":502}}"#,
        )])
        .await;
        let err = run(&s, request(), Mode::Batch).await.0.unwrap_err();
        assert!(err.to_string().contains("upstream refused"), "{err}");

        // Same shape mid-stream, where the frame arrives after the connection
        // succeeded and nothing else would have reported it.
        let s = stub(vec![Reply::sse(
            "data: {\"error\":{\"message\":\"upstream refused\"}}\n\n",
        )])
        .await;
        let err = run(&s, request(), Mode::Stream).await.0.unwrap_err();
        assert!(err.to_string().contains("upstream refused"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_formatted_error_never_contains_the_key() {
        // A gateway that echoes the auth header back is not hypothetical, and
        // the error it produces is the thing that ends up in a log.
        let s = stub(vec![Reply::error(
            401,
            format!(r#"{{"error":{{"message":"bad token Bearer {TEST_KEY}"}}}}"#),
        )])
        .await;
        let err = run(&s, request(), Mode::Batch).await.0.unwrap_err();
        assert!(!format!("{err}").contains(TEST_KEY), "{err}");
        assert!(!format!("{err:?}").contains(TEST_KEY), "{err:?}");
        assert!(format!("{err}").contains("[redacted]"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_debug_of_the_provider_shows_no_key() {
        let p = OpenAiCompatProvider::new(OPENROUTER, ApiKey::new(TEST_KEY), None);
        let shown = format!("{p:?}");
        assert!(!shown.contains(TEST_KEY), "{shown}");
        assert!(shown.contains("openrouter"), "{shown}");
    }

    // -- the registry -------------------------------------------------------

    #[test]
    fn both_hosts_are_reachable_by_name_and_declare_their_own_variable() {
        for (name, env, model) in [
            ("openrouter", "OPENROUTER_API_KEY", OPENROUTER.default_model),
            ("openai", "OPENAI_API_KEY", OPENAI.default_model),
        ] {
            let k = crate::kind(name).expect("registered");
            assert_eq!(k.name(), name);
            assert_eq!(k.env_var(), env);
            assert_eq!(k.default_model(), model);
            // A hosted API without a key is a misconfiguration worth refusing.
            assert!(k.requires_key());
            // Neither host sells a server-side search on this endpoint. The
            // startup line reads this to tell the user whether the search
            // setting they hold means anything on the provider they chose, and
            // a setting that silently does nothing is the failure that check
            // exists to prevent. `:online` on an OpenRouter id is a different
            // mechanism — part of the model name, not a request flag — and
            // nothing here can turn it on.
            assert!(!k.web_search(), "{name}");
            assert_eq!(
                k.build(ApiKey::new(TEST_KEY), Some("x/y".into()))
                    .model_id(),
                "x/y"
            );
            assert_eq!(k.build(ApiKey::new(TEST_KEY), None).model_id(), model);
        }
    }
}

// endregion: Tests
