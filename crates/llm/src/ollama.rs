//! A local-model provider, and the second citizen that proves the provider
//! boundary is a boundary.
//!
//! Ported from the Mac branch, where it is the reason that branch exists.
//! Emma's stated direction is **model-agnostic**: Anthropic is not the assumed
//! provider, and a `Provider` implementation that is not Anthropic is the only
//! thing that can demonstrate the abstraction holds.
//!
//! **The one thing this must not repeat.** The branch takes its destination
//! from `OLLAMA_HOST` with no confirmation and no boot line, so a provider the
//! user believes is local can send the whole conversation to an arbitrary
//! remote host. Its own module doc concedes that this is "the setting that
//! actually matters here" and then does not surface it. A non-loopback host is
//! named at startup, or this does not ship.
//!
//! # Why this is a provider rather than a base-url swap
//!
//! [`crate::AnthropicProvider::with_base_url`] can point at anything, and that
//! is not enough: Ollama does not speak the Messages API. It has its own
//! `/api/chat`, and the differences are exactly the ones an agent loop depends
//! on. A tool definition is nested under `function` instead of carrying
//! `input_schema`; a tool call arrives in `message.tool_calls[]` with its
//! arguments already decoded rather than as a `tool_use` content block; a tool
//! result goes back as a whole message with `role: "tool"` rather than as a
//! `tool_result` block inside a user turn. Translating those is this file's
//! job.
//!
//! # Where the destination comes from, and how it is disclosed
//!
//! The default is loopback and says nothing. Anything else — a hostname, a LAN
//! address, a public one — produces a line from [`OllamaProvider::startup_notes`]
//! naming it, which the caller prints before the first call. That is a consent
//! *surface* and not a gate: nothing here refuses, because `CLAUDE.md` is
//! explicit that this project does not pretend to enforcement it does not have.
//! An attacker who can set your environment has already won; what the line
//! prevents is the honest mistake, where a stale `OLLAMA_HOST` in a shell
//! profile silently ships every file the model has read to a box on the other
//! side of a network the user forgot about.
//!
//! # What it deliberately does not do
//!
//! **No caching.** [`crate::Caching`] is a Messages-API billing feature. Ollama
//! re-reads the prompt every call, so the field is ignored rather than faked
//! and `Usage.cache_*` stay zero. Reporting cache hits that did not happen
//! would corrupt any cost figure computed from them — and `Usage`'s own doc
//! says a `0` there means "nothing cached", which is the truth here.
//!
//! **No effort levels.** [`crate::Effort`] has no equivalent. It is dropped,
//! not approximated: a model either has a thinking phase of its own or it does
//! not, and pretending to dial one would be a claim about behaviour this client
//! cannot make.
//!
//! **No API key.** Ollama on loopback is unauthenticated. See "where the
//! boundary did not fit" below for why that is not simply a matter of ignoring
//! the argument.
//!
//! # The trap that `num_predict` sets
//!
//! It caps thinking **and** answer together. A thinking model given a budget
//! sized for the answer alone spends it all on reasoning and returns empty
//! content, which reads as "the model cannot follow instructions" rather than
//! "the harness cut it off". [`Request::max_tokens`] is passed straight through
//! as `num_predict`, so a caller's budget must already account for thinking —
//! which is the same contract [`Request::max_tokens`] documents for the
//! Anthropic path, so no caller has to learn a second rule.
//!
//! # Where the boundary did not fit
//!
//! Three places, and they are the finding this port exists to produce. None is
//! fatal and none is fixable from inside this file.
//!
//! 1. **[`crate::ProviderKind::env_var`] means "the variable that outranks the
//!    stored key".** The Mac branch returns `OLLAMA_HOST` from it, which is not
//!    a key — and in *this* tree that is worse than untidy. `auth::load_default`
//!    reads that variable and, if it is set, wraps its value in
//!    [`crate::ApiKey`], whose constructor registers the string in the
//!    process-wide scrub list. The host would then be replaced by `[redacted]`
//!    in every error message for the rest of the run: the exact string this
//!    module exists to make visible, erased by the machinery meant to hide
//!    secrets. So [`Ollama::env_var`] names a key variable that Ollama does not
//!    use, and `a_host_is_never_returned_as_a_key_variable` holds that line.
//! 2. **There is no way for a provider to require no key.** `main.rs` calls
//!    `auth::load_default(kind)?` unconditionally, so registering this provider
//!    as it stands makes Emma refuse to start with `no API key for ollama`. The
//!    Mac branch had a `requires_key()` on the trait; this tree does not.
//!    [`OllamaProvider::requires_key`] is here as an inherent method so that
//!    moving it onto the trait is a one-line change rather than a redesign.
//! 3. **There is no way for a provider to say anything at startup.** The trait
//!    is `model_id` and `send`. Anthropic needs nothing — its endpoint is a
//!    constant — so nothing ever asked. [`OllamaProvider::startup_notes`] has
//!    the signature a default-bodied trait method would have, for the same
//!    reason.
//!
//! Everything else fitted. [`Request`]'s four parts, [`crate::ContentBlock`],
//! [`crate::ToolResult`], [`AssistantTurn`], [`Usage`], [`Event`] and [`Mode`]
//! all translated without contortion, and the two Anthropic-shaped knobs
//! (`caching`, `effort`) are ignorable because they are asks rather than
//! promises.
//!
//! # What is not verified here
//!
//! **No Ollama server was contacted.** Every wire claim below is made against a
//! loopback stub written in this file, which agrees with its author by
//! construction — the failure mode `CLAUDE.md` calls a false receipt, and the
//! one this crate has already paid for once. What would settle it: `ollama
//! serve` on the box, `emma --provider ollama` against a pulled model, and the
//! request bodies read off the wire. Until then the honest status of every
//! shape here is *believed*, not *certified*.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::retry::retry_after_seconds;
use crate::{
    trim_body, ApiKey, AssistantTurn, Content, ContentBlock, Event, LlmError, Message, Mode,
    Provider, ProviderKind, Request, Retry, Role, TextBlock, ThinkingBlock, ToolCall, Usage,
};

// region: The destination, and saying so
// ---------------------------------------------------------------------------
// The destination, and saying so
//
// Where the conversation goes, how a spelling of it is normalised, and the one
// question this module owes the user an answer to before the first call: is
// that address this machine?
// ---------------------------------------------------------------------------

/// Where Ollama listens unless something says otherwise.
///
/// The literal address rather than `localhost`, which is a *name* and resolves
/// through whatever the box's resolver believes — on Windows that is commonly
/// `::1` first, against a server bound only to `127.0.0.1`. A default that
/// depends on name resolution is a default that fails differently on different
/// machines.
const DEFAULT_HOST: &str = "http://127.0.0.1:11434";

/// The model assumed when the caller names none.
///
/// **This tag is unverified and carries no capability claim.** The Mac branch
/// defaulted to `gemma4:26b` and justified it with "the benchmark's value
/// table"; no such benchmark is in this tree, so the justification could not be
/// ported and the tag was not kept on the strength of it. What is here is a
/// long-standing tag from Ollama's own library, chosen because it is likely to
/// resolve — nothing in this repository has run `ollama list` against a real
/// server, and nobody here has measured any local model at all.
///
/// The failure mode when it is wrong is legible rather than silent: `/api/chat`
/// answers with a status and a body naming the model it could not find, which
/// surfaces as [`LlmError::Api`]. Override with `--model` or `emma set-model`.
const DEFAULT_MODEL: &str = "llama3.1:8b";

/// A local model is slow, and this is not a network timeout.
///
/// **The figure is not measured here.** The Mac branch justified 3600s with an
/// eleven-minute run on hardware this repository has no record of; that number
/// is not a fact of this tree and is not repeated as one. The value is kept
/// because of the direction the two errors point: a timeout sized like an API's
/// aborts correct work on a slow box and is indistinguishable from a broken
/// model, while a generous one only matters when the server has genuinely hung,
/// and the bound exists so that a hung server cannot wedge the loop forever.
/// What would settle a tighter figure: time a real long-form answer on the
/// target machine.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);

/// The floor for `num_ctx`, in tokens.
///
/// Ollama's own default is small enough that an agent conversation overruns it
/// within a few turns, and the overrun is silent — the server drops the
/// beginning of the prompt, so the model loses its instructions rather than
/// returning an error anyone could act on. A request never asks for less than
/// this.
///
/// **It used to be the whole of the answer, and that was the same silent loss
/// one level up.** Emma's default `max_context` is 120,000, so a request sized
/// against that cap arrived here, went out under a 32,768 window, and was
/// clipped at the front without a word — a flat constant chosen to be generous
/// against Ollama's default is still a cap when the caller's budget is nearly
/// four times larger. The Mac branch's measurement, which is what moved this:
/// an audit of 117 logged calls found 28 over the window, 8 of them showing the
/// plateau signature at ~32k. `num_ctx` is derived per request now and this
/// constant is only the lower bound.
const DEFAULT_NUM_CTX: u32 = 32_768;

/// Rounding step for a derived `num_ctx`, in tokens.
///
/// A window that moves by a handful of tokens on every call makes Ollama
/// re-allocate its KV cache for no benefit, so the derived value is rounded up
/// to a multiple of this and small growth in the conversation reuses the window
/// the previous turn already loaded.
const NUM_CTX_STEP: u32 = 4_096;

/// The ceiling on a derived `num_ctx`, in tokens.
///
/// A window is memory: Ollama allocates the KV cache for whatever is asked, so
/// an unbounded derivation lets one runaway request exhaust the host. Past
/// this, `OLLAMA_NUM_CTX` is the way to ask for more — deliberately, on a
/// machine somebody has checked.
const MAX_NUM_CTX: u32 = 262_144;

