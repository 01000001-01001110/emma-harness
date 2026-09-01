//! Diagrams for the agent loop and sessions chapter of `docs/`.
//!
//! Pages: loop-endings, loop-failure, loop-memo, loop-one-turn,
//! session-compaction, session-fold.
//!
//! One function per diagram, each naming the source file it reads and failing
//! if that file or the item in it is gone. `super::tools_lsp` is the worked
//! example; the rules for adding one are in this module's parent.
//!
//! Three pages stay hand-drawn. Their pictures are control-flow arguments —
//! string literals, a forked iteration, arithmetic in a function body — and
//! nothing `crate::rust` can read states the boxes:
//!
//! - `loop-failure`: `fail()` kinds in `run_tool_call`. They are string
//!   literals (`"no_such_tool"`, `"panicked"`, …) and `e.kind()`, not an enum.
//!   Closest readable item: `ToolError` in `crates/tool-api`, three variants.
//! - `loop-one-turn`: one iteration of `run_goal`, including the tools / no
//!   tools fork. Closest readable item: `Request` in `crates/llm` (seven
//!   fields; the page's "four parts" are the ones that change per call).
//! - `session-compaction`: `compact_if_needed`'s trigger, target, and the
//!   abandon-if-not-smaller branch. Closest readable item: `enum Compacted`
//!   (`Nothing`, `Done`), which is the return type, not the procedure.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::rust::{self, Item};
use crate::shapes;
use crate::svg::Diagram;

fn fn_item(name: &str) -> Item {
    Item {
        name: name.into(),
        doc: String::new(),
        detail: String::new(),
    }
}

/// The four endings the empty-tool-calls branch of `run_goal` can produce.
///
/// Looked up on `Ending`, in the order the checks run, which is not the enum's
/// declaration order (`Done`, `KicksExhausted`, `Stalled`, `Answered`, …).
/// Swapping two variants in the enum would leave a declaration-order diagram
/// looking right while the function did the other thing.
pub fn loop_endings(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/agent.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "run_goal") {
        bail!("{} no longer declares `run_goal`", path.display());
    }
    let variants = rust::enum_variants(&file, "Ending")
        .with_context(|| format!("{} declares Ending", path.display()))?;

    let names = ["Done", "Stalled", "Answered", "KicksExhausted"];
    let mut steps = Vec::new();
    for name in names {
        steps.push(
            variants
                .iter()
                .find(|v| v.name == name)
                .cloned()
                .with_context(|| {
                    format!("Ending::{name} is a rung of the empty-tool-calls ladder")
                })?,
        );
    }

    let n = steps.len();
    let drawn: Vec<&str> = steps.iter().map(|s| s.name.as_str()).collect();
    Ok(shapes::ladder(
        "endings",
        format!(
            "{n} endings the empty-tool-calls branch of run_goal can produce, \
             in the order the checks run: {}. Falling through all {n} sends a \
             kick and continues the loop.",
            drawn.join(", ")
        ),
        &steps,
        Some("Done"),
    ))
}

/// The memo as the writer, the set, and the reader.
///
/// `failed_now` is a local in `run_goal` and a field on `Resumed`; the field is
/// the item a machine can name. `memo_key` is required to exist because both
/// sides and a resume share it, but it is not a box — a fourth box would invent
/// a step the picture does not have.
pub fn loop_memo(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/agent.rs");
    let file = rust::parse(&path)?;
    for name in ["run_goal", "run_tool_call", "memo_key"] {
        if !rust::has_fn(&file, name) {
            bail!("{} no longer declares `{name}`", path.display());
        }
    }
    let fields = rust::struct_fields(&file, "Resumed")
        .with_context(|| format!("{} declares Resumed", path.display()))?;
    let failed_now = fields
        .iter()
        .find(|f| f.name == "failed_now")
        .cloned()
        .with_context(|| "Resumed.failed_now is the memo the loop restores")?;

    let steps = vec![fn_item("run_goal"), failed_now, fn_item("run_tool_call")];
    let n = steps.len();
    Ok(shapes::ladder(
        "memo",
        format!(
            "The memo is {n} named pieces: run_goal writes it, failed_now holds \
             it, run_tool_call only reads it. Clearing lives in the writer, so \
             a reader of run_tool_call alone sees a permanent blacklist."
        ),
        &steps,
        Some("failed_now"),
    ))
}

