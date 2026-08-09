//! Emma's web surface: `WebFetch` and `WebSearch`.
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
//! and returns only what is usable, plus the reasons for anything it left out
//! so the omission can be reported rather than being silent.
//!
//! **`read_only` is a lie of omission here, and it is worth naming.**
//! `ToolMeta::read_only` is documented as "cannot modify the filesystem, spawn
//! a process, or reach the network", and both tools reach the network — one of
//! them by driving a browser. They are declared `read_only: true` because the
//! approval gate's real question is "can this change something", and neither
//! can: nothing is written inside the working directory, nothing is submitted,
//! no form is filled. But egress is its own risk — it is how a prompt-injected
//! page exfiltrates, and how a private repository's contents leave the
//! machine — and one boolean cannot express both "may not write" and "may not
//! talk to the outside". The honest fix is a second axis on `ToolMeta`, not a
//! second meaning for this one.

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
/// Brave key. Either missing means that tool is absent from the registry —
/// which the model can reason about — rather than present and broken, which it
/// cannot.
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
        // Same assertions tools/fs makes: these bytes are what the model sees
        // and what the registry's schema digest covers, so a tool that forgot
        // its description would register fine and simply be unusable.
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
        // Spelling is the compatibility contract.
        assert_eq!(fetch::WebFetch::new().name(), "WebFetch");
        assert_eq!(
            search::WebSearch::with_key(emma_llm::ApiKey::new("k")).name(),
            "WebSearch"
        );
    }
}