/// Characters per token, the same conservative ratio the rest of the workspace
/// estimates with ([`crate::anthropic`]'s packer, `agent.rs`'s weigh).
///
/// It under-counts what a tokeniser actually produces, which is the wrong
/// direction for a window; [`NUM_CTX_HEADROOM_FACTOR`] is what covers that.
const CHARS_PER_TOKEN: usize = 4;

/// How much slack goes on the estimated prompt before the answer's own budget,
/// as a divisor: a quarter again.
///
/// `chars / 4` under-counts, so the window has to be larger than the estimate
/// rather than equal to it. A quarter is enough for the gap this estimator
/// shows without doubling what the server allocates.
const NUM_CTX_HEADROOM_FACTOR: usize = 4;

/// How a host came to be the destination. Carried only so the disclosure line
/// can tell the user *where to go and change it* — a note that names an address
/// but not the setting behind it leaves them grepping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostSource {
    /// Nothing was set; [`DEFAULT_HOST`].
    Default,
    /// `OLLAMA_HOST`.
    Env,
    /// [`OllamaProvider::with_host`] — a caller said so in code or config.
    Explicit,
}

/// The host portion of a URL: no scheme, no userinfo, no port, no path.
///
/// Hand-written rather than pulled from a URL crate because the whole question
/// is "is this loopback", the answer must be conservative, and every shape this
/// cannot parse falls out as *not* loopback — which is the direction that
/// discloses rather than the direction that hides.
fn host_only(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // `rsplit_once`, not `split_once`: a password may contain an `@`, and the
    // last one is the delimiter.
    let authority = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    if let Some(inner) = authority.strip_prefix('[') {
        return inner.split(']').next().unwrap_or(inner);
    }
    // A bare IPv6 literal has no brackets and several colons, so the last colon
    // is not a port separator. Two or more colons means "this is an address",
    // and anything ambiguous stays whole and fails the loopback check.
    if authority.matches(':').count() >= 2 {
        return authority;
    }
    authority.split(':').next().unwrap_or(authority)
}

/// Whether an address is this machine.
///
/// `localhost` is included because it is what the Ollama CLI's own
/// documentation tells people to write, and treating the conventional spelling
/// of loopback as suspicious would make the disclosure line fire on the common
/// case — a warning that fires when nothing is wrong is a warning people learn
/// to skip, which is how the one that matters gets skipped too.
///
/// Everything else is false, including a name this function cannot resolve. It
/// does not resolve names: a DNS lookup here would be a network call made to
/// decide whether to warn about a network call, and a name that resolves to
/// `127.0.0.1` today can resolve elsewhere tomorrow without the string
/// changing.
fn is_loopback(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" {
        return true;
    }
    if let Ok(v6) = host.parse::<Ipv6Addr>() {
        // The mapped form matters: `::ffff:127.0.0.1` is loopback written the
        // long way, and a stack that hands it over is not doing anything
        // unusual.
        return v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback());
    }
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return v4.is_loopback();
    }
    false
}

/// The line the user sees before a single token is sent, or `None` when the
/// destination is this machine and there is nothing to disclose.
///
/// Plain text on purpose: it travels through the caller's ordinary reporting
/// path, which may be a pipe or a file, and the constraint that redirected
/// output contains zero escape bytes is a property of every string that reaches
/// it — including this one.
fn disclosure(url: &str, source: HostSource) -> Option<String> {
    if is_loopback(host_only(url)) {
        return None;
    }
    let where_from = match source {
        HostSource::Default => "This is the built-in default, which should not be possible; \
                                report it."
            .to_string(),
        HostSource::Env => {
            format!("It came from OLLAMA_HOST. Unset that variable to use {DEFAULT_HOST} instead.")
        }
        HostSource::Explicit => {
            format!(
                "It was configured explicitly, not read from the environment. The default is \
                     {DEFAULT_HOST}."
            )
        }
    };
    Some(format!(
        "ollama: this conversation — instructions, file contents and all — will be sent to \
         {url}, which is not this machine. {where_from}"
    ))
}

// endregion: The destination, and saying so

// region: The provider
// ---------------------------------------------------------------------------
// The provider
//
// Construction, the two settings read from the environment, and the inherent
// methods that would be trait methods if the trait had asked.
// ---------------------------------------------------------------------------

pub struct OllamaProvider {
    client: reqwest::Client,
    host: String,
    model: String,
    /// Computed once, at the moment the host is decided, so that a caller
    /// cannot hold a provider whose destination and whose disclosure disagree.
    note: Option<String>,
    retry: Retry,
}

/// Hand-written, and it shows the host on purpose.
///
/// [`ApiKey`]'s `Debug` hides its contents because printing a key is the
/// accident. Here the accident is the opposite one — a host nobody noticed — so
/// a `{:?}` of this struct in a log or a panic message names it.
impl std::fmt::Debug for OllamaProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaProvider")
            .field("host", &self.host)
            .field("model", &self.model)
            .finish()
    }
}

impl OllamaProvider {
    pub fn new(model: Option<String>) -> Self {
        let (host, source) = match std::env::var("OLLAMA_HOST")
            .ok()
            .filter(|h| !h.trim().is_empty())
        {
            Some(h) => (h, HostSource::Env),
            None => (DEFAULT_HOST.to_string(), HostSource::Default),
        };
        Self {
            client: reqwest::Client::new(),
            host: String::new(),
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            note: None,
            retry: Retry::default(),
        }
        .set_host(host, source)
    }

    /// Point at a specific server. Used by the stub tests, and by any caller
    /// that resolves a host from settings rather than the environment.
    pub fn with_host(self, host: impl Into<String>) -> Self {
        self.set_host(host.into(), HostSource::Explicit)
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    /// The one place `host` is written, which is what makes the disclosure
    /// impossible to skip: there is no route to a destination that did not pass
    /// through the classifier.
    fn set_host(mut self, host: String, source: HostSource) -> Self {
        // `OLLAMA_HOST` is conventionally set bare, as `box.lan:11434`, by the
        // CLI's own documentation. Accepting that spelling costs one branch and
        // saves a confusing refusal — and the scheme is added *before* the
        // classifier runs, so the two see the same string the request will use.
        let host = host.trim().to_string();
        let host = if host.starts_with("http://") || host.starts_with("https://") {
            host
        } else {
            format!("http://{host}")
        };
        let host = host.trim_end_matches('/').to_string();
        self.note = disclosure(&host, source);
        self.host = host;
        self
    }

    /// Anything the user must be told about this provider's configuration
    /// before the first call.
    ///
    /// **This is the shape a trait method would have.** [`Provider`] has no
    /// startup hook — see the module doc — so this is inherent, and a caller
    /// holding an `Arc<dyn Provider>` cannot reach it. Wiring it is one
    /// defaulted method on the trait and one loop in `main.rs`;
    /// `the_provider_is_not_registered_until_its_host_is_announced` fails the
    /// build if the provider is registered before that happens.
    pub fn startup_notes(&self) -> Vec<String> {
        self.note.iter().cloned().collect()
    }

    /// Whether a key must be resolved before this provider can be built.
    ///
    /// Inherent for the same reason as [`OllamaProvider::startup_notes`]: the
    /// trait has no such question, and `main.rs` resolves a key
    /// unconditionally. See the module doc, point 2.
    pub fn requires_key(&self) -> bool {
        false
    }

    fn endpoint(&self) -> String {
        format!("{}/api/chat", self.host)
    }

    /// The window this request will be sent under.
    ///
    /// `OLLAMA_NUM_CTX` wins outright when it names a usable value, because a
    /// person who names a window has a reason the derivation cannot see — a
    /// model that will not load at a larger one, or a host with less memory
    /// than this assumes. Otherwise the window is derived from the request, so
    /// a long conversation gets a window that holds it instead of being clipped
    /// at a constant.
    ///
    /// Read per call rather than cached. The variable can change between calls
    /// in a long-lived process, and the read costs nothing beside an HTTP
    /// request to a model that is about to think for a minute.
    fn num_ctx(&self, request: &Request) -> u32 {
        match parse_num_ctx(std::env::var("OLLAMA_NUM_CTX").ok().as_deref()) {
            Some(asked) => asked,
            None => derive_num_ctx(estimate_prompt_tokens(request), request.max_tokens),
        }
    }
}

/// The parsing half of [`OllamaProvider::num_ctx`], split out so it can be
/// tested.
///
/// The env read stays in the caller because `set_var` is process-global: on the
/// Mac branch three tests mutating `OLLAMA_NUM_CTX` raced, and which one failed
/// depended on thread interleaving. A pure function has no ordering to get
/// wrong.
///
/// `None` means "nobody said", not "use the floor": an unusable value has to be
/// indistinguishable from an absent one so that a typo falls through to the
/// derivation rather than pinning the window at the constant the derivation
/// exists to replace.
fn parse_num_ctx(raw: Option<&str>) -> Option<u32> {
    raw.and_then(|v| v.parse().ok()).filter(|n| *n > 0)
}

/// What one request weighs, in tokens, by `chars / CHARS_PER_TOKEN`.
///
/// Every part of the request that crosses the wire is counted, not just the
/// conversation: the system prompt and the tool schemas are prompt tokens too,
/// and a window sized against the messages alone clips on exactly the calls
/// carrying the largest tool surface — the ones where the model most needs to
/// see what it may call.
fn estimate_prompt_tokens(request: &Request) -> usize {
    let messages: usize = request
        .history
        .iter()
        .chain(request.query.iter())
        .map(|m| m.content.wire_len())
        .sum();
    let tools: usize = request.tools.iter().map(|t| t.to_string().len()).sum();
    (request.instructions.len() + messages + tools) / CHARS_PER_TOKEN
}

/// The window that holds `prompt` tokens of input and `answer` tokens of output.
///
/// Both halves, because Ollama's `num_ctx` covers the prompt and the generation
/// together: a window sized to the prompt alone truncates the front of the
/// conversation as the answer is written, which is the same silent loss arriving
/// a few hundred tokens later.
fn derive_num_ctx(prompt: usize, answer: u32) -> u32 {
    let want = prompt
        .saturating_add(prompt / NUM_CTX_HEADROOM_FACTOR)
        .saturating_add(answer as usize);
    // A `usize` that does not fit a `u32` is already past the ceiling, so the
    // saturating conversion and the clamp agree.
    let want = u32::try_from(want).unwrap_or(MAX_NUM_CTX);
    let stepped = want
        .checked_next_multiple_of(NUM_CTX_STEP)
        .unwrap_or(MAX_NUM_CTX);
    stepped.clamp(DEFAULT_NUM_CTX, MAX_NUM_CTX)
}

// endregion: The provider

// region: Emma's shapes to Ollama's
// ---------------------------------------------------------------------------
// Emma's shapes to Ollama's
//
// Tools, messages, and the one restructuring that is easy to get wrong: a tool
// result stops being a block inside a turn and becomes a message of its own.
// ---------------------------------------------------------------------------

/// Anthropic tool definitions to Ollama's.
///
/// Emma renders its tool surface once, in Anthropic's shape, and every provider
/// translates from there. The schema itself is JSON Schema in both, so only the
/// envelope moves: `input_schema` becomes `parameters`, nested under
/// `function`.
fn tools_to_ollama(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let name = t.get("name").cloned().unwrap_or(Value::Null);
            let description = t.get("description").cloned().unwrap_or(Value::Null);
            let parameters = t
                .get("input_schema")
                .or_else(|| t.get("parameters"))
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            json!({
                "type": "function",
                "function": {"name": name, "description": description, "parameters": parameters}
            })
        })
        .collect()
}