/// The state a fold walk keeps, from the struct itself.
///
/// The hand-drawn picture was the close_turn / answered / close_goal flow.
/// That flow is an argument about when half a turn is worse than none of it,
/// and it belongs in the quoted methods. The heading above the figure is
/// "The state a walk needs", which is `Fold`'s fields.
pub fn session_fold(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/session.rs");
    let file = rust::parse(&path)?;
    for name in ["fold", "fold_records", "close_turn", "place_turn"] {
        if !rust::has_fn(&file, name) {
            bail!("{} no longer declares `{name}`", path.display());
        }
    }
    let fields = rust::struct_fields(&file, "Fold")
        .with_context(|| format!("{} declares Fold", path.display()))?;

    let n = fields.len();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    Ok(shapes::set(
        "fold",
        format!(
            "The {n} fields of Fold: {}. Two lists because the loop keeps two; \
             damage is the records the fold refused, so a resume can say the \
             conversation it rebuilt is known not to match.",
            names.join(", ")
        ),
        &fields,
        4,
        Some("pending"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root")
    }

    /// **If this breaks:** the endings page shows a rung `Ending` does not
    /// have, or omits one the empty-tool-calls branch still produces.
    ///
    /// The four names are the checks' order in `run_goal`, looked up on the
    /// real enum. Adding `Ending::Foo` does not need this test edited; renaming
    /// `Stalled` does.
    #[test]
    fn the_endings_diagram_draws_the_four_variants_the_empty_tool_calls_branch_produces() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs parses");
        let variants = rust::enum_variants(&file, "Ending").expect("Ending exists");

        let d = loop_endings(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        for name in ["Done", "Stalled", "Answered", "KicksExhausted"] {
            assert!(
                variants.iter().any(|v| v.name == name),
                "{name} is not an Ending variant: {variants:?}"
            );
            assert!(
                drawn.contains(&name.to_string()),
                "{name} missing: {drawn:?}"
            );
        }
        assert_eq!(
            drawn,
            ["Done", "Stalled", "Answered", "KicksExhausted"],
            "check order, not enum declaration order: {drawn:?}"
        );
        assert!(
            d.caption.contains(&drawn.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    /// **If this breaks:** the file is renamed or `Ending` is, and the
    /// generator draws something else rather than saying so.
    #[test]
    fn a_missing_source_fails_the_endings_diagram_rather_than_emptying_it() {
        let Err(e) = loop_endings(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs"), "{msg}");
    }

    /// **If this breaks:** the memo page shows a writer, set, or reader the
    /// loop no longer names.
    #[test]
    fn the_memo_diagram_draws_the_writer_the_set_and_the_reader() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs parses");
        for name in ["run_goal", "run_tool_call", "memo_key"] {
            assert!(rust::has_fn(&file, name), "{name} in agent.rs");
        }
        let fields = rust::struct_fields(&file, "Resumed").expect("Resumed exists");
        assert!(
            fields.iter().any(|f| f.name == "failed_now"),
            "failed_now on Resumed: {fields:?}"
        );

        let d = loop_memo(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(
            drawn,
            ["run_goal", "failed_now", "run_tool_call"],
            "{drawn:?}"
        );
        assert!(
            d.caption.contains(&drawn.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    /// **If this breaks:** the file is renamed and the generator draws an
    /// empty ladder rather than saying so.
    #[test]
    fn a_missing_source_fails_the_memo_diagram_rather_than_emptying_it() {
        let Err(e) = loop_memo(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs"), "{msg}");
    }

    /// **If this breaks:** the fold page shows fields `Fold` does not have, or
    /// omits a new one. Asserted against the real struct, so adding a field
    /// does not need this test edited.
    #[test]
    fn the_fold_diagram_draws_the_fields_the_struct_declares() {
        let root = root();
        let file =
            rust::parse(&root.join("crates/emma/src/session.rs")).expect("session.rs parses");
        let fields = rust::struct_fields(&file, "Fold").expect("Fold exists");

        let d = session_fold(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), fields.len(), "{drawn:?}");
        for f in &fields {
            assert!(drawn.contains(&f.name), "{} is missing: {drawn:?}", f.name);
        }
        assert!(
            d.caption.contains(&fields.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    /// **If this breaks:** the file is renamed or `Fold` is, and the
    /// generator draws something else rather than saying so.
    #[test]
    fn a_missing_source_fails_the_fold_diagram_rather_than_emptying_it() {
        let Err(e) = session_fold(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("session.rs"), "{msg}");
    }
}
