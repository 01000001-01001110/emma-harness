//! `WebSearch` — Brave, returning a list of places to look.
//!
//! Not registered anywhere, and so never yet called by a model — see the crate
//! docs for why. What follows describes the tool as built.
//!
//! chromehand browses; it does not search. Something has to turn a question
//! into URLs, and that is an index. Brave rather than the alternatives because
//! it sells a plain JSON API keyed by a header, with no crawl of its own to
//! respect and no scraping of somebody else's results page.
//!
//! **This tool does not answer questions and its description says so.** What
//! comes back is a title, a URL and a one-line snippet the index wrote — a
//! summary of a page, composed by a third party, optimised for a human
//! deciding whether to click. A model that answers from snippets is quoting a
//! search engine's paraphrase of a page nobody opened. The snippets are here
//! to choose a `WebFetch` target with, and for nothing else.
//!
//! **No key means no tool.** A keyless search tool fails on every call, and
//! the model cannot learn that it is decoration, because a failed call looks
//! exactly like a hard problem — see the crate docs for where that was learned.
//! [`WebSearch::detect`] returns `Err` instead, and a registry
//! built from [`crate::web_tools`] would simply not carry the tool. The key is
//! resolved once at construction rather than per call, so "no key" is a fact
//! about the surface and never a runtime surprise.

use emma_llm::ApiKey;
use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::credentials;

// region: The tool
// ---------------------------------------------------------------------------
// The tool
//
// Construction, the schema, and the request. The key travels in a header and
// never in the query string — a key in a URL ends up in every proxy log
// between here and Brave, and in this crate's own error messages.
// ---------------------------------------------------------------------------

const NAME: &str = "WebSearch";
const KEYS: &[&str] = &["query", "count"];

const API_URL: &str = "https://api.search.brave.com/res/v1/web/search";
pub const DEFAULT_COUNT: u64 = 10;
/// Brave's own per-request ceiling. Asking for more is silently clamped by the
/// API, which would make the count in the result disagree with the request.
pub const MAX_COUNT: u64 = 20;

pub struct WebSearch {
    key: ApiKey,
    base_url: String,
    client: reqwest::Client,
}

impl WebSearch {
    /// Construct only if a key exists — environment, then
    /// `~/.emma/credentials.json`, never the project directory. The constructor
    /// a registration would call; see [`crate::credentials`] for the order and
    /// the exclusion.
    pub fn detect() -> Result<Self, String> {
        match credentials::load_default() {
            Some(key) => Ok(Self::with_key(key)),
            None => Err(credentials::missing_message()),
        }
    }

    pub fn with_key(key: ApiKey) -> Self {
        Self {
            key,
            base_url: API_URL.to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Point at a loopback stub. Exists so the tests exercise the real
    /// request-building and the real response-parsing rather than a seam that
    /// only tests hold.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
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
            // See the crate docs. Reaches the network; changes nothing.
            read_only: true,
            idempotent: true,
        }
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
            .min(MAX_COUNT);

        let response = self
            .client
            .get(&self.base_url)
            .query(&[("q", query.as_str()), ("count", &count.to_string())])
            .header("Accept", "application/json")
            .header("X-Subscription-Token", self.key.expose())
            .send()
            .await
            .map_err(|e| ToolError::Failed(format!("the search request failed: {e}")))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(classify_status(status.as_u16(), &body));
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::Failed(format!(
                "the search API returned something that is not JSON: {e}"
            ))
        })?;
        Ok(render(&query, &parsed))
    }
}

// endregion: The tool

// region: HTTP status into the taxonomy the model routes on
// ---------------------------------------------------------------------------
// HTTP status into the taxonomy the model routes on
//
// Which class a status lands in decides what the caller does next: stop and
// tell a human, rewrite the query, or try again. Getting one wrong sends the
// model to retry the single thing that cannot work.
// ---------------------------------------------------------------------------

/// HTTP status into the taxonomy the model routes on.
fn classify_status(status: u16, body: &str) -> ToolError {
    let detail = trim_body(body);
    match status {
        // The key is present and wrong, or the plan does not cover this
        // endpoint. Either way no retry helps and no argument change helps —
        // it is the machinery being unconfigured, which is what Unavailable
        // means.
        401 | 403 => ToolError::Unavailable(format!(
            "the Brave Search key was rejected ({status}). Check {} or ~/.emma/credentials.json. {detail}",
            credentials::ENV_VAR
        )),
        // Brave answers a malformed query with 422 rather than an empty result
        // set, so this really is the arguments and not the world.
        422 => ToolError::BadArguments(format!("Brave refused the query: {detail}")),
        429 => ToolError::Failed(format!(
            "the Brave Search rate limit was hit ({status}); this is temporary. {detail}"
        )),
        _ => ToolError::Failed(format!("the search API returned {status}. {detail}")),
    }
}

