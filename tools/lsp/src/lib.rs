//! Emma's code-intelligence tool surface: `FindReferences`, `GoToDefinition`,
//! `Hover`, `DocumentSymbols` — a language server's answers instead of text
//! search.
//!
//! `Grep` finds a name. It cannot tell a call from a comment, does not know that
//! `Config` here is the type declared over there, and cannot answer "what breaks
//! if I change this signature". rust-analyzer can, because it is the compiler's
//! own understanding of the project. This crate is the wire between them.
//!
//! # The four rules that shape everything here
//!
//! **An empty answer is only an answer when the index was complete.** This is
//! the rule the crate is built around and the one that would make it worse than
//! useless if it were dropped. rust-analyzer answers `references` the moment it
//! starts and returns `[]` for the first ten to sixty seconds; "no references"
//! and "not indexed yet" are the same shape, and a model told the first will
//! delete the function. So readiness is tracked explicitly through the server's
//! own progress notifications ([`client`]), it travels with every result, and
//! [`render::no_results_line`] chooses between two different sentences on it.
//! When Emma cannot tell, it says it cannot tell.
//!
//! It stays `Ok` rather than becoming `Failed`, on a deliberate ruling recorded
//! in [`client`]: Emma's loop will not repeat a call that failed with nothing
//! changed since, and "wait a moment and ask again" is exactly what should
//! happen next. An error would forbid the only correct move.
//!
//! **Never quietly answer a different question.** No server, or a file this
//! crate has no server for, is [`ToolError::Unavailable`](emma_tool_api::ToolError::Unavailable)
//! naming what was looked for — never a fall back to grep. A tool that silently
//! answers something adjacent is worse than one that refuses, because nothing in
//! the result says which question was answered. [`server`] holds the resolution
//! order, the override and both refusals.
//!
//! **Nothing escapes the root.** Every path in goes through
//! `tools/fs`'s `path::resolve_existing`, and every path *out* — a language
//! server answers about `~/.cargo/registry` and the standard library as a matter
//! of course — is filtered against the same root and reported as a count rather
//! than dropped. Emma decided containment is a boundary it enforces; a path
//! arriving over a pipe does not get an exemption.
//!
//! **Cleanup lives in `Drop`.** `tools/web` leaked a Chrome per session because
//! its teardown ran in `main` — correct for a binary, wrong for a library. The
//! server here is spawned `kill_on_drop` *and* signalled from `Client::drop`,
//! and `tests/lifecycle.rs` checks it against a real process rather than
//! trusting the flag.
//!
//! # What was cut, and why
//!
//! **`Diagnostics` — cut.** It was the third most valuable tool on the list and
//! it is not here, because it is not the same mechanism. Diagnostics are not a
//! request/response: the server *pushes* `textDocument/publishDiagnostics` when
//! it feels like it, so "get the diagnostics for this file" means waiting an
//! unknowable time for a notification that may never come — the readiness
//! problem again, but without the progress notifications that make readiness
//! solvable. Worse, the diagnostics worth having are `cargo check`'s, and
//! rust-analyzer only produces those by *running cargo check*, which this crate
//! deliberately disables (see [`client::INIT_OPTIONS`]) because it is what makes
//! `read_only: true` a fact. With it off, the tool would return
//! syntax-and-name-resolution errors only and silently omit every type error —
//! a confidently incomplete answer, which is the exact failure everything else
//! here is arranged to prevent. `Bash` running `cargo check` is the honest tool
//! for this, and it already exists.
//!
//! **`WorkspaceSymbols` — cut.** Cheap to add and genuinely useful, and left out
//! for one reason: it is the query most sensitive to a partial index, and it has
//! no per-file anchor that would let a caller sanity-check the answer. Four
//! tools that are right beat five where one is subtly thin. It is the obvious
//! next thing to add.
//!
//! # The layers
//!
//! [`proto`] is the transport and nothing else. [`server`] decides which binary.
//! [`client`] owns one running server: handshake, readiness, request
//! correlation, death. [`pool`] owns the clients and the crash accounting.
//! [`doc`] converts paths to URIs and "the symbol on line 42" to a position.
//! [`render`] is everything the model reads. [`tools`] is four thin shells over
//! all of it.

