//! Anthropic Messages API over raw HTTP, in batch and SSE-streaming form.
//!
//! Model default is `claude-opus-5`. Thinking is left at the model's default —
//! on this family that is adaptive-on, and the fixed `budget_tokens` budget
//! returns 400 — with depth controlled by `output_config.effort`. Sampling
//! parameters are not sent; they also return 400.
//!
//! # Caching, and whether it is worth it here
//!
//! Emma has no corpus. The only stable prefix is the instructions plus the tool
//! schema — a few thousand tokens, not fifty. The arithmetic at that size, at
//! list price and with the published multipliers (write 1.25×, read 0.1×,
//! minimum cacheable prefix 512 tokens on `claude-opus-5`):
//!
//! - A 2,000-token prefix costs **+500 token-equivalents** to write once.
//! - Every later call that reads it saves **1,800**.
//! - Break-even is therefore P(at least one more call reads this entry within
//!   the 5-minute TTL) ≳ **0.28**.
//!
//! A coding turn is never one model call — the model reads a file, gets the
//! result, and is called again seconds later. P is effectively 1.0, so the
//! write pays back on the *second* call of the first turn and every call after
//! that is 90% off on the prefix. That is why caching is on by default, and why
//! [`MIN_CACHEABLE`] gates it *per model*: below that model's floor the marker
//! is accepted, silently does nothing, and the request pays the overhead for no
//! entry. The floor is 512 on `claude-opus-5` and 4,096 on `claude-haiku-4-5`,
//! which is why it cannot be one constant. The honest summary is "worth it, but
//! only because the loop is multi-call and only above the minimum" —
//! [`Caching::Off`] exists so that claim can be measured rather than believed.
//!
//! The saving is smaller than tustle-agent's measured 84%, because there the
//! cached prefix was a 33k-token corpus. Here the prefix is small and the real
//! money is in the conversation, which is why the history and in-flight tool
//! results get breakpoints too.

