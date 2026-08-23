//! The five tools, and the boundary each one sits on.
//!
//! The surface is split where the *approval question* changes, not where it
//! would be convenient:
//!
//! | Tool           | The question a human is answering                          |
//! | -------------- | ---------------------------------------------------------- |
//! | `BrowserOpen`  | may a browser start, and reach this host                    |
//! | `BrowserRead`  | may bytes keep flowing from the host this session is on now |
//! | `BrowserAct`   | may the model change state on somebody else's server        |
//! | `BrowserFill`  | may the model put data into somebody else's form            |
//! | `BrowserClose` | (none — it only ends what this surface started)             |
//!
//! **Why not one `Browse` tool with an `action` argument.** One tool is one
//! schema describing eight mutually exclusive argument shapes, which JSON Schema
//! expresses badly and models follow badly — a `click` carrying a `text` field
//! is a wasted turn. More importantly it is one `read_only` bit covering both
//! "read this page again" and "type into this form", and one name for permission
//! rules to match, so `BrowserRead(domain:example.com)` — read there, do not act
//! there — would be inexpressible. That is the same collapse `lib.rs` describes
//! for `read_only` versus `reaches_network`, and the fix there was to split the
//! axis rather than overload it.
//!
//! `BrowserAct`'s own `action` enum is the one mode switch kept, because those
//! six verbs share their arguments, share their result shape (`mini_digest`),
//! and sit on the same side of every gate.
//!
//! # The cross-origin re-ask, which is the sharp part
//!
//! The gate reads a destination from `network_target` *before* the call, out of
//! its arguments — and a session call's arguments say `session=abc`, not where
//! `abc` now points. So these tools answer `network_target` **from the pool**,
//! with the host the session is on *now*, and the pool records the `final_url`
//! after every verb. A grant for `example.com` therefore stops covering the
//! session the moment a click lands on `evil.example`, and the next call asks.
//!
//! Two honest limits on that, both stated rather than discovered:
//!
//! - **The re-ask is one call late.** The bytes for a redirect or a click-through
//!   have already gone by the time the pool learns the host changed. What the
//!   re-ask actually protects is everything *after*: the content of the new host
//!   cannot be read, and nothing can be clicked there, without a fresh answer.
//!   `BrowserAct` returning only `mini_digest` and never page text is what makes
//!   that worth something — a cross-origin hop yields a URL and a title, and the
//!   payload needs `BrowserRead`, which asks.
//! - **A remembered rule means what it says.** `BrowserRead(domain:example.com)`
//!   allows reading a session **while it is on `example.com`** and nothing else;
//!   it does not travel with the session. The rule that does travel is the
//!   tool-wide one (`BrowserAct`, no domain), which is a deliberate second
//!   keystroke at the prompt — and even that is bounded underneath by
//!   chromehand's interaction allowlist, a user-owned file in `~/.emma/` that no
//!   answer at a prompt can write.
//!
//! # What is deliberately not here
//!
//! **`screenshot`.** It exists in `chromehand::actions` and is left out.
//! `ToolOutcome::content` is a `String`; until the tool-result path can carry an
//! image block, a screenshot tool writes a PNG that nothing in the loop can
//! look at. Shipping it would be a verb that returns nothing usable.
//!
//! **Submitting a form.** `BrowserFill` fills and never submits — see the
//! comment on [`BrowserFill`], which is where the two reasons are written down.

use std::sync::Arc;

use emma_tool_api::{NetworkTarget, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use super::pool::{BrowserPool, Session};
use super::render;
use crate::args;
use crate::chromehand::forms::{FillField, FillSpec};
use crate::chromehand::{actions, forms};
use crate::digest_md;
use crate::fetch::map_error;

// region: The shared shape
// ---------------------------------------------------------------------------
// The shared shape
//
// Every session tool does the same four things — find the session, connect,
// run one verb, record where it ended up — and the fourth is the one that must
// never be skipped, because it is what moves the host the gate is asked about.
// ---------------------------------------------------------------------------

/// Text budget for a session read. Same default as `WebFetch`.
const DEFAULT_MAX_CHARS: u64 = crate::fetch::DEFAULT_MAX_CHARS;
const MAX_MAX_CHARS: u64 = crate::fetch::MAX_MAX_CHARS;
const DEFAULT_MAX_LINKS: u64 = crate::fetch::DEFAULT_MAX_LINKS;
const MAX_MAX_LINKS: u64 = crate::fetch::MAX_MAX_LINKS;

/// How long a `wait_for` waits when the caller does not say.
const DEFAULT_WAIT_MS: u64 = 10_000;
/// And the ceiling, because a wait is a turn the user is watching nothing
/// happen in.
const MAX_WAIT_MS: u64 = 60_000;

/// The session this call names, or an argument error listing what *is* open.
///
/// **Called from `validate_args`, which the loop runs before the approval gate.**
/// That ordering is what lets `network_target` assume the session exists: an
/// unknown id is refused as a bad argument with a useful message, rather than
/// reaching the gate as "this tool declared egress and named no host", which is
/// a fail-closed denial with a sentence about a defect in the tool.
fn need_session(pool: &BrowserPool, tool: &str, args_v: &Value) -> Result<Session, ToolError> {
    let id = args::req_str(args_v, tool, "session")?.trim();
    pool.get(id).ok_or_else(|| {
        let open = pool.ids();
        ToolError::BadArguments(if open.is_empty() {
            format!(
                "there is no browser session '{id}' — none are open. Open one with BrowserOpen \
                 first; a session does not survive the end of a goal"
            )
        } else {
            format!(
                "there is no browser session '{id}'. Open now: {}",
                open.join(", ")
            )
        })
    })
}

/// Where a *session* call is going: the host it is on now, from the pool.
fn session_target(pool: &BrowserPool, args_v: &Value, errand: &str) -> Option<NetworkTarget> {
    let id = args_v.get("session")?.as_str()?.trim();
    let s = pool.get(id)?;
    if s.host.is_empty() {
        return None;
    }
    Some(NetworkTarget::new(
        &s.host,
        format!("{errand} — session {id} is on {}", s.url),
    ))
}

/// One connection, for the length of one verb.
///
/// **Why this is a guard and not a helper that takes a closure.** The
/// [`Live::finish`] call is what records where the verb left the page, and that
/// single line is the cross-origin re-ask: forget it on one verb and that verb
/// becomes the one a page can use to move a session to another origin without
/// the gate ever being asked about the new host. So the verb's result can only
/// be got *out* by going through `finish` — the type makes forgetting it a
/// compile error rather than a review item.
struct Live<'a> {
    pool: &'a BrowserPool,
    connected: crate::chromehand::session::Connected,
}

