//! The model boundary.
//!
//! Emma's loop sees `Request` in and `AssistantTurn` out, and nothing else.
//! Provider types stay behind the [`Provider`] trait, which is what lets the
//! loop's whole test suite drive a scripted fake — `Fake` in
//! `crates/emma/tests/support` — with no socket and no key.
//!
//! Rust has no official Anthropic SDK, so [`anthropic`] speaks the Messages
//! API over raw HTTP, which is the documented path for unsupported languages.
//! It is the only provider that exists today; a second one is planned and not
//! built.
//!
//! **The wire shape used to leak, and no longer does.** `AssistantTurn` once
//! carried `raw_content: serde_json::Value` — Anthropic's own content array,
//! echoed back unread — and the loop built its own
//! `{"type":"tool_result","tool_use_id":…}` objects to answer it. Both are gone:
//! a turn is now `Vec<ContentBlock>` and a result is [`ToolResult`], so the only
//! code that knows what the Messages API looks like is [`anthropic`]. See
//! [`content`] for the rule that made typing it safe — anything this client
//! cannot model *exactly* is not modelled at all, and travels as
//! [`ContentBlock::Passthrough`] — and for why that is not `raw_content` under
//! another name.
//!
//! What has not changed is the reason the blob existed: this model family
//! rejects a thinking block whose signature was re-derived, so a turn rebuilt
//! from `text` + tool calls makes the *next* call fail. That state now has a
//! name ([`ThinkingBlock::signature`]) instead of a hiding place. A second
//! provider is still real work — OpenAI puts tool calls in a `tool_calls` array
//! and each result in its own `role:"tool"` message — but it is work inside a
//! new file beside `anthropic.rs`, not work in the loop.
//!
//! **The one shape that must not drift.** A `Request` names its four parts in
//! prefix order — instructions, tools, history, query — because the provider's
//! cache is an exact prefix match and anything that varies per turn must sit
//! *after* everything that does not. That ordering is a struct here rather
//! than a comment in the renderer so that building a request with a per-turn
//! byte at position zero is not something the type lets you express.

pub mod anthropic;
pub mod auth;
pub mod content;
pub mod kind;
pub mod models;
pub mod ollama;
pub mod openai_compat;
mod retry;
pub mod roster;
#[cfg(test)]
pub mod stub;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;

pub use anthropic::{AnthropicProvider, DEFAULT_MODEL};
pub use auth::{ApiKey, AuthError};
pub use content::{
    Content, ContentBlock, RedactedThinkingBlock, TextBlock, ThinkingBlock, ToolCall, ToolResult,
};
pub use kind::{kind, ProviderKind, UnknownProvider, DEFAULT_PROVIDER};
pub use models::{limits, Limits};
pub use openai_compat::{OpenAiCompatProvider, Wire, OPENAI, OPENROUTER};
pub use retry::Retry;
pub use roster::{ModelInfo, Roster, RosterError};

// region: The request, in prefix order
// ---------------------------------------------------------------------------
// The request, in prefix order
//
// Everything that goes out: who spoke, how hard the model should work, whether
// to ask for cache breakpoints, and the four-part `Request` whose field order
// is the prefix order the cache depends on.
// ---------------------------------------------------------------------------

/// Who a message came from. Only these two, because the Messages API accepts
/// only these two in `messages`: instructions travel in the top-level `system`
/// field, which is where the prefix ordering above needs them anyway. There is
/// no third variant to add later without changing where the stable bytes sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// A plain string or typed content blocks — see [`Content`]. Blocks that
    /// this client does not model survive the round trip whole rather than
    /// being decoded lossily; that rule is [`content`]'s, and it is what lets
    /// this be a type instead of a `serde_json::Value`.
    pub content: Content,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(text.into()),
        }
    }

    /// An assistant turn as the provider produced it — thinking blocks,
    /// signatures and all. This is the constructor the loop uses to put a turn
    /// back into the conversation, and the reason it takes blocks rather than a
    /// string is that rebuilding a turn from its text loses the signature and
    /// makes the *next* call fail.
    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Blocks(content),
        }
    }

    /// An assistant turn Emma composed rather than received — a compaction
    /// summary, or the note that says the previous goal stopped short. Separate
    /// from [`Message::assistant`] because these two have nothing in common
    /// except their role: one is a record of what a model said, the other is a
    /// sentence written here, and neither should be able to be passed where the
    /// other is meant.
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(text.into()),
        }
    }

    /// The user turn that carries results back for one or more tool calls. The
    /// API requires every `tool_use` in the preceding assistant turn to be
    /// answered in a single user message, so this takes all of them at once.
    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        Self {
            role: Role::User,
            content: Content::Blocks(results.into_iter().map(ContentBlock::ToolResult).collect()),
        }
    }
}