/// Emma's messages to Ollama's.
///
/// **A tool result becomes its own message.** In the Messages API a result is a
/// `tool_result` block inside a *user* turn, so one Emma message can carry
/// several of them. Ollama wants one message per result, with `role: "tool"`.
/// Flattening that is why this returns a `Vec` per input message rather than
/// mapping one to one, and getting it wrong makes the model answer as though
/// the tool never ran.
fn messages_to_ollama(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        match &m.content {
            Content::Text(t) => out.push(json!({"role": role, "content": t})),
            Content::Blocks(blocks) => {
                // Every `ToolUse` id in this message, so a `ToolResult` can find
                // the *name* of the call it answers: Ollama pairs a result to a
                // call by `tool_name`, where Emma pairs by id.
                let use_by_id: std::collections::HashMap<&str, &str> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse(c) => Some((c.id.as_str(), c.name.as_str())),
                        _ => None,
                    })
                    .collect();

                let mut text = String::new();
                let mut calls = Vec::new();
                for b in blocks {
                    match b {
                        ContentBlock::Text(t) => text.push_str(&t.text),
                        ContentBlock::Thinking(_) | ContentBlock::RedactedThinking(_) => {
                            // Not echoed back. There is no signed-thinking
                            // contract here to satisfy — the whole reason
                            // `ThinkingBlock::signature` exists on the Anthropic
                            // path — and replaying a previous turn's reasoning
                            // as if it were input has no defined meaning to
                            // Ollama.
                        }
                        ContentBlock::ToolUse(c) => {
                            let mut call = Map::new();
                            call.insert("id".into(), json!(c.id));
                            call.insert("type".into(), json!("function"));
                            call.insert(
                                "function".into(),
                                json!({"name": c.name, "arguments": c.input}),
                            );
                            // Keys the block carried that this client does not
                            // model travel back out, same rule as `content.rs`.
                            for (k, v) in &c.extra {
                                call.entry(k.clone()).or_insert_with(|| v.clone());
                            }
                            calls.push(Value::Object(call));
                        }
                        ContentBlock::ToolResult(r) => {
                            // Emitted here rather than collected, so ordering
                            // survives: a result must follow the call it
                            // answers.
                            let tool_name = use_by_id
                                .get(r.tool_use_id.as_str())
                                .copied()
                                .unwrap_or(r.tool_use_id.as_str());
                            out.push(json!({
                                "role": "tool",
                                "content": r.content,
                                "tool_name": tool_name,
                            }));
                        }
                        // Whatever this is, it is a shape this client could not
                        // model on the Anthropic wire, so it has no translation
                        // here either. Dropped rather than forwarded: sending
                        // Anthropic's bytes to Ollama would not mean anything.
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

// endregion: Emma's shapes to Ollama's

// region: Ollama's reply to a turn
// ---------------------------------------------------------------------------
// Ollama's reply to a turn
//
// Reading a reply, tolerantly. This is the half with the scar on it: a strict
// decoder in this crate once passed every test and then decoded no real tool
// call, because the provider put a key on the block that the client had never
// heard of.
// ---------------------------------------------------------------------------

/// The arguments of one tool call, read tolerantly.
///
/// **Two near-miss shapes are accepted here rather than downstream.** A cursor
/// read-only audit on the Mac branch, 2026-08-26, measured both against local
/// models; in each the call's intent was unambiguous while the error the user
/// saw pointed somewhere else entirely.
///
/// 1. `arguments` serialised to a JSON *string* rather than nested, so the tool
///    layer rejected a perfectly good call with "takes a JSON object, got a
///    string".
/// 2. The key spelled `args` or `parameters`, so the lookup missed, the input
///    defaulted to `{}`, and the tool blamed the model for a missing
///    `file_path` that had in fact been supplied.
///
/// Neither tolerance can reach a call that was already correct. The alias
/// lookup runs only once `arguments` is absent, so a present `arguments` always
/// wins and the keys are never merged; the string decode replaces the value
/// only when it parses to an *object*, so a string that is not JSON, or that
/// encodes a scalar or an array, is handed on unchanged. That last part is
/// deliberate — the downstream "got a string" error naming the real payload is
/// a better diagnosis than an invented `{}` that turns a protocol fault into a
/// bogus missing-field complaint.
fn tool_call_input(f: &Value) -> Value {
    let raw = ["arguments", "args", "parameters"]
        .iter()
        .find_map(|k| f.get(*k))
        .cloned()
        .unwrap_or_else(|| json!({}));

    if let Value::String(encoded) = &raw {
        if let Ok(Value::Object(decoded)) = serde_json::from_str::<Value>(encoded) {
            return Value::Object(decoded);
        }
    }
    raw
}

/// One `tool_calls[]` entry to a [`ToolCall`].
///
/// **Ids are synthesised.** The Messages API gives every `tool_use` an id and
/// the matching result quotes it; Ollama's tool calls carry no id at all in the
/// shapes observed on the Mac branch. Emma pairs a result to its call by that
/// id, so one is minted per call from the position and the tool name. It is
/// stable within a turn, which is the only place the pairing is read. An id the
/// server *did* send is preferred over the synthetic one, because a server that
/// sends one will expect it back.
///
/// **Unknown keys are kept, and that is not decoration.** `content.rs` records
/// what strict decoding cost this crate: the Messages API puts a `caller` key
/// on every real `tool_use`, the decoder demoted the block, the loop saw no
/// tool calls, and a whole goal ended reported as "answered — no tools were
/// needed" with a green suite behind it. The same class of thing is arriving
/// here — `index` is already on some servers' entries — so anything not read is
/// carried in [`ToolCall::extra`] rather than being a reason to reject the
/// call.
fn tool_call_from(call: &Value, position: usize) -> ToolCall {
    // Some servers nest under `function`, some flatten. `unwrap_or(call)` reads
    // both without either shape being the special case.
    let f = call.get("function").unwrap_or(call);
    let name = f
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("ollama-{position}-{name}"));

    // Everything on the entry and on its `function` object that this client did
    // not read. `id`/`type` are dropped because they are the envelope rather
    // than model output.
    let mut extra = Map::new();
    for (source, skip) in [
        (call, ["id", "type", "function"].as_slice()),
        (f, ["name", "arguments", "args", "parameters"].as_slice()),
    ] {
        if let Some(obj) = source.as_object() {
            for (k, v) in obj {
                if !skip.contains(&k.as_str()) {
                    extra.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
    }

    ToolCall {
        id,
        name,
        input: tool_call_input(f),
        extra,
    }
}

/// A whole `/api/chat` reply to an [`AssistantTurn`].
///
/// Streaming and batch both land here, because the streaming reader assembles
/// the frames into a body of exactly this shape rather than growing a second
/// decoder. Two decoders for one wire format is two things that can disagree
/// about what the model said, and `AssistantTurn::text` exists in `lib.rs`
/// because that lesson was already learned one layer up.
fn turn_from_ollama(body: &Value) -> Result<AssistantTurn, LlmError> {
    let message = body
        .get("message")
        .ok_or_else(|| LlmError::Protocol("ollama reply had no `message` field".to_string()))?;

    let mut content = Vec::new();
    if let Some(thinking) = message.get("thinking").and_then(Value::as_str) {
        if !thinking.is_empty() {
            content.push(ContentBlock::Thinking(ThinkingBlock {
                thinking: thinking.to_string(),
                // No signature, and none invented. `ThinkingBlock::signature`
                // is `Option` precisely so that "this model does not sign its
                // reasoning" is representable rather than faked with `""`.
                signature: None,
                extra: Map::new(),
            }));
        }
    }
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            content.push(ContentBlock::Text(TextBlock {
                text: text.to_string(),
                extra: Map::new(),
            }));
        }
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (i, call) in calls.iter().enumerate() {
            content.push(ContentBlock::ToolUse(tool_call_from(call, i)));
        }
    }

    // **`done_reason` is not `stop_reason`.** The loop branches on `"tool_use"`
    // to decide whether to keep going, and Ollama says `"stop"` even in the
    // reply that carried tool calls. Deriving it from the content is the only
    // reading that keeps the loop correct.
    let has_calls = content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse(_)));
    let stop_reason = if has_calls {
        "tool_use".to_string()
    } else {
        match body.get("done_reason").and_then(Value::as_str) {
            Some("length") => "max_tokens".to_string(),
            _ => "end_turn".to_string(),
        }
    };

    let usage = Usage {
        // Left at zero here, and stamped by `send` with the window it asked
        // for. The reply carries no `num_ctx` of its own — Ollama echoes
        // nothing about the window — so a parser reading only the body has no
        // honest number to put here, and a guess is worse than "not reported".
        context_window: 0,
        input_tokens: body
            .get("prompt_eval_count")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        output_tokens: body.get("eval_count").and_then(Value::as_i64).unwrap_or(0),
        // No prompt cache here. Zero rather than invented — `Usage`'s own doc
        // says a `0` in these fields means "nothing cached", never "unknown",
        // and that is exactly true of every Ollama call.
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        // Ollama searches nothing for the model. Zero is the truth, not a
        // placeholder.
        server_tool_use: Default::default(),
    };

    Ok(AssistantTurn {
        content,
        stop_reason,
        usage,
    })
}

// endregion: Ollama's reply to a turn

// region: The call
// ---------------------------------------------------------------------------
// The call
//
// One request, its retries, and the two ways the bytes come back. Streaming is
// here rather than stubbed because a local model is the case where it matters
// most: the answer takes minutes, and a caller that asked to watch it arrive
// and got silence instead cannot tell the run from a hang.
// ---------------------------------------------------------------------------

async fn notify(events: Option<&mpsc::Sender<Event>>, event: Event) {
    if let Some(tx) = events {
        // Best effort: a closed receiver means the turn is being cancelled,
        // which the caller already knows about.
        let _ = tx.send(event).await;
    }
}

/// A failed response to the error that says what to do about it.
///
/// The 500 arm is the one with a story. Ollama renders tool calls through the
/// model's own chat template and then parses what came back, so a model whose
/// template is XML-shaped answers `HTTP 500: XML syntax error … element
/// <function> closed by </parameter>` when generated arguments contain angle
/// brackets — which Rust generics produce constantly. The Mac branch measured
/// it on 2026-08-24 and lost a whole goal to it being classified as fatal. The
/// next sample usually does not trip it, so it is transient in practice even
/// though the status suggests a broken server, and [`LlmError::Unavailable`] is
/// the variant that gets it another attempt.
async fn classify(resp: reqwest::Response) -> LlmError {
    let status = resp.status().as_u16();
    let retry_after = retry_after_seconds(
        resp.headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
    );
    let body = resp.text().await.unwrap_or_default();
    let message = trim_body(&body);
    match status {
        // Unauthenticated on loopback, but a reverse proxy in front of a shared
        // server is a real deployment, and its 401/403/429 mean here what they
        // mean anywhere.
        401 => LlmError::Unauthorized {
            fix: "Ollama itself asks for no key; whatever answered 401 sits in front of it. \
                  Check OLLAMA_HOST and the proxy or gateway at that address."
                .into(),
            message,
        },
        403 => LlmError::Forbidden { message },
        429 => LlmError::RateLimited {
            retry_after,
            retry_hint: String::new(),
            message,
        },
        400 => LlmError::BadRequest { message },
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

async fn read_batch(resp: reqwest::Response) -> Result<AssistantTurn, LlmError> {
    let raw = resp
        .text()
        .await
        .map_err(|e| LlmError::Transport(e.to_string()))?;
    let body: Value = serde_json::from_str(&raw)
        .map_err(|e| LlmError::Protocol(format!("{e}: {}", trim_body(&raw))))?;
    turn_from_ollama(&body)
}

/// Read the NDJSON stream and assemble the body a batch call would have
/// returned.
///
/// One JSON object per line, each carrying a slice of `message`; the last
/// carries `done: true` and the counts. Rather than decode into an
/// `AssistantTurn` directly, this rebuilds the batch shape and hands it to
/// [`turn_from_ollama`], so the two modes cannot drift apart —
/// `streaming_assembles_the_same_turn_as_batch` is the assertion, and it is
/// only meaningful because there is one decoder under both.
///
/// A malformed line is skipped rather than fatal. Bytes have already reached
/// the user's terminal by the time one arrives, so the choice is between losing
/// a fragment and losing the turn.
async fn read_stream(
    resp: reqwest::Response,
    events: Option<&mpsc::Sender<Event>>,
) -> Result<AssistantTurn, LlmError> {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut tail = json!({});
    let mut saw_any = false;

    while let Some(item) = stream.next().await {
        let bytes = item.map_err(|e| LlmError::Transport(e.to_string()))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(idx) = buf.find('\n') {
            let line = buf[..idx].to_string();
            buf.drain(..idx + 1);
            apply_frame(
                &line,
                &mut text,
                &mut thinking,
                &mut calls,
                &mut tail,
                &mut saw_any,
                events,
            )
            .await;
        }
    }
    // A final line with no trailing newline is a whole frame, and it is the one
    // carrying the counts.
    let rest = std::mem::take(&mut buf);
    apply_frame(
        &rest,
        &mut text,
        &mut thinking,
        &mut calls,
        &mut tail,
        &mut saw_any,
        events,
    )
    .await;

    if !saw_any {
        return Err(LlmError::Protocol(
            "ollama streamed no readable frames".to_string(),
        ));
    }

    let mut message = json!({"role": "assistant", "content": text});
    if !thinking.is_empty() {
        message["thinking"] = json!(thinking);
    }
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    let mut body = tail;
    body["message"] = message;
    turn_from_ollama(&body)
}

/// Fold one NDJSON line into the accumulators, narrating what a human would
/// want to see arrive.
#[allow(clippy::too_many_arguments)] // Six accumulators and a sender; the
                                     // alternative is a struct whose only
                                     // purpose is to be this argument list.
async fn apply_frame(
    line: &str,
    text: &mut String,
    thinking: &mut String,
    calls: &mut Vec<Value>,
    tail: &mut Value,
    saw_any: &mut bool,
    events: Option<&mpsc::Sender<Event>>,
) {
    if line.trim().is_empty() {
        return;
    }
    let Ok(frame) = serde_json::from_str::<Value>(line) else {
        return;
    };
    *saw_any = true;
    if let Some(message) = frame.get("message") {
        if let Some(piece) = message.get("content").and_then(Value::as_str) {
            if !piece.is_empty() {
                text.push_str(piece);
                notify(events, Event::TextDelta(piece.to_string())).await;
            }
        }
        if let Some(piece) = message.get("thinking").and_then(Value::as_str) {
            thinking.push_str(piece);
        }
        if let Some(new) = message.get("tool_calls").and_then(Value::as_array) {
            for call in new {
                // The position is the one `turn_from_ollama` will use for the
                // synthetic id, so what the caller is told a call is named
                // matches what the turn ends up carrying.
                let decoded = tool_call_from(call, calls.len());
                calls.push(call.clone());
                notify(
                    events,
                    Event::ToolUseStarted {
                        id: decoded.id,
                        name: decoded.name,
                    },
                )
                .await;
            }
        }
    }
    // The last frame carries `done_reason` and the counts. Keeping the whole
    // frame rather than picking fields means a count added later arrives
    // without a change here.
    if frame.get("done").and_then(Value::as_bool) == Some(true) {
        *tail = frame;
    }
}

#[async_trait::async_trait]
impl Provider for OllamaProvider {
    fn model_id(&self) -> &str {
        &self.model
    }

    /// The host, when it is not the loopback default.
    ///
    /// This is the whole reason the trait grew a startup hook. The inherent
    /// method below is kept because the stub tests hold an `OllamaProvider`
    /// rather than an `Arc<dyn Provider>`.
    fn startup_notes(&self) -> Vec<String> {
        OllamaProvider::startup_notes(self)
    }

    async fn send(
        &self,
        request: Request,
        mode: Mode,
        events: Option<mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        // Derived once, outside the retry loop, so that every attempt and the
        // number finally reported are the same window. What a call ran under is
        // the difference between "the model ignored the top of the
        // conversation" and "the model never saw it", and from a session log
        // alone the two are indistinguishable until this is recorded.
        let num_ctx = self.num_ctx(&request);
        let body = self.body(&request, mode, num_ctx);
        let events = events.as_ref();
        let mut attempt: u32 = 1;
        loop {
            let sent = self
                .client
                .post(self.endpoint())
                .timeout(REQUEST_TIMEOUT)
                .json(&body)
                .send()
                .await;

            let failure = match sent {
                Ok(resp) if resp.status().is_success() => {
                    // Past here bytes may already have reached the user, so a
                    // mid-stream failure is surfaced rather than retried — a
                    // retry would replay text the terminal has printed. Same
                    // rule as the Anthropic path.
                    // Stamped here rather than in the parser: the reply carries
                    // no window, so the only honest source is the number this
                    // client asked for.
                    return match mode {
                        Mode::Batch => read_batch(resp).await,
                        Mode::Stream => read_stream(resp, events).await,
                    }
                    .map(|mut turn| {
                        turn.usage.context_window = i64::from(num_ctx);
                        turn
                    });
                }
                Ok(resp) => classify(resp).await,
                Err(e) => LlmError::Transport(e.to_string()),
            };

            let Some(delay) = self.retry.delay_for(&failure, attempt) else {
                return Err(failure);
            };
            // **A retry the user cannot see is a hang.** The Mac branch's retry
            // loop was silent, which on a local model means minutes of nothing
            // followed by an answer, and a user who hits Ctrl-C loses the work.
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
}

impl OllamaProvider {
    /// The request body, in [`Request`]'s prefix order.
    ///
    /// The ordering is not load-bearing here the way it is on the Anthropic
    /// path — there is no prefix cache to invalidate — but it is kept because
    /// the instructions genuinely belong first, and because a reader comparing
    /// the two providers should not have to work out whether a difference is
    /// meaningful.
    ///
    /// `num_ctx` is passed in rather than computed here because the caller has
    /// to report the same number on [`Usage::context_window`], and a body that
    /// derived its own would let the two drift.
    fn body(&self, request: &Request, mode: Mode, num_ctx: u32) -> Value {
        let mut messages = vec![json!({"role": "system", "content": request.instructions})];
        messages.extend(messages_to_ollama(&request.history));
        messages.extend(messages_to_ollama(&request.query));

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": mode == Mode::Stream,
            "options": {
                // Thinking and answer share this budget. See the module doc.
                "num_predict": request.max_tokens,
                "num_ctx": num_ctx,
            },
        });
        // Absent rather than empty: a server that sees `tools: []` may still
        // switch to its tool-calling template, and a model with no tools
        // offered should be answering prose.
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(tools_to_ollama(&request.tools));
        }
        body
    }
}

// endregion: The call

// region: Identity
// ---------------------------------------------------------------------------
// Identity
//
// What `emma set-provider ollama` looks up. Registered in `KINDS` since
// 2026-08-31, once `Provider::startup_notes` and `ProviderKind::requires_key`
// existed and `main.rs` used both — the two conditions the test at the bottom
// of this file and the module doc's points 2 and 3 were holding out for.
// ---------------------------------------------------------------------------

/// Identity, for the provider registry.
pub struct Ollama;

impl ProviderKind for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    /// **Not `OLLAMA_HOST`, and that is the point.**
    ///
    /// This method means "the variable that outranks the stored *key*", and
    /// `auth::load_default` wraps whatever it finds there in an [`ApiKey`] —
    /// whose constructor registers the string for scrubbing from every error
    /// message this process ever prints. Returning the host variable would
    /// therefore redact the host from the output, which is the precise opposite
    /// of what this module exists to do.
    ///
    /// The name returned is one Ollama itself does not use. A user who exports
    /// it is authenticating against a reverse proxy, which is the only
    /// situation in which an Ollama deployment has a key at all.
    fn env_var(&self) -> &'static str {
        "OLLAMA_API_KEY"
    }

    fn default_model(&self) -> &'static str {
        DEFAULT_MODEL
    }

    /// No key, and this is what made registering possible.
    ///
    /// `main.rs` used to resolve a key unconditionally, so adding this provider
    /// to `KINDS` made Emma refuse to start with `no API key for ollama` — a
    /// local model that needs no credential, blocked by a credential check.
    fn requires_key(&self) -> bool {
        false
    }

    fn build(&self, _key: ApiKey, model: Option<String>) -> Arc<dyn Provider> {
        // The key is accepted and dropped. Ollama on loopback is
        // unauthenticated, and forwarding a key that was resolved for a
        // *different* provider to a host the user may not control is worse than
        // ignoring it. See the module doc, point 2, for why this argument
        // exists at all.
        Arc::new(OllamaProvider::new(model))
    }
}

