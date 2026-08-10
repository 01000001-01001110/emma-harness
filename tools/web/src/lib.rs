//! Emma's web surface: `WebFetch` and `WebSearch`.
//!
//! **Nothing in this crate is reachable from Emma.** `tools/web` is a
//! `[workspace] members` entry that the binary does not depend on: there is no
//! reference to `emma_tools_web`, `WebFetch` or `WebSearch` anywhere under
//! `crates/`, `crates/emma/Cargo.toml` does not list this crate, and
//! `main.rs` registers the filesystem tools, the task tools and `Skill` and
//! stops. The code below compiles and its tests pass; the model has never
//! called either tool, because neither is in any registry. Read every
//! "registers", "the model", and "the approval gate" in this crate as
//! describing what *would* happen once a caller exists.
//!
//! That is a deferral, not an oversight. The gate a tool must pass to run
//! unattended keys entirely on [`emma_tool_api::ToolMeta::read_only`], and the
//! last paragraph of this comment is the reason that single boolean cannot yet
//! answer for a tool that talks to the outside. Wiring these two through a
//! one-bit gate would decide that open question by accident, so the fork
//! landed and the decision it waits on did not.
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
//! **Neither tool would be registered unless it can work.** tustle-agent
//! shipped a `web_search` that registered with no API key and failed on every
//! call; a turn that touched it died, and the model had no way to learn that
//! the tool was decoration. So [`web_tools`] resolves the browser and the key
//! *first* and returns only what is usable, plus the reasons for anything it
//! left out so the omission can be reported rather than being silent. That
//! function is the intended entry point and currently has no caller — the
//! discipline is built and waiting, not exercised.
//!
//! **`read_only` cannot answer for these two, and that is why they are not
//! wired.** Both tools reach the network — one of them by driving a browser —
//! and both declare `read_only: true`. That declaration is honest about what
//! the gate actually asks, which is "can this change local state": nothing is
//! written inside the working directory, nothing is submitted, no form is
//! filled. But egress is a different risk — it is how a prompt-injected page
//! turns a read tool into an exfiltration channel, and every local-damage
//! check still passes while it happens. One boolean cannot express both "may
//! not write" and "may not talk to the outside". Flipping these to
//! `read_only: false` was considered and refused: a gate that fires on every
//! page read trains the operator to click through it, which costs the gate on
//! `Write` too. The proposed answer is a second axis on `ToolMeta` — most
//! usefully a human-granted per-domain grant rather than a per-call prompt —
//! and it is proposed, not decided. See `ToolMeta::read_only`'s own doc, which
//! records the same open question from the other side.

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
/// No caller: nothing in `crates/` invokes this. It is the seam a future
/// registration would go through, and the detection it performs is why that
/// registration can be safe when it happens.
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
        // this test loses the only place that is caught. It matters more here
        // than elsewhere precisely because these two are not wired up yet —
        // there is no live turn that would notice the omission first.
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