/// How hard the model works before answering. Not a token budget — the fixed
/// thinking budget was removed on this model family and returns 400.
///
/// **Not every model implements every level**, and sending one that a model
/// does not implement is also a 400. Which levels a given model takes is
/// [`models::Limits::efforts`]; the choice of what to send is
/// [`models::Limits::clamp_effort`].
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

    /// Position on the ladder, for "the best level at or below this one".
    ///
    /// A number rather than `#[derive(PartialOrd)]` because deriving it would
    /// also make `<` work on the enum everywhere, and a comparison between two
    /// efforts only means something in the one place that clamps them — a
    /// model's supported set has holes, so `a < b` says nothing about whether
    /// `b` can be sent where `a` can.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
            Self::XHigh => 3,
            Self::Max => 4,
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
    ///
    /// A ceiling the caller wants, not one the model promised. Every model has
    /// its own maximum and exceeding it is a 400, so the provider lowers this
    /// to the model's when the model's is lower — see [`models`].
    pub max_tokens: u32,
    /// Likewise an ask: the provider clamps it down to the best level the
    /// chosen model actually implements, or drops it entirely for a model with
    /// no effort parameter.
    pub effort: Effort,
    pub caching: Caching,
    /// Whether the provider may search the web on the model's behalf, using
    /// its own search tool, on its own side of the wire.
    ///
    /// **An ask, like `effort` and `max_tokens` above, and the provider decides
    /// what it means.** Anthropic answers it by adding its server-side
    /// `web_search` tool to the request; nothing runs on this machine, no host
    /// is reached from here, and the model's queries go to the same place the
    /// conversation already goes. Ollama has no equivalent and ignores the
    /// flag, so a request that asks on a local model gets no search and no
    /// error. The loop sets this from the user's settings, off unless asked; a
    /// bare [`Request::new`] leaves it off, because a test or a sub-call that
    /// did not ask for a search must not be billed for one.
    ///
    /// This is the second of two search paths and the optional one. Emma's own
    /// `WebSearch` tool renders a results page in the local Chrome, needs no
    /// key, and goes through the egress gate; it is what the model has by
    /// default. This flag is for whoever wants the provider's search as well,
    /// which Claude Code and Codex both use on their own providers. What is
    /// not coming back is a search vendor's own API key: the owner's ruling on
    /// 2026-09-05, after the tool that needed one was removed.
    pub web_search: bool,
    /// Sampling temperature, or `None` for "the provider's default".
    ///
    /// An ask, like the other knobs here: a provider that has no such
    /// parameter ignores it. Carried on the request rather than a global
    /// because the settings resolve it per provider.
    pub temperature: Option<f64>,
}

impl Request {
    pub fn new(instructions: impl Into<String>, tools: Vec<serde_json::Value>) -> Self {
        Self {
            instructions: instructions.into(),
            tools,
            history: Vec::new(),
            query: Vec::new(),
            // What Emma wants, not what it will necessarily get. xhigh is the
            // documented setting for coding and agentic work and Emma is
            // nothing else, and 32,000 is a budget rather than a ceiling — a
            // model that caps lower gets its own cap, a model without xhigh
            // gets the best level it has. `models` holds both, and the
            // provider applies them, because a `Request` does not know which
            // model it will be sent to.
            max_tokens: 32_000,
            effort: Effort::XHigh,
            caching: Caching::On,
            web_search: false,
            temperature: None,
        }
    }