impl<'a> Live<'a> {
    async fn connect(pool: &'a BrowserPool, id: &str) -> Result<Live<'a>, ToolError> {
        let connected = pool
            .connect(id)
            .await
            .map_err(|e| map_error(crate::chromehand::classify(e)))?;
        Ok(Live { pool, connected })
    }

    fn page(&self) -> &crate::chromehand::session::Connected {
        &self.connected
    }

    /// Record where the page ended up, disconnect without closing Chrome, and
    /// translate chromehand's failure taxonomy into Emma's.
    async fn finish<T>(self, result: Result<T, String>) -> Result<T, ToolError> {
        let id = self.connected.id.clone();
        // The page's own idea of where it is, read after the verb and on the
        // failure path too — a navigation that errored still moved.
        let landed = self
            .connected
            .page
            .url()
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        crate::chromehand::session::disconnect(self.connected);
        if !landed.is_empty() {
            self.pool.arrived_at(&id, &landed);
        }
        result.map_err(|e| map_error(crate::chromehand::classify(e)))
    }
}

// endregion: The shared shape

// region: BrowserOpen
// ---------------------------------------------------------------------------
// BrowserOpen
//
// The only tool that starts a process, and the only one whose `read_only: false`
// is about this machine rather than about somebody else's server.
// ---------------------------------------------------------------------------

/// Start a browser session on a URL.
pub struct BrowserOpen {
    pool: Arc<BrowserPool>,
}

impl BrowserOpen {
    pub fn new(pool: Arc<BrowserPool>) -> Self {
        Self { pool }
    }
}

const OPEN_KEYS: &[&str] = &["url", "headful"];

#[async_trait::async_trait]
impl Tool for BrowserOpen {
    fn name(&self) -> &'static str {
        "BrowserOpen"
    }

    fn description(&self) -> &str {
        include_str!("../descriptions/browser_open.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "An http:// or https:// URL. Loopback, file: and chrome: URLs are refused."
                },
                "headful": {
                    "type": "boolean",
                    "description": "Show the browser window. Default false (headless). Use it when the user needs to watch, or take over."
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // **Not `WebFetch`'s answer, and the difference is durability.**
            // `WebFetch` spawns a Chrome, reads, and kills it inside one call,
            // so it leaves this machine as it found it and declares
            // `read_only: true` honestly. This leaves a *live* browser running
            // with an unauthenticated CDP port on localhost — chromehand's own
            // recorded warning is that while it exists, any local process can
            // puppet it. That is a lasting change to the machine, so it asks.
            read_only: false,
            reaches_network: true,
            // Two opens are two browsers, and the second is not free.
            idempotent: false,
        }
    }

    fn network_target(&self, args_v: &Value) -> Option<NetworkTarget> {
        let url = args_v.get("url")?.as_str()?.trim();
        let parsed = url::Url::parse(url).ok()?;
        Some(NetworkTarget::new(
            parsed.host_str()?,
            format!("open a browser session at {url}"),
        ))
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, "BrowserOpen", OPEN_KEYS)?;
        let url = args::req_str(args_v, "BrowserOpen", "url")?;
        if url.trim().is_empty() {
            return Err(ToolError::BadArguments("BrowserOpen.url is empty".into()));
        }
        args::opt_bool(args_v, "BrowserOpen", "headful")?;
        Ok(())
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(args_v).await)
    }
}

impl BrowserOpen {
    async fn run(&self, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let url = args::req_str(&args_v, "BrowserOpen", "url")?
            .trim()
            .to_string();
        let headful = args::opt_bool(&args_v, "BrowserOpen", "headful")?.unwrap_or(false);

        let session = self.pool.open(&url, headful).await.map_err(map_error)?;

        // The first read comes back with the page, selectors included, so the
        // ordinary loop is open → act → read rather than open → read → act.
        let live = Live::connect(&self.pool, &session.id).await?;
        let read = actions::session_digest(live.page(), DEFAULT_MAX_CHARS as usize).await;
        let digest = live.finish(read).await?;
        let (snapshot, _) = crate::chromehand::digest::compute_delta(&session.id, None, &digest);
        self.pool.set_delta_baseline(&session.id, snapshot);

        let limits = digest_md::Limits {
            max_links: DEFAULT_MAX_LINKS as usize,
            max_chars: DEFAULT_MAX_CHARS as usize,
            show_selectors: true,
            selector_filter: None,
        };
        let rendered = digest_md::render(&digest, &limits).map_err(ToolError::Failed)?;
        let live = self.pool.get(&session.id).unwrap_or(session);

        let mut content = format!(
            "Browser session `{}` is open at {}.\n\
             It closes when this goal ends, or when you call BrowserClose. \
             Chrome is holding a few hundred megabytes for as long as it lives.\n\n",
            live.id, live.url
        );
        content.push_str(&rendered.markdown);
        let outcome = ToolOutcome::new(content)
            .with_display(format!("session {} — {}", live.id, rendered.display));
        Ok(match rendered.truncation {
            Some(reason) => outcome.truncated_because(reason),
            None => outcome,
        })
    }
}

// endregion: BrowserOpen

// region: BrowserRead
// ---------------------------------------------------------------------------
// BrowserRead
//
// The only tool that returns page content, which is why it is the one the
// cross-origin re-ask actually protects.
// ---------------------------------------------------------------------------

/// Read the page a session is on.
pub struct BrowserRead {
    pool: Arc<BrowserPool>,
}

impl BrowserRead {
    pub fn new(pool: Arc<BrowserPool>) -> Self {
        Self { pool }
    }
}

const READ_KEYS: &[&str] = &["session", "delta", "max_chars", "max_links", "selectors"];

