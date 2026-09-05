//! Emma's web surface: `WebFetch` and the browser session tools.
//!
//! All of them are registered by `crates/emma/src/main.rs` through
//! [`web_tools`], which returns only the ones that can actually work on this
//! machine.
//!
//! **`WebSearch` is a results page rendered in that same Chrome.** It left
//! on 2026-09-05 as a tool over a search vendor's JSON API, because that needed
//! the vendor's key in `~/.emma/credentials.json` and the owner ruled no such
//! key comes back. It returned the same day as a browser tool: open the
//! engine's results page, take the links, throw the browser away. No key, no
//! account, and it works on a local model exactly as on a paid one. See
//! `search.rs` for what an engine's challenge page costs and how the tool
//! reports it.
//!
//! The names are Claude Code's, exactly, so a hook matcher or an allow-list
//! written for one works for the other.
//!
//! **`WebFetch` renders one page in real Chrome and returns what it says.** It
//! is the reading half of what used to be a pair; the searching half, which
//! turned a question into URLs, is the provider's job now, and a model that
//! answers from a search engine's snippets is still quoting a summary of a page
//! it never opened, whoever ran the search.
//!
//! **No tool is registered unless it can work.** tustle-agent shipped a
//! `web_search` that registered with no API key and failed on every call; a
//! turn that touched it died, and the model had no way to learn that the tool
//! was decoration. So [`web_tools`] resolves the browser *first* and returns
//! only what is usable, plus the reasons for anything it left out, which
//! `main.rs` prints so the omission is reported rather than silent.
//!
//! **`read_only` cannot answer for `WebFetch` on its own.** It reaches the
//! network by driving a browser, and it declares `read_only: true`.
//! That declaration is honest about what the bit asks, which is "can this
//! change local state": nothing is written inside the working directory,
//! nothing is submitted, no form is filled. But egress is a different risk —
//! it is how a prompt-injected page turns a read tool into an exfiltration
//! channel, and every local-damage check still passes while it happens. One
//! boolean cannot express both "may not write" and "may not talk to the
//! outside". Flipping these to `read_only: false` was considered and refused:
//! a gate that fires on every page read trains the operator to click through
//! it, which costs the gate on `Write` too. What landed instead is a second
//! axis with a *per-host, session-scoped* grant — asked once per host, never
//! per call, and outliving nothing. See `ToolMeta::reaches_network`, which
//! records what that axis does and, more usefully, what it does not: it gates
//! where bytes go and says nothing about trusting what comes back.

use std::sync::Arc;

use emma_tool_api::Tool;

mod args;
pub mod browser;
pub mod chromehand;
pub mod digest_md;
pub mod fetch;
pub mod search;

pub use browser::{browser_tools, BrowserPool};
pub use fetch::WebFetch;
pub use search::WebSearch;

/// What the web surface turned out to be on this machine.
pub struct WebSurface {
    pub tools: Vec<Arc<dyn Tool>>,
    /// One line per tool that could not be registered, phrased for a human to
    /// act on. Empty when everything is available.
    pub skipped: Vec<String>,
    /// The live browser sessions, when the browser tools were registered.
    ///
    /// **The caller must hold this for the length of the run.** Dropping it
    /// kills every open Chrome — which is exactly right at exit and exactly
    /// wrong in the middle of a goal. It is handed back rather than kept inside
    /// the tools because there is no other way for a binary to end the sessions
    /// deliberately, and because `tools/lsp` records what happens when teardown
    /// is left to `main` to remember: "`tools/web` leaked a Chrome per session".
    pub browser: Option<Arc<browser::BrowserPool>>,
}

/// Build the surface against the real environment.
///
/// `WebFetch` and the browser tools need a Chrome that can actually be found.
/// Missing means they are left out of the returned set: an absent tool is one
/// the model can reason about, where a present and broken one is not.
///
/// Called once, at startup, by `crates/emma/src/main.rs`. The detection it
/// performs is the reason that registration is safe: everything it returns has
/// already been checked against this machine.
pub fn web_tools() -> WebSurface {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut skipped = Vec::new();

    match fetch::WebFetch::detect() {
        Ok(t) => tools.push(Arc::new(t)),
        Err(why) => skipped.push(format!("WebFetch is not available: {why}")),
    }

    // The session surface, under exactly the same rule: a browser that cannot be
    // found means five tools that describe a capability this machine does not
    // have, which costs a turn per attempt and teaches the model nothing.
    // `WebFetch::detect` has already resolved Chrome, so its verdict is reused
    // rather than asked again — two detections that could disagree is a surface
    // where `WebFetch` is present and `BrowserOpen` is not for no visible reason.
    let browser = match fetch::WebFetch::detect() {
        Ok(_) => {
            // Search rides the same verdict: it is a results page rendered in
            // the same Chrome, and a machine that can fetch can search.
            tools.push(Arc::new(search::WebSearch::new()));
            let (browser_tools, pool) = browser::browser_tools(fetch::home_allowlist());
            tools.extend(browser_tools);
            Some(pool)
        }
        Err(why) => {
            skipped.push(format!(
                "WebSearch and the browser session tools (BrowserOpen, BrowserRead, BrowserAct, BrowserFill, \
                 BrowserClose) are not available: {why}"
            ));
            None
        }
    };

    WebSurface {
        tools,
        skipped,
        browser,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fetch_tool_ships_a_real_description_and_schema() {
        // Same assertions tools/fs makes. A description and a schema are the
        // entire instruction manual a model gets for a tool, and nothing at
        // registration time checks that either says anything: a tool with a
        // stub description registers cleanly and is simply unusable. Deleting
        // this test loses the only place that is caught.
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(fetch::WebFetch::new())];
        for tool in tools {
            assert!(
                tool.description().len() > 120,
                "{} has a stub description",
                tool.name()
            );
            let schema = tool.input_schema();
            assert_eq!(schema["type"], "object", "{} schema", tool.name());
            let properties = schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{} has no properties", tool.name()));
            assert!(!properties.is_empty(), "{} has no parameters", tool.name());
            for (name, spec) in properties {
                assert!(
                    spec["description"].is_string(),
                    "{}.{name} has no description",
                    tool.name()
                );
            }
            assert!(
                schema["required"].is_array(),
                "{} declares nothing required",
                tool.name()
            );
        }
    }

    #[test]
    fn the_name_is_claude_codes() {
        // Spelling is the compatibility contract: a hook matcher or an
        // allow-list a user already wrote for Claude Code matches on the exact
        // name. Rename the tool and every such rule silently stops applying —
        // silently, because a matcher that matches nothing looks identical to
        // a tool that was never called.
        assert_eq!(fetch::WebFetch::new().name(), "WebFetch");
    }

    #[test]
    fn search_ships_a_real_description_and_schema_too() {
        let tool: Arc<dyn Tool> = Arc::new(search::WebSearch::new());
        assert!(tool.description().len() > 120, "stub description");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"]["description"].is_string());
        assert_eq!(schema["required"], serde_json::json!(["query"]));
        assert_eq!(tool.name(), "WebSearch");
    }
}
