//! Emma's task list: `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, backed
//! by a markdown file a person can read.
//!
//! The names are Claude Code's, exactly, so a hook matcher or an allow-list
//! written for one works for the other.
//!
//! **The file is the product, the tools are the accessor.** `.emma/tasks/
//! tasks.md` is not a serialisation format that happens to be text. It is a
//! document somebody has open in an editor while the agent works, and every
//! constraint in [`doc`] follows from that: a line nobody changed is written
//! back byte for byte, nothing is ever reordered, and completing a task ticks a
//! box where it sits. The moment a person's edit disappears they stop trusting
//! the file, then stop reading it, and a task list nobody reads is theatre with
//! a write amplification problem.
//!
//! **Emptiness is a result.** No file yet, no tasks, no tasks matching a
//! filter — all `Ok`. The errors are facts about the call: an id that is not
//! there, a status that is not a status, an update that would change nothing.
//!
//! **Nothing is cached between calls.** The file is read, changed and written
//! inside a single `invoke`, because the other writer is a human with an editor
//! and no lock is available. See [`store`] for the guard and, more usefully,
//! for what it still cannot catch.
//!
//! **Done-detection.** [`open_count`] answers "is anything still outstanding?"
//! without going through a tool call, for a loop that wants to ask cheaply and
//! often. It is evidence, not proof — see the note on that function.
//!
//! **The crate, in layers.** [`doc`] is the format and the parser and holds
//! every round-tripping decision; [`store`] is the file, the stale-read guard
//! and the atomic replace; [`render`] is how a task reads back to the model;
//! `args` extracts and rejects arguments. On top of those sit the four tools,
//! one per file — [`create`], [`get`], [`list`], [`update`] — each of which is
//! a thin shell: validate, resolve the path, call into `store`, format the
//! result. The reasoning lives in `doc` and `store`; the tool files mostly
//! record why their particular errors are the kind of error they are.

use std::path::Path;
use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError};

mod args;
pub mod create;
pub mod doc;
pub mod get;
pub mod list;
pub mod render;
pub mod store;
pub mod update;

pub use create::TaskCreate;
pub use doc::{Doc, Status, TaskView, RELATIVE_PATH};
pub use get::TaskGet;
pub use list::TaskList;
pub use update::TaskUpdate;

/// The whole surface. Unlike `fs_tools`, these share no mutable state — the
/// file is the state, and it is re-read on every call.
pub fn task_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(TaskCreate::new()),
        Arc::new(TaskGet::new()),
        Arc::new(TaskList::new()),
        Arc::new(TaskUpdate::new()),
    ]
}

/// How many tasks are not completed. A missing file is zero.
///
/// Here so the loop can ask "is anything still open?" without a model call and
/// without paying for a rendered list. **It is evidence about the goal, not the
/// goal.** An empty list means the agent believes it is finished, which is a
/// strictly better signal than the same agent asserting it in prose — it had to
/// write the claims down first, in a file a human can read afterwards — but it
/// is still the agent's own bookkeeping. A model that has learned closing tasks
/// ends the loop can close them, and a model that never opened one has an empty
/// list from the start. Use it to decide when to *stop asking*, never as the
/// only thing that decides the work is done.
pub fn open_count(root: &Path) -> Result<usize, ToolError> {
    // Goes through the tool context so the same containment check applies:
    // a caller passing a root is not a reason to skip it.
    let ctx = ToolCtx {
        cwd: root.to_path_buf(),
        session_id: String::new(),
        turn_id: String::new(),
        // Nothing here spawns background work; an empty registry is the honest
        // value rather than a shared one this call has no business holding.
        background: Default::default(),
    };
    let file = store::tasks_path(&ctx)?;
    let (doc, _) = store::load(&file)?;
    Ok(doc.open_count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_the_four_claude_code_names() {
        // Spelling is the compatibility contract, exactly as in tools/fs.
        let names: Vec<&str> = task_tools().iter().map(|t| t.name()).collect();
        assert_eq!(names, ["TaskCreate", "TaskGet", "TaskList", "TaskUpdate"]);
    }

    /// The description is what the model reads and the schema is what it fills
    /// in, so both are behaviour rather than documentation. This catches the
    /// half-finished tool — a stub sentence, a parameter with no description, a
    /// schema declaring nothing required — none of which any functional test
    /// would notice, because every functional test passes correct arguments.
    #[test]
    fn every_tool_ships_a_real_description_and_schema() {
        for tool in task_tools() {
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

    /// Pins the honest declaration in both directions. The approval gate keys
    /// on `read_only`, so a writer that quietly flipped to `true` would stop
    /// prompting and this would go red — that is the point, and it is why the
    /// prompt for the two writers is dodged by naming them in
    /// `crates/emma/src/approval.rs` instead of by editing `meta()` here.
    /// `tests/tools.rs` proves the other direction, that the two claiming
    /// `read_only` genuinely touch nothing.
    #[test]
    fn only_the_readers_claim_read_only() {
        let mut claimed: Vec<&str> = task_tools()
            .iter()
            .filter(|t| t.meta().read_only)
            .map(|t| t.name())
            .collect();
        claimed.sort_unstable();
        assert_eq!(claimed, ["TaskGet", "TaskList"]);
    }
}