#[async_trait::async_trait]
impl Tool for BrowserRead {
    fn name(&self) -> &'static str {
        "BrowserRead"
    }

    fn description(&self) -> &str {
        include_str!("../descriptions/browser_read.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session": { "type": "string", "description": "The session id BrowserOpen returned." },
                "delta": {
                    "type": "boolean",
                    "description": "Return only what changed since the previous read. Default true, and it is what makes a long session affordable. Set false for the whole page."
                },
                "max_chars": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("Characters of page text. Default {DEFAULT_MAX_CHARS}, capped at {MAX_MAX_CHARS}.")
                },
                "max_links": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("Links to list. Default {DEFAULT_MAX_LINKS}, capped at {MAX_MAX_LINKS}.")
                },
                "selectors": {
                    "type": "string",
                    "description": "Only list interactive elements whose selector, label, name or type contains this text, case-insensitively. Use it when you are looking for one control on a busy page — 'search', 'email', 'submit'."
                }
            },
            "required": ["session"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // Honest: it evaluates JS in a page that is already loaded and
            // changes nothing anywhere.
            read_only: true,
            // …and this is the axis that carries the whole risk. The host it
            // names is the session's *current* one, which is what re-asks after
            // a cross-origin hop.
            reaches_network: true,
            idempotent: true,
        }
    }

    fn network_target(&self, args_v: &Value) -> Option<NetworkTarget> {
        session_target(&self.pool, args_v, "read the page")
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, "BrowserRead", READ_KEYS)?;
        need_session(&self.pool, "BrowserRead", args_v)?;
        args::opt_bool(args_v, "BrowserRead", "delta")?;
        args::opt_str(args_v, "BrowserRead", "selectors")?;
        for key in ["max_chars", "max_links"] {
            if let Some(0) = args::opt_u64(args_v, "BrowserRead", key)? {
                return Err(ToolError::BadArguments(format!(
                    "BrowserRead.{key} must be at least 1"
                )));
            }
        }
        Ok(())
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(args_v).await)
    }
}

impl BrowserRead {
    async fn run(&self, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let session = need_session(&self.pool, "BrowserRead", &args_v)?;
        let want_delta = args::opt_bool(&args_v, "BrowserRead", "delta")?.unwrap_or(true);
        let max_chars = args::opt_u64(&args_v, "BrowserRead", "max_chars")?
            .unwrap_or(DEFAULT_MAX_CHARS)
            .min(MAX_MAX_CHARS);
        let max_links = args::opt_u64(&args_v, "BrowserRead", "max_links")?
            .unwrap_or(DEFAULT_MAX_LINKS)
            .min(MAX_MAX_LINKS);
        let filter = args::opt_str(&args_v, "BrowserRead", "selectors")?
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty());

        let live = Live::connect(&self.pool, &session.id).await?;
        let read = actions::session_digest(live.page(), max_chars as usize).await;
        let digest = live.finish(read).await?;

        // The baseline is always refreshed, whichever branch renders, so a
        // `delta: false` read does not make the *next* delta diff against a page
        // two steps old.
        let (snapshot, delta_out) = crate::chromehand::digest::compute_delta(
            &session.id,
            session.delta_baseline.as_ref(),
            &digest,
        );
        self.pool.set_delta_baseline(&session.id, snapshot);

        let baseline = delta_out
            .get("delta")
            .and_then(|d| d.get("baseline"))
            .and_then(Value::as_bool)
            == Some(true);
        if want_delta && !baseline {
            let rendered = render::delta(&session.id, &delta_out);
            return Ok(ToolOutcome::new(rendered.content).with_display(rendered.display));
        }

        let limits = digest_md::Limits {
            max_links: max_links as usize,
            max_chars: max_chars as usize,
            show_selectors: true,
            selector_filter: filter,
        };
        let rendered = digest_md::render(&digest, &limits).map_err(ToolError::Failed)?;
        let outcome = ToolOutcome::new(rendered.markdown).with_display(rendered.display);
        Ok(match rendered.truncation {
            Some(reason) => outcome.truncated_because(reason),
            None => outcome,
        })
    }
}

// endregion: BrowserRead

// region: BrowserAct
// ---------------------------------------------------------------------------
// BrowserAct
//
// Where the capability actually increases: this is the model taking an action,
// possibly authenticated, on a server that is not this machine.
// ---------------------------------------------------------------------------

/// The six verbs, in one enum, because they share arguments, share a result
/// shape and sit on the same side of every gate.
const ACTIONS: &[&str] = &[
    "click", "type", "select", "navigate", "back", "forward", "wait_for",
];

pub struct BrowserAct {
    pool: Arc<BrowserPool>,
}

impl BrowserAct {
    pub fn new(pool: Arc<BrowserPool>) -> Self {
        Self { pool }
    }
}

const ACT_KEYS: &[&str] = &[
    "session",
    "action",
    "selector",
    "text",
    "value",
    "url",
    "timeout_ms",
];

