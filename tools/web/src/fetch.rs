//! `WebFetch` — one page, rendered in real Chrome, returned as markdown.
//!
//! Not registered anywhere, and so never yet called by a model — see the crate
//! docs for why the wiring is deferred rather than missed. What follows
//! describes the tool as built.
//!
//! **Why a browser rather than an HTTP client and an html-to-markdown crate.**
//! Three reasons, in order of how often they bite. A large share of the pages
//! worth reading are JavaScript shells that serve an empty `<div>` to `curl`.
//! A further share refuse plain fetches outright — WorkAtAStartup answers a
//! bare request with 406 and renders fine in Chrome. And what comes back is
//! not a DOM dump: chromehand strips boilerplate and pre-extracts the
//! interactive inventory, which upstream measured at 4.6–5.9× smaller than the
//! raw HTML on live pages, so the saving is in the model's context and not
//! just on the wire. That figure is upstream's and is not reproducible here —
//! the live-network bench that produced it stayed in canonical. What this fork
//! keeps is the raw material for the comparison: every digest carries
//! `economy.raw_html_chars` alongside `economy.digest_chars`.
//!
//! The cost is honest: this spawns Chrome, and Chrome is heavy. A stateless
//! read is two to three seconds of launch before any bytes are read. That is
//! the price of pages that actually render, and it is why chromehand has a
//! session mode — not used here, because sessions are state and state needs an
//! owner.
//!
//! **What this tool cannot do, deliberately.** It reads. It does not click,
//! type, fill or submit. `WebFetch::run` calls exactly one thing —
//! [`chromehand::digest_url`] — and that function's only verbs are launch,
//! probe, tear down. The interaction code is vendored whole in
//! `crate::chromehand::actions` and `::forms` and is reachable only from the
//! `browser-miner` CLI, where a human is the one typing. The scope decision
//! was `digest` only: a browser that can submit a form inside an agent loop is
//! a separate decision with its own approval story, and folding it into "add
//! web tools" is how such a thing gets decided by accident. chromehand already
//! gates those verbs behind a user-owned domain allowlist and a two-key rule
//! for auto-submit; neither has an equivalent in Emma's approval flow yet.

use std::path::PathBuf;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::chromehand::{self, DigestOptions, MinerError};
use crate::digest_md;

// region: Limits, and where the allowlist may live
// ---------------------------------------------------------------------------
// Limits, and where the allowlist may live
//
// The caps the model can move within, and the one file that can restrict what
// it may reach. Home, never the project directory — a file inside the
// repository silently changing which domains are reachable is the same class
// of surprise as a credentials file being picked up from there.
// ---------------------------------------------------------------------------

const NAME: &str = "WebFetch";
const KEYS: &[&str] = &["url", "max_chars"];

/// chromehand's own default. Roughly two thousand tokens of prose — enough for
/// most articles, and the cap is raisable per call.
pub const DEFAULT_MAX_CHARS: u64 = chromehand::DEFAULT_MAX_TEXT_CHARS as u64;
/// A page is not a corpus. Past this the model is being handed something it
/// should be searching, not reading.
pub const MAX_MAX_CHARS: u64 = 200_000;

/// The allowlist file, if the user keeps one. **Home, never the project
/// directory** — see [`chromehand::load_policy`].
fn home_allowlist() -> Option<PathBuf> {
    let path = emma_llm::auth::home_dir()?
        .join(".emma")
        .join("browser-allowlist.json");
    path.is_file().then_some(path)
}

// endregion: Limits, and where the allowlist may live

// region: The tool
// ---------------------------------------------------------------------------
// The tool
//
// Construction, the schema and description the model would read, and the run
// path. Note how little happens in `run`: validate, clamp, call, render. The
// tool is thin because the judgement lives in chromehand and the honesty lives
// in the renderer.
// ---------------------------------------------------------------------------

pub struct WebFetch {
    allowlist: Option<PathBuf>,
}

impl WebFetch {
    /// Construct only if a browser exists — the constructor a registration
    /// would call.
    ///
    /// The check builds a `BrowserConfig` and throws the result away. That
    /// looks pointless and is not: building the config is where chromiumoxide
    /// resolves the Chrome executable, so the failure surfaces here, at
    /// startup, with a message a human can act on, rather than on a first
    /// fetch several turns into a task. A tool that is present and cannot work
    /// is worse than an absent one — the model plans around it and loses the
    /// turn discovering otherwise.
    pub fn detect() -> Result<Self, String> {
        chromiumoxide::browser::BrowserConfig::builder()
            .build()
            .map_err(|e| {
                format!("Chrome or Chromium could not be found ({e}). Install it, or set CHROME")
            })?;
        Ok(Self {
            allowlist: home_allowlist(),
        })
    }

    /// Construct without the browser check. For tests and for callers that
    /// have already decided.
    pub fn new() -> Self {
        Self {
            allowlist: home_allowlist(),
        }
    }
}

