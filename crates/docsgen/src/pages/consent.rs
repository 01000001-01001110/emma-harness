//! Diagrams for the Consent & permissions chapter of `docs/`.
//!
//! Pages: consent-axes, consent-egress, consent-exempt, consent-ladder, config-discovery.
//!
//! One function per diagram, each naming the source file it reads and failing
//! if that file or the item in it is gone. `super::tools_lsp` is the worked
//! example; the rules for adding one are in this module's parent.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::rust;
use crate::shapes;
use crate::svg::Diagram;

/// The in-process approval grants, from the fields that store them.
///
/// Both grants are sets of strings, but they answer different questions. The
/// diagram draws the fields as a set because the struct declares membership,
/// not an order or a relationship between them.
pub fn consent_egress(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/approval.rs");
    let file = rust::parse(&path)?;
    let fields = rust::struct_fields(&file, "Approvals")
        .with_context(|| format!("{} declares the approval grants", path.display()))?;
    let grants: Vec<_> = fields
        .into_iter()
        .filter(|field| field.detail == "Mutex<HashSet<String>>")
        .collect();
    if grants.is_empty() {
        bail!(
            "struct Approvals in {} declares no Mutex<HashSet<String>> grant fields",
            path.display()
        );
    }

    let n = grants.len();
    let names: Vec<&str> = grants.iter().map(|field| field.name.as_str()).collect();
    Ok(shapes::set(
        "egress",
        format!(
            "The {n} in-process grant sets declared on Approvals: {}. Each is a \
             Mutex<HashSet<String>>, keeping the grants in separate fields.",
            names.join(", ")
        ),
        &grants,
        2,
        Some("hosts_allowed"),
    ))
}

/// The approval ladder, read as the guards of `Approvals::decide`.
///
/// The order is the design, and it is a property of the function rather than
/// of the prose beside it: `decide` is a run of `if <cond> { return }` where
/// the first that answers is the answer. `fn_guards` returns those in order.
///
/// This found a gap on arrival. `approval.rs`'s module doc carries a
/// hand-numbered precedence block of eight steps that does not include the
/// egress check, which the function performs second -- before the bypass. The
/// numbered list has since been corrected; this diagram is the reason it can
/// no longer drift.
pub fn consent_ladder(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/approval.rs");
    let file = rust::parse(&path)?;
    let rungs = rust::fn_guards(&file, "decide")
        .with_context(|| format!("{} declares the approval ladder", path.display()))?;

    let n = rungs.len();
    Ok(shapes::ladder(
        "ladder",
        format!(
            "The {n} checks Approvals::decide makes, in the order it makes \
             them; the first that answers is the answer and nothing below it \
             gets a say. A PreToolUse hook denial is checked earlier still, in \
             the loop, and is not one of these. An `ask` rule is not a rung \
             either: it sets a flag that skips the three rungs below it rather \
             than returning."
        ),
        &rungs,
        Some("egress()"),
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

    /// **If this breaks:** the egress page shows a grant field `Approvals` no
    /// longer declares, or omits a new in-process grant set.
    #[test]
    fn the_egress_diagram_draws_the_grant_sets_approvals_declares() {
        let root = root();
        let file =
            rust::parse(&root.join("crates/emma/src/approval.rs")).expect("approval.rs parses");
        let fields = rust::struct_fields(&file, "Approvals").expect("Approvals exists");
        let expected: Vec<_> = fields
            .iter()
            .filter(|field| field.detail == "Mutex<HashSet<String>>")
            .collect();
        assert!(!expected.is_empty(), "Approvals has no in-process grants");

        let d = consent_egress(&root).expect("the diagram builds");
        let drawn: Vec<&str> = d
            .layers
            .iter()
            .flatten()
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(drawn.len(), expected.len(), "{drawn:?}");
        for field in &expected {
            assert!(
                drawn.contains(&field.name.as_str()),
                "{} is missing: {drawn:?}",
                field.name
            );
        }
        assert!(
            d.caption.contains(&expected.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    /// **If this breaks:** the approval ladder is drawn in an order the gate
    /// does not check in, which is the only claim the picture makes. Asserted
    /// against `decide` itself rather than a written list, because the written
    /// list in `approval.rs` was missing a rung when this was added.
    #[test]
    fn the_ladder_is_the_order_decide_checks_in() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/approval.rs")).expect("approval.rs");
        let rungs = rust::fn_guards(&file, "decide").expect("decide has guards");

        let d = consent_ladder(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(
            drawn,
            rungs.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            "the drawn order must be the checked order"
        );
        assert!(
            d.layers.iter().all(|l| l.len() == 1),
            "a ladder is one rung per row"
        );
        assert_eq!(
            d.edges.len(),
            rungs.len() - 1,
            "each rung leads to the next"
        );
    }

    /// **If this breaks:** the egress check stops being a rung -- it is the one
    /// the page is about, and it was absent from the hand-written precedence
    /// list for long enough to be worth pinning.
    #[test]
    fn egress_is_one_of_the_rungs_and_sits_above_the_bypass() {
        let d = consent_ladder(&root()).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        let egress = drawn.iter().position(|r| r.starts_with("egress"));
        let bypass = drawn.iter().position(|r| r.contains("SkipAll"));
        let (Some(egress), Some(bypass)) = (egress, bypass) else {
            panic!("both rungs must be present: {drawn:?}");
        };
        assert!(egress < bypass, "egress is checked first: {drawn:?}");
    }

    /// **If this breaks:** the ladder page loses its source and draws nothing.
    #[test]
    fn a_missing_source_fails_the_ladder_rather_than_emptying_it() {
        let Err(e) = consent_ladder(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("approval.rs"));
    }

    /// **If this breaks:** the source file is moved and the generator draws an
    /// empty set instead of reporting that its source is gone.
    #[test]
    fn a_missing_source_fails_the_egress_diagram_rather_than_emptying_it() {
        let Err(e) = consent_egress(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("approval.rs"), "{msg}");
    }
}