#[async_trait::async_trait]
impl Tool for BrowserAct {
    fn name(&self) -> &'static str {
        "BrowserAct"
    }

    fn description(&self) -> &str {
        include_str!("../descriptions/browser_act.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session": { "type": "string", "description": "The session id BrowserOpen returned." },
                "action": {
                    "type": "string",
                    "enum": ACTIONS,
                    "description": "click (needs selector) · type (selector + text) · select (selector + value) · navigate (url) · back · forward · wait_for (selector or text; neither means wait for the network to go quiet)."
                },
                "selector": {
                    "type": "string",
                    "description": "A CSS selector from a BrowserRead listing. Use them verbatim — they are computed to be stable and a hand-written guess usually matches nothing."
                },
                "text": { "type": "string", "description": "For type: the text to type. For wait_for: text to wait for on the page." },
                "value": { "type": "string", "description": "For select: the option's value or its visible text." },
                "url": { "type": "string", "description": "For navigate: an http:// or https:// URL." },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("For wait_for. Default {DEFAULT_WAIT_MS}, capped at {MAX_WAIT_MS}.")
                }
            },
            "required": ["session", "action"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // Clicking a button on somebody else's server changes state that is
            // not this machine's, which is not the question this bit asks — and
            // the bit is nonetheless the only thing standing between the user
            // and a silent side effect, and there is no third axis. So it is
            // declared `false` and the gate asks. The frequency argument that
            // protects `WebFetch` does not apply: acting on a page is not
            // something that happens forty times a turn.
            read_only: false,
            reaches_network: true,
            idempotent: false,
        }
    }

    fn network_target(&self, args_v: &Value) -> Option<NetworkTarget> {
        // A deliberate navigation names its *destination*, which is the one case
        // where the gate can ask before the bytes rather than after: the host is
        // in the arguments. Every other verb asks about where the session is.
        if args_v.get("action").and_then(Value::as_str) == Some("navigate") {
            let url = args_v.get("url")?.as_str()?.trim();
            let parsed = url::Url::parse(url).ok()?;
            return Some(NetworkTarget::new(
                parsed.host_str()?,
                format!("navigate a browser session to {url}"),
            ));
        }
        let verb = args_v
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("act");
        session_target(&self.pool, args_v, &format!("{verb} on the page"))
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, "BrowserAct", ACT_KEYS)?;
        need_session(&self.pool, "BrowserAct", args_v)?;
        let action = args::req_str(args_v, "BrowserAct", "action")?;
        if !ACTIONS.contains(&action) {
            return Err(ToolError::BadArguments(format!(
                "BrowserAct.action must be one of {}, got '{action}'",
                ACTIONS.join(", ")
            )));
        }
        // Per-action requirements, stated as the missing argument rather than as
        // "invalid arguments": the model can fix a missing selector and cannot
        // fix a sentence that does not say which one.
        let has = |k: &str| {
            args_v
                .get(k)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.trim().is_empty())
        };
        let need = |k: &str| -> Result<(), ToolError> {
            if has(k) {
                Ok(())
            } else {
                Err(ToolError::BadArguments(format!(
                    "BrowserAct.{action} needs {k}"
                )))
            }
        };
        match action {
            "click" => need("selector")?,
            "type" => {
                need("selector")?;
                if args_v.get("text").and_then(Value::as_str).is_none() {
                    return Err(ToolError::BadArguments("BrowserAct.type needs text".into()));
                }
            }
            "select" => {
                need("selector")?;
                need("value")?;
            }
            "navigate" => need("url")?,
            _ => {}
        }
        if let Some(0) = args::opt_u64(args_v, "BrowserAct", "timeout_ms")? {
            return Err(ToolError::BadArguments(
                "BrowserAct.timeout_ms must be at least 1".into(),
            ));
        }
        Ok(())
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(args_v).await)
    }
}

impl BrowserAct {
    async fn run(&self, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let session = need_session(&self.pool, "BrowserAct", &args_v)?;
        let action = args::req_str(&args_v, "BrowserAct", "action")?.to_string();
        let selector = args::opt_str(&args_v, "BrowserAct", "selector")?
            .unwrap_or_default()
            .to_string();
        let text = args::opt_str(&args_v, "BrowserAct", "text")?
            .unwrap_or_default()
            .to_string();
        let value = args::opt_str(&args_v, "BrowserAct", "value")?
            .unwrap_or_default()
            .to_string();
        let url = args::opt_str(&args_v, "BrowserAct", "url")?
            .unwrap_or_default()
            .trim()
            .to_string();
        let timeout = args::opt_u64(&args_v, "BrowserAct", "timeout_ms")?
            .unwrap_or(DEFAULT_WAIT_MS)
            .min(MAX_WAIT_MS);

        // `chromehand::actions::navigate` runs no policy check of its own — the
        // CLI checked before calling it. Without this line, `navigate` would be
        // the hole through which `file:///` and loopback URLs reach a browser
        // that every other entry point refuses them from.
        let policy = self.pool.policy().map_err(map_error)?;
        if action == "navigate" {
            policy.check(&url).map_err(ToolError::BadArguments)?;
        }

        let id = session.id.clone();
        let live = Live::connect(&self.pool, &id).await?;
        let c = live.page();
        let raw = match action.as_str() {
            "click" => actions::click(c, &policy, &selector).await,
            "type" => actions::type_text(c, &policy, &selector, &text).await,
            "select" => actions::select(c, &policy, &selector, &value).await,
            "navigate" => actions::navigate(c, &url).await,
            direction @ ("back" | "forward") => actions::history(c, direction).await,
            _ => {
                let sel = (!selector.is_empty()).then_some(selector.as_str());
                let txt = (!text.is_empty()).then_some(text.as_str());
                // Neither condition given means "wait for the network to go
                // quiet", which is the useful default after a click that fires
                // an XHR — and it is a real condition, not a sleep.
                let idle = sel.is_none() && txt.is_none();
                actions::wait_for(c, sel, txt, None, idle, timeout).await
            }
        };
        let result = live.finish(raw).await?;

        let rendered = render::act(&action, &id, &result);
        Ok(ToolOutcome::new(rendered.content).with_display(rendered.display))
    }
}

// endregion: BrowserAct

// region: BrowserFill
// ---------------------------------------------------------------------------
// BrowserFill
//
// Its own tool, and its own permission name, even though what it ships today is
// close to a batch of `type` calls. The reason is in the comment on the struct.
// ---------------------------------------------------------------------------

/// Fill several form fields in one call. **Never submits.**
///
/// # Why this is a separate tool from `BrowserAct`
///
/// Not for the batching. `BrowserFill(domain:example.com)` has to be a rule a
/// user can write, and it has to keep meaning the same thing when submission
/// lands: "may Emma put my data into forms on this host". Folding fill into
/// `BrowserAct` now and splitting it out later would silently change what every
/// already-written `BrowserAct` rule permits, which is the one kind of change a
/// permissions file must never make quietly.
///
/// # Why it does not submit, in this pass
///
/// The design this was built from asks for `submit` behind a prompt that shows
/// the payload, on top of chromehand's existing two-key rule. Both halves are
/// currently unavailable, and neither is a matter of effort:
///
/// 1. **chromehand's second key is not out-of-band here.** `forms::submit`
///    requires `allow_auto_submit` in `data/browser-miner-config.json`, resolved
///    against the *process's working directory* — which, inside Emma, is the
///    user's repository. `Bash` can write that file. A second key both parties
///    can turn is one key. Making it a home-directory file means editing
///    vendored code that is deliberately kept verbatim, and it is worth doing
///    deliberately rather than as a side effect of this pass.
/// 2. **"The payload was shown" cannot be guaranteed from here.** The prompt is
///    rendered by `emma::approval`, and an `allow` rule or an `a` answer skips
///    it entirely. A tool cannot force its own gate. Until the gate can pin one
///    tool to always-ask-with-the-payload, "a submission never happens without
///    the payload having been shown" would be a claim nothing enforces.
///
/// So the guarantee this ships is the strict one: **nothing in this surface
/// submits a form.** `chromehand::actions::click` additionally refuses
/// submit-typed controls structurally, and `forms::submit` is not reachable from
/// any tool. The user submits by hand in a `headful` session, which is also the
/// best payload display there is — the real form, in a real window.
pub struct BrowserFill {
    pool: Arc<BrowserPool>,
}