    pub fn with_temperature(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn with_web_search(mut self, on: bool) -> Self {
        self.web_search = on;
        self
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

// endregion: The request, in prefix order

// region: The turn that comes back
// ---------------------------------------------------------------------------
// The turn that comes back
//
// The typed result of one model call, identical whether the bytes arrived as
// one JSON body or as a stream. `Usage` carries the caching scar; `content`
// carries the blocks that must be echoed back untouched.
// ---------------------------------------------------------------------------

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
    /// What the provider did on its own side of the wire during this call.
    /// Defaulted, because most providers report nothing here and a provider
    /// that never searches must not read as "unknown".
    #[serde(default)]
    pub server_tool_use: ServerToolUse,
    /// The context window the provider actually ran this call in, in tokens,
    /// when it says. Zero means "not reported", never "zero tokens".
    ///
    /// Ollama reports it as `num_ctx` in effect, and it is the number that
    /// catches a silent clip: a request whose prompt is larger than this was
    /// truncated on the provider's side without an error. The fork measured
    /// that clip at 28 of 117 logged calls under a flat 32,768 default, which
    /// is why the number is recorded rather than assumed.
    #[serde(default)]
    pub context_window: i64,
}

/// Work the provider performed for the model inside one call, billed apart
/// from tokens. Nested to match the wire, where Anthropic puts it under
/// `usage.server_tool_use`, so the batch decoder needs no special case.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolUse {
    /// Web searches the provider ran during this call. Each is a separate
    /// charge from the tokens, which is why it is counted here and shown to the
    /// user rather than folded into the token spend.
    #[serde(default)]
    pub web_search_requests: i64,
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
    /// The turn itself, in order, including the thinking blocks that must be
    /// echoed back unedited. This is the whole turn — [`AssistantTurn::text`]
    /// and [`AssistantTurn::tool_calls`] are views onto it.
    pub content: Vec<ContentBlock>,
    pub stop_reason: String,
    pub usage: Usage,
}

impl AssistantTurn {
    /// Every text block concatenated — what a human read.
    ///
    /// Derived rather than stored, and that is the point: this and `content`
    /// used to be two fields, which is two things that can disagree about what
    /// the model said. A projection cannot drift from what it projects.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Every tool call in the turn, in the order the model made them.
    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(c) => Some(c),
                _ => None,
            })
            .collect()
    }
}

// endregion: The turn that comes back

// region: The trait, and what it narrates in flight
// ---------------------------------------------------------------------------
// The trait, and what it narrates in flight
//
// `Provider` is the whole boundary. `Event` and `Mode` are how a caller chooses
// between watching a call happen and simply waiting for it.
// ---------------------------------------------------------------------------

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

    /// Lines to print before the first call, naming anything about this
    /// provider that the user did not choose and would want to know.
    ///
    /// **Empty by default, because a provider whose endpoint is a constant has
    /// nothing to disclose.** Anthropic is that case, which is why nothing ever
    /// asked. Ollama is not: it takes its destination from `OLLAMA_HOST`, so a
    /// stale value in a shell profile sends the whole conversation — every file
    /// the model has read — to a machine the user has forgotten about. This is
    /// the surface that says so.
    ///
    /// Disclosure, not a gate. Nothing here refuses; `CLAUDE.md` is explicit
    /// that this project does not pretend to enforcement it does not have. What
    /// a printed line prevents is the honest mistake.
    fn startup_notes(&self) -> Vec<String> {
        Vec::new()
    }

    /// `events` is optional in both modes: pass `None` and the call is silent,
    /// pass a sender and retries become visible even in `Batch`.
    async fn send(
        &self,
        request: Request,
        mode: Mode,
        events: Option<mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError>;
}

// endregion: The trait, and what it narrates in flight

// region: Errors, and keeping the key out of them
// ---------------------------------------------------------------------------
// Errors, and keeping the key out of them
//
// One variant per thing a user can act on, the process-wide scrub list that
// makes redaction a property of the type rather than of each call site, and the
// helpers every message passes through on the way out.
// ---------------------------------------------------------------------------

/// What went wrong, phrased so the message alone tells the user what to do.
///
/// **No formatting of this type can contain an API key.** That is enforced at
/// the one place every variant leaves the type — the `Display` and `Debug`
/// impls below, which render the sentence and then run it through
/// [`scrub_secrets`]. See the note on those impls for why the guarantee lives
/// there rather than at each construction site, and for what it does not cover.
pub enum LlmError {
    Auth(AuthError),