// endregion: Identity

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Three groups. Pure translation, which needs no server; the disclosure, which
// is the security requirement; and the wire, against a loopback stub.
//
// **The stub agrees with its author.** It was written here from the Mac
// branch's reading of Ollama's API, and no Ollama server was contacted. It can
// prove that this client is self-consistent and that a guarantee has a test on
// it. It cannot prove that a real server sends these bytes, and this crate has
// already shipped a decoder that passed every fixture and read no real tool
// call.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolResult;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // region: context size

    /// `None` is the whole point of the signature: an absent or unusable
    /// `OLLAMA_NUM_CTX` has to fall through to the derivation, not to the
    /// floor. If this returned `DEFAULT_NUM_CTX` for `None` again, every
    /// request would go out under the flat constant the derivation replaced,
    /// and the clip would be back with the tests still green.
    #[test]
    fn an_absent_or_unusable_num_ctx_leaves_the_derivation_to_decide() {
        assert_eq!(parse_num_ctx(None), None);
        assert_eq!(parse_num_ctx(Some("8192")), Some(8_192));
        // A typo, and a zero that would make the server drop the whole prompt.
        assert_eq!(parse_num_ctx(Some("not-a-number")), None);
        assert_eq!(parse_num_ctx(Some("0")), None);
    }

    /// A short conversation is not given a smaller window than Ollama's own
    /// default beats: the floor is still a floor.
    #[test]
    fn a_small_request_still_gets_the_floor() {
        assert_eq!(derive_num_ctx(10, 100), DEFAULT_NUM_CTX);
    }

    /// **The defect this derivation exists for.** Emma's default `max_context`
    /// is 120,000; under the old flat constant a request that size went out
    /// under a 32,768 window and Ollama dropped the front of it without a word.
    /// Delete the derivation — send `DEFAULT_NUM_CTX` — and this goes red.
    #[test]
    fn a_request_larger_than_the_old_default_gets_a_window_that_holds_it() {
        let window = derive_num_ctx(120_000, 32_000);
        assert!(
            window >= 120_000 + 32_000,
            "a 120,000 token prompt with a 32,000 token answer got a {window} window"
        );
        assert_eq!(window % NUM_CTX_STEP, 0, "{window} is not on the step");
    }

    /// A window is memory Ollama allocates, so the derivation is bounded even
    /// when the arithmetic is not.
    #[test]
    fn a_derived_window_is_capped_rather_than_unbounded() {
        assert_eq!(derive_num_ctx(usize::MAX, 32_000), MAX_NUM_CTX);
    }

    /// Everything that crosses the wire is counted, not only the messages: a
    /// window sized against the conversation alone clips on exactly the calls
    /// carrying the largest tool surface.
    #[test]
    fn the_estimate_counts_the_instructions_and_the_tool_schemas() {
        let bare = Request::new("x".repeat(4_000), Vec::new());
        let mut loaded = Request::new("x".repeat(4_000), vec![json!({"name": "y".repeat(4_000)})]);
        loaded.query = vec![Message::user("z".repeat(4_000))];
        assert!(estimate_prompt_tokens(&bare) >= 1_000);
        assert!(estimate_prompt_tokens(&loaded) >= estimate_prompt_tokens(&bare) + 2_000);
    }

    /// A window that moved by a few tokens per call would make Ollama
    /// re-allocate its KV cache on every turn. Small growth lands on the window
    /// the previous turn already loaded.
    #[test]
    fn small_growth_reuses_the_same_window() {
        assert_eq!(derive_num_ctx(60_000, 4_000), derive_num_ctx(60_010, 4_000));
    }

    // endregion: context size

    // region: the disclosure

    /// **If this breaks:** a user with a stale `OLLAMA_HOST` in a shell profile
    /// ships their instructions, their file contents and their whole
    /// conversation to a box on someone else's network, from a provider whose
    /// documentation says "on this machine", and nothing anywhere says so.
    ///
    /// The two halves are separate claims and both are load-bearing. Loopback
    /// must stay silent, or the line is noise people learn to skip; anything
    /// else must produce a line that names the address, or there is no
    /// disclosure at all.
    #[test]
    fn a_host_that_is_not_this_machine_is_named_and_a_loopback_one_is_not() {
        for quiet in [
            "http://127.0.0.1:11434",
            "http://127.1.2.3:11434",
            "http://localhost:11434",
            "http://LOCALHOST:11434",
            "http://[::1]:11434",
            "http://[::ffff:127.0.0.1]:11434",
            "https://localhost",
            DEFAULT_HOST,
        ] {
            assert_eq!(
                disclosure(quiet, HostSource::Env),
                None,
                "a loopback host was announced: {quiet}"
            );
        }

        for loud in [
            // Documentation-range addresses (RFC 5737), like the `example.com`
            // and `2001:db8::` neighbours below, so nothing in this list can
            // be mistaken for a machine somebody owns.
            "http://198.51.100.50:11434",
            "http://203.0.113.7:11434",
            "https://ollama.example.com",
            "http://box.lan:11434",
            "http://[2001:db8::1]:11434",
            // Not loopback, whatever the userinfo says.
            "http://localhost@evil.example/api",
            // `0.0.0.0` is a bind address, not a destination; a request sent to
            // it is not something this client can vouch for.
            "http://0.0.0.0:11434",
        ] {
            let note = disclosure(loud, HostSource::Env).unwrap_or_else(|| {
                panic!("a non-loopback host was not announced: {loud}");
            });
            assert!(
                note.contains(loud),
                "the note must name the address it is about: {note}"
            );
            assert!(
                note.contains("OLLAMA_HOST"),
                "the note must name the setting to change: {note}"
            );
            // The line goes through the caller's ordinary reporting, which may
            // be a pipe or a file. Redirected output contains zero escape
            // bytes, and that is a property of every string that reaches it.
            assert!(
                !note.contains('\u{1b}') && !note.contains('\r'),
                "the note carried escape bytes: {note:?}"
            );
        }
    }

    /// **If this breaks:** the disclosure exists and nothing asks for it,
    /// because the only route to a host bypassed the classifier.
    ///
    /// `set_host` is the single writer. This asserts the property through the
    /// two public doors rather than through that function, so a third door
    /// added later has to go through it too or fail here.
    #[test]
    fn a_provider_carries_the_note_for_the_host_it_actually_holds() {
        let local = OllamaProvider::new(None).with_host("127.0.0.1:11434");
        assert!(local.startup_notes().is_empty(), "{:?}", local);
        assert_eq!(local.endpoint(), "http://127.0.0.1:11434/api/chat");

        let remote = OllamaProvider::new(None).with_host("box.lan:11434");
        let notes = remote.startup_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("http://box.lan:11434"), "{notes:?}");
        // The bare spelling the Ollama CLI documents is normalised before the
        // classifier sees it, so the note and the request agree on the string.
        assert_eq!(remote.endpoint(), "http://box.lan:11434/api/chat");

        // …and a trailing slash does not make a different host of it.
        let slashed = OllamaProvider::new(None).with_host("http://box.lan:11434/");
        assert_eq!(slashed.endpoint(), "http://box.lan:11434/api/chat");
        assert_eq!(slashed.startup_notes().len(), 1);
    }

    /// **If this breaks:** `OLLAMA_HOST` is read and the disclosure is computed
    /// from something else — the default, say — so the line says "this machine"
    /// while the request goes somewhere else. Every other test here reaches the
    /// host through [`OllamaProvider::with_host`]; this is the only one that
    /// exercises the environment, which is the route a real user takes.
    ///
    /// **Why this one test may touch process-global state and the sibling for
    /// `OLLAMA_NUM_CTX` may not.** `set_var` is visible to every thread, and
    /// the suite runs in parallel. Nothing else asserts on a provider built by
    /// [`OllamaProvider::new`] alone — the stub helper calls `with_host`
    /// immediately, which recomputes both the host and the note — so this
    /// variable cannot change another test's answer. `OLLAMA_NUM_CTX` is read
    /// on every send and *is* asserted by the stub tests, so a test setting it
    /// would be a race, which is exactly the failure the Mac branch hit and why
    /// `parse_num_ctx` is a pure function.
    #[test]
    fn the_environment_is_where_the_host_is_read_from() {
        std::env::set_var("OLLAMA_HOST", "remote.example:11434");
        let p = OllamaProvider::new(None);
        std::env::remove_var("OLLAMA_HOST");

        assert_eq!(p.endpoint(), "http://remote.example:11434/api/chat");
        let notes = p.startup_notes();
        assert_eq!(notes.len(), 1, "the env host was not announced: {notes:?}");
        assert!(notes[0].contains("remote.example"), "{notes:?}");
        // …and it names the variable to unset, not some other route to a host.
        // A note that says "configured explicitly" sends the reader looking in
        // a settings file for something that is in their shell profile.
        assert!(notes[0].contains("OLLAMA_HOST"), "{notes:?}");

        // The control: with nothing set, the default is loopback and silent.
        let p = OllamaProvider::new(None);
        assert_eq!(p.endpoint(), format!("{DEFAULT_HOST}/api/chat"));
        assert!(p.startup_notes().is_empty());
    }

    /// **If this breaks:** the host is registered as a secret and every error
    /// message that would have named it prints `[redacted]` instead — the
    /// disclosure defeated by the redaction machinery.
    ///
    /// `auth::load_default` reads `ProviderKind::env_var` and hands what it
    /// finds to `ApiKey::new`, which calls `remember_secret`. The Mac branch
    /// returned `OLLAMA_HOST` here.
    #[test]
    fn a_host_is_never_returned_as_a_key_variable() {
        let var = Ollama.env_var();
        assert_ne!(
            var, "OLLAMA_HOST",
            "the host variable would be wrapped in an ApiKey and scrubbed from \
             every error message this process prints"
        );
        assert!(
            !var.contains("HOST"),
            "`env_var` names a key, not a destination: {var}"
        );
    }

    /// **If this breaks:** the provider is reachable from `emma set-provider`
    /// before anything prints the host, which is the one condition this port
    /// was not allowed to ship without.
    ///
    /// A source-reading test, with the limits that implies: it proves a call
    /// exists in a file, not that the line reaches a terminal. It is here
    /// because the wiring it guards is in two files this port may not edit, and
    /// the failure it catches — registering the provider and forgetting the
    /// line — is silent in every other way. When `Provider` grows a startup
    /// hook and `main.rs` prints it, this test goes green on its own; the
    /// mutation that shows it can fail is adding `&Ollama` to `KINDS`.
    #[test]
    fn the_provider_is_not_registered_until_its_host_is_announced() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let kinds = std::fs::read_to_string(dir.join("src/kind.rs"))
            .expect("kind.rs is in this crate and must be readable");
        // The registry line, not the whole file: `Ollama` appears in a doc
        // comment or a test without being reachable.
        let registered = kinds
            .lines()
            .skip_while(|l| !l.contains("static KINDS"))
            .take(3)
            .any(|l| l.contains("Ollama"));
        if !registered {
            return;
        }
        let main = dir.join("../emma/src/main.rs");
        let wiring = std::fs::read_to_string(&main).unwrap_or_default();
        assert!(
            wiring.contains("startup_notes"),
            "ollama is in KINDS but {} never asks for startup_notes, so a \
             non-loopback OLLAMA_HOST is never named to the user",
            main.display()
        );
    }

    // endregion: the disclosure

    // region: translation

    #[test]
    fn tools_keep_their_schema_inside_a_function_envelope() {
        let tools = vec![json!({
            "name": "Read",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        })];
        let out = tools_to_ollama(&tools);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["type"], "function");
        assert_eq!(out[0]["function"]["name"], "Read");
        assert_eq!(out[0]["function"]["description"], "Read a file");
        assert_eq!(out[0]["function"]["parameters"]["type"], "object");
        assert_eq!(
            out[0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        assert!(out[0]["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("path")));
    }

    /// **If this breaks:** the model is answered as though the tool never ran.
    ///
    /// Two claims. A `ToolResult` leaves the assistant turn and becomes its own
    /// `role: "tool"` message; and it is keyed by the call's *name*, because
    /// that is what Ollama pairs on. An earlier version of this file put
    /// `tool_use_id` in `tool_name`.
    #[test]
    fn a_tool_result_becomes_its_own_message_keyed_by_the_call_name() {
        let msg = Message {
            role: Role::Assistant,
            content: Content::Blocks(vec![
                ContentBlock::ToolUse(ToolCall {
                    id: "toolu_01deadbeef".into(),
                    name: "Read".into(),
                    input: json!({"path": "/tmp/test.txt"}),
                    extra: Map::new(),
                }),
                ContentBlock::ToolResult(ToolResult {
                    tool_use_id: "toolu_01deadbeef".into(),
                    content: "file contents here".into(),
                    is_error: false,
                    extra: Map::new(),
                }),
            ]),
        };
        let wire = messages_to_ollama(&[msg]);

        let tool = wire
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
            .expect("the result must be its own message");
        assert_eq!(tool["content"], "file contents here");
        assert_eq!(tool["tool_name"], "Read");
        assert_ne!(tool["tool_name"], "toolu_01deadbeef");

        let assistant = wire
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("the call must stay on the assistant turn");
        let calls = assistant["tool_calls"].as_array().expect("tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "toolu_01deadbeef");
        assert_eq!(calls[0]["function"]["name"], "Read");
        assert_eq!(
            calls[0]["function"]["arguments"],
            json!({"path": "/tmp/test.txt"})
        );
    }

    /// Two calls to the same tool in one turn must stay distinguishable on the
    /// wire, or the results pair to the wrong call.
    #[test]
    fn two_calls_to_one_tool_stay_distinct() {
        let msg = Message {
            role: Role::Assistant,
            content: Content::Blocks(vec![
                ContentBlock::ToolUse(ToolCall {
                    id: "call_AAA".into(),
                    name: "lookup".into(),
                    input: json!({"q": "alpha"}),
                    extra: Map::new(),
                }),
                ContentBlock::ToolUse(ToolCall {
                    id: "call_BBB".into(),
                    name: "lookup".into(),
                    input: json!({"q": "beta"}),
                    extra: Map::new(),
                }),
            ]),
        };
        let wire = messages_to_ollama(&[msg]);
        let calls = wire
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant message")["tool_calls"]
            .as_array()
            .expect("tool_calls")
            .clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_AAA");
        assert_eq!(calls[1]["id"], "call_BBB");
        assert_ne!(calls[0], calls[1]);
    }

    /// Thinking is not echoed back and a `Passthrough` is not forwarded, but
    /// neither may take the turn's text with it.
    #[test]
    fn a_turn_survives_blocks_that_have_no_translation() {
        let msg = Message {
            role: Role::Assistant,
            content: Content::Blocks(vec![
                ContentBlock::Thinking(ThinkingBlock {
                    thinking: "step one".into(),
                    signature: Some("sig".into()),
                    extra: Map::new(),
                }),
                ContentBlock::Passthrough(json!({"type": "server_tool_use", "id": "s1"})),
                ContentBlock::text("the answer"),
            ]),
        };
        let wire = messages_to_ollama(&[msg]);
        assert_eq!(wire.len(), 1, "{wire:?}");
        assert_eq!(wire[0]["content"], "the answer");
        assert!(wire[0].get("tool_calls").is_none());
        let rendered = wire[0].to_string();
        assert!(
            !rendered.contains("sig"),
            "a signature went out: {rendered}"
        );
    }

    // endregion: translation

    // region: reading a reply

    fn one_call_with(function: Value) -> Value {
        json!({
            "message": {"content": "", "tool_calls": [{"function": function}]},
            "done_reason": "stop"
        })
    }

    /// Pulls the single tool call out of a decoded turn, or fails loudly.
    fn only_tool_call(body: &Value) -> ToolCall {
        let turn = turn_from_ollama(body).expect("turn decoded");
        match &turn.content[0] {
            ContentBlock::ToolUse(c) => c.clone(),
            other => panic!("expected a tool call, got {other:?}"),
        }
    }

    #[test]
    fn a_reply_with_tool_calls_stops_for_tools_whatever_done_reason_says() {
        let body = json!({
            "message": {
                "content": "",
                "tool_calls": [{
                    "id": "call_001",
                    "type": "function",
                    "function": {"name": "Read", "arguments": {"path": "/tmp/test.txt"}}
                }]
            },
            // Ollama says "stop" in the very reply that carried the calls.
            "done_reason": "stop"
        });
        let turn = turn_from_ollama(&body).expect("turn decoded");
        assert_eq!(
            turn.stop_reason, "tool_use",
            "the loop branches on this to decide whether to keep going"
        );
        assert_eq!(turn.tool_calls().len(), 1);
        assert_eq!(turn.tool_calls()[0].name, "Read");
        assert_eq!(turn.tool_calls()[0].input, json!({"path": "/tmp/test.txt"}));
    }

    #[test]
    fn a_length_stop_is_reported_as_one_and_an_ordinary_one_is_not() {
        let body = |reason: &str| {
            json!({"message": {"content": "hi"}, "done_reason": reason,
                   "prompt_eval_count": 11, "eval_count": 3})
        };
        assert_eq!(
            turn_from_ollama(&body("length")).unwrap().stop_reason,
            "max_tokens"
        );
        assert_eq!(
            turn_from_ollama(&body("stop")).unwrap().stop_reason,
            "end_turn"
        );
        let turn = turn_from_ollama(&body("stop")).unwrap();
        assert_eq!(turn.text(), "hi");
        assert_eq!(turn.usage.input_tokens, 11);
        assert_eq!(turn.usage.output_tokens, 3);
        // Nothing is cached here, and a `0` in these fields means exactly that.
        assert_eq!(turn.usage.cache_read_input_tokens, 0);
        assert_eq!(turn.usage.billable_input_tokens(), 11);
    }

    /// **If this breaks:** a real tool call decodes as no tool call, the loop
    /// reports "answered — no tools were needed", and the suite stays green.
    ///
    /// This is `content.rs`'s scar, transplanted. There, the Messages API put a
    /// `caller` key on every `tool_use`, a strict decoder demoted the block,
    /// and a whole live goal ended on turn one with every fixture passing. The
    /// keys named here — `index`, `caller` — are the same class of thing: an
    /// unknown key must not stop a call from being a call, and it must not
    /// vanish either.
    #[test]
    fn an_unknown_key_on_a_tool_call_neither_rejects_it_nor_disappears() {
        let body = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "some_future_field": {"whatever": 1},
                "tool_calls": [{
                    "index": 0,
                    "caller": {"type": "direct"},
                    "type": "function",
                    "function": {
                        "name": "Read",
                        "arguments": {"file_path": "a.txt"},
                        "confidence": 0.9
                    }
                }]
            },
            "done_reason": "stop",
            "a_count_added_next_year": 7
        });
        let call = only_tool_call(&body);
        assert_eq!(call.name, "Read");
        assert_eq!(call.input, json!({"file_path": "a.txt"}));
        assert_eq!(call.extra["index"], json!(0));
        assert_eq!(call.extra["caller"], json!({"type": "direct"}));
        assert_eq!(call.extra["confidence"], json!(0.9));

        // …and what is carried comes back out on the next request rather than
        // being kept only in memory.
        let echoed = messages_to_ollama(&[Message::assistant(vec![ContentBlock::ToolUse(call)])]);
        assert_eq!(echoed[0]["tool_calls"][0]["index"], json!(0));
        assert_eq!(echoed[0]["tool_calls"][0]["caller"]["type"], "direct");
    }

    /// A server that sends its own id must get that id back; the synthetic one
    /// is a substitute for something absent, not a replacement for something
    /// present.
    #[test]
    fn a_server_supplied_id_outranks_the_synthetic_one() {
        let with_id = json!({
            "message": {"content": "", "tool_calls": [
                {"id": "real_1", "function": {"name": "Read", "arguments": {}}}
            ]},
            "done_reason": "stop"
        });
        assert_eq!(only_tool_call(&with_id).id, "real_1");

        let without = one_call_with(json!({"name": "Read", "arguments": {}}));
        assert_eq!(only_tool_call(&without).id, "ollama-0-Read");
    }

    /// Local models routinely serialise the argument object to a JSON string
    /// rather than nesting it. It used to pass straight through and the tool
    /// layer rejected it with "takes a JSON object, got a string", pointing the
    /// user at the model when the call was perfectly legible.
    #[test]
    fn a_json_string_of_arguments_is_decoded_into_the_object_it_encodes() {
        let call = one_call_with(json!({
            "name": "Read",
            "arguments": "{\"file_path\":\"a.txt\"}"
        }));
        assert_eq!(only_tool_call(&call).input, json!({"file_path": "a.txt"}));
    }

    /// Second near-miss shape: the key spelled `args` or `parameters`. The
    /// lookup missed, the input defaulted to `{}`, and the tool complained
    /// about a missing field that had been supplied.
    #[test]
    fn args_and_parameters_are_accepted_as_spellings_of_arguments() {
        for key in ["args", "parameters"] {
            let call = one_call_with(json!({"name": "Read", key: {"file_path": "a.txt"}}));
            assert_eq!(
                only_tool_call(&call).input,
                json!({"file_path": "a.txt"}),
                "{key}"
            );
        }
    }

    /// A tolerance that changes correct input is a regression, not a fix. The
    /// canonical shape survives byte for byte even when the aliases are also
    /// present: first present key wins, nothing merges across keys.
    #[test]
    fn the_canonical_arguments_object_outranks_the_aliases_and_is_untouched() {
        let plain = one_call_with(json!({
            "name": "Read",
            "arguments": {"file_path": "a.txt", "offset": 3}
        }));
        assert_eq!(
            only_tool_call(&plain).input,
            json!({"file_path": "a.txt", "offset": 3})
        );

        let shadowed = one_call_with(json!({
            "name": "Read",
            "arguments": {"file_path": "real.txt"},
            "args": {"file_path": "alias.txt"},
            "parameters": {"limit": 9}
        }));
        assert_eq!(
            only_tool_call(&shadowed).input,
            json!({"file_path": "real.txt"}),
            "an alias shadowed or merged into a present `arguments`"
        );
    }

    /// The string fallback is a decode attempt, never a rescue. When the string
    /// is not a JSON *object*, the original is kept so the tool layer's "got a
    /// string" error still names what arrived; substituting `{}` would turn a
    /// legible protocol fault into a bogus missing-field complaint.
    #[test]
    fn an_arguments_string_that_is_not_an_object_survives_as_that_string() {
        for raw in ["file_path=a.txt", "42", "[1, 2]"] {
            let call = one_call_with(json!({"name": "Read", "arguments": raw}));
            assert_eq!(only_tool_call(&call).input, json!(raw), "{raw}");
        }
    }

    #[test]
    fn a_reply_with_no_message_is_a_protocol_error_rather_than_an_empty_turn() {
        let err = turn_from_ollama(&json!({"done_reason": "stop"})).unwrap_err();
        assert!(format!("{err}").contains("message"), "{err}");
    }

    // endregion: reading a reply

    // region: the loopback stub
    // ------------------------------------------------------------------------
    // A real server on 127.0.0.1, not an injectable transport. The pattern is
    // `anthropic.rs`'s and the reasoning is the same: the claims worth making
    // are about wire bytes, and a transport trait would stub out exactly the
    // layer they live in. Smaller here — no SSE, no headers to assert beyond
    // one `retry-after`.

    struct Reply {
        status: u16,
        content_type: &'static str,
        body: String,
    }

    impl Reply {
        fn json(body: impl Into<String>) -> Self {
            Self {
                status: 200,
                content_type: "application/json",
                body: body.into(),
            }
        }

        fn ndjson(body: impl Into<String>) -> Self {
            Self {
                status: 200,
                content_type: "application/x-ndjson",
                body: body.into(),
            }
        }

        fn error(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                content_type: "application/json",
                body: body.into(),
            }
        }

        fn wire(&self) -> String {
            format!(
                "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                self.status,
                self.content_type,
                self.body.len(),
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
            url: format!("http://{addr}"),
            seen,
        }
    }

    fn provider(stub: &Stub) -> OllamaProvider {
        OllamaProvider::new(Some("test-model".into()))
            .with_host(stub.url.clone())
            // The curve itself is pinned by `retry`'s own tests; here it would
            // only make the suite slow.
            .with_retry(Retry {
                max_attempts: 3,
                base: Duration::from_millis(1),
                cap: Duration::from_millis(5),
            })
    }

    fn request() -> Request {
        Request::new(
            "you are emma",
            vec![json!({
                "name": "Read",
                "description": "Read a file",
                "input_schema": {"type": "object", "properties": {}}
            })],
        )
        .with_query(vec![Message::user("read a.txt")])
    }

    async fn run(s: &Stub, mode: Mode) -> (Result<AssistantTurn, LlmError>, Vec<Event>) {
        let (tx, mut rx) = mpsc::channel(64);
        let out = provider(s).send(request(), mode, Some(tx)).await;
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        (out, events)
    }

    fn batch_body() -> String {
        json!({
            "model": "test-model",
            "message": {
                "role": "assistant",
                "thinking": "let me look",
                "content": "here it is",
                "tool_calls": [{"function": {"name": "Read", "arguments": {"path": "a.txt"}}}]
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 120,
            "eval_count": 40
        })
        .to_string()
    }

    /// The same turn as [`batch_body`], cut into frames the way a server
    /// streams it: content in pieces, the calls in one frame, the counts last.
    fn stream_body() -> String {
        [
            json!({"message": {"role": "assistant", "thinking": "let me ", "content": ""}, "done": false}),
            json!({"message": {"role": "assistant", "thinking": "look", "content": "here "}, "done": false}),
            json!({"message": {"role": "assistant", "content": "it is"}, "done": false}),
            json!({"message": {"role": "assistant", "content": "",
                   "tool_calls": [{"function": {"name": "Read", "arguments": {"path": "a.txt"}}}]},
                   "done": false}),
            json!({"message": {"role": "assistant", "content": ""}, "done": true,
                   "done_reason": "stop", "prompt_eval_count": 120, "eval_count": 40}),
        ]
        .iter()
        .map(|v| format!("{v}\n"))
        .collect()
    }

    /// **If this breaks:** the model is asked to stream when the caller wanted
    /// one body, or the budget and context size never reach the server.
    #[tokio::test]
    async fn the_request_carries_the_prefix_in_order_and_the_options_that_matter() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let (turn, _) = run(&s, Mode::Batch).await;
        turn.expect("the stub replied");

        let sent = s.last();
        assert_eq!(sent["model"], "test-model");
        assert_eq!(sent["stream"], false);
        assert_eq!(sent["options"]["num_predict"], json!(32_000));
        // A three-line conversation derives below the floor, so the floor is
        // what goes out. The sibling below is the one that moves.
        assert_eq!(sent["options"]["num_ctx"], json!(DEFAULT_NUM_CTX));

        let messages = sent["messages"].as_array().expect("messages");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "you are emma");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "read a.txt");

        // The tool surface arrives in Ollama's envelope, not Anthropic's.
        assert_eq!(sent["tools"][0]["type"], "function");
        assert_eq!(sent["tools"][0]["function"]["name"], "Read");
        assert!(sent["tools"][0].get("input_schema").is_none());
    }

    /// **If this breaks:** a long conversation is clipped on the server and
    /// nothing in the session record says so. This is the end-to-end form of
    /// the defect — the derivation reaching the wire, and the same number
    /// coming back on [`Usage::context_window`] so a log can be audited for
    /// the clip afterwards rather than guessed at.
    ///
    /// It asserts on the *relationship* between the two rather than a literal,
    /// because pinning a magic number here would make the test a restatement of
    /// the constants instead of a check that they are wired together.
    #[tokio::test]
    async fn a_long_conversation_widens_the_window_and_the_window_is_reported() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        // 600,000 characters ≈ 150,000 tokens: past Emma's default
        // `max_context` of 120,000, and well past the old flat 32,768.
        let long = Request::new("you are emma", Vec::new())
            .with_query(vec![Message::user("x".repeat(600_000))]);
        let turn = provider(&s).send(long, Mode::Batch, None).await.unwrap();

        let sent = s.last();
        let asked = sent["options"]["num_ctx"].as_i64().expect("num_ctx");
        assert!(
            asked > i64::from(DEFAULT_NUM_CTX),
            "a 150,000 token prompt went out under a {asked} window"
        );
        assert!(
            asked >= 150_000,
            "the window does not hold the prompt: {asked}"
        );
        assert_eq!(
            turn.usage.context_window, asked,
            "the reported window is not the one the wire carried"
        );
    }

    #[tokio::test]
    async fn a_request_with_no_tools_sends_no_tools_key_at_all() {
        let s = stub(vec![Reply::json(batch_body())]).await;
        let p = provider(&s);
        let _ = p
            .send(Request::new("sys", Vec::new()), Mode::Batch, None)
            .await;
        assert!(
            s.last().get("tools").is_none(),
            "an empty tools array can switch the server to its tool template"
        );
    }

    /// **If this breaks:** a streamed run and a batched run of the same reply
    /// disagree about what the model said, and only one of them is right.
    ///
    /// The two bodies are the same turn in the two wire forms, which is what
    /// makes comparing the parsed turns mean anything. The second half is the
    /// narration: a local model takes minutes, and a caller that asked to watch
    /// the answer arrive and got silence cannot tell the run from a hang.
    #[tokio::test]
    async fn streaming_assembles_the_same_turn_as_batch_and_narrates_it() {
        let b = stub(vec![Reply::json(batch_body())]).await;
        let (batched, _) = run(&b, Mode::Batch).await;
        let batched = batched.expect("batch turn");

        let s = stub(vec![Reply::ndjson(stream_body())]).await;
        let (streamed, events) = run(&s, Mode::Stream).await;
        let streamed = streamed.expect("streamed turn");

        assert_eq!(streamed, batched, "the two modes decoded different turns");
        assert_eq!(streamed.text(), "here it is");
        assert_eq!(streamed.stop_reason, "tool_use");
        assert_eq!(streamed.usage.output_tokens, 40);
        assert_eq!(s.last()["stream"], true);

        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["here ", "it is"], "{events:?}");
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::ToolUseStarted { name, .. } if name == "Read"
            )),
            "a tool call arrived with no sign of it: {events:?}"
        );
    }

    /// **If this breaks:** a transient failure ends the goal with the local
    /// model still loaded and willing — or it is retried in silence, which for
    /// a model that thinks for minutes is indistinguishable from a hang.
    ///
    /// The 500 is the real one: Ollama parses tool calls back out of the
    /// model's own chat template, so an XML-shaped template fails on arguments
    /// containing angle brackets, which Rust generics produce constantly.
    #[tokio::test]
    async fn a_template_parse_500_is_retried_and_the_retry_is_visible() {
        let s = stub(vec![
            Reply::error(
                500,
                "XML syntax error on line 53: element <function> closed by </parameter>",
            ),
            Reply::json(batch_body()),
        ])
        .await;
        let (turn, events) = run(&s, Mode::Batch).await;
        assert_eq!(
            turn.expect("the second attempt succeeded").text(),
            "here it is"
        );
        assert_eq!(s.requests().len(), 2, "the request was not re-sent");

        let retries: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::Retrying { .. }))
            .collect();
        assert_eq!(retries.len(), 1, "a silent retry is a hang: {events:?}");
        let Event::Retrying { reason, .. } = retries[0] else {
            unreachable!()
        };
        assert!(reason.contains("XML syntax error"), "{reason}");
    }

    /// A 400 is the request being wrong, and sending it again is a slower way
    /// to get the same error. The body has to survive into the message: "model
    /// not found" is the whole diagnosis.
    #[tokio::test]
    async fn a_bad_request_is_reported_once_with_what_the_server_said() {
        let s = stub(vec![Reply::error(
            400,
            r#"{"error":"model 'nope:1b' not found"}"#,
        )])
        .await;
        let (out, events) = run(&s, Mode::Batch).await;
        let err = out.expect_err("a 400 must not be swallowed");
        assert!(!err.retryable());
        assert!(format!("{err}").contains("nope:1b"), "{err}");
        assert_eq!(s.requests().len(), 1, "a 400 was retried");
        assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
    }

    /// A reply that is not JSON is a protocol error naming what arrived, not a
    /// turn with empty content.
    #[tokio::test]
    async fn a_reply_that_is_not_json_says_so_and_shows_it() {
        let s = stub(vec![Reply::json("<html>502 Bad Gateway</html>")]).await;
        let (out, _) = run(&s, Mode::Batch).await;
        let err = out.expect_err("html decoded as a turn");
        let shown = format!("{err}");
        assert!(shown.contains("could not read"), "{shown}");
        assert!(shown.contains("502 Bad Gateway"), "{shown}");
    }

    /// A stream that ends before any frame arrives is a failure, not an empty
    /// answer. An empty turn would be reported to the user as the model
    /// choosing to say nothing.
    #[tokio::test]
    async fn an_empty_stream_is_an_error_rather_than_an_empty_turn() {
        let s = stub(vec![Reply::ndjson("")]).await;
        let (out, _) = run(&s, Mode::Stream).await;
        assert!(out.is_err(), "an empty stream decoded as a turn");
    }

    // endregion: the loopback stub

    // region: certification against a real Ollama

    /// **Not a unit test — the receipt.** `#[ignore]`d because it needs a real
    /// Ollama on `127.0.0.1:11434` holding `gemma4:12b`, which no CI runner
    /// has; run it with
    /// `cargo test -p emma-llm --lib the_derived_window_reaches_a_real_ollama
    /// -- --ignored --nocapture`.
    ///
    /// It exists because the stub proves the client's arithmetic and nothing
    /// about the server's acceptance of it. A `num_ctx` Ollama refuses, or
    /// silently floors, would pass every test above. This one puts a recording
    /// proxy between the provider and the real server, so the number asserted
    /// on is the number the socket carried, and it fails if the model does not
    /// answer under that window.
    #[tokio::test]
    #[ignore = "needs a real Ollama at 127.0.0.1:11434 with gemma4:12b"]
    async fn the_derived_window_reaches_a_real_ollama() {
        // A forwarding proxy rather than a mock: the reply has to come from the
        // model, or this proves only that a fake accepts the field.
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = seen.clone();
        tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut upstream = tokio::net::TcpStream::connect("127.0.0.1:11434")
                .await
                .expect("a real Ollama is listening");
            let mut raw: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = client.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                raw.extend_from_slice(&chunk[..n]);
                upstream.write_all(&chunk[..n]).await.unwrap();
                if let Some(h) = headers_end(&raw) {
                    if raw.len() >= h + content_length(&raw[..h]) {
                        let len = content_length(&raw[..h]);
                        captured
                            .lock()
                            .unwrap()
                            .push(serde_json::from_slice(&raw[h..h + len]).unwrap());
                        break;
                    }
                }
            }
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });

        // ~200,000 characters of filler: past the old flat 32,768 window by a
        // wide margin, so a server that ignored `num_ctx` would drop the
        // question at the front and answer something else.
        let filler = "the quick brown fox jumps over the lazy dog. ".repeat(4_400);
        let req = Request::new("You are Emma. Answer in one word.", Vec::new()).with_query(vec![
            Message::user(format!(
                "{filler}\n\nIgnore the text above. What colour is the sky?"
            )),
        ]);
        let turn = OllamaProvider::new(Some("gemma4:12b".into()))
            .with_host(format!("http://{addr}"))
            .send(req, Mode::Batch, None)
            .await
            .expect("the real server answered");

        let sent = seen.lock().unwrap()[0].clone();
        let wire = sent["options"]["num_ctx"].as_i64().expect("num_ctx");
        println!("wire num_ctx = {wire}");
        println!("usage.context_window = {}", turn.usage.context_window);
        println!("usage.input_tokens = {}", turn.usage.input_tokens);
        assert!(wire > i64::from(DEFAULT_NUM_CTX), "wire window {wire}");
        assert_eq!(turn.usage.context_window, wire);
        // The server accepted the window rather than flooring it: the prompt it
        // counted is larger than the old constant, so nothing was dropped to
        // fit.
        assert!(
            turn.usage.input_tokens > i64::from(DEFAULT_NUM_CTX),
            "the server counted only {} prompt tokens — it clipped",
            turn.usage.input_tokens
        );
    }

    // endregion: certification against a real Ollama
}

// endregion: Tests
