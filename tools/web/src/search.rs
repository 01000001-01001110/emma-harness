//! `WebSearch` — a search results page, rendered in real Chrome, reduced to
//! the places it points at.
//!
//! Registered when a browser can be found, under exactly the condition
//! `WebFetch` registers — see [`crate::web_tools`] — and gated per host by
//! `emma::approval`, which asks this tool where it is going rather than reading
//! its arguments.
//!
//! **Why a browser and not an API.** This tool existed once before, over a
//! search vendor's JSON API, and it needed that vendor's key in
//! `~/.emma/credentials.json`. The owner's ruling on 2026-09-05 was that no
//! search-vendor key comes back. What Emma already has is a real Chrome, driven
//! by the same code `WebFetch` uses, and a search engine's results page is a
//! page: open it, take the links, throw the browser away. No key, no vendor
//! account, no bill per search, and it works on a local model exactly as it
//! works on a paid one, which a provider-side search cannot.
//!
//! **What was tried before this shape was settled, on 2026-09-05, from this
//! machine, so nobody repeats it.** Four engines from a fresh headless Chrome:
//! DuckDuckGo's HTML and Lite endpoints both served a "bots use DuckDuckGo
//! too" challenge; Startpage served an "Access Temporarily Suspended" page;
//! Bing served a results page that looked right and was not — three runs
//! returned Army PDFs, vacation packages and Bing's own version page for a
//! query about ratatui. That last one is the dangerous shape: not a block, a
//! plausible fabrication. The single difference that fixed it was the
//! User-Agent. Headless Chrome announces itself as `HeadlessChrome/N`, and
//! Bing degrades that client silently; the same launch with an ordinary Chrome
//! User-Agent string got "About 59,500 results" with ratatui.rs first, with
//! `navigator.webdriver` still `true`. A persistent profile and a visible
//! window also worked and were not needed. DuckDuckGo challenged the ordinary
//! User-Agent too, so it is out.
//!
//! **One engine, and no silent fallback, on purpose.** The approval gate grants
//! a host per call from [`Tool::network_target`], before anything runs. A tool
//! that tried a second engine when the first blocked would reach a host the
//! human never saw in a prompt, which is the redirect hazard `WebFetch` refuses
//! one function over. So [`ENGINE`] is one constant, the prompt names it, and a
//! block is a failure that says what to do: open the same URL in a visible
//! session with `BrowserOpen`, where a person can pass a challenge.
//!
//! **Places to look, not an answer.** What comes back is a title, a URL and the
//! engine's snippet per result, in the engine's order. The snippet is the
//! engine's summary and the output says so every time.

use std::sync::OnceLock;

