//! `WebFetch` — one page, rendered in real Chrome, returned as markdown.
//!
//! Registered when a browser can be found — see [`crate::web_tools`] — and
//! gated per host by `emma::approval`, which asks this tool where it is going
//! rather than reading its arguments.
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
//! for auto-submit. Emma's own gate now grants per host, which is the same
//! shape one level up — and still not an approval story for submitting a form,
//! which is a decision about what may be *sent*, not about where.

use std::path::PathBuf;

use emma_tool_api::{NetworkTarget, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
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
const KEYS: &[&str] = &["url", "max_chars", "max_links", "offset"];

/// chromehand's own default. Roughly two thousand tokens of prose — enough for
/// most articles, and the cap is raisable per call.
pub const DEFAULT_MAX_CHARS: u64 = chromehand::DEFAULT_MAX_TEXT_CHARS as u64;
/// A page is not a corpus. Past this the model is being handed something it
/// should be searching, not reading.
pub const MAX_MAX_CHARS: u64 = 200_000;

/// Links listed by default. Enough for an article plus its navigation.
///
/// **Why this is a separate knob rather than part of `max_chars`.** The two
/// cut different things and a page can hit one while nowhere near the other:
/// a news hub is four thousand characters of prose and a hundred links, so
/// `max_chars` was never the cap that bound and raising it would have returned
/// exactly the same fifty links. Before this argument existed there was no way
/// to ask for the rest at all, which made the truncation notice's advice
/// unfollowable — the honesty bug and the missing knob were the same bug.
pub const DEFAULT_MAX_LINKS: u64 = 50;
/// The browser's own collector stops at 120 links per page, so this is a
/// ceiling and not a preference: above it there is nothing left to return.
pub const MAX_MAX_LINKS: u64 = digest_md::COLLECTOR_LINK_BUDGET as u64;

/// The allowlist file, if the user keeps one. **Home, never the project
/// directory** — see [`chromehand::load_policy`].
pub(crate) fn home_allowlist() -> Option<PathBuf> {
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
                         Bounds the prose only — it does not affect how many links are listed. \
                         Truncation is always reported, with the numbers."
                    )
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": format!(
                        "Characters of page text to skip before the returned window starts. \
                         Default 0, the top of the page. A truncated read names the offset that \
                         continues it. Continuing costs a second fetch of the page — nothing is \
                         cached — so raise {DEFAULT_MAX_CHARS}-character windows only as far as \
                         the reading needs."
                    )
                },
                "max_links": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!(
                        "Links to list. Default {DEFAULT_MAX_LINKS}, capped at {MAX_MAX_LINKS} because the browser \
                         collects no more than that from one page. Raise it for an index or hub page, \
                         where the links are the content."
                    )
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // Drives a browser, but cannot modify the working directory,
            // submit a form, or run anything the model chose. The throwaway
            // Chrome profile lives in the system temp directory and is removed
            // on teardown. `read_only` asks "can this damage this machine" and
            // the honest answer is no.
            read_only: true,
            // …and the whole of the risk this tool carries is on the other
            // axis. See [`WebFetch::network_target`], which is what the
            // approval gate grants against.
            reaches_network: true,
            // Two reads of the same URL have the same effect as one — none.
            // Not a claim that the page will say the same thing twice.
            idempotent: true,
        }
    }

    /// The host in the URL the model asked for.
    ///
    /// Parsed here rather than in the gate: `emma::approval` must not learn
    /// that this tool's destination lives in an argument called `url`, or
    /// "adding a tool is a crate plus one registry line" stops being true.
    ///
    /// A URL that will not parse, or one with no host, yields `None` — which
    /// the gate treats as a refusal rather than a pass. That is the right way
    /// round: the URLs that fail to parse here are the same ones chromehand
    /// would refuse anyway, and they must not be the ones that slip through
    /// unasked.
    fn network_target(&self, args_v: &Value) -> Option<NetworkTarget> {
        let url = args_v.get("url")?.as_str()?.trim();
        let parsed = url::Url::parse(url).ok()?;
        Some(NetworkTarget::new(
            parsed.host_str()?,
            format!("read {url}"),
        ))
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
        if let Some(0) = args::opt_u64(args_v, NAME, "max_links")? {
            return Err(ToolError::BadArguments(
                "WebFetch.max_links must be at least 1".into(),
            ));
        }
        args::opt_u64(args_v, NAME, "offset")?;
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
        let window = text_window(&args_v)?;
        let max_chars = window.max_chars as u64;
        let max_links = args::opt_u64(&args_v, NAME, "max_links")?
            .unwrap_or(DEFAULT_MAX_LINKS)
            .min(MAX_MAX_LINKS);

        let opts = DigestOptions {
            max_text_chars: window.max_chars,
            text_offset: window.offset,
            allowlist: self.allowlist.clone(),
            ..DigestOptions::default()
        };

        // The whole mapping, in one place. A blocked page, a rendered 404 and
        // a page with no text never arrive here — they are `Ok` digests, and
        // they render as prose that says what happened.
        let digest = chromehand::digest_url(&url, &opts)
            .await
            .map_err(map_error)?;

        // **The host the gate asked about is the one in the argument; the bytes
        // came from wherever that host redirected to.** See
        // `crossed_a_host_the_gate_never_saw` for the whole argument.
        if let Some(refusal) =
            crossed_a_host_the_gate_never_saw(&url, digest.get("final_url").and_then(Value::as_str))
        {
            return Err(ToolError::Failed(refusal));
        }

        // `reading`, so the selectors stay off: this tool cannot click what it
        // finds, and addresses for a thing nothing can address are a thousand
        // tokens of noise. `BrowserRead` is where they come back.
        let limits = digest_md::Limits::reading(max_links as usize, max_chars as usize)
            .at_offset(window.offset);
        let rendered = digest_md::render(&digest, &limits).map_err(ToolError::Failed)?;
        Ok(into_outcome(rendered))
    }
}