impl BrowserFill {
    pub fn new(pool: Arc<BrowserPool>) -> Self {
        Self { pool }
    }
}

const FILL_KEYS: &[&str] = &["session", "fields"];

/// How much of one value the approval prompt shows before cutting.
///
/// The prompt is the payload, not a summary of it — but a pasted essay in a
/// textarea would push the rest of the fields off the screen, and a prompt
/// nobody can take in is the same failure as a prompt with no detail.
const PROMPT_VALUE_CHARS: usize = 60;

#[async_trait::async_trait]
impl Tool for BrowserFill {
    fn name(&self) -> &'static str {
        "BrowserFill"
    }

    fn description(&self) -> &str {
        include_str!("../descriptions/browser_fill.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session": { "type": "string", "description": "The session id BrowserOpen returned." },
                "fields": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The fields to fill, each addressed by a selector from a BrowserRead listing.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "selector": { "type": "string", "description": "The field's CSS selector." },
                            "value": { "type": "string", "description": "Text to type, or the option to choose in a <select>." },
                            "checked": { "type": "boolean", "description": "For a checkbox or radio: the state to leave it in." }
                        },
                        "required": ["selector"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["session", "fields"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            reaches_network: true,
            idempotent: false,
        }
    }

    /// The host, and **the payload**, in the one line the human reads.
    ///
    /// `emma::approval::network_preview` prints `detail` under the host, so this
    /// is where the field names and the values go. It is the payload rather than
    /// a count of it, because "fill 4 fields?" is not a question anybody can
    /// evaluate, and an unevaluatable prompt manufactures consent.
    fn network_target(&self, args_v: &Value) -> Option<NetworkTarget> {
        let id = args_v.get("session")?.as_str()?.trim();
        let s = self.pool.get(id)?;
        if s.host.is_empty() {
            return None;
        }
        let payload: Vec<String> = args_v
            .get("fields")?
            .as_array()?
            .iter()
            .map(|f| {
                let selector = f.get("selector").and_then(Value::as_str).unwrap_or("?");
                match (
                    f.get("value").and_then(Value::as_str),
                    f.get("checked").and_then(Value::as_bool),
                ) {
                    (Some(v), _) => format!("{selector} = {}", clip(v, PROMPT_VALUE_CHARS)),
                    (None, Some(c)) => {
                        format!("{selector} = {}", if c { "checked" } else { "unchecked" })
                    }
                    _ => format!("{selector} = (nothing)"),
                }
            })
            .collect();
        Some(NetworkTarget::new(
            &s.host,
            format!("fill a form at {}\n  {}", s.url, payload.join("\n  ")),
        ))
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, "BrowserFill", FILL_KEYS)?;
        need_session(&self.pool, "BrowserFill", args_v)?;
        let fields = args_v
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ToolError::BadArguments("BrowserFill requires fields, an array".into())
            })?;
        if fields.is_empty() {
            return Err(ToolError::BadArguments(
                "BrowserFill.fields is empty — there is nothing to fill".into(),
            ));
        }
        for (i, f) in fields.iter().enumerate() {
            let obj = f.as_object().ok_or_else(|| {
                ToolError::BadArguments(format!(
                    "BrowserFill.fields[{i}] must be an object with a selector"
                ))
            })?;
            for key in obj.keys() {
                if !["selector", "value", "checked"].contains(&key.as_str()) {
                    return Err(ToolError::BadArguments(format!(
                        "BrowserFill.fields[{i}] does not take {key}; accepted are selector, value, checked"
                    )));
                }
            }
            match obj.get("selector").and_then(Value::as_str) {
                Some(s) if !s.trim().is_empty() => {}
                _ => {
                    return Err(ToolError::BadArguments(format!(
                        "BrowserFill.fields[{i}] needs a selector"
                    )))
                }
            }
            if obj.get("value").is_none() && obj.get("checked").is_none() {
                return Err(ToolError::BadArguments(format!(
                    "BrowserFill.fields[{i}] needs value or checked"
                )));
            }
        }
        Ok(())
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(args_v).await)
    }
}