use emma_tool_api::{NetworkTarget, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::browser::pool::host_of;
use crate::chromehand::{self, DigestOptions};
use crate::fetch::map_error;

// region: The tool
// ---------------------------------------------------------------------------
// The tool
//
// Construction, the schema, and the one URL this tool ever opens. The query
// travels in the URL because that is how a results page is addressed; nothing
// secret is in it, and the gate shows the human the query verbatim before the
// browser starts.
// ---------------------------------------------------------------------------

const NAME: &str = "WebSearch";
const KEYS: &[&str] = &["query", "count"];

/// The engine, as a host and a URL template. One value, because the gate
/// approves one host per call and the prompt names this one. See the module
/// doc for what the alternatives did.
pub const ENGINE: Engine = Engine {
    host: "www.bing.com",
    template: "https://www.bing.com/search?q={q}",
};

/// A search engine, by the host the gate will be asked about and the URL a
/// query becomes.
#[derive(Debug, Clone, Copy)]
pub struct Engine {
    pub host: &'static str,
    pub template: &'static str,
}

impl Engine {
    /// The results page for `query`, with the query percent-encoded so a
    /// `&` or a `#` in what the model typed stays inside the `q` parameter
    /// rather than ending it.
    pub fn url(&self, query: &str) -> String {
        self.template.replace("{q}", &encode(query))
    }
}

/// Percent-encode a query for a URL parameter. Hand-rolled over one small set
/// rather than pulling a crate for it: everything outside unreserved ASCII is
/// encoded, and a space becomes `+`, which every engine reads as a space.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub const DEFAULT_COUNT: u64 = 10;
/// A results page lists about ten entries. Past that the model is being handed
/// a second page it did not ask for, and the honest move is to say the query
/// is too broad.
pub const MAX_COUNT: u64 = 20;

pub struct WebSearch {
    engine: Engine,
}

impl WebSearch {
    /// Construct only if a browser exists. Same check as `WebFetch`, because it
    /// is the same browser: a machine with no Chrome has no search either, and
    /// an absent tool is one the model can reason about where a present and
    /// broken one is not.
    pub fn detect() -> Result<Self, String> {
        chromiumoxide::browser::BrowserConfig::builder()
            .build()
            .map_err(|e| {
                format!("Chrome or Chromium could not be found ({e}). Install it, or set CHROME")
            })?;
        Ok(Self::new())
    }

    /// Construct without the browser check. For tests and for callers that
    /// have already decided.
    pub fn new() -> Self {
        Self { engine: ENGINE }
    }

    /// Point at another engine. For tests against a fixture server, and for
    /// the day the constant above stops answering.
    pub fn with_engine(mut self, engine: Engine) -> Self {
        self.engine = engine;
        self
    }
}

impl Default for WebSearch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/web_search.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What to search for, as a person would type it into a search box."
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("How many results to return. Default {DEFAULT_COUNT}, capped at {MAX_COUNT}.")
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // Changes nothing on this machine…
            read_only: true,
            // …and the query itself is the payload. This is the tool the
            // exfiltration argument in `ToolMeta::reaches_network` is written
            // about most directly: a search is an arbitrary string the model
            // chose, sent to a third party, and it reads as a read.
            reaches_network: true,
            idempotent: true,
        }
    }

    /// The engine's host, and the query going to it.
    ///
    /// The `detail` is the query verbatim, because the query *is* the thing
    /// leaving the machine. A prompt saying "WebSearch wants to reach
    /// www.bing.com" and not what it is asking about cannot distinguish a
    /// search for a crate name from a search for the contents of a file.
    fn network_target(&self, args_v: &Value) -> Option<NetworkTarget> {
        let query = args_v.get("query")?.as_str()?.trim();
        Some(NetworkTarget::new(
            self.engine.host,
            format!("search for: {query}"),
        ))
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        let query = args::req_str(args_v, NAME, "query")?;
        if query.trim().is_empty() {
            return Err(ToolError::BadArguments("WebSearch.query is empty".into()));
        }
        if let Some(0) = args::opt_u64(args_v, NAME, "count")? {
            return Err(ToolError::BadArguments(
                "WebSearch.count must be at least 1".into(),
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

impl WebSearch {
    async fn run(&self, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let query = args::req_str(&args_v, NAME, "query")?.trim().to_string();
        let count = args::opt_u64(&args_v, NAME, "count")?
            .unwrap_or(DEFAULT_COUNT)
            .min(MAX_COUNT) as usize;

        let url = self.engine.url(&query);
        // The allowlist is `WebFetch`'s and applies to what the model reads;
        // an engine's results page is not a page the model reads, and the
        // human approved the engine's host at the gate. Left `None` so a user
        // who allowlisted three documentation sites can still search for a
        // fourth.
        let opts = DigestOptions {
            user_agent: Some(user_agent().to_string()),
            ..DigestOptions::default()
        };
        let digest = chromehand::digest_url(&url, &opts)
            .await
            .map_err(map_error)?;

        // Same rule as `WebFetch`: the host the gate approved is the one in
        // the argument, and a results page that redirected elsewhere is not
        // read back.
        let landed = digest.get("final_url").and_then(Value::as_str);
        if let (Some(asked), Some(landed)) = (host_of(&url), landed.and_then(host_of)) {
            if asked != landed {
                return Err(ToolError::Failed(format!(
                    "refused to read search results: {asked} redirected to {landed}, and the \
                     approval for this call was for {asked}. Nothing was read back."
                )));
            }
        }

        Ok(render(&query, count, self.engine.host, &digest))
    }
}

// endregion: The tool

// region: The User-Agent, and why it is not the default
// ---------------------------------------------------------------------------
// The User-Agent, and why it is not the default
//
// Headless Chrome announces itself, and the engine treats that announcement
// as a reason to serve something other than results. See the module doc for
// what "something other" looked like. The string is built from the installed
// Chrome's own major version so it does not age into a tell of its own.
// ---------------------------------------------------------------------------

/// A User-Agent string an engine reads as an ordinary desktop Chrome.
///
/// `HeadlessChrome/N` is what `--headless=new` sends, and Bing served three
/// different sets of wrong results to it before this existed. The version
/// comes from asking the installed Chrome (`--version`), once per process, so
/// the string tracks whatever is on the machine; if that cannot be read, the
/// fallback is the major that produced real results on 2026-09-05. The platform
/// segment is the build's own.
pub fn user_agent() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        let major = installed_chrome_major().unwrap_or(FALLBACK_CHROME_MAJOR);
        format!(
            "Mozilla/5.0 ({PLATFORM}) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/{major}.0.0.0 Safari/537.36"
        )
    })
}

/// The major that produced real results when this was written. Used only when
/// the installed Chrome will not say its version.
const FALLBACK_CHROME_MAJOR: u32 = 140;

#[cfg(target_os = "windows")]
const PLATFORM: &str = "Windows NT 10.0; Win64; x64";
#[cfg(target_os = "macos")]
const PLATFORM: &str = "Macintosh; Intel Mac OS X 10_15_7";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const PLATFORM: &str = "X11; Linux x86_64";

/// `chrome --version` prints a line like `Google Chrome 140.0.7339.128`; the
/// first dotted number's first component is the major. `None` when there is no
/// Chrome, it will not run, or the line has no version in it.
fn installed_chrome_major() -> Option<u32> {
    let exe = chromiumoxide::detection::default_executable(Default::default()).ok()?;
    let out = std::process::Command::new(exe)
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    major_in(&text)
}

fn major_in(text: &str) -> Option<u32> {
    text.split_whitespace()
        .find(|w| w.contains('.') && w.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .and_then(|v| v.split('.').next())
        .and_then(|m| m.parse().ok())
}

// endregion: The User-Agent, and why it is not the default

// region: A digest into places to look
// ---------------------------------------------------------------------------
// A digest into places to look
//
// The reduction, and the two sentences it always carries: that zero results
// is an answer, and that these are places to look rather than the answer
// itself. Both are in the output rather than only the description, because
// the description is read once and the output is read every time.
// ---------------------------------------------------------------------------

/// One result: where it goes, what the page calls itself, and the engine's
/// one-line summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub url: String,
    pub title: String,
    pub snippet: String,
}

/// The results out of a results-page digest, in page order, deduplicated by
/// destination, capped at `count`.
///
/// Bing does not link to results directly. Every result anchor goes to
/// `www.bing.com/ck/a?…&u=a1<base64url of the real URL>`, a click-tracking
/// redirect, so "a link that leaves the engine's host" finds nothing and the
/// redirect has to be decoded. The decoded URL is what the model is shown and
/// what `WebFetch` will be given, never the tracking one.
///
/// Titles and snippets are not on the anchors chromehand collected; those
/// carry the cite line (`reddit.com https://www.reddit.com › r › …`). They are
/// in the page's text, where each result reads as four lines: host, the cite
/// URL with `›` separators, the title, then the snippet. So for each decoded
/// URL the text is searched for the cite line whose host and first path
/// segment match, and the two lines after it are the title and snippet. When
/// that fails the anchor text stands in for the title and the snippet is
/// empty; a result with a URL and no title is still a place to look.
pub fn hits(digest: &Value, engine_host: &str, count: usize) -> Vec<Hit> {
    let engine = engine_host.to_ascii_lowercase();
    let links = digest
        .pointer("/digest/interactive/links")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let text = digest
        .pointer("/digest/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    let lines: Vec<&str> = text.lines().map(str::trim).collect();

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for link in links {
        let href = link
            .get("href")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let anchor = link
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let Some(target) = destination(href, &engine) else {
            continue;
        };
        if !seen.insert(target.clone()) {
            continue;
        }
        let (title, snippet) = title_and_snippet(&lines, &target)
            .unwrap_or_else(|| (anchor.chars().take(200).collect(), String::new()));
        if title.is_empty() {
            continue;
        }
        out.push(Hit {
            url: target,
            title,
            snippet,
        });
        if out.len() >= count {
            break;
        }
    }
    out
}

/// Where a result anchor actually goes, or `None` for the engine's own
/// navigation. A `ck/a` redirect is decoded; any other link on the engine's
/// host is chrome; a link off the engine's host is taken as it is.
fn destination(href: &str, engine: &str) -> Option<String> {
    if !href.starts_with("http") {
        return None;
    }
    let host = host_of(href)?.to_ascii_lowercase();
    if host == engine || host.ends_with(&format!(".{}", apex(engine))) {
        return decode_click_redirect(href);
    }
    Some(href.to_string())
}

/// `https://www.bing.com/ck/a?…&u=a1aHR0cHM6Ly9…&…` to the URL inside `u`.
///
/// The value is `a1` followed by the destination in base64url without
/// padding. Anything that does not fit that shape is not a result and yields
/// `None`, which is how the engine's other links on its own host are left out.
fn decode_click_redirect(href: &str) -> Option<String> {
    let query = href.split_once('?')?.1;
    let u = query.split('&').find_map(|kv| kv.strip_prefix("u="))?;
    let encoded = u.strip_prefix("a1")?;
    let bytes = base64url_decode(encoded)?;
    let url = String::from_utf8(bytes).ok()?;
    if url.starts_with("http") {
        Some(url)
    } else {
        None
    }
}

/// base64url, unpadded, hand-rolled: one table, no dependency, and a `None`
/// on any byte outside the alphabet rather than a guess.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        })
    }
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &c in s.as_bytes() {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// The title and snippet for `target`, read out of the page text by the cite
/// line the engine prints above each result.
///
/// A cite line is the destination's host and path with `/` shown as ` › `,
/// so it is matched on the host and, when there is one, the first path
/// segment. The line after it is the title; the next non-empty line after
/// that is the snippet. Both are capped, because a hostile page controls them.
fn title_and_snippet(lines: &[&str], target: &str) -> Option<(String, String)> {
    let host = host_of(target)?.to_ascii_lowercase();
    let first_segment = target
        .splitn(4, '/')
        .nth(3)
        .and_then(|p| p.split('/').next())
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase);
    let idx = lines.iter().position(|l| {
        let low = l.to_ascii_lowercase();
        let is_cite = low.starts_with("http") && low.contains(&host);
        // A target with a path matches the cite line carrying its first
        // segment; a target that is just a host matches the cite line that is
        // just that host. Without the second rule a sitelink to `ratatui.rs/`
        // under the first result took the first result's title, which the
        // live run on 2026-09-05 showed as two results with one title.
        is_cite
            && match &first_segment {
                Some(seg) => low.contains(&format!("› {seg}")),
                None => low.trim_end_matches('/').ends_with(&host),
            }
    })?;
    let title = lines.get(idx + 1)?.trim();
    if title.is_empty() || title.starts_with("http") {
        return None;
    }
    let snippet = lines[idx + 2..]
        .iter()
        .find(|l| !l.is_empty())
        .copied()
        .unwrap_or("");
    Some((
        title.chars().take(200).collect(),
        snippet.chars().take(300).collect(),
    ))
}

/// `www.bing.com` to `bing.com`: the last two labels, which is right for the
/// host this tool names and wrong for `co.uk`, which it does not.
fn apex(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() >= 2 {
        labels[labels.len() - 2..].join(".")
    } else {
        host.to_string()
    }
}

/// Results as markdown, or the reason there are none.
///
/// Zero results is a *result*: the query found nothing, which is a fact about
/// the web and not a failure of the call. A challenge page is the one case that
/// is neither: the engine refused to answer a client it took for a bot, and
/// that is a failure the model must be told about in those words, because the
/// links on a challenge page are not results and returning them as if they
/// were is the fabrication this project refuses everywhere.
fn render(query: &str, count: usize, engine_host: &str, digest: &Value) -> ToolOutcome {
    let url = digest.get("url").and_then(Value::as_str).unwrap_or("");
    let blocked = digest
        .get("looks_blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if blocked {
        let title = digest
            .get("page_title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let note = format!(
            "# Search: {query}\n\nNo results: {engine_host} answered with a challenge page rather \
             than results (\"{title}\"). It took this browser for a bot. This is the engine's \
             decision and not a fault in the query. The way through is BrowserOpen on this same \
             URL with headful: true, then BrowserRead: a visible window is one a person can pass \
             the challenge in, and the results page reads the same once it loads. URL: {url}\n"
        );
        return ToolOutcome::new(note).with_display(format!("{query} — blocked by {engine_host}"));
    }

    let found = hits(digest, engine_host, count);
    let mut out = format!("# Search: {query}\n\n");
    if found.is_empty() {
        out.push_str(
            "No results. The query found nothing — this is an answer, not a failure. Try \
             different words, or fetch a page directly if you already know where to look.\n",
        );
        return ToolOutcome::new(out).with_display(format!("{query} — no results"));
    }
    out.push_str(
        "These are places to look, not an answer. The snippets are the search engine's \
         summaries; use WebFetch on a URL before relying on what it says.\n\n",
    );
    for (i, hit) in found.iter().enumerate() {
        out.push_str(&format!("{}. **{}**\n   {}\n", i + 1, hit.title, hit.url));
        if !hit.snippet.is_empty() {
            out.push_str(&format!("   {}\n", hit.snippet));
        }
        out.push('\n');
    }
    ToolOutcome::new(out).with_display(format!(
        "{query} — {} result{}",
        found.len(),
        if found.len() == 1 { "" } else { "s" }
    ))
}

// endregion: A digest into places to look

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Everything decidable without a browser: the URL a query becomes, the
// redirect decoding, the reduction of a digest to hits, and the three
// renderings. The fixture is the shape Bing served on 2026-09-05, cut down.
// The one case that needs Chrome and the live network is `tests/search.rs`,
// `#[ignore]`d, and it is the certification.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `https://ratatui.rs/concepts/backends/alternate-screen/` as Bing
    /// encodes it: `a1` then base64url, no padding.
    const RATATUI_U: &str =
        "a1aHR0cHM6Ly9yYXRhdHVpLnJzL2NvbmNlcHRzL2JhY2tlbmRzL2FsdGVybmF0ZS1zY3JlZW4v";
    const DOCS_RS_U: &str =
        "a1aHR0cHM6Ly9kb2NzLnJzL3JhdGF0dWkvbGF0ZXN0L3JhdGF0dWkvc3RydWN0LlRlcm1pbmFsLmh0bWw";

    fn bing_link(cite: &str, u: &str) -> Value {
        json!({
            "text": cite,
            "href": format!("https://www.bing.com/ck/a?!&&p=deadbeef&u={u}&ntb=1"),
            "in_content": true
        })
    }

    fn bing_digest(links: Vec<Value>, blocked: bool) -> Value {
        // The page text as Bing served it on 2026-09-05, cut to two results:
        // host, cite line, title, blank, snippet, blank.
        let text = [
            "About 59,500 results",
            "ratatui.rs",
            "https://ratatui.rs › concepts › backends › alternate-screen",
            "Alternate Screen - Ratatui",
            "",
            "Try running this code on your own and experiment with EnterAlternateScreen.",
            "",
            "docs.rs",
            "https://docs.rs › ratatui › latest › ratatui › struct.Terminal.html",
            "Terminal in ratatui - Rust - Docs.rs",
            "",
            "Ratatui wraps the current hook so it can restore terminal state first.",
        ]
        .join(
            "
",
        );
        json!({
            "url": "https://www.bing.com/search?q=x",
            "final_url": "https://www.bing.com/search?q=x&rdr=1",
            "page_title": if blocked { "Just a moment" } else { "x - Search" },
            "looks_blocked": blocked,
            "digest": {
                "interactive": { "links": links },
                "text": text
            }
        })
    }

    #[test]
    fn a_query_is_percent_encoded_into_the_engine_url() {
        // `&` and `#` inside the query must not end the parameter, and a space
        // is the one character every engine reads as `+`.
        assert_eq!(
            ENGINE.url("rust &mut self #1"),
            "https://www.bing.com/search?q=rust+%26mut+self+%231"
        );
    }

    #[test]
    fn a_click_redirect_decodes_to_the_page_it_tracks() {
        let href = format!("https://www.bing.com/ck/a?!&&p=abc&u={RATATUI_U}&ntb=1");
        assert_eq!(
            decode_click_redirect(&href).as_deref(),
            Some("https://ratatui.rs/concepts/backends/alternate-screen/")
        );
        // Not a redirect: the engine's own search page.
        assert_eq!(
            decode_click_redirect("https://www.bing.com/search?q=x"),
            None
        );
        // A `u` that is not `a1`+base64url is refused rather than guessed at.
        assert_eq!(
            decode_click_redirect("https://www.bing.com/ck/a?u=zz%%"),
            None
        );
    }

    /// **If this breaks:** the engine's own navigation is being returned as
    /// results, a tracking URL is being handed to the model instead of the page
    /// it tracks, or the title is not being read off the page text.
    #[test]
    fn hits_decode_the_redirects_and_read_titles_from_the_text() {
        let d = bing_digest(
            vec![
                bing_link(
                    "ratatui.rs https://ratatui.rs › concepts › backends",
                    RATATUI_U,
                ),
                bing_link(
                    "ratatui.rs https://ratatui.rs › concepts › backends",
                    RATATUI_U,
                ),
                bing_link("docs.rs https://docs.rs › ratatui › latest", DOCS_RS_U),
                json!({ "text": "Page 2", "href": "https://www.bing.com/search?q=x&first=11" }),
                json!({ "text": "IMAGES", "href": "/images/search?q=x" }),
            ],
            false,
        );
        let got = hits(&d, "www.bing.com", 10);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(
            got[0].url,
            "https://ratatui.rs/concepts/backends/alternate-screen/"
        );
        assert_eq!(got[0].title, "Alternate Screen - Ratatui");
        assert!(
            got[0].snippet.starts_with("Try running this code"),
            "{}",
            got[0].snippet
        );
        assert_eq!(
            got[1].url,
            "https://docs.rs/ratatui/latest/ratatui/struct.Terminal.html"
        );
        assert_eq!(got[1].title, "Terminal in ratatui - Rust - Docs.rs");
    }

    #[test]
    fn a_result_whose_title_is_not_in_the_text_keeps_its_anchor_text() {
        // A destination the text never mentions still gets listed: a URL with
        // the anchor for a title is a place to look, and dropping it would be
        // the silent kind of loss.
        let other = "a1aHR0cHM6Ly9leGFtcGxlLm9yZy9wYWdl"; // https://example.org/page
        let d = bing_digest(vec![bing_link("example.org", other)], false);
        let got = hits(&d, "www.bing.com", 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].url, "https://example.org/page");
        assert_eq!(got[0].title, "example.org");
        assert_eq!(got[0].snippet, "");
    }

    #[test]
    fn count_caps_the_hits() {
        let links = (0..30)
            .map(
                |i| json!({ "text": format!("r{i}"), "href": format!("https://site{i}.example/") }),
            )
            .collect();
        assert_eq!(hits(&bing_digest(links, false), "www.bing.com", 3).len(), 3);
    }

    /// **If this breaks:** a challenge page's links are being handed to the
    /// model as search results, which is a fabricated answer wearing the
    /// shape of a real one.
    #[test]
    fn a_challenge_page_is_reported_as_a_block_and_not_as_results() {
        let d = bing_digest(vec![bing_link("help", RATATUI_U)], true);
        let out = render("x", 10, "www.bing.com", &d);
        assert!(out.content.contains("challenge page"), "{}", out.content);
        assert!(out.content.contains("BrowserOpen"), "{}", out.content);
        assert!(!out.content.contains("ratatui.rs"), "{}", out.content);
        let display = out.display.as_deref().unwrap_or("");
        assert!(display.contains("blocked"), "{display}");
    }

    #[test]
    fn no_hits_is_an_answer_and_says_so() {
        let out = render("x", 10, "www.bing.com", &bing_digest(vec![], false));
        assert!(
            out.content.contains("this is an answer, not a failure"),
            "{}",
            out.content
        );
        let display = out.display.as_deref().unwrap_or("");
        assert!(display.ends_with("no results"), "{display}");
    }

    #[test]
    fn results_render_as_numbered_places_to_look() {
        let d = bing_digest(
            vec![
                bing_link("ratatui.rs", RATATUI_U),
                bing_link("docs.rs", DOCS_RS_U),
            ],
            false,
        );
        let out = render("x", 10, "www.bing.com", &d);
        assert!(
            out.content.contains("places to look, not an answer"),
            "{}",
            out.content
        );
        assert!(
            out.content.contains(
                "1. **Alternate Screen - Ratatui**\n   https://ratatui.rs/concepts/backends/alternate-screen/\n   Try running"
            ),
            "{}",
            out.content
        );
        assert!(
            out.content.contains("2. **Terminal in ratatui"),
            "{}",
            out.content
        );
        assert_eq!(out.display.as_deref(), Some("x — 2 results"));
    }

    #[test]
    fn the_gate_is_told_the_engine_and_the_query() {
        let t = WebSearch::new()
            .network_target(&json!({ "query": "  contents of .env " }))
            .expect("a query names a target");
        assert_eq!(t.host, "www.bing.com");
        assert_eq!(t.detail, "search for: contents of .env");
    }

    #[test]
    fn bad_arguments_are_named() {
        let t = WebSearch::new();
        assert!(t.validate_args(&json!({ "query": "   " })).is_err());
        assert!(t
            .validate_args(&json!({ "query": "x", "count": 0 }))
            .is_err());
        assert!(t
            .validate_args(&json!({ "query": "x", "extra": 1 }))
            .is_err());
        assert!(t
            .validate_args(&json!({ "query": "x", "count": 5 }))
            .is_ok());
    }

    #[test]
    fn the_user_agent_does_not_say_headless_and_carries_a_major() {
        let ua = user_agent();
        assert!(!ua.contains("Headless"), "{ua}");
        assert!(ua.contains("Chrome/"), "{ua}");
        assert_eq!(major_in("Google Chrome 140.0.7339.128"), Some(140));
        assert_eq!(major_in("Chromium 139.0.1 snap"), Some(139));
        assert_eq!(major_in("no version here"), None);
    }
}

// endregion: Tests
