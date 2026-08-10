//! Emma's web surface: `WebFetch` and `WebSearch`.
//!
//! Both are registered by `crates/emma/src/main.rs` through [`web_tools`],
//! which returns only the ones that can actually work on this machine.
//!
//! This crate sat unreachable for a while, and deliberately: the approval gate
//! keyed entirely on [`emma_tool_api::ToolMeta::read_only`], one boolean that
//! cannot express both "may not write" and "may not talk to the outside", and
//! wiring these two through it would have decided that question by accident.
//! The decision was made rather than dodged — `ToolMeta` gained a second axis,
//! [`emma_tool_api::ToolMeta::reaches_network`], and the gate consults the two
//! separately. Both tools declare it, both answer
//! [`emma_tool_api::Tool::network_target`] with the host they are about to
//! reach, and a human grants a host once per session. The last paragraph of
//! this comment is the argument that shape came out of.
//!
//! The names are Claude Code's, exactly, so a hook matcher or an allow-list
//! written for one works for the other.
//!
//! **Two different jobs, and they are not the same job.** `WebSearch` returns a
//! list of *places to look* — titles, URLs, one-line snippets. It is not an
//! answer and its description says so. `WebFetch` renders one page in real
//! Chrome and returns what it says. A model that answers from search snippets
//! is quoting a search engine's summary of a page it never opened.
//!
//! **Neither tool is registered unless it can work.** tustle-agent shipped a
//! `web_search` that registered with no API key and failed on every call; a
//! turn that touched it died, and the model had no way to learn that the tool
//! was decoration. So [`web_tools`] resolves the browser and the key *first*
//! and returns only what is usable, plus the reasons for anything it left out,
//! which `main.rs` prints so the omission is reported rather than silent.
//!
//! **`read_only` cannot answer for these two on its own.** Both reach the
//! network — one by driving a browser — and both declare `read_only: true`.
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
pub mod chromehand;
pub mod credentials;
pub mod digest_md;
pub mod fetch;
pub mod search;

pub use fetch::WebFetch;
pub use search::WebSearch;

/// What the web surface turned out to be on this machine.
pub struct WebSurface {
    pub tools: Vec<Arc<dyn Tool>>,
    /// One line per tool that could not be registered, phrased for a human to
    /// act on. Empty when everything is available.
    pub skipped: Vec<String>,
}

/// Build the surface against the real environment.
///
/// `WebFetch` needs a Chrome that can actually be found; `WebSearch` needs a
/// Brave key. Either missing means that tool is left out of the returned set —
/// an absent tool is one the model can reason about, where a present and
/// broken one is not.
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
    match search::WebSearch::detect() {
        Ok(t) => tools.push(Arc::new(t)),
        Err(why) => skipped.push(format!("WebSearch is not available: {why}")),
    }

    WebSurface { tools, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_tools_ship_a_real_description_and_schema() {
        // Same assertions tools/fs makes. A description and a schema are the
        // entire instruction manual a model gets for a tool, and nothing at
        // registration time checks that either says anything: a tool with a
        // stub description registers cleanly and is simply unusable. Deleting
        // this test loses the only place that is caught.
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(fetch::WebFetch::new()),
            Arc::new(search::WebSearch::with_key(emma_llm::ApiKey::new("k"))),
        ];
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
    fn the_names_are_claude_codes() {
        // Spelling is the compatibility contract: a hook matcher or an
        // allow-list a user already wrote for Claude Code matches on the exact
        // name. Rename either tool and every such rule silently stops
        // applying — silently, because a matcher that matches nothing looks
        // identical to a tool that was never called.
        assert_eq!(fetch::WebFetch::new().name(), "WebFetch");
        assert_eq!(
            search::WebSearch::with_key(emma_llm::ApiKey::new("k")).name(),
            "WebSearch"
        );
    }
}
