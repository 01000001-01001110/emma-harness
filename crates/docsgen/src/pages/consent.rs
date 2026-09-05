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

/// What the gate reads off the tool, looked up where each answer lives.
///
/// Two of the four are fields on `ToolMeta`, two are methods on `Tool`;
/// `Approvals::request` is verified as the function they are read in. The
/// page's further claim -- that the gate checks none of them -- is not
/// something a lookup can state, and it stays in the caption.
pub fn consent_axes(root: &Path) -> Result<Diagram> {
    let approval = root.join("crates/emma/src/approval.rs");
    let approval_file = rust::parse(&approval)?;
    if !rust::has_fn(&approval_file, "request") {
        bail!("{} no longer declares `request`", approval.display());
    }

    let api = root.join("crates/tool-api/src/lib.rs");
    let api_file = rust::parse(&api)?;
    for name in ["name", "network_target"] {
        if !rust::has_fn(&api_file, name) {
            bail!("{} no longer declares `Tool::{name}`", api.display());
        }
    }
    let fields = rust::struct_fields(&api_file, "ToolMeta")
        .with_context(|| format!("{} declares the two axes", api.display()))?;
    let axis = |name: &str| -> Result<rust::Item> {
        fields
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .with_context(|| format!("ToolMeta::{name} is one of the two axes"))
    };

    let from = fn_item("Approvals::request");
    let answers = vec![
        fn_item("name"),
        axis("read_only")?,
        axis("reaches_network")?,
        fn_item("network_target"),
    ];
    let n = answers.len();
    Ok(shapes::fan(
        "axes",
        format!(
            "The {n} answers {} reads off the tool before deciding: name, the \
             two ToolMeta axes read_only and reaches_network, and the \
             NetworkTarget the tool itself computes from args. The gate checks \
             none of them; each is the tool's own declaration. args itself goes \
             to the preview renderer and is never parsed for a host.",
            from.name,
        ),
        &from,
        &answers,
        &[],
    ))
}