use std::sync::Arc;

use emma_tool_api::Tool;

mod args;
pub mod client;
pub mod doc;
pub mod pool;
pub mod proto;
pub mod render;
pub mod server;
pub mod tools;

pub use client::{Client, Readiness};
pub use pool::Pool;
pub use server::Server;
pub use tools::{DocumentSymbols, FindReferences, GoToDefinition, Hover};

/// The whole surface, wired to one pool.
///
/// Returns the pool as well, for the same reason `fs_tools` returns its read
/// tracker: a harness that wants to shut the servers down politely, or ask which
/// one is running, can — without any tool having to expose it.
///
/// **This is the only supported way to build the set.** `ToolCtx` carries no
/// session state, so the pool lives in the tool structs, and the invariant "all
/// four share one pool" is a wiring convention rather than something the type
/// system holds. Four tools built with four pools is four rust-analyzers
/// indexing the same workspace, and it compiles. The same known defect
/// `tools/fs` records about its read tracker, recorded here for the same reason:
/// cheaper to notice at four tools than at fourteen.
pub fn lsp_tools() -> (Vec<Arc<dyn Tool>>, Arc<Pool>) {
    let pool = Arc::new(Pool::new());
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FindReferences::new(pool.clone())),
        Arc::new(GoToDefinition::new(pool.clone())),
        Arc::new(Hover::new(pool.clone())),
        Arc::new(DocumentSymbols::new(pool.clone())),
    ];
    (tools, pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_the_four_tools_and_they_share_one_pool() {
        let (tools, pool) = lsp_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            [
                "FindReferences",
                "GoToDefinition",
                "Hover",
                "DocumentSymbols"
            ]
        );
        // One pool, four tools holding it, plus the one returned: five strong
        // references. The assertion is the sharing invariant `lsp_tools`
        // promises, which nothing else can check.
        assert_eq!(Arc::strong_count(&pool), 5);
    }

    /// The same shape `tools/fs` asserts, for the same reason: a tool that
    /// forgot its description registers fine and is simply unusable.
    #[test]
    fn every_tool_ships_a_real_description_and_schema() {
        let (tools, _) = lsp_tools();
        for tool in tools {
            assert!(
                tool.description().len() > 200,
                "{} has a stub description",
                tool.name()
            );
            let schema = tool.input_schema();
            assert_eq!(schema["type"], "object", "{}", tool.name());
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
            assert!(schema["required"].is_array(), "{}", tool.name());
        }
    }

    /// The rule from `tools/fs`'s `nothing_machine_specific_reaches_the_hashed_surface`,
    /// which applies here for exactly the same reason and bites harder: the
    /// server's path and version are the most tempting things to put in a
    /// description, and doing it would make `tool_schema_hash` — the thing that
    /// says which tool surface produced a given answer — a property of the box.
    #[test]
    fn nothing_machine_specific_reaches_the_hashed_surface() {
        let (tools, _) = lsp_tools();
        for tool in tools {
            let surface = format!("{}\n{}", tool.description(), tool.input_schema());
            for token in [
                "rust-analyzer",
                ".cargo",
                ".vscode",
                "Program Files",
                server::OVERRIDE_ENV,
            ] {
                assert!(
                    !surface.contains(token),
                    "{} names {token:?}, which is machine-specific",
                    tool.name()
                );
            }
            // And the positive half: the description must tell the model where
            // the readiness answer is, or removing the machine-specific claim
            // was just removing information.
            assert!(
                surface.contains("first two lines"),
                "{} does not tell the model to read the header",
                tool.name()
            );
            assert!(
                surface.to_lowercase().contains("indexing"),
                "{} does not warn about a partial index",
                tool.name()
            );
        }
    }

    /// Every tool must register — which is the `read_only ⟹ idempotent` check
    /// in `Registry::register`, run against these declarations.
    #[test]
    fn the_declarations_cohere_enough_to_register() {
        let (tools, _) = lsp_tools();
        let mut registry = emma_tool_api::Registry::new();
        for tool in tools {
            registry.register(tool);
        }
        assert_eq!(registry.names().len(), 4);
    }
}
