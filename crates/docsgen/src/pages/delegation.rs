//! Diagrams for the Delegation chapter of `docs/`.
//!
//! Pages: delegation-footer.
//!
//! One function per diagram, each naming the source file it reads and failing
//! if that file or the item in it is gone. `super::tools_lsp` is the worked
//! example; the rules for adding one are in this module's parent.

#![allow(unused_imports)]

use std::path::Path;

use anyhow::{Context, Result};

use crate::rust;
use crate::shapes;
use crate::svg::Diagram;

/// What the parent is told about a sub-run, read from the struct that carries
/// it.
///
/// The page's claim is that two things come back from different places: the
/// sub's final message, written by its model, and the loop's own record of
/// what happened. `Facts` is the second one, and its fields are the whole of
/// it -- every line of the footer is composed from a field here, so the field
/// list is the answer to "what can the parent see".
///
/// `ending` is highlighted because it is the field the page turns on: a
/// footer built from testimony could say the run succeeded while the record
/// said otherwise.
pub fn delegation_footer(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/delegate.rs");
    let file = rust::parse(&path)?;
    let fields = rust::struct_fields(&file, "Facts")
        .with_context(|| format!("{} declares what the footer reports", path.display()))?;

    // The footer is composed by `Facts::footer` from `Facts::from(records)`.
    // Both are named so a rename fails the diagram rather than leaving it
    // describing a path that no longer exists.
    for f in ["footer", "from"] {
        if !rust::has_fn(&file, f) {
            anyhow::bail!("{} no longer declares `{f}`", path.display());
        }
    }

    let n = fields.len();
    Ok(shapes::set(
        "footer",
        format!(
            "The {n} fields of Facts, which is what the parent is told about a \
             sub-run. Facts::from reads the loop's own records and Facts::footer \
             renders them, so every line comes from what the loop logged rather \
             than from the subagent's final message -- the two arrive from \
             different places and only one of them is testimony."
        ),
        &fields,
        3,
        Some("ending"),
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

    /// **If this breaks:** the delegation page shows a footer field that no
    /// longer exists, or omits one that was added -- and a reader checking
    /// whether a sub-run's denials reach the parent gets the wrong answer.
    ///
    /// Asserted against `Facts` itself rather than a list, so a new field
    /// appears without this test being edited.
    #[test]
    fn the_footer_diagram_draws_every_field_facts_declares() {
        let root = root();
        let file =
            rust::parse(&root.join("crates/emma/src/delegate.rs")).expect("delegate.rs parses");
        let fields = rust::struct_fields(&file, "Facts").expect("Facts exists");

        let d = delegation_footer(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), fields.len(), "{drawn:?}");
        for f in &fields {
            assert!(drawn.contains(&f.name), "{} is missing: {drawn:?}", f.name);
        }
        assert!(
            d.edges.is_empty(),
            "a field list is membership, not a sequence"
        );
        assert!(
            d.caption.contains(&fields.len().to_string()),
            "the caption counts them: {}",
            d.caption
        );
    }

    /// **If this breaks:** the footer is composed somewhere else and the page
    /// still points at `Facts::from` and `Facts::footer`.
    #[test]
    fn the_diagram_fails_when_the_functions_it_names_are_gone() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/delegate.rs")).expect("parses");
        for f in ["footer", "from"] {
            assert!(rust::has_fn(&file, f), "{f} is named by the caption");
        }
        let Err(e) = delegation_footer(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("delegate.rs"), "{e:#}");
    }
}
