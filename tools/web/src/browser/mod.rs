//! A browser Emma can drive, not just read from.
//!
//! `WebFetch` opens a throwaway Chrome, reads one page, and kills it. That is
//! the whole of what this crate exposed for a long time — one of roughly
//! fourteen capabilities in the vendored `chromehand`, whose `actions`, `forms`
//! and `session` modules could click, type, select, fill and persist a session,
//! and were reachable only from the `browser-miner` CLI where a human types each
//! command. This module is the rest of it, as five tools.
//!
//! **What is genuinely new, said plainly.** Emma already had network egress and
//! already ran shell commands, so "the agent can reach the internet" is not the
//! change. The change is that the agent can take **authenticated, side-effecting
//! action as the user**: a session holds cookies, so a page read here can be a
//! page only that user can see, and a click here is a click from their account.
//! That is a different category from reading a page, and it is why the surface
//! is split on the approval boundary rather than on convenience, why acting
//! additionally requires a user-owned allowlist, and why nothing here submits a
//! form. See [`tools`] for each of those.
//!
//! **And what is not solved.** A page can contain text that persuades the model
//! to click a button on that page. Nothing here prevents that; the gates bound
//! the blast radius — an allowlisted set of domains, a fresh question after a
//! cross-origin hop, no submission at all — and `lib.rs`'s standing note applies
//! unchanged: the network axis gates where bytes go and says nothing about
//! trusting what comes back.
//!
//! # The loop this is shaped for
//!
//! 1. `BrowserOpen` → a session id and the first page, selectors included.
//! 2. `BrowserAct {click, selector}` → forty tokens: where am I, was I blocked.
//! 3. `BrowserRead` → what changed, as a selector-keyed delta.
//! 4. …repeat; `BrowserClose` at the end, and the goal's end closes it anyway.
//!
//! # Selector addressing is the honest weak spot
//!
//! Every element in a read carries a stable CSS selector, and that is the only
//! way anything here is addressed. It fails on canvas-drawn applications, on
//! sites whose class names are hashed per build, and on anything that only makes
//! sense visually. `chromehand::actions::screenshot` exists and is deliberately
//! not exposed: `ToolOutcome::content` is a `String`, so until the tool-result
//! path can carry an image block, a screenshot verb writes a PNG that nothing in
//! the loop can look at.
//!
//! The other weak spot is that the pages worth automating are the ones that
//! fight back. `looks_blocked` is honest about anti-bot challenges and there is
//! no plan to work around them, so logins, checkouts and anything behind a
//! challenge will frequently just refuse.

use std::path::PathBuf;
use std::sync::Arc;

use emma_tool_api::Tool;

pub mod pool;
pub mod render;
pub mod tools;

pub use pool::{BrowserPool, Session};
pub use tools::{BrowserAct, BrowserClose, BrowserFill, BrowserOpen, BrowserRead};

/// The whole surface, wired to one pool.
///
/// **This is the only supported way to build the set**, for the reason
/// `lsp_tools` gives about its own: `ToolCtx` carries no session state, so the
/// pool lives in the tool structs and "all five share one pool" is a wiring
/// convention rather than something the type system holds. Five tools built with
/// five pools is five independent registries, four of which cannot see the
/// session the fifth opened — and it compiles.
///
/// The pool is returned as well, and the caller **must hold it**. Dropping it
/// kills every live browser, which is exactly right at the end of a run and
/// exactly wrong in the middle of one.
pub fn browser_tools(allowlist: Option<PathBuf>) -> (Vec<Arc<dyn Tool>>, Arc<BrowserPool>) {
    let pool = Arc::new(BrowserPool::new(allowlist));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(BrowserOpen::new(pool.clone())),
        Arc::new(BrowserRead::new(pool.clone())),
        Arc::new(BrowserAct::new(pool.clone())),
        Arc::new(BrowserFill::new(pool.clone())),
        Arc::new(BrowserClose::new(pool.clone())),
    ];
    (tools, pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_five_tools_sharing_one_pool() {
        let (tools, pool) = browser_tools(None);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            [
                "BrowserOpen",
                "BrowserRead",
                "BrowserAct",
                "BrowserFill",
                "BrowserClose"
            ]
        );
        // Five tools holding a clone, plus the one returned: six. This is the
        // only thing that catches "five tools, five pools", which compiles fine
        // and would mean BrowserClose could not find BrowserOpen's session.
        assert_eq!(Arc::strong_count(&pool), 6);
    }

    /// The same assertion `tools/fs` and `tools/lsp` make. A description and a
    /// schema are the entire instruction manual a model gets, and nothing at
    /// registration time checks that either says anything.
    #[test]
    fn every_tool_ships_a_real_description_and_schema() {
        let (tools, _) = browser_tools(None);
        for tool in tools {
            assert!(
                tool.description().len() > 300,
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

    /// Prompt weight is a real cost: five descriptions ride in the request
    /// prefix of every call, on every turn, forever. This is not a style rule —
    /// it is the number that would otherwise grow one paragraph at a time with
    /// nobody noticing. Raise it deliberately or write less.
    #[test]
    fn the_whole_surface_stays_inside_its_prompt_budget() {
        let (tools, _) = browser_tools(None);
        let bytes: usize = tools
            .iter()
            .map(|t| t.description().len() + t.input_schema().to_string().len())
            .sum();
        assert!(
            bytes < 12_000,
            "the browser surface costs {bytes} bytes of every request prefix"
        );
    }

    /// Hook matchers and permission rules will be written as `Browser*`. The
    /// prefix is the compatibility contract here — there is no Claude Code
    /// equivalent to match spelling with, so this is the only thing pinning it.
    #[test]
    fn every_name_carries_the_prefix_rules_will_be_written_against() {
        let (tools, _) = browser_tools(None);
        for tool in tools {
            assert!(
                tool.name().starts_with("Browser"),
                "{} breaks the Browser* prefix",
                tool.name()
            );
        }
    }
}