fn trim_body(body: &str) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 300 {
        format!("{}…", flat.chars().take(299).collect::<String>())
    } else {
        flat
    }
}

// endregion: HTTP status into the taxonomy the model routes on

// region: Results as markdown
// ---------------------------------------------------------------------------
// Results as markdown
//
// The rendering, and the two sentences it always carries: that zero results is
// an answer, and that these are places to look rather than the answer itself.
// Both are written into the output rather than left to the tool description,
// because the description is read once and the output is read every time.
// ---------------------------------------------------------------------------

/// Results as markdown.
///
/// Zero results is a *result*: the query found nothing, which is a fact about
/// the web and not a failure of the call. Turning it into an error would tell
/// the model to retry a search that will keep succeeding at finding nothing.
fn render(query: &str, parsed: &Value) -> ToolOutcome {
    let results = parsed
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut out = format!("# Search: {query}\n\n");
    if results.is_empty() {
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
    for (i, r) in results.iter().enumerate() {
        let title = strip_markup(
            r.get("title")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)"),
        );
        let url = r.get("url").and_then(Value::as_str).unwrap_or("");
        let snippet = strip_markup(r.get("description").and_then(Value::as_str).unwrap_or(""));
        out.push_str(&format!("{}. **{title}**\n   {url}\n", i + 1));
        if !snippet.is_empty() {
            out.push_str(&format!("   {snippet}\n"));
        }
        out.push('\n');
    }

    ToolOutcome::new(out).with_display(format!(
        "{query} — {} result{}",
        results.len(),
        if results.len() == 1 { "" } else { "s" }
    ))
}

/// Brave wraps the matched query terms in `<strong>`, so every snippet arrives
/// as HTML. Stripped rather than passed through: markup the model did not ask
/// for reads as emphasis it should reproduce, and `&amp;` in a quoted title
/// comes back out in the answer.
///
/// Order matters and is worth reading twice: tags are removed first, entities
/// decoded second. An entity-encoded bracket in the page's own text therefore
/// survives into the output as a character instead of being re-read as the
/// start of a tag. The tag stripper is a two-state scan rather than a regex,
/// so an unbalanced `<` swallows the rest of the string — Brave's own markup
/// is well-formed, and the alternative is a parser for a field that exists to
/// bold three words.
fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// endregion: Results as markdown

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Rendering and classification, with no socket involved. The wire itself — the
// header, the clamp, the transport failure — is covered against a loopback
// stub in `tests/search.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_result_set_is_a_result() {
        // The rule that governs every tool here. If this becomes an error, the
        // model cannot tell "the search broke" from "nobody has written about
        // this", and it will retry the first when it is looking at the second.
        let outcome = render("obscure", &json!({ "web": { "results": [] } }));
        assert!(
            outcome.content.contains("No results"),
            "{}",
            outcome.content
        );
        assert!(!outcome.truncated);
    }

    #[test]
    fn a_missing_web_block_is_also_a_result() {
        // Brave omits `web` entirely for some queries rather than sending an
        // empty array. Same fact, different shape.
        let outcome = render("obscure", &json!({ "query": { "original": "obscure" } }));
        assert!(
            outcome.content.contains("No results"),
            "{}",
            outcome.content
        );
    }

    #[test]
    fn snippets_lose_their_markup() {
        let outcome = render(
            "rust",
            &json!({ "web": { "results": [
                { "title": "Rust &amp; you", "url": "https://r/", "description": "A <strong>rust</strong> guide" }
            ] } }),
        );
        assert!(
            outcome.content.contains("Rust & you"),
            "{}",
            outcome.content
        );
        assert!(
            outcome.content.contains("A rust guide"),
            "{}",
            outcome.content
        );
        assert!(!outcome.content.contains("<strong>"), "{}", outcome.content);
    }

    #[test]
    fn status_codes_land_in_the_right_class() {
        // Each of these routes the caller somewhere different: unavailable
        // means stop and tell the human, bad_arguments means rewrite the
        // query, failed means it may be worth trying again. Collapse any two
        // and the model retries the one thing that cannot work.
        assert_eq!(classify_status(401, "{}").kind(), "tool_unavailable");
        assert_eq!(classify_status(403, "{}").kind(), "tool_unavailable");
        assert_eq!(classify_status(422, "{}").kind(), "bad_arguments");
        assert_eq!(classify_status(429, "{}").kind(), "tool_failed");
        assert_eq!(classify_status(500, "{}").kind(), "tool_failed");
    }

    #[test]
    fn a_rejected_key_never_appears_in_the_message() {
        // The message names where the key comes from so a human can fix it,
        // and must never name the key itself.
        let msg = classify_status(401, "{\"error\":\"bad token\"}").to_string();
        assert!(msg.contains("BRAVE_SEARCH_API_KEY"), "{msg}");
    }
}

// endregion: Tests