fn fn_item(name: &str) -> rust::Item {
    rust::Item {
        name: name.into(),
        doc: String::new(),
        detail: String::new(),
    }
}

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

    /// **If this breaks:** the axes page shows an answer `request` no longer
    /// reads, or omits one it still reads. The fields are looked up on the
    /// real `ToolMeta`, so a renamed field fails here rather than drawing an
    /// old spelling.
    #[test]
    fn the_axes_diagram_draws_the_answers_request_reads_off_the_tool() {
        let root = root();
        let api = rust::parse(&root.join("crates/tool-api/src/lib.rs")).expect("tool-api parses");
        let fields = rust::struct_fields(&api, "ToolMeta").expect("ToolMeta exists");
        for name in ["name", "network_target"] {
            assert!(rust::has_fn(&api, name), "{name} is declared on Tool");
        }
        for name in ["read_only", "reaches_network"] {
            assert!(
                fields.iter().any(|f| f.name == name),
                "{name} is a ToolMeta field: {fields:?}"
            );
        }
        let expected: Vec<String> = [
            "Approvals::request",
            "name",
            "read_only",
            "reaches_network",
            "network_target",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let d = consent_axes(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn, expected, "{drawn:?}");
        assert_eq!(d.edges.len(), expected.len() - 1, "one edge per answer");
        let answers = expected.len() - 1;
        assert!(
            d.caption.contains(&answers.to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    /// **If this breaks:** the axes page loses one of its two source files and
    /// the generator draws from the other alone rather than saying so.
    #[test]
    fn a_missing_source_fails_the_axes_diagram_rather_than_emptying_it() {
        let Err(e) = consent_axes(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("approval.rs"), "{msg}");
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

#[cfg(test)]
mod write_axes {
    use super::*;
    use crate::inject;

    /// Writes the axes page, which this module owns but `render_all` in
    /// `lib.rs` does not list yet; the marker pair is already in the page.
    #[test]
    fn write_the_axes_page() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root");
        let page = root.join("docs").join("consent-axes.html");
        let before = std::fs::read_to_string(&page).expect("the axes page reads");
        let svg = consent_axes(&root).expect("the diagram builds").render();
        let after = inject(&before, "consent-axes", &svg).expect("the markers are present");
        std::fs::write(&page, after).expect("the axes page writes");
    }
}

/// How a harness directory is found, read as the two ways `discover_in`
/// returns one.
///
/// The page's claim is an order: the override answers first, and otherwise a
/// walk climbs from the working directory. Both halves are guards in
/// `harness::discover_in`, and the second is marked with what it repeats over
/// -- it runs once per ancestor and, within each, once per candidate name, so
/// drawing it level with the override would turn a search into a straight
/// line.
///
/// The candidate names come from the constants beside it rather than from the
/// prose: `ROOT_DIR_NAME` is Emma's own and `CLAUDE_DIR_NAME` is the one it
/// reads for compatibility.
pub fn config_discovery(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/harness/src/lib.rs");
    let file = rust::parse(&path)?;

    let rungs = rust::fn_guards(&file, "discover_in")
        .with_context(|| format!("{} declares the discovery order", path.display()))?;
    let env = rust::const_value(&file, "ROOT_ENV")
        .with_context(|| format!("{} names the override variable", path.display()))?;
    let own = rust::const_value(&file, "ROOT_DIR_NAME")
        .with_context(|| format!("{} names Emma's own directory", path.display()))?;
    let compat = rust::const_value(&file, "CLAUDE_DIR_NAME")
        .with_context(|| format!("{} names the compatible directory", path.display()))?;

    let steps: Vec<rust::Item> = rungs
        .iter()
        .map(|r| rust::Item {
            name: r.name.clone(),
            doc: r.doc.clone(),
            detail: if r.doc.is_empty() {
                env.clone()
            } else {
                format!("{own} | {compat}")
            },
        })
        .collect();

    let n = steps.len();
    Ok(shapes::ladder(
        "discovery",
        format!(
            "The {n} ways discover_in returns a harness directory. {env} answers \
             first and, when it names something that is not a directory, the \
             search is refused rather than continued. Otherwise the walk climbs \
             from the working directory, trying {own} and {compat} at each \
             ancestor -- the second rung repeats per ancestor, which is why it \
             is marked and not drawn level with the first."
        ),
        &steps,
        Some(rungs[0].name.as_str()),
    ))
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root")
    }

    /// **If this breaks:** the discovery page shows an order the harness does
    /// not search in, or loses the fact that the second check repeats.
    ///
    /// Asserted against `fn_guards` on the real function rather than against a
    /// written list, so a third return added to `discover_in` appears here
    /// without this test being edited.
    #[test]
    fn the_discovery_diagram_draws_the_returns_discover_in_declares() {
        let root = root();
        let file = rust::parse(&root.join("crates/harness/src/lib.rs")).expect("harness parses");
        let rungs = rust::fn_guards(&file, "discover_in").expect("discover_in has guards");

        let d = config_discovery(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(
            drawn,
            rungs.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            "the drawn order is the order discover_in returns in"
        );
        assert!(
            rungs.iter().any(|r| !r.doc.is_empty()),
            "one rung repeats per ancestor and must say so: {rungs:?}"
        );
        for name in ["ROOT_ENV", "ROOT_DIR_NAME", "CLAUDE_DIR_NAME"] {
            let v = rust::const_value(&file, name).expect(name);
            assert!(
                d.caption.contains(&v),
                "the caption names {name} ({v}): {}",
                d.caption
            );
        }
    }

    /// **If this breaks:** the source moves and the page draws an empty ladder
    /// instead of reporting that its source is gone.
    #[test]
    fn a_missing_source_fails_the_discovery_diagram_rather_than_emptying_it() {
        let Err(e) = config_discovery(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("lib.rs"), "{e:#}");
    }
}

/// The named hole in the gate: every tool that skips the prompt by being on a
/// list rather than by being read-only.
///
/// The page argues something wider -- that the gate has a ceiling and no
/// floor, with no unconditional deny where one could sit. **That half is not
/// drawn, and cannot be:** an absence is not a fact any lookup returns, and a
/// picture asserting it would be a person's argument wearing a generated
/// diagram's authority. It stays in the page's prose where a reader can see
/// who is making it.
///
/// What is drawn is the half a machine can check: the membership of `EXEMPT`.
/// A tool added to that list stops asking, and this is the picture that
/// changes when one is.
pub fn consent_exempt(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/approval.rs");
    let file = rust::parse(&path)?;
    let exempt = rust::const_list(&file, "EXEMPT")
        .with_context(|| format!("{} declares the exemption", path.display()))?;

    let n = exempt.len();
    let names: Vec<&str> = exempt.iter().map(|i| i.name.as_str()).collect();
    Ok(shapes::set(
        "exempt",
        format!(
            "The {n} tools on EXEMPT: {}. Each skips the prompt because it is \
             named here rather than because it is read-only, which is why the \
             list is short and why adding to it is a decision rather than a \
             tidy-up. What sits above the prompt -- deny rules and hook denials \
             -- and the absence of any unconditional deny below it are argued in \
             the prose on this page; neither is a fact this diagram can read.",
            names.join(", ")
        ),
        &exempt,
        2,
        None,
    ))
}

#[cfg(test)]
mod exempt_tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root")
    }

    /// **If this breaks:** the page shows an exemption list that is not the
    /// one the gate consults, and a reader cannot tell which tools skip the
    /// prompt. A tool added to `EXEMPT` stops asking; this is what makes that
    /// visible.
    #[test]
    fn the_exempt_diagram_draws_the_list_the_gate_consults() {
        let root = root();
        let file =
            rust::parse(&root.join("crates/emma/src/approval.rs")).expect("approval.rs parses");
        let exempt = rust::const_list(&file, "EXEMPT").expect("EXEMPT exists");

        let d = consent_exempt(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), exempt.len(), "{drawn:?}");
        for tool in &exempt {
            assert!(
                drawn.contains(&tool.name),
                "{} missing: {drawn:?}",
                tool.name
            );
            assert!(d.caption.contains(&tool.name), "the caption names it too");
        }
        assert!(d.edges.is_empty(), "membership is not a sequence");
    }

    /// **If this breaks:** the source moves and the page draws an empty set,
    /// which would read as "no tool is exempt" -- the opposite of a warning.
    #[test]
    fn a_missing_source_fails_the_exempt_diagram_rather_than_emptying_it() {
        let Err(e) = consent_exempt(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("approval.rs"), "{e:#}");
    }
}
