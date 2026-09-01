//! One function per diagram: which facts it reads, and which shape it uses.
//!
//! Each function names a source file and an item in it, and fails if either is
//! gone. That is the point of the split -- [`crate::rust`] knows how to read
//! Rust, [`crate::shapes`] knows how to lay a diagram out, and everything
//! specific to one page in `docs/` is here where it can be read against that
//! page.
//!
//! # The rule for adding one
//!
//! The diagram must be derivable from something a machine can read. If drawing
//! it needs a person's reading of a function's control flow, it is an argument
//! rather than a structure, and it belongs in the page's prose where a reader
//! can see who is making it.

use std::path::Path;

use anyhow::{Context, Result};

use crate::rust;
use crate::shapes;
use crate::svg::Diagram;

/// The language server's readiness states, from the enum itself.
///
/// The page's claim is that there are four and that three of them mean the
/// answer may be incomplete. Both come from `Readiness`: the variants, and
/// which of them return a caveat.
pub fn tools_lsp(root: &Path) -> Result<Diagram> {
    let path = root.join("tools/lsp/src/client.rs");
    let file = rust::parse(&path)?;
    let states = rust::enum_variants(&file, "Readiness")
        .with_context(|| format!("{} declares the readiness states", path.display()))?;

    let n = states.len();
    let names: Vec<&str> = states.iter().map(|s| s.name.as_str()).collect();
    Ok(shapes::set(
        "lsp",
        format!(
            "The {n} readiness states a language-server answer can be produced \
             in: {}. Ready is the only one that adds no caveat to the answer; \
             in the others an empty result is not evidence that there is \
             nothing to find.",
            names.join(", ")
        ),
        &states,
        2,
        Some("Ready"),
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

    /// **If this breaks:** the language-server page shows a set of states the
    /// client does not have -- a state removed from the enum keeps its box, or
    /// one added never gets one.
    ///
    /// Asserted against the real `Readiness` rather than a count, so adding a
    /// fifth state does not need this test edited.
    #[test]
    fn the_readiness_diagram_draws_the_enum_the_client_declares() {
        let root = root();
        let file = rust::parse(&root.join("tools/lsp/src/client.rs")).expect("client.rs parses");
        let states = rust::enum_variants(&file, "Readiness").expect("Readiness exists");

        let d = tools_lsp(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), states.len(), "{drawn:?}");
        for s in &states {
            assert!(drawn.contains(&s.name), "{} is missing: {drawn:?}", s.name);
        }
        assert!(
            d.caption.contains(&states.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    /// **If this breaks:** the file is renamed or the enum is, and the
    /// generator draws something else rather than saying so.
    #[test]
    fn a_missing_source_fails_the_diagram_rather_than_emptying_it() {
        let Err(e) = tools_lsp(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("client.rs"), "{msg}");
    }
}
