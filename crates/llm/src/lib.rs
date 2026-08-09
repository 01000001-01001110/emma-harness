//! The model boundary.
//!
//! Emma's loop sees `Request` in and `AssistantTurn` out, and nothing else.
//! Provider types stay behind this trait so a second provider — or a fake one
//! in a test — costs a `impl Provider` and no change anywhere else.
//!
//! Rust has no official Anthropic SDK, so [`anthropic`] speaks the Messages
//! API over raw HTTP, which is the documented path for unsupported languages.
//!
//! **The one shape that must not drift.** A `Request` names its four parts in
//! prefix order — instructions, tools, history, query — because the provider's
//! cache is an exact prefix match and anything that varies per turn must sit
//! *after* everything that does not. That ordering is a struct here rather
//! than a comment in the renderer so that building a request with a per-turn
//! byte at position zero is not something the type lets you express.

pub mod anthropic;
pub mod auth;
mod retry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

pub use anthropic::{AnthropicProvider, DEFAULT_MODEL};
pub use auth::{ApiKey, AuthError};
pub use retry::Retry;

/// Who a message came from. Only these two cross the boundary: mid-conversation
/// `system` messages are an operator channel Emma does not have, and letting
/// the loop construct one would put a per-turn byte ahead of the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// A plain string or an array of content blocks, passed through verbatim so
    /// `tool_use` / `tool_result` round-trips — and the thinking blocks that
    /// must be echoed back unedited — survive the trip out and back.
    pub content: serde_json::Value,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: serde_json::Value::String(text.into()),
        }
    }

    pub fn assistant(content: serde_json::Value) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// The user turn that carries results back for one or more tool calls. The
    /// API requires every `tool_use` in the preceding assistant turn to be
    /// answered in a single user message, so this takes all of them at once.
    pub fn tool_results(results: Vec<serde_json::Value>) -> Self {
        Self {
            role: Role::User,
            content: serde_json::Value::Array(results),
        }
    }
}

/// How hard the model works before answering. Not a token budget — the fixed
/// thinking budget was removed on this model family and returns 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Whether to ship `cache_control` breakpoints. On by default because at
/// Emma's prompt size the arithmetic clears break-even on the second model
/// call of any turn; `Off` exists so `emma --no-cache` can prove that claim
/// rather than assert it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caching {
    On,
    Off,
}

/// One model call, in prefix order.
///
/// `history` is every completed turn; `query` is this turn only — the user's
/// message first, then the `tool_use` / `tool_result` pairs the loop appends as
/// it works. Keeping them apart is what lets the renderer mark the end of the
/// history without guessing where the stable part stops.
#[derive(Debug, Clone)]
pub struct Request {
    /// Instructions. First, and byte-identical across every call of a session.
    pub instructions: String,
    /// The tool surface, exactly as `Registry::wire_definitions()` renders it.
    pub tools: Vec<serde_json::Value>,
    pub history: Vec<Message>,
    pub query: Vec<Message>,
    /// Caps thinking **and** answer together on this model family, so a value
    /// sized around the answer alone truncates mid-thought.
    pub max_tokens: u32,
    pub effort: Effort,
    pub caching: Caching,
}

impl Request {
    pub fn new(instructions: impl Into<String>, tools: Vec<serde_json::Value>) -> Self {
        Self {
            instructions: instructions.into(),
            tools,
            history: Vec::new(),
            query: Vec::new(),
            // xhigh is the documented setting for coding and agentic work, and
            // Emma is nothing else.
            max_tokens: 32_000,
            effort: Effort::XHigh,
            caching: Caching::On,
        }
    }

    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }

    pub fn with_query(mut self, query: Vec<Message>) -> Self {
        self.query = query;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Provider-reported token counts for one model call.
///
/// **`input_tokens` is not the input.** With prompt caching in play the
/// Messages API splits the input three ways and `input_tokens` carries only the
/// *uncached remainder* — the bytes that were neither read from nor written to
/// the cache. In tustle-agent, where this was learned the expensive way, a
/// 72,549-token turn was recorded as `input_tokens: 2`. Anything that means
/// "the context this call carried" must use [`Usage::billable_input_tokens`]
/// or [`Usage::billable_total_tokens`]; summing the bare field under-counts by
/// up to ~10× the moment caching starts hitting, which silently loosens every
/// cap folded from it.
///
/// The two cache fields default to `0`, which covers providers that do not
/// report them and requests that sent no `cache_control` at all. A `0` here
/// means "nothing cached", never "unknown".
///
/// There is deliberately no `total()`. tustle-agent had to keep one and
/// `#[deprecated]` it so the wrong call compiled wrong rather than merely
/// reading wrong; Emma has no callers predating caching, so the trap cannot
/// recur here — the only way to get a total is to ask for the honest one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Uncached input only. See the type-level note before summing this.
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Input tokens written into the cache on this call (billed at 1.25×).
    #[serde(default)]
    pub cache_creation_input_tokens: i64,
    /// Input tokens served from the cache on this call (billed at 0.1×).
    #[serde(default)]
    pub cache_read_input_tokens: i64,
}

impl Usage {
    /// Every input token this call was billed for: uncached remainder + cache
    /// writes + cache reads.
    ///
    /// Deliberately *not* weighted by price — a cost view applies the
    /// 1.0 / 1.25 / 0.1 multipliers to the three fields separately; cap
    /// semantics count tokens.
    pub fn billable_input_tokens(&self) -> i64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }

    /// [`Usage::billable_input_tokens`] plus output.
    pub fn billable_total_tokens(&self) -> i64 {
        self.billable_input_tokens() + self.output_tokens
    }
}