    // The command that stores a key is `emma api`. This crate used to say
    // `emma auth` and rely on `emma::commands::rename_auth` to rewrite the word
    // at the printing boundary; that was a workaround for a wrong string, and
    // the string is now right. `rename_auth` is therefore a no-op in practice
    // and can be deleted once nothing else depends on it.
    // `fix` is the provider's own sentence: its environment variable and the
    // command that stores its key. It is a field rather than a constant because
    // this used to name Anthropic's variable unconditionally, and a second
    // provider inheriting that string sent a user to check a key that was
    // never involved in the request that failed.
    Unauthorized {
        fix: String,
        message: String,
    },

    Forbidden {
        message: String,
    },

    RateLimited {
        retry_after: Option<Duration>,
        retry_hint: String,
        message: String,
    },

    BadRequest {
        message: String,
    },

    Unavailable {
        status: u16,
        message: String,
    },

    Api {
        status: u16,
        message: String,
    },

    Transport(String),

    Protocol(String),
}

impl From<AuthError> for LlmError {
    fn from(e: AuthError) -> Self {
        Self::Auth(e)
    }
}

impl std::error::Error for LlmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Auth(e) => Some(e),
            _ => None,
        }
    }
}

impl LlmError {
    /// The sentence, before scrubbing. Private, and the *only* caller is the
    /// pair of impls below — a raw message must never escape by another route.
    fn sentence(&self) -> String {
        match self {
            Self::Auth(e) => format!("no API key: {e}"),
            Self::Unauthorized { fix, message } => {
                format!("the API rejected this key (HTTP 401). {fix} Provider said: {message}")
            }
            Self::Forbidden { message } => format!(
                "the API key is valid but not allowed to do this (HTTP 403) — check the key's \
                 workspace and model permissions. Provider said: {message}"
            ),
            Self::RateLimited {
                retry_hint,
                message,
                ..
            } => format!("rate limited (HTTP 429){retry_hint}. Provider said: {message}"),
            Self::BadRequest { message } => {
                format!("the API rejected the request as invalid (HTTP 400): {message}")
            }
            Self::Unavailable { status, message } => {
                format!("the API is unavailable (HTTP {status}): {message}")
            }
            Self::Api { status, message } => format!("the API returned HTTP {status}: {message}"),
            Self::Transport(e) => format!("could not reach the API: {e}"),
            Self::Protocol(e) => format!("the API sent a response this client could not read: {e}"),
        }
    }

    fn variant(&self) -> &'static str {
        match self {
            Self::Auth(_) => "Auth",
            Self::Unauthorized { .. } => "Unauthorized",
            Self::Forbidden { .. } => "Forbidden",
            Self::RateLimited { .. } => "RateLimited",
            Self::BadRequest { .. } => "BadRequest",
            Self::Unavailable { .. } => "Unavailable",
            Self::Api { .. } => "Api",
            Self::Transport(_) => "Transport",
            Self::Protocol(_) => "Protocol",
        }
    }
}

// The redaction lives here, in the impls, rather than at each site that builds
// a variant.
//
// The reason is that construction sites are where the guarantee kept failing.
// `AnthropicProvider` holds the key and scrubbed what it built; `Assembly::apply`
// and `turn_from_message` are free functions the key was never handed to, so
// they built `Api` variants out of raw provider prose — and the class-level
// promise on `LlmError` read as held while two paths did not hold it, until an
// audit went looking.
// Threading the key into those two functions would fix those two, and the third
// one somebody adds next year would leak again, silently, exactly as these did.
// Formatting is the one thing every variant from every site must pass through
// to reach a terminal or a log, so putting the scrub here makes the property
// structural rather than a habit each new call site has to remember.
//
// `Debug` is hand-written for the same reason: `unwrap_err()` in a test and
// `{:?}` in a log both print it, and a derived `Debug` would print the fields
// raw. It prints the variant name plus the redacted sentence, which is the
// structure a reader of `{:?}` actually wants.
//
// **What this does not cover.**
//
// - Reading a field directly. `err.message` is `pub` and still carries the
//   provider's bytes verbatim on the two paths above; only formatting is
//   guarded. `AnthropicProvider::classify` therefore still scrubs at
//   construction as well, so the stored field is clean on the path where a key
//   is in scope.
// - A key this process never wrapped in [`ApiKey`]. The scrub list is fed by
//   `ApiKey::new`, which every key Emma resolves goes through; a caller that
//   builds an auth header from a bare `String` is not covered.
// - An echo that is not byte-identical. A gateway that base64s, truncates or
//   re-cases the key defeats a substring replace, here and in [`redact`].
// - Forgetting. The list is process-wide and append-only, which is right for a
//   CLI that holds one key for its lifetime and is state a long-lived host
//   would have to think about.
impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&scrub_secrets(&self.sentence()))
    }
}