impl Default for WebFetch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/web_fetch.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "An http:// or https:// URL. Loopback, file: and chrome: URLs are refused."
                },
                "max_chars": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!(
                        "Characters of page text to return. Default {DEFAULT_MAX_CHARS}, capped at {MAX_MAX_CHARS}. \
                         Truncation is always reported."
                    )
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // Reaches the network and drives a browser, but cannot modify the
            // working directory, submit a form, or run anything the model
            // chose. The throwaway Chrome profile lives in the system temp
            // directory and is removed on teardown. See the crate docs: the
            // field cannot express "may not write" and "may not talk to the
            // outside" at once, the gate asks the first question, and the
            // second one is the reason this tool is not wired up yet.
            read_only: true,
            // Two reads of the same URL have the same effect as one — none.
            // Not a claim that the page will say the same thing twice.
            idempotent: true,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        let url = args::req_str(args_v, NAME, "url")?;
        if url.trim().is_empty() {
            return Err(ToolError::BadArguments("WebFetch.url is empty".into()));
        }
        if let Some(0) = args::opt_u64(args_v, NAME, "max_chars")? {
            return Err(ToolError::BadArguments(
                "WebFetch.max_chars must be at least 1".into(),
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

impl WebFetch {
    async fn run(&self, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let url = args::req_str(&args_v, NAME, "url")?.trim().to_string();
        let max_chars = args::opt_u64(&args_v, NAME, "max_chars")?
            .unwrap_or(DEFAULT_MAX_CHARS)
            .min(MAX_MAX_CHARS);

        let opts = DigestOptions {
            max_text_chars: max_chars as usize,
            allowlist: self.allowlist.clone(),
            ..DigestOptions::default()
        };

        // The whole mapping, in one place. A blocked page, a rendered 404 and
        // a page with no text never arrive here — they are `Ok` digests, and
        // they render as prose that says what happened.
        let digest = chromehand::digest_url(&url, &opts)
            .await
            .map_err(map_error)?;

        let rendered = digest_md::render(&digest).map_err(ToolError::Failed)?;
        let outcome = ToolOutcome::new(rendered.markdown).with_display(rendered.display);
        Ok(if rendered.truncated {
            outcome.truncated()
        } else {
            outcome
        })
    }
}

// endregion: The tool

// region: chromehand's failures into Emma's
// ---------------------------------------------------------------------------
// chromehand's failures into Emma's
//
// The seam the whole tool exists to get right, kept to one function so the
// three classes cannot drift apart across call sites.
// ---------------------------------------------------------------------------

/// chromehand's failure taxonomy, into Emma's.
///
/// This is the seam the whole tool exists to get right, so it is one function
/// with one arm per variant rather than a chain of string checks at the call
/// site:
///
/// - a refusal or bad input is the *arguments* being wrong — the model can fix
///   a URL, so it must be told which part was wrong rather than that the web
///   is broken;
/// - a browser failure is a failure of the machinery, retryable;
/// - no Chrome at all is `Unavailable`, which reads "I cannot do this" and
///   never "this cannot be done".
fn map_error(e: MinerError) -> ToolError {
    match e {
        MinerError::Refused(m) => ToolError::BadArguments(m),
        MinerError::Browser(m) => ToolError::Failed(m),
        MinerError::Unavailable(m) => ToolError::Unavailable(m),
    }
}

// endregion: chromehand's failures into Emma's

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The mapping and the argument checks — everything decidable without a
// browser. The cases that need one are in `tests/fetch.rs` and
// `tests/integration.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn err(args: Value) -> ToolError {
        WebFetch::new().validate_args(&args).unwrap_err()
    }

    #[test]
    fn the_mapping_keeps_the_three_classes_apart() {
        // Collapsing any two of these is the failure this crate is written to
        // avoid: a refused URL the model could fix, a browser that broke and
        // could be retried, and a machine with no browser on it.
        assert_eq!(
            map_error(MinerError::Refused("refused: scheme 'file'".into())).kind(),
            "bad_arguments"
        );
        assert_eq!(
            map_error(MinerError::Browser("timeout: no result".into())).kind(),
            "tool_failed"
        );
        assert_eq!(
            map_error(MinerError::Unavailable("no chrome".into())).kind(),
            "tool_unavailable"
        );
    }

    #[test]
    fn a_misspelled_parameter_is_named_not_ignored() {
        let msg = err(json!({ "url": "https://x/", "maxChars": 10 })).to_string();
        assert!(msg.contains("maxChars"), "{msg}");
        assert!(msg.contains("max_chars"), "{msg}");
    }

    #[test]
    fn an_empty_url_is_an_argument_error() {
        assert_eq!(err(json!({ "url": "  " })).kind(), "bad_arguments");
        assert_eq!(err(json!({})).kind(), "bad_arguments");
    }
}

// endregion: Tests