/// One assistant turn, assembled identically whether the bytes arrived as a
/// single JSON body or as a stream of SSE frames.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTurn {
    /// Every text block concatenated — what a human read.
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: String,
    pub usage: Usage,
    /// The assistant turn as content blocks, for echoing back on the next call.
    /// Thinking blocks are included and unedited: this model family rejects
    /// modified ones, and dropping them breaks the turn.
    pub raw_content: serde_json::Value,
}

/// Anything worth showing a human while the call is in flight.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Assistant text, as it arrives. Streaming mode only.
    TextDelta(String),
    /// A tool call has started; its arguments are still streaming.
    ToolUseStarted { id: String, name: String },
    /// A request failed and is being retried. Emitted in both modes, because a
    /// retry the user cannot see is indistinguishable from a hang.
    Retrying {
        attempt: u32,
        max_attempts: u32,
        delay: Duration,
        reason: String,
    },
}

/// Whether to receive the turn as one JSON body or as a stream.
///
/// Both produce the same [`AssistantTurn`]; the difference is only whether the
/// loop gets [`Event::TextDelta`]s on the way. A terminal wants `Stream`; a
/// `--print` run or an internal sub-call is fine with `Batch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Batch,
    Stream,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn model_id(&self) -> &str;

    /// `events` is optional in both modes: pass `None` and the call is silent,
    /// pass a sender and retries become visible even in `Batch`.
    async fn send(
        &self,
        request: Request,
        mode: Mode,
        events: Option<mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError>;
}

/// What went wrong, phrased so the message alone tells the user what to do.
///
/// Every variant that carries provider text carries it *redacted*: the key is
/// scrubbed before construction, so a proxy or misconfigured gateway echoing
/// the auth header back cannot put it in a log line. See
/// [`redact`].
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no API key: {0}")]
    Auth(#[from] AuthError),

    #[error(
        "the API rejected this key (HTTP 401). Check ANTHROPIC_API_KEY, or run `emma auth` to \
         store a working one. Provider said: {message}"
    )]
    Unauthorized { message: String },

    #[error(
        "the API key is valid but not allowed to do this (HTTP 403) — check the key's workspace \
         and model permissions. Provider said: {message}"
    )]
    Forbidden { message: String },

    #[error("rate limited (HTTP 429){retry_hint}. Provider said: {message}")]
    RateLimited {
        retry_after: Option<Duration>,
        retry_hint: String,
        message: String,
    },

    #[error("the API rejected the request as invalid (HTTP 400): {message}")]
    BadRequest { message: String },

    #[error("the API is unavailable (HTTP {status}): {message}")]
    Unavailable { status: u16, message: String },

    #[error("the API returned HTTP {status}: {message}")]
    Api { status: u16, message: String },

    #[error("could not reach the API: {0}")]
    Transport(String),

    #[error("the API sent a response this client could not read: {0}")]
    Protocol(String),
}

impl LlmError {
    /// Failures where the same request may succeed unchanged. A 400 is not one
    /// of them — retrying a malformed request just spends the user's time.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::Unavailable { .. } | Self::Transport(_)
        )
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Remove every occurrence of `key` from `text`.
///
/// Applied to provider bodies and transport errors before they become an
/// `LlmError`. The alternative — trusting that nothing upstream ever echoes an
/// auth header — is a bet the user pays for once and cannot take back, because
/// a leaked key in a log is leaked for as long as the log lives.
pub(crate) fn redact(text: &str, key: &str) -> String {
    if key.is_empty() {
        return text.to_string();
    }
    text.replace(key, "[redacted]")
}

/// Cut a provider body down to something that fits on a terminal line without
/// losing the part that says what was wrong.
pub(crate) fn trim_body(body: &str) -> String {
    const MAX: usize = 600;
    let body = body.trim();
    if body.len() <= MAX {
        return body.to_string();
    }
    let mut end = MAX;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &body[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billable_counts_all_three_input_fields() {
        let usage = Usage {
            input_tokens: 41,
            output_tokens: 17,
            cache_creation_input_tokens: 3_200,
            cache_read_input_tokens: 29_000,
        };
        assert_eq!(usage.billable_input_tokens(), 41 + 3_200 + 29_000);
        assert_eq!(usage.billable_total_tokens(), 41 + 3_200 + 29_000 + 17);
        // The scar, stated as an assertion: the bare field is ~800× short here.
        assert!(usage.billable_input_tokens() > usage.input_tokens * 100);
    }

    #[test]
    fn redaction_is_exhaustive_not_first_match() {
        let key = "sk-ant-api03-AAAABBBBCCCC";
        let text = format!("header {key} was sent; retry with {key}");
        let out = redact(&text, key);
        assert!(!out.contains(key), "{out}");
        assert_eq!(out.matches("[redacted]").count(), 2);
    }

    #[test]
    fn a_bad_request_is_not_retried_but_a_429_is() {
        assert!(!LlmError::BadRequest {
            message: "x".into()
        }
        .retryable());
        assert!(LlmError::RateLimited {
            retry_after: None,
            retry_hint: String::new(),
            message: "x".into()
        }
        .retryable());
        assert!(LlmError::Transport("connection reset".into()).retryable());
    }
}