use crate::models;
use crate::retry::retry_after_seconds;
use crate::{
    redact, trim_body, ApiKey, AssistantTurn, Caching, ContentBlock, Event, LlmError, Message,
    Mode, Provider, Request, Retry, Usage,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;

pub const DEFAULT_MODEL: &str = "claude-opus-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

// region: Cache breakpoints
// ---------------------------------------------------------------------------
// Cache breakpoints
//
// The size gate and the three places a marker may land: the system anchor, the
// end of history, and the end of an in-flight tool round-trip. Every one of
// them is conditional on clearing the provider's minimum, because below it a
// marker is accepted, does nothing, and still costs.
// ---------------------------------------------------------------------------

/// Smallest prefix each model will cache at all, longest-prefix keyed so a
/// dated snapshot (`claude-haiku-4-5-20251001`) resolves to the same row as its
/// alias. Below its model's floor a `cache_control` marker is **accepted and
/// silently does nothing** — no error, no entry, and the marked span still
/// billed at the uncached rate. That is why every breakpoint below is gated,
/// and why the gate cannot be one number: the floor is not monotonic with
/// release date, and the newest models have the *lowest* floors. A single
/// `512` was right for `claude-opus-5` and eight times too low for
/// `claude-haiku-4-5`, which a user reaches with one `/model` or one
/// Haiku-typed delegation.
///
/// Transcribed 2026-08-11 from Anthropic's published prompt-caching docs.
/// **A snapshot of facts that change without asking us** — floors have moved
/// with every model generation, so re-check this table when adding a model
/// rather than pattern-matching from the rows above it.
///
/// Its twin is `models::TABLE`, and they are deliberately not one table:
/// `Limits` is the shape `GET /v1/models` parses into, that endpoint reports no
/// cache minimum, and the two unknown-model fallbacks argue in opposite
/// directions (see [`MIN_CACHEABLE_UNKNOWN`]). The cost of the split is two
/// tables to update per release; `notes/plan-caching-defects.md` §"Where it
/// lives" carries the argument and the revisit trigger.
const MIN_CACHEABLE: &[(&str, usize)] = &[
    ("claude-opus-5", 512),
    ("claude-fable-5", 512),
    ("claude-mythos-5", 512),
    ("claude-opus-4-8", 1_024),
    ("claude-sonnet-5", 1_024),
    ("claude-sonnet-4-6", 1_024),
    ("claude-sonnet-4-5", 1_024),
    ("claude-opus-4-7", 2_048),
    ("claude-opus-4-6", 4_096),
    ("claude-haiku-4-5", 4_096),
];

/// What we assume for a model this table has never heard of: **the highest
/// floor anyone ships**, which is the opposite direction from
/// [`models::UNKNOWN`]'s conservative *low* ceiling, and deliberately so.
///
/// - *Guess too low* and we reproduce the defect this table exists to fix:
///   markers on prefixes under the real floor, the marked span billed
///   uncached, no symptom anywhere except the invoice, unbounded in time.
/// - *Guess too high* (chosen) and an unknown model whose real floor is lower
///   goes unmarked while its estimated prefix sits under 4,096 tokens
///   (~16 KB rendered). The cost is the forgone 0.9× read discount on that
///   span — cents per call — and it ends by itself, because history grows past
///   the threshold within a few turns.
///
/// Silent-and-forever versus visible-and-self-healing is the whole argument.
const MIN_CACHEABLE_UNKNOWN: usize = 4_096;

/// The smallest prefix `model` will cache, or [`MIN_CACHEABLE_UNKNOWN`] if this
/// table has never heard of it.
fn min_cacheable_tokens(model: &str) -> usize {
    MIN_CACHEABLE
        .iter()
        .filter(|(id, _)| model.starts_with(id))
        // Longest match wins, so a shorter row added later cannot swallow a
        // model whose id it happens to prefix.
        .max_by_key(|(id, _)| id.len())
        .map(|(_, floor)| *floor)
        .unwrap_or(MIN_CACHEABLE_UNKNOWN)
}

/// Conservative chars-per-token, used only to decide whether a prefix clears
/// the minimum. Over-estimating tokens would waste a marker, so the estimate
/// leans low.
const CHARS_PER_TOKEN: usize = 4;

fn clears_minimum(chars: usize, floor: usize) -> bool {
    chars / CHARS_PER_TOKEN >= floor
}

/// 5-minute TTL. Not the 1-hour variant: that doubles the write price and only
/// pays at ≥3 reads per entry, and Emma has no measured session cadence to
/// justify it. Revisit with `cache_read_input_tokens` data, not with intuition.
fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

/// Attach a breakpoint to the last content block of `content`, promoting a
/// plain string to a one-element block array because `cache_control` only
/// exists on blocks.
fn mark_breakpoint(content: &mut Value) -> bool {
    match content {
        Value::String(s) => {
            if s.is_empty() {
                return false;
            }
            *content = json!([{ "type": "text", "text": s.clone(), "cache_control": ephemeral() }]);
            true
        }
        Value::Array(items) => match items.last_mut().and_then(|b| b.as_object_mut()) {
            Some(obj) => {
                obj.insert("cache_control".to_string(), ephemeral());
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// `system` as a block array carrying the cross-turn anchor.
///
/// This block is byte-identical for every call of every turn until the
/// instructions or the tool registry change, so it is written once and read
/// for the rest of the session. Nothing here introduces bytes of its own: the
/// text is `instructions` verbatim.
///
/// `tools_chars` counts toward the minimum because the server renders
/// `tools → system → messages`, so the prefix this breakpoint captures already
/// includes the tools array.
fn system_field(instructions: &str, tools_chars: usize, caching: Caching, floor: usize) -> Value {
    if instructions.is_empty() {
        // An empty text block is rejected outright; send the bare string.
        return Value::String(String::new());
    }
    let mut block = json!({ "type": "text", "text": instructions });
    if caching == Caching::On && clears_minimum(tools_chars + instructions.len(), floor) {
        block["cache_control"] = ephemeral();
    }
    json!([block])
}

/// How many bytes a message list renders to.
///
/// The *rendered* length, not the length of the text inside — the quotes and
/// the escapes are bytes the provider tokenises too, and this number is only
/// ever compared against the model's row in [`MIN_CACHEABLE`]. It was
/// `m.content.to_string().len()` over `serde_json::Value` and is the same
/// arithmetic over the typed form, deliberately: a breakpoint that moved
/// because the estimator changed units would be a caching regression nothing
/// on screen would report.
fn chars_of(messages: &[Message]) -> usize {
    messages.iter().map(|m| m.content.wire_len()).sum()
}

/// `messages` as `history ++ query`, with up to two more breakpoints.
///
/// **End of history.** Byte-stable for the whole session up to the last
/// completed turn, so it is read by every remaining call of this turn and by
/// every later turn.
///
/// **End of the query, but only once a tool round-trip is already in flight.**
/// A marker here is written on this call and can only be read if the model
/// calls another tool, so it needs P(another call) ≳ 0.28 to pay for its 1.25×
/// write. Before the first tool call that is a coin flip; after one it is what
/// a coding agent does all day — it reads, then edits, then runs the tests.
/// The gate is that evidence, and it is a judgement, not a measurement: when
/// `cache_read_input_tokens` data exists for real sessions, check it.
fn messages_field(
    req: &Request,
    prefix_chars: usize,
    floor: usize,
) -> Result<Vec<Value>, LlmError> {
    let render = |m: &Message| {
        serde_json::to_value(m).map_err(|e| LlmError::Protocol(format!("render message: {e}")))
    };
    let mut rendered: Vec<Value> = req
        .history
        .iter()
        .chain(req.query.iter())
        .map(render)
        .collect::<Result<_, _>>()?;

    if req.caching == Caching::Off {
        return Ok(rendered);
    }

    let history_chars = chars_of(&req.history);
    if !req.history.is_empty() && clears_minimum(prefix_chars + history_chars, floor) {
        if let Some(content) = rendered[req.history.len() - 1].get_mut("content") {
            mark_breakpoint(content);
        }
    }

    // More than one message in `query` means the user turn plus at least one
    // tool round-trip.
    if req.query.len() > 1
        && clears_minimum(prefix_chars + history_chars + chars_of(&req.query), floor)
    {
        let last = rendered.len() - 1;
        if let Some(content) = rendered[last].get_mut("content") {
            mark_breakpoint(content);
        }
    }
    Ok(rendered)
}

// endregion: Cache breakpoints

// region: The provider: rendering, classifying, retrying
// ---------------------------------------------------------------------------
// The provider: rendering, classifying, retrying
//
// One struct holding the key, the model and the retry policy, and the path a
// call takes through it: render the body, send it, turn a non-success status
// into a typed error, and decide whether to send it again.
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    http: reqwest::Client,
    key: ApiKey,
    model: String,
    base_url: String,
    retry: Retry,
}

// Hand-written so no future field can print the key by being added.
impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    pub fn new(key: ApiKey, model: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                // Generous: a long agentic turn at high effort can think for
                // minutes before the first byte in batch mode.
                .timeout(Duration::from_secs(600))
                .build()
                .expect("reqwest client"),
            key,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            // Deliberately not read from the environment. Emma runs inside
            // repositories with whatever env the shell carries; an env var that
            // redirects where the key is sent is a credential-exfiltration
            // switch, and no feature here is worth that.
            base_url: API_URL.to_string(),
            retry: Retry::default(),
        }
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    /// Point at something other than the real API — a corporate gateway, or a
    /// stub in a test. Explicit by construction, never ambient.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn scrub(&self, text: impl AsRef<str>) -> String {
        redact(text.as_ref(), self.key.expose())
    }

    fn render(&self, req: &Request, mode: Mode) -> Result<Value, LlmError> {
        let tools_chars = serde_json::to_string(&req.tools)
            .map(|s| s.len())
            .unwrap_or(0);
        // The cache floor is per-model and this is the only place the model and
        // the gating are both in scope — the same argument as the clamping
        // below, and the reason the floor is a parameter rather than a const.
        let floor = min_cacheable_tokens(&self.model);
        let system = system_field(&req.instructions, tools_chars, req.caching, floor);
        let messages = messages_field(req, tools_chars + req.instructions.len(), floor)?;

        // The request says what Emma wants; the model says what it will take.
        // Clamping happens here because this is the only place both are in
        // scope, and because it is the last point every request passes through
        // however it was built — a struct literal somewhere in the loop gets
        // the same treatment as `Request::new`.
        let limits = models::limits(&self.model);

        // serde_json orders object keys deterministically, which is what the
        // cache needs: two identical requests must render identical bytes.
        let mut body = json!({
            "model": self.model,
            "max_tokens": limits.clamp_max_tokens(req.max_tokens),
            "system": system,
            "messages": messages,
            "tools": req.tools,
        });
        // Absent rather than defaulted: on a model with no effort parameter the
        // field itself is the 400, so there is no value that would be safe to
        // send. Omitting it takes the provider's own default, which is the
        // closest thing to "as hard as this model works".
        if let Some(effort) = limits.clamp_effort(req.effort) {
            body["output_config"] = json!({ "effort": effort.as_str() });
        }
        if mode == Mode::Stream {
            body["stream"] = json!(true);
        }
        Ok(body)
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
            401 => LlmError::Unauthorized { message },
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
                .header("x-api-key", self.key.expose())
                .header("anthropic-version", API_VERSION)
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
        let msg: Value = serde_json::from_str(&raw)
            .map_err(|e| LlmError::Protocol(self.scrub(format!("{e}: {}", trim_body(&raw)))))?;
        turn_from_message(&msg)
    }

    async fn read_stream(
        &self,
        resp: reqwest::Response,
        events: Option<&mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut assembly = Assembly::default();

        while let Some(item) = stream.next().await {
            let bytes = item.map_err(|e| LlmError::Transport(self.scrub(e.to_string())))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(idx) = buf.find("\n\n") {
                let frame = buf[..idx].to_string();
                buf.drain(..idx + 2);
                let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) else {
                    continue;
                };
                let Ok(ev) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                assembly.apply(&ev, events).await?;
            }
        }
        assembly.finish()
    }
}

/// The provider's own error prose, or the whole body when it is not shaped the
/// way the API documents. "request failed" is never enough to act on; the
/// message the API wrote is.
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

async fn notify(events: Option<&mpsc::Sender<Event>>, event: Event) {
    if let Some(tx) = events {
        // Best effort: a closed receiver means the turn is being cancelled,
        // which the caller already knows about.
        let _ = tx.send(event).await;
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn send(
        &self,
        request: Request,
        mode: Mode,
        events: Option<mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        let body = self.render(&request, mode)?;
        self.execute(body, mode, events.as_ref()).await
    }
}

// endregion: The provider: rendering, classifying, retrying

// region: Assembling a streamed turn
// ---------------------------------------------------------------------------
// Assembling a streamed turn
//
// The SSE state machine. Frames arrive as fragments of indexed blocks; this
// rebuilds them into the same content array batch mode receives in one piece,
// including the blocks it does not understand.
// ---------------------------------------------------------------------------

/// Blocks under assembly, keyed by the index the API assigns them, so the
/// reconstructed turn is in the order it was sent regardless of frame arrival.
#[derive(Default)]
struct Assembly {
    blocks: BTreeMap<usize, Partial>,
    stop_reason: String,
    usage: Usage,
}

enum Partial {
    Text(String),
    Thinking {
        text: String,
        signature: String,
    },
    Tool {
        id: String,
        name: String,
        json: String,
    },
    /// A block type this client does not know how to build. Kept verbatim from
    /// `content_block_start` so it still round-trips back to the API — dropping
    /// an unknown block would silently corrupt the turn on replay.
    ///
    /// This variant exists because of a bug found by porting: tustle-agent's
    /// equivalent match arm was `_ => {}`, so every block that was not `text`
    /// or `tool_use` — thinking blocks among them — was dropped on the floor
    /// and never reached the content echoed back on the next call. The failure
    /// is invisible until a turn depends on the block being there.
    Opaque(Value),
}

impl Assembly {
    async fn apply(
        &mut self,
        ev: &Value,
        events: Option<&mpsc::Sender<Event>>,
    ) -> Result<(), LlmError> {
        let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match ev.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                let u = ev.pointer("/message/usage");
                self.usage.input_tokens = pick(u, "input_tokens");
                // Absent on a request that sent no `cache_control` at all, and
                // on providers that do not report them — both leave 0, which
                // is the baseline any saving is measured against. Note the
                // `input_tokens` above is the *uncached remainder* once these
                // are non-zero; see `Usage`.
                self.usage.cache_creation_input_tokens = pick(u, "cache_creation_input_tokens");
                self.usage.cache_read_input_tokens = pick(u, "cache_read_input_tokens");
                self.usage.output_tokens = pick(u, "output_tokens");
            }
            "content_block_start" => {
                let block = ev.get("content_block").cloned().unwrap_or_default();
                let partial = match block.get("type").and_then(Value::as_str) {
                    Some("text") => Partial::Text(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ),
                    Some("thinking") => Partial::Thinking {
                        text: block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    },
                    Some("tool_use") => {
                        let id = str_at(&block, "id");
                        let name = str_at(&block, "name");
                        notify(
                            events,
                            Event::ToolUseStarted {
                                id: id.clone(),
                                name: name.clone(),
                            },
                        )
                        .await;
                        Partial::Tool {
                            id,
                            name,
                            json: String::new(),
                        }
                    }
                    _ => Partial::Opaque(block),
                };
                self.blocks.insert(index, partial);
            }
            "content_block_delta" => {
                let delta = ev.get("delta").cloned().unwrap_or_default();
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let piece = str_at(&delta, "text");
                        if let Some(Partial::Text(s)) = self.blocks.get_mut(&index) {
                            s.push_str(&piece);
                        }
                        notify(events, Event::TextDelta(piece)).await;
                    }
                    Some("thinking_delta") => {
                        if let Some(Partial::Thinking { text, .. }) = self.blocks.get_mut(&index) {
                            text.push_str(&str_at(&delta, "thinking"));
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(Partial::Thinking { signature, .. }) =
                            self.blocks.get_mut(&index)
                        {
                            signature.push_str(&str_at(&delta, "signature"));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(Partial::Tool { json, .. }) = self.blocks.get_mut(&index) {
                            json.push_str(&str_at(&delta, "partial_json"));
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(sr) = ev.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = sr.to_string();
                }
                let u = ev.get("usage");
                let out = pick(u, "output_tokens");
                // Guarded rather than assigned: `pick` cannot tell "absent"
                // from "zero", so an unguarded write would let a
                // `message_delta` carrying only a stop reason erase the count
                // that `message_start` established.
                if out != 0 {
                    self.usage.output_tokens = out;
                }
            }
            "error" => {
                let message = ev
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("no detail")
                    .to_string();
                return Err(LlmError::Api {
                    status: 200,
                    message: format!("the stream ended in an error: {message}"),
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<AssistantTurn, LlmError> {
        let mut content = Vec::new();
        for (_, block) in self.blocks {
            content.push(match block {
                Partial::Text(text) => json!({ "type": "text", "text": text }),
                Partial::Thinking { text, signature } => {
                    let mut v = json!({ "type": "thinking", "thinking": text });
                    if !signature.is_empty() {
                        v["signature"] = json!(signature);
                    }
                    v
                }
                Partial::Tool { id, name, json } => {
                    let input = parse_tool_input(&json)?;
                    json!({ "type": "tool_use", "id": id, "name": name, "input": input })
                }
                Partial::Opaque(v) => v,
            });
        }
        turn_from_content(Value::Array(content), self.stop_reason, self.usage)
    }
}

/// A tool call whose arguments did not parse is a fault, not an empty call:
/// invoking a tool with `{}` because the JSON was truncated would run the wrong
/// action with default arguments.
///
/// The empty-string case is not that case and is deliberately `{}`: a
/// `tool_use` block that carried no `input_json_delta` at all is a call to a
/// tool that takes no arguments, not a truncated one. Truncation leaves a
/// partial fragment behind, and a fragment does not parse.
fn parse_tool_input(json: &str) -> Result<Value, LlmError> {
    if json.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(json)
        .map_err(|e| LlmError::Protocol(format!("tool arguments were not valid JSON: {e}")))
}

// endregion: Assembling a streamed turn

// region: From content blocks to a typed turn
// ---------------------------------------------------------------------------
// From content blocks to a typed turn
//
// The one place text and tool calls are extracted from content, so batch and
// streaming cannot drift apart in what they report.
// ---------------------------------------------------------------------------

fn str_at(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn pick(usage: Option<&Value>, key: &str) -> i64 {
    usage
        .and_then(|u| u.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// Batch mode: the whole message arrived at once.
fn turn_from_message(msg: &Value) -> Result<AssistantTurn, LlmError> {
    if msg.get("type").and_then(Value::as_str) == Some("error") {
        return Err(LlmError::Api {
            status: 200,
            message: api_message(&msg.to_string()),
        });
    }
    let content = msg
        .get("content")
        .cloned()
        .ok_or_else(|| LlmError::Protocol("the response carried no content array".into()))?;
    let usage: Usage = msg
        .get("usage")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| LlmError::Protocol(format!("usage: {e}")))?
        .unwrap_or_default();
    let stop_reason = msg
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    turn_from_content(content, stop_reason, usage)
}

/// The single place a typed turn is derived from content blocks, so batch and
/// streaming cannot drift apart in what they extract.
///
/// This is the whole of the Anthropic-to-Emma translation for a turn, and it is
/// three lines because [`ContentBlock::from_value`] does the deciding — text and
/// tool calls used to be pulled out here by hand into fields beside the blob,
/// and they are now views on the parsed blocks. What a block this client does
/// not model becomes is that function's business too, and the answer is that it
/// is kept whole.
fn turn_from_content(
    content: Value,
    stop_reason: String,
    usage: Usage,
) -> Result<AssistantTurn, LlmError> {
    let Value::Array(blocks) = content else {
        return Err(LlmError::Protocol(
            "content was not an array of blocks".into(),
        ));
    };
    Ok(AssistantTurn {
        content: blocks.into_iter().map(ContentBlock::from_value).collect(),
        stop_reason,
        usage,
    })
}

// endregion: From content blocks to a typed turn

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effort, ToolCall, ToolResult};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // region: stub
    // ---------------------------------------------------------------- stub --
    //
    // A loopback HTTP server rather than an injectable transport. The choice is
    // deliberate: the interesting assertions are about *wire bytes* — where the
    // cache breakpoints land, that the prefix order survives serialisation,
    // that a `retry-after` header is honoured, that reqwest re-sends on a fresh
    // connection — and a transport trait would test the assembler while
    // stubbing out exactly the layer those claims live in. It would also put a
    // seam in the public API that exists only for tests.

    struct Reply {
        status: u16,
        content_type: &'static str,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Reply {
        fn json(body: impl Into<String>) -> Self {
            Self {
                status: 200,
                content_type: "application/json",
                headers: Vec::new(),
                body: body.into(),
            }
        }

        fn sse(body: impl Into<String>) -> Self {
            Self {
                status: 200,
                content_type: "text/event-stream",
                headers: Vec::new(),
                body: body.into(),
            }
        }

        fn error(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                content_type: "application/json",
                headers: Vec::new(),
                body: body.into(),
            }
        }

        fn with_header(mut self, k: &str, v: &str) -> Self {
            self.headers.push((k.to_string(), v.to_string()));
            self
        }

        fn wire(&self) -> String {
            let extra: String = self
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}\r\n"))
                .collect();
            format!(
                "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n{}\r\n{}",
                self.status,
                self.content_type,
                self.body.len(),
                extra,
                self.body
            )
        }
    }

    struct Stub {
        url: String,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl Stub {
        fn requests(&self) -> Vec<Value> {
            self.seen.lock().unwrap().clone()
        }

        fn last(&self) -> Value {
            self.requests().pop().expect("the stub saw a request")
        }
    }

    fn headers_end(raw: &[u8]) -> Option<usize> {
        raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    fn content_length(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())?
            })
            .unwrap_or(0)
    }

    /// Serve `replies` in order, one per connection, recording each request
    /// body. `connection: close` on every reply means a retry arrives as a new
    /// connection, which is what makes the sequence observable.
    async fn stub(replies: Vec<Reply>) -> Stub {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = seen.clone();
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut raw: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    if let Some(h) = headers_end(&raw) {
                        let len = content_length(&raw[..h]);
                        if raw.len() >= h + len {
                            if let Ok(v) = serde_json::from_slice(&raw[h..h + len]) {
                                captured.lock().unwrap().push(v);
                            }
                            break;
                        }
                    }
                }
                let _ = sock.write_all(reply.wire().as_bytes()).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            }
        });
        Stub {
            url: format!("http://{addr}/v1/messages"),
            seen,
        }
    }

    fn provider(stub: &Stub) -> AnthropicProvider {
        AnthropicProvider::new(ApiKey::new(TEST_KEY), None)
            .with_base_url(stub.url.clone())
            // Backoff is exercised by unit tests in `retry`; here it would only
            // make the suite slow.
            .with_retry(Retry {
                max_attempts: 3,
                base: Duration::from_millis(1),
                cap: Duration::from_millis(5),
            })
    }

    const TEST_KEY: &str = "sk-ant-api03-TESTKEYTESTKEYTESTKEY";

    // endregion: stub

    // region: fixtures
    // ------------------------------------------------------------ fixtures --

    /// One assistant turn: a thinking block, some text, and two tool calls.
    /// The batch and SSE fixtures below are the same message in the two wire
    /// forms — that is what makes comparing their parsed turns meaningful.
    fn batch_body() -> String {
        json!({
            "type": "message",
            "role": "assistant",
            "stop_reason": "tool_use",
            "content": [
                { "type": "thinking", "thinking": "check both files", "signature": "sig-abc" },
                { "type": "text", "text": "Reading them now." },
                { "type": "tool_use", "id": "toolu_01", "name": "Read", "input": { "path": "a.rs" } },
                { "type": "tool_use", "id": "toolu_02", "name": "Read", "input": { "path": "b.rs" } }
            ],
            "usage": {
                "input_tokens": 41,
                "output_tokens": 17,
                "cache_creation_input_tokens": 3200,
                "cache_read_input_tokens": 29000
            }
        })
        .to_string()
    }

    fn sse_body() -> String {
        [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":41,"cache_creation_input_tokens":3200,"cache_read_input_tokens":29000}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"check both "}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"files"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading "}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"them now."}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_01","name":"Read"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_02","name":"Read"}}"#,
            r#"{"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"b.rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":3}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":17}}"#,
            r#"{"type":"message_stop"}"#,
        ]
        .iter()
        .map(|d| format!("event: x\ndata: {d}\n\n"))
        .collect()
    }

    fn big_instructions() -> String {
        // Comfortably over 512 tokens at chars/4, so the size gate opens.
        "You are Emma. ".repeat(200)
    }

    fn request() -> Request {
        Request::new(big_instructions(), vec![json!({ "name": "Read" })])
    }

    async fn run(
        stub: &Stub,
        req: Request,
        mode: Mode,
    ) -> (Result<AssistantTurn, LlmError>, Vec<Event>) {
        let (tx, mut rx) = mpsc::channel::<Event>(256);
        let out = provider(stub).send(req, mode, Some(tx)).await;
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        (out, events)
    }

    fn cache_marks(v: &Value) -> usize {
        match v {
            Value::Object(o) => o
                .iter()
                .map(|(k, val)| usize::from(k == "cache_control") + cache_marks(val))
                .sum(),
            Value::Array(a) => a.iter().map(cache_marks).sum(),
            _ => 0,
        }
    }

    // endregion: fixtures

    // region: tests
    // --------------------------------------------------------------- tests --

    #[tokio::test(flavor = "multi_thread")]
    async fn usage_carries_the_cache_fields_and_the_honest_total() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let (turn, _) = run(&s, request(), Mode::Batch).await;
        let usage = turn.unwrap().usage;

        assert_eq!(usage.input_tokens, 41);
        assert_eq!(usage.output_tokens, 17);
        assert_eq!(usage.cache_creation_input_tokens, 3_200);
        assert_eq!(usage.cache_read_input_tokens, 29_000);

        // The whole point: the true input is ~800× the field a naive fold reads.
        assert_eq!(usage.billable_input_tokens(), 41 + 3_200 + 29_000);
        assert_eq!(usage.billable_total_tokens(), 41 + 3_200 + 29_000 + 17);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn absent_cache_fields_parse_as_zero_not_as_an_error() {
        let body = json!({
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 41, "output_tokens": 17 }
        })
        .to_string();
        let s = stub(vec![Reply::json(body)]).await;
        let (turn, _) = run(&s, request(), Mode::Batch).await;
        let usage = turn.unwrap().usage;
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.billable_input_tokens(), 41);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streaming_assembles_the_same_turn_as_batch() {
        let batch = stub(vec![Reply::json(batch_body())]).await;
        let (from_batch, _) = run(&batch, request(), Mode::Batch).await;

        let streamed = stub(vec![Reply::sse(sse_body())]).await;
        let (from_stream, events) = run(&streamed, request(), Mode::Stream).await;

        let (a, b) = (from_batch.unwrap(), from_stream.unwrap());
        assert_eq!(a.text(), b.text());
        assert_eq!(a.tool_calls(), b.tool_calls());
        assert_eq!(a.stop_reason, b.stop_reason);
        assert_eq!(a.usage, b.usage);
        // Including the blocks echoed back next turn — a thinking block that
        // lost its signature in reassembly is rejected on the next call.
        assert_eq!(a.content, b.content);
        assert_eq!(
            a.content[0],
            ContentBlock::Thinking(crate::ThinkingBlock {
                thinking: "check both files".into(),
                signature: Some("sig-abc".into()),
                ..Default::default()
            }),
            "the signature must survive both paths identically"
        );
        assert_eq!(a, b);

        // …and only the streaming path narrates.
        let deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::TextDelta(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Reading ", "them now."]);
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ToolUseStarted { name, .. } if name == "Read"
        )));
        assert!(run(&batch, request(), Mode::Batch)
            .await
            .1
            .iter()
            .all(|e| !matches!(e, Event::TextDelta(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn several_tool_calls_in_one_turn_all_parse() {
        for mode in [Mode::Batch, Mode::Stream] {
            let s = stub(vec![match mode {
                Mode::Batch => Reply::json(batch_body()),
                Mode::Stream => Reply::sse(sse_body()),
            }])
            .await;
            let (turn, _) = run(&s, request(), mode).await;
            let turn = turn.unwrap();
            let calls = turn.tool_calls();
            assert_eq!(calls.len(), 2, "{mode:?}");
            assert_eq!(calls[0].id, "toolu_01");
            assert_eq!(calls[0].name, "Read");
            assert_eq!(calls[0].input, json!({ "path": "a.rs" }));
            assert_eq!(calls[1].id, "toolu_02");
            assert_eq!(calls[1].input, json!({ "path": "b.rs" }));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_429_is_retried_after_the_delay_the_api_asked_for_and_is_visible() {
        let s = stub(vec![
            Reply::error(429, r#"{"error":{"message":"rate limit"}}"#)
                .with_header("retry-after", "0"),
            Reply::error(529, r#"{"error":{"message":"overloaded"}}"#),
            Reply::json(batch_body()),
        ])
        .await;
        let (turn, events) = run(&s, request(), Mode::Batch).await;

        assert!(turn.is_ok(), "{:?}", turn.err());
        assert_eq!(s.requests().len(), 3, "both failures should be re-sent");

        let retries: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Retrying {
                    attempt,
                    delay,
                    reason,
                    ..
                } => Some((*attempt, *delay, reason.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(retries.len(), 2, "a silent retry is a hang: {events:?}");
        // The provider said 0s, so the curve must not be used instead.
        assert_eq!(retries[0].1, Duration::from_secs(0));
        assert!(retries[0].2.contains("429"), "{}", retries[0].2);
        assert!(retries[1].1 > Duration::ZERO, "backoff for a 5xx");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_429_that_never_clears_reports_how_long_to_wait() {
        let s = stub(vec![
            Reply::error(429, r#"{"error":{"message":"rate limit"}}"#)
                .with_header("retry-after", "0"),
            Reply::error(429, r#"{"error":{"message":"rate limit"}}"#)
                .with_header("retry-after", "0"),
            Reply::error(429, r#"{"error":{"message":"rate limit"}}"#)
                .with_header("retry-after", "9"),
        ])
        .await;
        let (turn, _) = run(&s, request(), Mode::Batch).await;
        let msg = turn.unwrap_err().to_string();
        assert!(msg.contains("rate limited"), "{msg}");
        assert!(msg.contains("9s"), "{msg}");
        assert_eq!(s.requests().len(), 3, "the attempt cap must hold");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_400_is_surfaced_with_what_the_api_objected_to() {
        let s = stub(vec![Reply::error(
            400,
            r#"{"error":{"message":"messages.1: roles must alternate"}}"#,
        )])
        .await;
        let (turn, _) = run(&s, request(), Mode::Batch).await;
        let msg = turn.unwrap_err().to_string();
        assert!(msg.contains("roles must alternate"), "{msg}");
        assert_eq!(
            s.requests().len(),
            1,
            "a malformed request must not be re-sent"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_401_says_the_key_is_the_problem() {
        let s = stub(vec![Reply::error(
            401,
            r#"{"error":{"message":"invalid x-api-key"}}"#,
        )])
        .await;
        let (turn, _) = run(&s, request(), Mode::Batch).await;
        let msg = turn.unwrap_err().to_string();
        assert!(msg.contains("ANTHROPIC_API_KEY"), "{msg}");
        // The command a user actually runs. This crate used to compose
        // `emma auth` and let `emma::commands::rename_auth` patch it on the way
        // out; the string is now correct at the source, so the rewrite has
        // nothing to do.
        assert!(msg.contains("emma api"), "{msg}");
        assert!(!msg.contains("emma auth"), "{msg}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_formatted_error_never_contains_the_key() {
        // A gateway that echoes the auth header back is not hypothetical, and
        // the error it produces is the thing that ends up in a log or a
        // bug report.
        let s = stub(vec![Reply::error(
            400,
            json!({ "error": { "message": format!("bad header x-api-key: {TEST_KEY}") } })
                .to_string(),
        )])
        .await;
        let (turn, _) = run(&s, request(), Mode::Batch).await;
        let err = turn.unwrap_err();

        let shown = format!("{err}");
        assert!(
            !shown.contains(TEST_KEY),
            "key leaked into Display: {shown}"
        );
        assert!(shown.contains("[redacted]"), "{shown}");
        let debugged = format!("{err:?}");
        assert!(
            !debugged.contains(TEST_KEY),
            "key leaked into Debug: {debugged}"
        );
        // …and the provider itself cannot print it either.
        let p = format!("{:?}", provider(&s));
        assert!(!p.contains(TEST_KEY), "{p}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mid_stream_error_frame_never_contains_the_key() {
        // The path `a_formatted_error_never_contains_the_key` cannot reach.
        // `Assembly::apply` is a free function the key was never handed to, so
        // nothing here can scrub at construction; a gateway that echoes the
        // auth header into an SSE `error` frame is putting it straight into an
        // `LlmError` that gets printed and logged.
        let body = format!(
            "data: {}\n\n",
            json!({
                "type": "error",
                "error": { "message": format!("upstream rejected x-api-key: {TEST_KEY}") }
            })
        );
        let s = stub(vec![Reply::sse(body)]).await;
        let (turn, _) = run(&s, request(), Mode::Stream).await;
        let err = turn.unwrap_err();

        let shown = format!("{err}");
        assert!(
            !shown.contains(TEST_KEY),
            "key leaked into Display: {shown}"
        );
        assert!(shown.contains("[redacted]"), "{shown}");
        let debugged = format!("{err:?}");
        assert!(
            !debugged.contains(TEST_KEY),
            "key leaked into Debug: {debugged}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_error_shaped_batch_body_never_contains_the_key() {
        // HTTP 200 with an error-shaped body: `classify` is never reached, so
        // `turn_from_message` builds the `Api` variant — again with no key in
        // scope to scrub with.
        let body = json!({
            "type": "error",
            "error": { "message": format!("upstream rejected x-api-key: {TEST_KEY}") }
        })
        .to_string();
        let s = stub(vec![Reply::json(body)]).await;
        let (turn, _) = run(&s, request(), Mode::Batch).await;
        let err = turn.unwrap_err();

        let shown = format!("{err}");
        assert!(
            !shown.contains(TEST_KEY),
            "key leaked into Display: {shown}"
        );
        assert!(shown.contains("[redacted]"), "{shown}");
        let debugged = format!("{err:?}");
        assert!(
            !debugged.contains(TEST_KEY),
            "key leaked into Debug: {debugged}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_wire_order_is_instructions_then_tools_then_history_then_query() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let req = request()
            .with_history(vec![
                Message::user("older question"),
                Message::assistant_text("older answer"),
            ])
            .with_query(vec![Message::user("the new question")]);
        let _ = run(&s, req, Mode::Batch).await;
        let sent = s.last();

        assert_eq!(sent["system"][0]["text"], big_instructions());
        assert_eq!(sent["tools"], json!([{ "name": "Read" }]));
        assert_eq!(sent["messages"][0]["content"], "older question");
        // Unmarked content travels as the plain string it was given as.
        assert_eq!(sent["messages"][2]["content"], "the new question");
        assert_eq!(sent["messages"].as_array().unwrap().len(), 3);
        assert_eq!(sent["output_config"]["effort"], "xhigh");
        // Thinking is left at the model's default; sending either the fixed
        // budget or a sampling parameter returns 400 on this family.
        assert!(sent.get("thinking").is_none(), "{sent}");
        assert!(sent.get("temperature").is_none(), "{sent}");
        assert_eq!(
            sent.get("stream"),
            None,
            "batch mode must not ask to stream"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn breakpoints_land_on_the_stable_prefix_and_nowhere_else() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let req = request()
            .with_history(vec![
                Message::user(big_instructions()),
                Message::assistant_text("older answer"),
            ])
            .with_query(vec![Message::user("the new question")]);
        let _ = run(&s, req, Mode::Batch).await;
        let sent = s.last();

        // System anchor, plus the end of history. Not the query: no tool
        // round-trip has happened yet, so a marker there is a 1.25× write
        // nothing is guaranteed to read.
        assert_eq!(cache_marks(&sent), 2, "sent: {sent}");
        assert_eq!(cache_marks(&sent["system"]), 1);
        assert_eq!(cache_marks(&sent["messages"][1]), 1);
        assert_eq!(cache_marks(&sent["messages"][2]), 0);
        assert_eq!(
            sent["system"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        // Everything before a breakpoint travels exactly as it did without one.
        assert_eq!(sent["messages"][0]["content"], big_instructions());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_already_using_tools_marks_its_newest_result() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let req = request().with_query(vec![
            Message::user(big_instructions()),
            Message::assistant(vec![ContentBlock::ToolUse(ToolCall {
                id: "toolu_01".into(),
                name: "Read".into(),
                input: json!({}),
                ..Default::default()
            })]),
            Message::tool_results(vec![ToolResult::ok("toolu_01", "fn main() {}")]),
        ]);
        let _ = run(&s, req, Mode::Batch).await;
        let sent = s.last();

        assert_eq!(cache_marks(&sent["messages"][2]), 1, "sent: {sent}");
        assert_eq!(sent["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(cache_marks(&sent["messages"][1]), 0);
    }

    /// A request carrying every content shape Emma can send, as the exact bytes
    /// that leave the socket.
    ///
    /// **Why a golden string rather than more field assertions.** The tests
    /// above pin where a `cache_control` lands and what order the four parts go
    /// in, and they all passed while the message model was a `serde_json::Value`
    /// blob and while it was typed — which is the point of them. What they
    /// cannot see is a byte that moved *inside* a block: a `signature` that
    /// stopped being emitted, an `is_error` that started being emitted as
    /// `false`, a `thinking` block re-serialised with its keys in a different
    /// order. Every one of those is a silent 400 on the next call or a cache
    /// miss that costs 10× and reports nothing.
    ///
    /// So this is the whole body, verbatim. It is meant to be annoying to
    /// change: if it goes red, the question is not "update the literal" but
    /// "which byte moved, and does the provider care".
    #[tokio::test(flavor = "multi_thread")]
    async fn the_whole_request_body_is_pinned_byte_for_byte() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let req = Request::new("INSTRUCTIONS ".repeat(200), vec![json!({ "name": "Read" })])
            .with_history(vec![
                Message::user("HISTORY ".repeat(200)),
                Message::assistant(vec![
                    ContentBlock::Thinking(crate::ThinkingBlock {
                        thinking: "the older thought".into(),
                        signature: Some("sig-history".into()),
                        ..Default::default()
                    }),
                    ContentBlock::text("the older answer"),
                ]),
            ])
            .with_query(vec![
                Message::user("QUERY ".repeat(200)),
                Message::assistant(vec![
                    // Unsigned on purpose: a streamed block that carried no
                    // `signature_delta` must not grow a `"signature": ""`.
                    ContentBlock::Thinking(crate::ThinkingBlock {
                        thinking: "unsigned".into(),
                        signature: None,
                        ..Default::default()
                    }),
                    ContentBlock::RedactedThinking(crate::RedactedThinkingBlock {
                        data: "REDACTED".into(),
                        ..Default::default()
                    }),
                    ContentBlock::ToolUse(ToolCall {
                        id: "toolu_ok".into(),
                        name: "Read".into(),
                        input: json!({ "path": "a.rs" }),
                        // The key every real `tool_use` carries. It is in the
                        // golden body because it has to come back out.
                        extra: serde_json::Map::from_iter([(
                            "caller".to_string(),
                            json!({ "type": "direct" }),
                        )]),
                    }),
                    ContentBlock::ToolUse(ToolCall {
                        id: "toolu_bad".into(),
                        name: "Read".into(),
                        input: json!({ "path": "b.rs" }),
                        ..Default::default()
                    }),
                    // A block this client cannot model, which must travel out
                    // exactly as it came in.
                    ContentBlock::Passthrough(
                        json!({ "type": "server_tool_use", "id": "srv_1", "name": "web_search" }),
                    ),
                ]),
                Message::tool_results(vec![
                    ToolResult::ok("toolu_ok", "fn main() {}"),
                    ToolResult::failed("toolu_bad", "{\"kind\":\"not_found\"}"),
                ]),
            ]);
        let _ = run(&s, req, Mode::Batch).await;

        let expected = concat!(
            r#"{"max_tokens":32000,"messages":["#,
            r#"{"content":"<<H>>","role":"user"},"#,
            r#"{"content":[{"signature":"sig-history","thinking":"the older thought","type":"thinking"},"#,
            r#"{"cache_control":{"type":"ephemeral"},"text":"the older answer","type":"text"}],"role":"assistant"},"#,
            r#"{"content":"<<Q>>","role":"user"},"#,
            r#"{"content":[{"thinking":"unsigned","type":"thinking"},"#,
            r#"{"data":"REDACTED","type":"redacted_thinking"},"#,
            r#"{"caller":{"type":"direct"},"id":"toolu_ok","input":{"path":"a.rs"},"name":"Read","type":"tool_use"},"#,
            r#"{"id":"toolu_bad","input":{"path":"b.rs"},"name":"Read","type":"tool_use"},"#,
            r#"{"id":"srv_1","name":"web_search","type":"server_tool_use"}],"role":"assistant"},"#,
            r#"{"content":[{"content":"fn main() {}","tool_use_id":"toolu_ok","type":"tool_result"},"#,
            r#"{"cache_control":{"type":"ephemeral"},"content":"{\"kind\":\"not_found\"}","is_error":true,"#,
            r#""tool_use_id":"toolu_bad","type":"tool_result"}],"role":"user"}],"#,
            r#""model":"claude-opus-5","output_config":{"effort":"xhigh"},"#,
            r#""system":[{"cache_control":{"type":"ephemeral"},"text":"<<I>>","type":"text"}],"#,
            r#""tools":[{"name":"Read"}]}"#,
        );
        // The three long strings are only long so the prefix clears the cache
        // minimum; spelling them out here would bury the shape this pins.
        let expected = expected
            .replace("<<I>>", &"INSTRUCTIONS ".repeat(200))
            .replace("<<H>>", &"HISTORY ".repeat(200))
            .replace("<<Q>>", &"QUERY ".repeat(200));
        assert_eq!(s.last().to_string(), expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_prefix_below_the_minimum_is_never_marked() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let req = Request::new("short instructions", vec![]).with_query(vec![Message::user("hi")]);
        let _ = run(&s, req, Mode::Batch).await;
        let sent = s.last();

        assert_eq!(
            cache_marks(&sent),
            0,
            "a marker below the minimum is never honoured, so it must not be sent: {sent}"
        );
        // …and the small system prompt still travels as a block array, one
        // shape for both paths.
        assert_eq!(
            sent["system"],
            json!([{ "type": "text", "text": "short instructions" }])
        );
    }

    // region: the per-model cache floor
    // ------------------------------------------- the per-model cache floor --

    /// The same `Request` through a provider pinned to `model`.
    async fn sent_as(model: &str, req: Request) -> Value {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let p = AnthropicProvider::new(ApiKey::new(TEST_KEY), Some(model.to_string()))
            .with_base_url(s.url.clone());
        let _ = p.send(req, Mode::Batch, None).await;
        s.last()
    }

    /// Over 4,096 tokens at chars/4, so even the highest floor opens.
    fn huge_instructions() -> String {
        "You are Emma. ".repeat(1_400)
    }

    /// The defect, made into a regression guard. `big_instructions()` is ~700
    /// estimated tokens: over Opus 5's 512 floor and well under Haiku 4.5's
    /// 4,096. Before the per-model table this sent a marker that Haiku accepts
    /// and ignores — no error, no cache entry, the marked span billed uncached,
    /// forever and invisibly.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_prefix_over_one_models_floor_and_under_anothers_is_marked_only_for_the_first() {
        let with_history = || {
            request()
                .with_history(vec![
                    Message::user(big_instructions()),
                    Message::assistant_text("older answer"),
                ])
                .with_query(vec![Message::user("the new question")])
        };
        assert_eq!(
            cache_marks(&sent_as("claude-haiku-4-5", with_history()).await),
            0,
            "Haiku 4.5 caches nothing under 4,096 tokens, so a marker there is pure cost"
        );
        // …and the identical request on a model whose floor it does clear is
        // still marked, so the fix is a per-model gate and not caching switched
        // off in general.
        assert_eq!(
            cache_marks(&sent_as(DEFAULT_MODEL, with_history()).await),
            2
        );
    }

    /// Above Haiku's own floor the three breakpoints land exactly where the
    /// placement tests pin them for Opus — the gate moved, the placement did
    /// not.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_high_floor_model_still_gets_every_breakpoint_once_it_clears_it() {
        let req = Request::new(huge_instructions(), vec![json!({ "name": "Read" })])
            .with_history(vec![
                Message::user(huge_instructions()),
                Message::assistant_text("older answer"),
            ])
            .with_query(vec![
                Message::user(huge_instructions()),
                Message::assistant(vec![ContentBlock::ToolUse(ToolCall {
                    id: "toolu_01".into(),
                    name: "Read".into(),
                    input: json!({}),
                    ..Default::default()
                })]),
                Message::tool_results(vec![ToolResult::ok("toolu_01", "fn main() {}")]),
            ]);
        let sent = sent_as("claude-haiku-4-5", req).await;

        assert_eq!(cache_marks(&sent), 3, "sent: {sent}");
        assert_eq!(cache_marks(&sent["system"]), 1);
        assert_eq!(cache_marks(&sent["messages"][1]), 1);
        assert_eq!(cache_marks(&sent["messages"][4]), 1);
    }

    /// A dated snapshot is the spelling the console hands a user, and it must
    /// not fall through to the unknown-model fallback by accident — here it
    /// would happen to give the same answer, so this asserts against Opus 5,
    /// where a missed row would be visibly wrong.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dated_snapshot_resolves_to_its_alias_on_the_wire() {
        assert_eq!(min_cacheable_tokens("claude-opus-5-20260101"), 512);
        assert_eq!(min_cacheable_tokens("claude-haiku-4-5-20251001"), 4_096);
        let sent = sent_as(
            "claude-opus-5-20260101",
            request().with_history(vec![Message::user(big_instructions())]),
        )
        .await;
        assert_eq!(cache_marks(&sent), 2, "sent: {sent}");
    }

    /// An id this table has never heard of gets the highest floor anyone
    /// ships, not the lowest — the reverse of `models::UNKNOWN`, and the whole
    /// point of keeping the two tables apart.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_model_is_gated_at_the_highest_known_floor() {
        assert_eq!(
            min_cacheable_tokens("claude-opus-9-unreleased"),
            MIN_CACHEABLE
                .iter()
                .map(|(_, f)| *f)
                .max()
                .expect("table is not empty")
        );
        // ~700 estimated tokens: marked on Opus 5, unmarked here.
        assert_eq!(
            cache_marks(
                &sent_as(
                    "claude-opus-9-unreleased",
                    request().with_history(vec![Message::user(big_instructions())])
                )
                .await
            ),
            0
        );
        // …and it is a gate, not a refusal: clear 4,096 and the markers return.
        assert_eq!(
            cache_marks(
                &sent_as(
                    "claude-opus-9-unreleased",
                    Request::new(huge_instructions(), vec![])
                        .with_history(vec![Message::user(huge_instructions())])
                )
                .await
            ),
            2
        );
    }

    #[test]
    fn no_cache_floor_row_shadows_another() {
        // Prefix matching is only unambiguous while no id prefixes another —
        // today none does, and `max_by_key` covers the day one is added. This
        // asserts the property rather than the tie-break, because a row like
        // `claude-opus` added later would silently swallow every Opus and no
        // other test in this file would notice.
        for (a, _) in MIN_CACHEABLE {
            for (b, _) in MIN_CACHEABLE {
                assert!(
                    a == b || !b.starts_with(a),
                    "{b} is shadowed by the shorter row {a}"
                );
            }
        }
        // The non-monotonicity that makes a single constant wrong: the newest
        // model has the lowest floor and an older, smaller one the highest.
        assert!(min_cacheable_tokens("claude-opus-5") < min_cacheable_tokens("claude-haiku-4-5"));
    }

    // endregion: the per-model cache floor

    #[tokio::test(flavor = "multi_thread")]
    async fn caching_off_sends_no_markers_at_all() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let mut req = request()
            .with_history(vec![Message::user(big_instructions())])
            .with_query(vec![Message::user("q")]);
        req.caching = Caching::Off;
        let _ = run(&s, req, Mode::Batch).await;
        assert_eq!(cache_marks(&s.last()), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_model_default_is_overridable() {
        let s = stub(vec![Reply::json(batch_body()), Reply::json(batch_body())]).await;
        let p = AnthropicProvider::new(ApiKey::new(TEST_KEY), None).with_base_url(s.url.clone());
        assert_eq!(p.model_id(), DEFAULT_MODEL);
        let _ = p.send(request(), Mode::Batch, None).await;
        assert_eq!(s.last()["model"], DEFAULT_MODEL);

        let p = AnthropicProvider::new(ApiKey::new(TEST_KEY), Some("claude-haiku-4-5".into()))
            .with_base_url(s.url.clone());
        let _ = p.send(request(), Mode::Batch, None).await;
        assert_eq!(s.last()["model"], "claude-haiku-4-5");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streaming_mode_asks_the_api_to_stream() {
        let s = stub(vec![Reply::sse(sse_body())]).await;
        let _ = run(&s, request(), Mode::Stream).await;
        assert_eq!(s.last()["stream"], json!(true));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_truncated_tool_argument_is_a_fault_not_an_empty_call() {
        // Invoking Write with `{}` because the JSON was cut off would run the
        // wrong action with default arguments.
        let body: String = [
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"Write"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        ]
        .iter()
        .map(|d| format!("data: {d}\n\n"))
        .collect();
        let s = stub(vec![Reply::sse(body)]).await;
        let (turn, _) = run(&s, request(), Mode::Stream).await;
        match turn {
            Err(LlmError::Protocol(m)) => assert!(m.contains("tool arguments"), "{m}"),
            other => panic!("a truncated call was accepted: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mid_stream_error_frame_ends_the_turn() {
        let body = format!(
            "data: {}\n\n",
            r#"{"type":"error","error":{"message":"overloaded mid-stream"}}"#
        );
        let s = stub(vec![Reply::sse(body)]).await;
        let (turn, _) = run(&s, request(), Mode::Stream).await;
        assert!(turn
            .unwrap_err()
            .to_string()
            .contains("overloaded mid-stream"));
        // Not retried: the caller may already have printed part of the answer.
        assert_eq!(s.requests().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn effort_is_carried_through() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let mut req = request();
        req.effort = Effort::Low;
        let _ = run(&s, req, Mode::Batch).await;
        assert_eq!(s.last()["output_config"]["effort"], "low");
    }

    /// Same `Request`, three models, three different sets of bytes on the wire.
    /// The unit tests in `models` pin the table; this pins that the table is
    /// what the socket sees, which is the part a wrong wiring would break
    /// while every table test still passed.
    async fn sent_to(model: &str) -> Value {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let p = AnthropicProvider::new(ApiKey::new(TEST_KEY), Some(model.to_string()))
            .with_base_url(s.url.clone());
        let _ = p.send(request(), Mode::Batch, None).await;
        s.last()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_model_with_a_lower_ceiling_is_never_sent_the_full_ask() {
        // `Request::new` asks for 32,000. A model this table does not know
        // must not be handed it: too high a max_tokens is a 400 before the
        // model generates anything, which is the failure this exists to stop.
        assert_eq!(
            sent_to("claude-opus-9-unreleased").await["max_tokens"],
            8_192
        );
        // …while a model that can take the ask still gets it in full.
        assert_eq!(sent_to(DEFAULT_MODEL).await["max_tokens"], 32_000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_model_without_xhigh_is_never_sent_xhigh() {
        // `Request::new` asks for xhigh.
        assert_eq!(
            sent_to(DEFAULT_MODEL).await["output_config"]["effort"],
            "xhigh"
        );
        // 4.6 has `max` but not `xhigh`, so the ask lands on `high` — down the
        // ladder to the best level it has, never up to `max`.
        let sent = sent_to("claude-opus-4-6").await;
        assert_eq!(sent["output_config"]["effort"], "high", "{sent}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_model_with_no_effort_parameter_is_sent_no_output_config() {
        // Haiku 4.5 rejects the field itself, so no value is safe — the key
        // has to be absent, not defaulted.
        let sent = sent_to("claude-haiku-4-5").await;
        assert!(
            sent.get("output_config").is_none(),
            "output_config must be absent, not defaulted: {sent}"
        );
        assert_eq!(sent["max_tokens"], 32_000, "{sent}");
        // An unknown model is the same shape for a different reason.
        let sent = sent_to("claude-opus-9-unreleased").await;
        assert!(sent.get("output_config").is_none(), "{sent}");
    }

    #[test]
    fn roles_render_as_the_api_spells_them() {
        assert_eq!(
            serde_json::to_value(Message::user("x")).unwrap()["role"],
            "user"
        );
        assert_eq!(
            serde_json::to_value(Message::assistant_text("x")).unwrap()["role"],
            "assistant"
        );
    }

    // The seam with the tool layer: a real `Registry` renders the tool surface
    // that goes on the wire, and a `tool_use` block coming back names something
    // the registry can look up. If either side drifts, the loop dispatches to a
    // tool that does not exist.
    mod tool_surface {
        use super::*;
        use emma_tool_api::{Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
        use std::sync::Arc;

        struct Read;

        #[async_trait]
        impl Tool for Read {
            fn name(&self) -> &'static str {
                "Read"
            }
            fn description(&self) -> &str {
                "Read a file from the filesystem."
            }
            fn input_schema(&self) -> Value {
                json!({ "type": "object", "properties": { "path": { "type": "string" } } })
            }
            fn meta(&self) -> ToolMeta {
                ToolMeta {
                    read_only: true,
                    reaches_network: false,
                    idempotent: true,
                }
            }
            async fn invoke(
                &self,
                _ctx: &ToolCtx,
                _args: Value,
            ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
                Ok(Ok(ToolOutcome::new("")))
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_registry_round_trips_through_the_provider() {
            let mut registry = Registry::new();
            registry.register(Arc::new(Read));

            let s = stub(vec![Reply::json(batch_body())]).await;
            let req = Request::new(big_instructions(), registry.wire_definitions());
            let (turn, _) = run(&s, req, Mode::Batch).await;

            // Out: the wire form the registry produced, byte for byte.
            assert_eq!(s.last()["tools"], Value::Array(registry.wire_definitions()),);
            // Back: every call names a tool the registry can dispatch.
            let turn = turn.unwrap();
            for call in turn.tool_calls() {
                assert!(
                    registry.get(&call.name).is_some(),
                    "the model called {}, which the registry does not have",
                    call.name
                );
            }
        }
    }

    // endregion: tests
}
