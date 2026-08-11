//! Emma's filesystem tool surface: `Read`, `Write`, `Edit`, `Glob`, `Grep`,
//! `Bash`.
//!
//! The names are Claude Code's, exactly, so a hook matcher or an allow-list
//! written for one works for the other.
//!
//! Three properties hold across every tool here, and each of them is a rule
//! that was worth writing down before any of the code:
//!
//! **Emptiness is a result.** A read of an empty file, a glob that matches
//! nothing and a grep with no hits all succeed. The only errors are facts about
//! the call — a path outside the root, an anchor that occurs twice, a shell
//! that is not installed.
//!
//! `Bash` is where that rule had to be restated rather than merely applied. An
//! exit status is not the machinery reporting on itself; it is usually the
//! answer. `grep -q` says "no" with exit 1, `test -f` says "not there" with
//! exit 1, `cargo test` says "three of these fail" with 101. The module doc
//! here once said a non-zero exit was `Failed`, which made all three tool
//! failures — the exact confusion the rule exists to prevent, and worse inside
//! the loop, where a failed tool is not invoked twice in one turn: one honest
//! `grep -q` miss would have poisoned `Bash` for the rest of the turn. The line
//! is drawn at whether the command *ran*. Could-not-spawn, timed-out and killed
//! are `Failed`; anything that started and finished is `Ok`, with
//! `exit status <n>` as the first line of the content. See [`bash`], which is
//! where that reasoning is recorded in full. It was deliberately not resolved
//! as "`Bash` is the documented exception", because "except X" is how a rule
//! starts becoming folklore.
//!
//! **Nothing escapes the root.** `ToolCtx::cwd` is canonicalised once per call
//! and every path is resolved and contained against it — including paths that
//! do not exist yet, and including symlinks that point outward. See
//! [`path::resolve`], which also lists the escape this cannot catch. The
//! exception is `Bash`, which starts in the root but is a shell and can walk
//! out of it; that is stated plainly in its description rather than implied
//! away.
//!
//! **Three tools share mutable state, deliberately.** `Read`, `Write` and
//! `Edit` hold the same [`session::ReadTracker`] so that `Write` can refuse to
//! clobber a file nobody has looked at. Nothing in the `Tool` trait carries
//! session state, so it lives in the tool structs behind an `Arc` and the
//! invariant is not expressible in the type system — which is why [`fs_tools`]
//! is the only supported way to build the set. Constructing a `Write` with a
//! tracker its `Read` does not share produces a `Write` that never refuses
//! anything, and it compiles. That is a known defect rather than a solved
//! problem; it is recorded on `ToolCtx` in `tool-api` as well.
//!
//! Half these tools are read-only and half are not, and the difference is
//! load-bearing: `emma::approval` gates on `ToolMeta::read_only`, so `Read`,
//! `Glob` and `Grep` run silently while `Write`, `Edit` and `Bash` prompt. The
//! declarations are checked rather than trusted — see `tests/read_only.rs`.

use std::sync::Arc;

use emma_tool_api::Tool;

// One module per tool, plus five that exist so the tools cannot disagree with
// each other: `args` (one spelling for every malformed-call message), `path`
// (the containment check), `walk` (the one directory traversal `Glob` and
// `Grep` share), `session` (the read tracker) and `hashline` (the one
// definition of what a line's hash is, shared by the tool that prints it and
// the two that check it — three copies of that function is how a `Read` starts
// labelling lines with hashes an `Edit` will not accept). `args` is private
// because it is only a way of writing the same error twice; the rest are public
// because `path` in particular is used from outside — `tools/tasks` resolves
// against the same containment.
mod args;
pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod hashline;
pub mod path;
pub mod read;
pub mod session;
pub mod walk;
pub mod write;

pub use bash::{resolve_shell, Bash, Shell, ShellKind, ShellSource};
pub use edit::Edit;
pub use glob::Glob;
pub use grep::Grep;
pub use read::Read;
pub use session::{LineHashes, ReadState, ReadTracker};
pub use write::Write;

/// The whole surface, wired to one read tracker.
///
/// Returns the tracker as well so a caller that wants to inspect or reset it —
/// a test, or a harness starting a new session — can, without any tool having
/// to expose it.
pub fn fs_tools() -> (Vec<Arc<dyn Tool>>, Arc<ReadTracker>) {
    let tracker = Arc::new(ReadTracker::new());
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(Read::new(tracker.clone())),
        Arc::new(Write::new(tracker.clone())),
        Arc::new(Edit::new(tracker.clone())),
        Arc::new(Glob::new()),
        Arc::new(Grep::new()),
        Arc::new(Bash::new()),
    ];
    (tools, tracker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_the_six_claude_code_names() {
        // Spelling is the compatibility contract. A rename here silently breaks
        // every hook matcher and allow-list written against Claude Code.
        let (tools, _) = fs_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["Read", "Write", "Edit", "Glob", "Grep", "Bash"]);
    }

    #[test]
    fn every_tool_ships_a_real_description_and_schema() {
        // These bytes are what the model sees and what the schema digest
        // covers. A tool that forgot its description would otherwise register
        // fine and simply be unusable.
        let (tools, _) = fs_tools();
        for tool in tools {
            let description = tool.description();
            assert!(
                description.len() > 120,
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
}