impl fmt::Debug for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.variant(), scrub_secrets(&self.sentence()))
    }
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

/// Every key this process has wrapped in an [`ApiKey`], so that a message built
/// somewhere the key was never passed can still be scrubbed on the way out.
///
/// Process-wide because the alternative is threading the key into every free
/// function that might one day format an error, which is the arrangement that
/// already failed twice. Append-only and never read except to scrub.
///
/// **Both accessors below recover a poisoned guard** rather than treating
/// poison as a failure, and the reason is specific to what is behind the lock.
///
/// Poison means some thread panicked while holding the guard. It says nothing
/// about the data — and a `Vec<String>` cannot be left in a state that is
/// unsafe to read. The only region a panic can interrupt is between the
/// membership check and the `push` below, and the worst outcome of that is a
/// key missing from the list, which is what a fresh `ApiKey::new` puts back.
/// There is no invariant here for poison to have broken.
///
/// The alternative is what this code did first, and it was a bug: propagating
/// the poison meant one unrelated panic anywhere in the process turned
/// redaction off for every error printed for the rest of the run — a security
/// control degrading silently to no control, on a condition nobody observes.
/// If the data here ever grows an invariant that a panic *could* break, this
/// must fail closed — redact everything — rather than go back to passing the
/// text through.
fn secrets() -> &'static RwLock<Vec<String>> {
    static SECRETS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Called by [`ApiKey::new`] — the one constructor every key in this process
/// passes through.
///
/// Short strings are ignored. A scrub list is a substring replace over every
/// error message the user ever sees, so an entry like `"x"` would corrupt them
/// all; a real key is far longer than this floor, and a "key" that is not is
/// not one worth protecting.
pub(crate) fn remember_secret(key: &str) {
    const MIN_LEN: usize = 8;
    if key.len() < MIN_LEN {
        return;
    }
    let mut list = secrets().write().unwrap_or_else(|e| e.into_inner());
    if !list.iter().any(|k| k == key) {
        list.push(key.to_string());
    }
}