impl BrowserFill {
    async fn run(&self, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let session = need_session(&self.pool, "BrowserFill", &args_v)?;
        let spec = FillSpec {
            fields: args_v
                .get("fields")
                .and_then(Value::as_array)
                .map(|fs| {
                    fs.iter()
                        .map(|f| FillField {
                            selector: f
                                .get("selector")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            value: f.get("value").and_then(Value::as_str).map(str::to_string),
                            checked: f.get("checked").and_then(Value::as_bool),
                            // **Never exposed.** chromehand's `fill` can attach a
                            // local file to an `input[type=file]`, which is a
                            // different capability entirely — it sends a file off
                            // this machine, and nothing in the schema above can
                            // ask for it.
                            file: None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };

        let policy = self.pool.policy().map_err(map_error)?;
        let live = Live::connect(&self.pool, &session.id).await?;
        let raw = forms::fill(live.page(), &policy, &spec, None).await;
        let result = live.finish(raw).await?;

        let requested = result
            .get("fields_requested")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let ok = result.get("fields_ok").and_then(Value::as_u64).unwrap_or(0);
        let mut content = format!(
            "Filled {ok} of {requested} fields in session {}.\n\
             Nothing was submitted — this tool cannot submit, and no tool here can. \
             Each field below was re-read from the live page, so it is what is actually \
             there rather than what was sent.\n\n",
            session.id
        );
        for entry in result
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let selector = entry.get("selector").and_then(Value::as_str).unwrap_or("?");
            let good = entry.get("ok").and_then(Value::as_bool) == Some(true);
            let now = entry
                .get("now_contains")
                .and_then(Value::as_str)
                .unwrap_or("");
            let why = entry
                .get("refused")
                .and_then(Value::as_str)
                .or_else(|| entry.get("error").and_then(Value::as_str));
            content.push_str(&match (good, why) {
                (true, _) => format!("- `{selector}` now reads: {now}\n"),
                (false, Some(why)) => format!("- `{selector}` NOT set: {why}\n"),
                (false, None) => format!("- `{selector}` NOT set (the field reads: {now})\n"),
            });
        }
        if ok < requested {
            content.push_str(
                "\nSome fields did not take. A password field is refused structurally — this \
                 surface never handles credentials — and a field the page disabled or \
                 re-rendered needs a fresh BrowserRead for its current selector.\n",
            );
        }
        Ok(ToolOutcome::new(content).with_display(format!("fill — {ok}/{requested} fields set")))
    }
}

fn clip(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n {
        format!("{}…", flat.chars().take(n).collect::<String>())
    } else {
        flat
    }
}

// endregion: BrowserFill

// region: BrowserClose
// ---------------------------------------------------------------------------
// BrowserClose
//
// The only tool here that makes the machine tidier than it found it, and the
// reason it is not gated.
// ---------------------------------------------------------------------------

pub struct BrowserClose {
    pool: Arc<BrowserPool>,
}

impl BrowserClose {
    pub fn new(pool: Arc<BrowserPool>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl Tool for BrowserClose {
    fn name(&self) -> &'static str {
        "BrowserClose"
    }

    fn description(&self) -> &str {
        include_str!("../descriptions/browser_close.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session": {
                    "type": "string",
                    "description": "The session to close. Omit to close every session this run opened."
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // **Declared `true`, and the argument is worth reading before it is
            // changed.** By the letter of the field this ends a process, and the
            // field says "no process spawned". But the only processes it can
            // touch are the ones this same surface started — an id not in the
            // pool is refused — and the effect is to *undo* the change
            // `BrowserOpen` made to this machine, including closing an
            // unauthenticated CDP port.
            //
            // The alternative is a prompt on cleanup, and the failure mode of a
            // prompt on cleanup is that somebody answers `n` and a browser
            // survives with a live remote-control port. This crate has already
            // leaked a Chrome once. Gating the thing that prevents it is how it
            // happens again.
            read_only: true,
            // Nothing leaves the machine: this is a local kill.
            reaches_network: false,
            idempotent: true,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, "BrowserClose", &["session"])?;
        args::opt_str(args_v, "BrowserClose", "session")?;
        Ok(())
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(args_v).await)
    }
}

impl BrowserClose {
    async fn run(&self, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        match args::opt_str(&args_v, "BrowserClose", "session")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(id) => {
                // An unknown id is a *result*, not an error: the session being
                // gone is what the caller wanted. Saying so plainly stops the
                // model retrying a close that has already happened.
                match self.pool.close(id).await {
                    Some(s) => Ok(ToolOutcome::new(format!(
                        "Closed browser session {id} (was at {}). Chrome process {} is gone.",
                        s.url, s.pid
                    ))
                    .with_display(format!("closed session {id}"))),
                    None => Ok(ToolOutcome::new(format!(
                        "There is no open browser session '{id}' — nothing to close."
                    ))
                    .with_display("nothing to close")),
                }
            }
            None => {
                let closed = self.pool.close_all().await;
                Ok(if closed.is_empty() {
                    ToolOutcome::new("No browser sessions were open.")
                        .with_display("nothing to close")
                } else {
                    ToolOutcome::new(format!(
                        "Closed {} browser session(s): {}. Every Chrome they started is gone.",
                        closed.len(),
                        closed.join(", ")
                    ))
                    .with_display(format!("closed {} session(s)", closed.len()))
                })
            }
        }
    }
}

// endregion: BrowserClose

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The declarations and the argument checks, all decidable without a browser.
// The two guarantees that need a real process — a Chrome never outlives the run,
// and a session drives a real page — are in `tests/browser_lifecycle.rs` and
// `tests/browser_live.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::browser_tools;

    fn pool_at(host_url: &str) -> Arc<BrowserPool> {
        let pool = Arc::new(BrowserPool::new(None));
        pool.insert_for_test(super::super::pool::test_session("s1", host_url));
        assert_eq!(pool.len(), 1);
        pool
    }

    fn args_for(session: &str) -> Value {
        json!({ "session": session })
    }

    /// The whole cross-origin mechanism, at the level the gate sees it. If
    /// `network_target` ever reads the host out of the *arguments* — which is
    /// what every other tool in the workspace does — a session that wandered to
    /// another origin keeps reporting the host the user approved, and one grant
    /// becomes unlimited egress.
    #[tokio::test]
    async fn a_session_read_names_the_host_the_session_is_on_now() {
        let pool = pool_at("https://example.com/start");
        let read = BrowserRead::new(pool.clone());
        assert_eq!(
            read.network_target(&args_for("s1")).unwrap().host,
            "example.com"
        );
        pool.arrived_at("s1", "https://evil.example/landing");
        assert_eq!(
            read.network_target(&args_for("s1")).unwrap().host,
            "evil.example",
            "the gate was told the original host after the session navigated away from it"
        );
    }

    #[tokio::test]
    async fn acting_asks_about_the_session_host_and_navigating_asks_about_the_destination() {
        let pool = pool_at("https://example.com/start");
        let act = BrowserAct::new(pool.clone());
        let click = json!({ "session": "s1", "action": "click", "selector": "#x" });
        assert_eq!(act.network_target(&click).unwrap().host, "example.com");
        // A deliberate navigation is the one case where the destination is
        // knowable before the bytes, so it is asked about before them.
        let go = json!({ "session": "s1", "action": "navigate", "url": "https://other.test/p" });
        assert_eq!(act.network_target(&go).unwrap().host, "other.test");
    }

    /// The prompt is the payload. `emma::approval` prints `detail` under the
    /// host, and "fill 4 fields?" is not a question anybody can evaluate.
    #[test]
    fn the_fill_prompt_carries_the_field_values_and_not_a_count_of_them() {
        let pool = pool_at("https://example.com/form");
        let fill = BrowserFill::new(pool);
        let target = fill
            .network_target(&json!({
                "session": "s1",
                "fields": [
                    { "selector": "#email", "value": "someone@example.com" },
                    { "selector": "#terms", "checked": true }
                ]
            }))
            .expect("no target");
        assert!(
            target.detail.contains("someone@example.com"),
            "{}",
            target.detail
        );
        assert!(
            target.detail.contains("#terms = checked"),
            "{}",
            target.detail
        );
    }

    /// The guarantee this pass actually ships, written as a property of the
    /// surface rather than of one code path: nothing here can submit. Delete the
    /// reasoning on `BrowserFill` and add a `submit` argument, and this fails.
    #[test]
    fn no_tool_in_this_surface_offers_a_way_to_submit_a_form() {
        let (tools, _pool) = browser_tools(None);
        // Every property name in every schema, however deeply nested — the
        // hazard is a `submit` flag appearing inside `fields[]` rather than at
        // the top level, where a shallow check would miss it.
        fn property_names(v: &Value, into: &mut Vec<String>) {
            if let Some(props) = v.get("properties").and_then(Value::as_object) {
                for (name, spec) in props {
                    into.push(name.clone());
                    property_names(spec, into);
                }
            }
            if let Some(items) = v.get("items") {
                property_names(items, into);
            }
        }
        for tool in &tools {
            let mut names = Vec::new();
            property_names(&tool.input_schema(), &mut names);
            assert!(
                !names.iter().any(|n| n.contains("submit")),
                "{} offers a submit argument; submission needs a payload-visible prompt and an \
                 out-of-band second key, and neither exists yet",
                tool.name()
            );
        }
        // …and the description says so, because a capability the model believes
        // exists is a wasted turn per attempt.
        let fill = tools
            .iter()
            .find(|t| t.name() == "BrowserFill")
            .expect("no BrowserFill");
        assert!(
            fill.description().to_lowercase().contains("never submit")
                || fill
                    .description()
                    .to_lowercase()
                    .contains("does not submit"),
            "BrowserFill's description does not say it cannot submit"
        );
    }

    #[test]
    fn the_two_tools_that_change_someone_elses_state_are_the_two_that_ask() {
        let (tools, _pool) = browser_tools(None);
        let meta = |name: &str| {
            tools
                .iter()
                .find(|t| t.name() == name)
                .unwrap_or_else(|| panic!("no {name}"))
                .meta()
        };
        // Acting and filling are gated; reading is not, and opening is because
        // it leaves a live browser behind.
        assert!(!meta("BrowserAct").read_only);
        assert!(!meta("BrowserFill").read_only);
        assert!(!meta("BrowserOpen").read_only);
        assert!(meta("BrowserRead").read_only);
        // Cleanup is not gated. See the argument on `BrowserClose::meta` before
        // changing this: a prompt on cleanup that is answered `n` leaves a
        // browser running with an unauthenticated CDP port.
        assert!(meta("BrowserClose").read_only);
        assert!(!meta("BrowserClose").reaches_network);
        // Everything that touches a page declares egress, or the gate never
        // sees the host at all.
        for name in ["BrowserOpen", "BrowserRead", "BrowserAct", "BrowserFill"] {
            assert!(meta(name).reaches_network, "{name} hides its egress");
        }
    }

    #[test]
    fn an_unknown_session_is_an_argument_error_that_lists_what_is_open() {
        let pool = pool_at("https://example.com/");
        let read = BrowserRead::new(pool);
        let err = read
            .validate_args(&args_for("nope"))
            .expect_err("an unknown session validated");
        assert_eq!(err.kind(), "bad_arguments");
        assert!(err.detail().contains("s1"), "{}", err.detail());
    }

    #[test]
    fn every_action_says_which_argument_it_is_missing() {
        let pool = pool_at("https://example.com/");
        let act = BrowserAct::new(pool);
        let cases = [
            (json!({ "session": "s1", "action": "click" }), "selector"),
            (
                json!({ "session": "s1", "action": "type", "selector": "#a" }),
                "text",
            ),
            (
                json!({ "session": "s1", "action": "select", "selector": "#a" }),
                "value",
            ),
            (json!({ "session": "s1", "action": "navigate" }), "url"),
        ];
        for (args_v, missing) in cases {
            let err = act.validate_args(&args_v).expect_err("validated");
            assert!(
                err.detail().contains(missing),
                "the error did not name the missing argument: {}",
                err.detail()
            );
        }
        // …and `back`, `forward` and `wait_for` need nothing beyond the session.
        for action in ["back", "forward", "wait_for"] {
            act.validate_args(&json!({ "session": "s1", "action": action }))
                .unwrap_or_else(|e| panic!("{action}: {e}"));
        }
    }

    #[test]
    fn an_unknown_action_lists_the_ones_that_exist() {
        let pool = pool_at("https://example.com/");
        let act = BrowserAct::new(pool);
        let err = act
            .validate_args(&json!({ "session": "s1", "action": "screenshot" }))
            .expect_err("validated");
        assert!(err.detail().contains("click"), "{}", err.detail());
    }

    #[test]
    fn a_fill_field_must_say_what_to_put_in_it() {
        let pool = pool_at("https://example.com/");
        let fill = BrowserFill::new(pool);
        for bad in [
            json!({ "session": "s1", "fields": [] }),
            json!({ "session": "s1", "fields": [{ "value": "x" }] }),
            json!({ "session": "s1", "fields": [{ "selector": "#a" }] }),
            // The one that matters: a local file cannot be attached to a form.
            json!({ "session": "s1", "fields": [{ "selector": "#a", "file": "/etc/passwd" }] }),
        ] {
            assert_eq!(
                fill.validate_args(&bad).expect_err("validated").kind(),
                "bad_arguments",
                "{bad}"
            );
        }
    }

    /// **The hole `navigate` would otherwise be.** Every other way into a
    /// browser here runs chromehand's URL policy — `BrowserOpen` does it before
    /// Chrome starts, `WebFetch` does it in `digest_url` — but
    /// `chromehand::actions::navigate` runs none of its own, because upstream's
    /// CLI checked before calling it. Without the check in `BrowserAct::run`,
    /// `navigate` is the one verb that reaches `file:///` and localhost, from a
    /// session the user opened for something else entirely.
    #[tokio::test]
    async fn navigating_a_session_is_held_to_the_same_url_policy_as_opening_one() {
        let pool = pool_at("https://example.com/");
        let act = BrowserAct::new(pool.clone());
        for url in [
            "file:///etc/passwd",
            "http://localhost:8080/admin",
            "http://127.0.0.1:9/x",
            "chrome://settings",
        ] {
            let err = act
                .run(json!({ "session": "s1", "action": "navigate", "url": url }))
                .await
                .expect_err("a refused URL was navigated to");
            assert_eq!(err.kind(), "bad_arguments", "{url}: {err}");
        }
        // …and the same refusals at the point a session is opened, which is the
        // half that runs before Chrome is even started.
        for url in ["file:///etc/passwd", "http://127.0.0.1:9/x"] {
            let err = pool
                .open(url, false)
                .await
                .expect_err("a refused URL opened a browser");
            assert!(
                matches!(err, crate::chromehand::MinerError::Refused(_)),
                "{url}: {err:?}"
            );
        }
    }

    #[test]
    fn a_misspelled_parameter_is_named_rather_than_ignored() {
        let pool = pool_at("https://example.com/");
        let read = BrowserRead::new(pool);
        let msg = read
            .validate_args(&json!({ "session": "s1", "maxChars": 10 }))
            .expect_err("validated")
            .to_string();
        assert!(msg.contains("maxChars"), "{msg}");
        assert!(msg.contains("max_chars"), "{msg}");
    }

    /// **What breaks in the real world:** `max_chars: 0` is a read that returns
    /// nothing, which the model reads as "the page is empty" rather than as
    /// "you asked for no characters". It then acts on a page it believes has no
    /// content. The refusal is what turns a silent wrong answer into a fixable
    /// argument error.
    ///
    /// The control is the point of the second half: `1` and an omitted budget
    /// both have to survive, or the clamp has become a blanket refusal and no
    /// read works at all.
    #[test]
    fn a_read_budget_of_zero_is_refused_and_an_ordinary_one_is_not() {
        let pool = pool_at("https://example.com/");
        let read = BrowserRead::new(pool);
        for key in ["max_chars", "max_links"] {
            let mut args_v = json!({ "session": "s1" });
            args_v[key] = json!(0);
            let err = read
                .validate_args(&args_v)
                .expect_err(&format!("{key}: zero validated"));
            assert_eq!(err.kind(), "bad_arguments");
            assert!(
                err.detail().contains(key) && err.detail().contains("at least 1"),
                "the message does not say what a usable value is: {}",
                err.detail()
            );
        }
        read.validate_args(&json!({ "session": "s1", "max_chars": 1, "max_links": 1 }))
            .expect("a budget of one was refused");
        read.validate_args(&json!({ "session": "s1" }))
            .expect("a read with no budget at all was refused");
    }

    /// **What breaks in the real world:** `timeout_ms: 0` is a `wait_for` that
    /// gives up before it looks, so every wait reports the condition was never
    /// met and the model concludes the page is broken. A number the caller can
    /// write that means "never succeed" is worse than no number.
    #[test]
    fn a_wait_of_zero_is_refused_and_an_ordinary_one_is_not() {
        let pool = pool_at("https://example.com/");
        let act = BrowserAct::new(pool);
        let err = act
            .validate_args(&json!({ "session": "s1", "action": "wait_for", "timeout_ms": 0 }))
            .expect_err("a zero timeout validated");
        assert!(err.detail().contains("at least 1"), "{}", err.detail());
        act.validate_args(&json!({ "session": "s1", "action": "wait_for", "timeout_ms": 1 }))
            .expect("a one-millisecond wait was refused");
        act.validate_args(&json!({ "session": "s1", "action": "wait_for" }))
            .expect("a wait with no timeout was refused");
    }

    /// **What breaks in the real world:** a form is filled on a host the user
    /// never approved. `BrowserFill` computes its own `network_target` rather
    /// than calling `session_target` — it has to, because the payload goes in
    /// the detail line — so the cross-origin re-ask is implemented *twice* in
    /// this file, and only `BrowserRead`'s copy had a test. A session that
    /// wandered to `evil.example` and then gets an email address typed into it
    /// is the whole failure this mechanism exists to prevent.
    #[tokio::test]
    async fn filling_asks_about_the_host_the_session_is_on_now() {
        let pool = pool_at("https://example.com/form");
        let fill = BrowserFill::new(pool.clone());
        let args = json!({
            "session": "s1",
            "fields": [{ "selector": "#email", "value": "someone@example.com" }]
        });
        assert_eq!(fill.network_target(&args).unwrap().host, "example.com");

        pool.arrived_at("s1", "https://evil.example/form");
        let after = fill.network_target(&args).expect("no target");
        assert_eq!(
            after.host, "evil.example",
            "the gate was asked about the host the session was opened on, so a grant for it \
             would silently cover typing into a form on another origin"
        );
        // …and the prompt shows the new URL, so the human is answering about the
        // page the data is actually going to.
        assert!(
            after.detail.contains("https://evil.example/form"),
            "{}",
            after.detail
        );
    }

    /// **What breaks in the real world:** the model is told a browser was closed
    /// when it was not, or is told nothing useful and closes again. The answer
    /// names the session and the URL it was on, which is also the only record of
    /// what was open once the registry entry is gone.
    ///
    /// Runs offline: the session behind this has `pid: 0` and a websocket on a
    /// port nothing listens on, so `close` fails its polite CDP call, kills
    /// nothing, and returns — which is exactly the path a browser that already
    /// exited takes.
    #[tokio::test]
    async fn closing_a_known_session_reports_what_it_ended_and_forgets_it() {
        let pool = Arc::new(BrowserPool::new(None));
        let id = format!("emma-tool-close-{}", std::process::id());
        pool.insert_for_test(super::super::pool::test_session(
            &id,
            "https://example.com/inbox",
        ));
        let close = BrowserClose::new(pool.clone());

        let out = close
            .run(json!({ "session": id }))
            .await
            .expect("closing an open session was reported as an error");
        assert!(out.content.contains(&id), "{}", out.content);
        assert!(
            out.content.contains("https://example.com/inbox"),
            "the answer does not say what was closed: {}",
            out.content
        );
        assert!(pool.is_empty(), "a closed session is still in the registry");

        // The second call is the control on "closed" meaning something: the same
        // id now gets the nothing-to-close answer rather than repeating the
        // first one.
        let again = close
            .run(json!({ "session": id }))
            .await
            .expect("a repeat close errored");
        assert!(
            again.content.contains("nothing to close"),
            "{}",
            again.content
        );
    }
}

// endregion: Tests