/// The slice of page text this call asked for.
///
/// Its own function so the defaults and the ceiling can be checked without a
/// browser: this is where a continuation the model was *told* to make either
/// reaches chromehand or is silently rounded back to the top of the page.
fn text_window(args_v: &Value) -> Result<chromehand::TextWindow, ToolError> {
    let max_chars = args::opt_u64(args_v, NAME, "max_chars")?
        .unwrap_or(DEFAULT_MAX_CHARS)
        .min(MAX_MAX_CHARS);
    let offset = args::opt_u64(args_v, NAME, "offset")?.unwrap_or(0);
    Ok(chromehand::TextWindow {
        offset: offset.min(usize::MAX as u64) as usize,
        max_chars: max_chars as usize,
    })
}

/// The renderer's verdict, as a `ToolOutcome`.
///
/// Its own function, and not three lines inlined above, only so that it can be
/// tested without a browser. That is not a stylistic preference: this is the
/// join where the reason the renderer wrote can be dropped on the floor, and
/// the *only* other thing that notices is a test that launches Chrome and
/// reaches the live network. A guarantee whose sole guard is `#[ignore]`d is a
/// guarantee nobody checks.
fn into_outcome(rendered: digest_md::Rendered) -> ToolOutcome {
    let outcome = ToolOutcome::new(rendered.markdown).with_display(rendered.display);
    // The renderer already wrote the sentence, numbers and remedy included; it
    // is carried up whole rather than re-summarised, because every re-summary
    // a truncation notice passes through is where the numbers get lost.
    match rendered.truncation {
        Some(reason) => outcome.truncated_because(reason),
        None => outcome,
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

/// Why this call must not return its bytes: navigation ended on a host nobody
/// approved. `None` when it ended where it was asked to.
///
/// **The gate reads `network_target` from the *arguments*, before the call.**
/// The human is shown `read https://example.com/x` and approves `example.com`;
/// a persisted `WebFetch(domain:example.com)` rule answers for every future
/// call without a prompt at all. Chrome then follows any HTTP or JavaScript
/// redirect it is given, and `chromehand::digest_url` checks its policy once,
/// against the input URL, before the browser starts. So a grant for one host
/// silently became a grant for wherever that host chooses to send us — and on a
/// page that is itself hostile, the page picks the destination.
///
/// The web-surface design already makes this argument and
/// the browser session tools already act on it: they re-derive
/// `network_target` from the pool's `final_url` so the *next* verb re-asks.
/// `WebFetch` is one call and cannot re-ask inside itself, so the honest move
/// is the other one this project keeps making — refuse, and say exactly what
/// happened, so the model can call `WebFetch` on the real URL and the human
/// sees a prompt naming the host they are actually being asked about. A tool
/// failure is an observation, not an abort.
///
/// Exact host equality, matched case-insensitively through
/// [`crate::browser::pool::host_of`] so it is spelled the way the gate spells
/// it. Not subdomain-tolerant: `Rule::domain` is exact unless the human wrote
/// `*.`, and being laxer here than the rule the human agreed to is the whole
/// defect in miniature. A redirect that keeps the host — `http` to `https`, a
/// path change, a trailing slash — is not a redirect for this purpose and
/// passes silently.
///
/// A `final_url` that is absent or unparseable yields `None`. That is
/// deliberate and it is the weak spot, stated rather than hidden: a navigation
/// that failed outright has no final URL to check, and refusing those would
/// turn every timeout into a scary security message. It means this guard is
/// evidence-based, not a boundary — it catches a redirect that Chrome reported,
/// and it cannot catch one it did not.
fn crossed_a_host_the_gate_never_saw(requested: &str, final_url: Option<&str>) -> Option<String> {
    let asked = crate::browser::pool::host_of(requested)?;
    let landed = crate::browser::pool::host_of(final_url?)?;
    if asked == landed {
        return None;
    }
    Some(format!(
        "refused to return this page: {asked} redirected to {landed}, and the approval for this \
         call was for {asked}. Nothing was read back. If {landed} is what you want, call WebFetch \
         with that URL directly, so the host you are actually reading is the one in the prompt."
    ))
}

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
pub(crate) fn map_error(e: MinerError) -> ToolError {
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

    /// A redirect off the approved host is refused, and an ordinary one is not.
    ///
    /// **The silent cases are asserted first, and they are the ones that make
    /// this worth having rather than a nuisance.** A guard that refused every
    /// `http` to `https` upgrade, every trailing slash and every failed
    /// navigation would be turned off within a day, and then the real case
    /// would not be caught either.
    #[test]
    fn a_redirect_to_another_host_is_refused_and_an_ordinary_one_is_not() {
        // Same host: the scheme, the path and the slash may all change.
        assert!(
            crossed_a_host_the_gate_never_saw(
                "http://example.com/a",
                Some("https://example.com/a/b/")
            )
            .is_none(),
            "an upgrade and a path change were treated as a cross-host redirect"
        );
        // Spelled differently by the server, and the same host all the same.
        assert!(
            crossed_a_host_the_gate_never_saw(
                "https://Example.COM./x",
                Some("https://example.com/x")
            )
            .is_none(),
            "a case difference was treated as a different host, which would make \
             the guard fire on ordinary pages"
        );
        // No final URL at all — a navigation that failed. Stated in the doc as
        // the weak spot; asserted here so the weakness is deliberate rather
        // than discovered later.
        assert!(crossed_a_host_the_gate_never_saw("https://example.com/x", None).is_none());
        assert!(
            crossed_a_host_the_gate_never_saw("https://example.com/x", Some("")).is_none(),
            "an empty final_url was read as a redirect to nowhere"
        );

        // And the case the whole thing exists for.
        let refusal = crossed_a_host_the_gate_never_saw(
            "https://docs.rs/x",
            Some("https://evil.example/steal"),
        )
        .expect("a cross-host redirect was allowed to return its bytes");
        assert!(
            refusal.contains("docs.rs") && refusal.contains("evil.example"),
            "the refusal names neither the host approved nor the host reached, so \
             nobody can tell what happened: {refusal}"
        );
        assert!(
            refusal.contains("Nothing was read back"),
            "the refusal does not say whether the page reached the model, which is \
             the only question a reader has: {refusal}"
        );
        assert!(
            refusal.contains("call WebFetch"),
            "the refusal blocks the model without telling it the way through, so \
             it will retry the same call: {refusal}"
        );

        // A subdomain is a different host. `Rule::domain` is exact unless the
        // human wrote `*.`, and being laxer here than the rule they agreed to
        // is the defect this guard exists to close.
        assert!(
            crossed_a_host_the_gate_never_saw(
                "https://example.com/x",
                Some("https://www.example.com/x")
            )
            .is_some(),
            "a subdomain redirect passed, which is broader than the grant the \
             human actually gave"
        );
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

    /// The renderer knows which cap bound; the model and the human only find
    /// out if this join carries it. Reduce this to `outcome.truncated()` and
    /// the result is exactly the field report — a warning with no subject.
    #[test]
    fn the_renderers_reason_reaches_the_outcome_intact() {
        let reason = "50 of 70 links shown, 20 dropped by max_links=50";
        let cut = into_outcome(digest_md::Rendered {
            markdown: "# page".into(),
            truncation: Some(reason.into()),
            display: "page".into(),
        });
        assert!(cut.truncated);
        assert_eq!(cut.truncation.as_deref(), Some(reason));

        let whole = into_outcome(digest_md::Rendered {
            markdown: "# page".into(),
            truncation: None,
            display: "page".into(),
        });
        assert!(!whole.truncated);
        assert!(whole.truncation.is_none());
    }

    #[test]
    fn both_budgets_reject_zero_rather_than_silently_meaning_unlimited() {
        assert_eq!(
            err(json!({ "url": "https://x/", "max_chars": 0 })).kind(),
            "bad_arguments"
        );
        assert_eq!(
            err(json!({ "url": "https://x/", "max_links": 0 })).kind(),
            "bad_arguments"
        );
    }

    /// The schema is where the model learns a knob exists at all. A truncation
    /// notice that says "re-read with max_links=70" against a tool whose schema
    /// never mentioned `max_links` is advice the model has no reason to
    /// believe, and `deny_unknown` would refuse the retry.
    #[test]
    fn the_schema_offers_the_argument_the_truncation_notice_advertises() {
        let schema = WebFetch::new().input_schema();
        let props = schema["properties"].as_object().expect("no properties");
        assert!(props.contains_key("max_links"), "{schema}");
        assert!(props.contains_key("max_chars"), "{schema}");
        assert!(props.contains_key("offset"), "{schema}");
        // And the ceiling is stated, because raising past what the browser
        // collected returns the same page and wastes a turn.
        let doc = props["max_links"]["description"].as_str().unwrap();
        assert!(doc.contains(&MAX_MAX_LINKS.to_string()), "{doc}");
    }

    #[test]
    fn an_empty_url_is_an_argument_error() {
        assert_eq!(err(json!({ "url": "  " })).kind(), "bad_arguments");
        assert_eq!(err(json!({})).kind(), "bad_arguments");
    }
}

// endregion: Tests