/// [`redact`] against every key this process knows about.
pub(crate) fn scrub_secrets(text: &str) -> String {
    let list = secrets().read().unwrap_or_else(|e| e.into_inner());
    list.iter().fold(text.to_string(), |acc, k| redact(&acc, k))
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

// endregion: Errors, and keeping the key out of them

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The three claims worth pinning at this level: billable input counts all three
// fields, redaction replaces every occurrence rather than the first, and the
// retryable set is exactly what it says.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billable_counts_all_three_input_fields() {
        let usage = Usage {
            context_window: 0,
            input_tokens: 41,
            output_tokens: 17,
            cache_creation_input_tokens: 3_200,
            cache_read_input_tokens: 29_000,
            server_tool_use: Default::default(),
        };
        assert_eq!(usage.billable_input_tokens(), 41 + 3_200 + 29_000);
        assert_eq!(usage.billable_total_tokens(), 41 + 3_200 + 29_000 + 17);
        // The scar, stated as an assertion: the bare field is ~800× short here.
        assert!(usage.billable_input_tokens() > usage.input_tokens * 100);
    }

    /// **No formatting of `LlmError` contains an API key — every variant, both
    /// formatters.**
    ///
    /// The type's own doc makes that claim, and until 2026-08-23 the tests
    /// under it did not. What was covered was `redact` and `scrub_secrets`, the
    /// helpers, plus one variant through `Display` over in `auth.rs`. The
    /// guarantee is about the *type*: nine variants, two impls, eighteen ways
    /// out.
    ///
    /// The distinction is the one `DEF-007` was filed over in another file — a
    /// guard that is correct one layer away from where the untrusted thing
    /// actually arrives. Here the guard is in the right place; what was missing
    /// was anything that would notice if it moved.
    ///
    /// `Debug` matters as much as `Display` and is the easier one to lose: it
    /// is what `{:?}`, `unwrap()` and a panic message all reach for, and a
    /// derived `Debug` would print the struct fields raw. That is one
    /// `#[derive(Debug)]` away at any time.
    #[test]
    fn no_variant_of_llm_error_can_print_an_api_key() {
        const KEY: &str = "sk-ant-api03-THISMUSTNEVERAPPEARINOUTPUT";
        remember_secret(KEY);

        let payload = format!("the provider echoed {KEY} back in its error body");
        let variants = vec![
            LlmError::Unauthorized {
                fix: String::new(),
                message: payload.clone(),
            },
            LlmError::Forbidden {
                message: payload.clone(),
            },
            LlmError::RateLimited {
                retry_after: None,
                retry_hint: payload.clone(),
                message: payload.clone(),
            },
            LlmError::BadRequest {
                message: payload.clone(),
            },
            LlmError::Unavailable {
                status: 503,
                message: payload.clone(),
            },
            LlmError::Api {
                status: 500,
                message: payload.clone(),
            },
            LlmError::Transport(payload.clone()),
            LlmError::Protocol(payload.clone()),
        ];

        for e in &variants {
            let shown = format!("{e}");
            assert!(
                !shown.contains(KEY),
                "Display leaked the key for {}: {shown}",
                e.variant()
            );
            let debugged = format!("{e:?}");
            assert!(
                !debugged.contains(KEY),
                "Debug leaked the key for {}: {debugged}",
                e.variant()
            );
        }

        // **The anti-vacuity half.** Every assertion above is satisfied by an
        // empty string, and by a formatter that prints nothing at all. The
        // scrub must remove the key and keep the sentence.
        let shown = format!("{}", LlmError::Protocol(payload.clone()));
        assert!(
            shown.contains("echoed") && shown.contains("back in its error body"),
            "the scrub removed the message along with the key, which would make \
             every assertion above pass for the wrong reason: {shown}"
        );
        assert!(
            shown.contains("[redacted]") || shown.contains("redacted"),
            "the key vanished without a mark, so a reader cannot tell the \
             message was altered: {shown}"
        );
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
    fn a_poisoned_scrub_list_still_redacts() {
        // A panic anywhere while the list is held poisons it for the rest of
        // the process. If that turned redaction off, one unrelated panic would
        // silently disable a security control for every error printed
        // afterwards — the failure class this whole change exists to remove.
        const FAKE: &str = "sk-ant-api03-POISONEDLOCKFIXTURE";
        let _key = ApiKey::new(FAKE);

        // The panic is deliberate and its message is noise; keep it off stderr
        // so a passing run does not read like a failing one.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let poisoner = std::thread::spawn(|| {
            let _guard = secrets().write().unwrap();
            panic!("poison the scrub list");
        });
        assert!(poisoner.join().is_err(), "the poisoner must have panicked");
        std::panic::set_hook(hook);
        assert!(secrets().read().is_err(), "the lock must now be poisoned");

        // Registering after the poison must still work, and formatting must
        // still scrub — both directions of the lock.
        const LATER: &str = "sk-ant-api03-REGISTEREDAFTERTHEPOISON";
        let _later = ApiKey::new(LATER);
        let err = LlmError::Protocol(format!("gateway said x-api-key={FAKE} then {LATER}"));

        let shown = format!("{err}");
        assert!(!shown.contains(FAKE), "{shown}");
        assert!(!shown.contains(LATER), "{shown}");
        assert!(!format!("{err:?}").contains(FAKE), "{err:?}");
    }

    #[test]
    fn no_variant_can_print_a_key_whoever_built_it() {
        // Deliberately a variant nothing in `anthropic` scrubs, built here with
        // no provider involved: the point is that the guarantee belongs to the
        // type, so a site that never saw the key — including one written after
        // this test — cannot leak it.
        const FAKE: &str = "sk-ant-api03-NOTAREALKEYJUSTAFIXTURE";
        let _key = ApiKey::new(FAKE);
        let err = LlmError::Protocol(format!("gateway said x-api-key={FAKE}"));

        let shown = format!("{err}");
        assert!(!shown.contains(FAKE), "{shown}");
        assert!(shown.contains("[redacted]"), "{shown}");
        assert!(!format!("{err:?}").contains(FAKE), "{err:?}");
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

// endregion: Tests
