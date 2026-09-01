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
