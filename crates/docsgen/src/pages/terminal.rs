//! Diagrams for the The terminal chapter of `docs/`.
//!
//! Pages: terminal-layout, terminal-surface.
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

/// The sidebar's width rule, read from the four constants that decide it.
///
/// The page's claim is a proportion with a floor and a ceiling. All three
/// numbers live in `term/sidebar.rs` as constants, so the diagram is those
/// constants rather than a transcription of them: `PROPORTION_MILLIS` in
/// thousandths, `MIN_WIDTH`, `MAX_WIDTH`, and `AUTO_COLLAPSE_COLS` for the
/// width below which the shell collapses the sidebar on its own.
///
/// What the picture does not show, because no constant states it: the
/// `min(total / 2)` applied after the clamp. On a terminal narrower than twice
/// the floor, half the width is the honest maximum, and that is an expression
/// rather than a value. The page says it in prose.
pub fn terminal_layout(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/term/sidebar.rs");
    let file = rust::parse(&path)?;

    let read = |name: &str| -> Result<String> {
        rust::const_value(&file, name)
            .with_context(|| format!("{} declares {name}", path.display()))
    };
    let millis = read("PROPORTION_MILLIS")?;
    let min = read("MIN_WIDTH")?;
    let max = read("MAX_WIDTH")?;
    let collapse = read("AUTO_COLLAPSE_COLS")?;

    // A percentage the reader recognises, from the thousandths the code keeps.
    // Written from the constant rather than beside it: `22.4` typed here would
    // be the transcription this crate exists to remove.
    let pct = millis
        .parse::<f64>()
        .map(|m| format!("{:.1}%", m / 10.0))
        .unwrap_or_else(|_| format!("{millis}/1000"));

    let steps = vec![
        rust::Item {
            name: "proportion".into(),
            doc: String::new(),
            detail: pct.clone(),
        },
        rust::Item {
            name: "floor".into(),
            doc: String::new(),
            detail: format!("{min} cols"),
        },
        rust::Item {
            name: "ceiling".into(),
            doc: String::new(),
            detail: format!("{max} cols"),
        },
        rust::Item {
            name: "collapse".into(),
            doc: String::new(),
            detail: format!("< {collapse}"),
        },
    ];

    Ok(shapes::ladder(
        "layout",
        format!(
            "The sidebar takes {pct} of the window, clamped to a floor of {min} \
             columns and a ceiling of {max}, and the shell collapses it below \
             {collapse} columns. A further cap of half the total is applied \
             after the clamp and is not drawn: it is an expression rather than \
             a constant."
        ),
        &steps,
        Some("proportion"),
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

    /// **If this breaks:** the layout page shows a proportion or a clamp the
    /// terminal does not use, which is the drift this crate exists to end.
    ///
    /// Asserted against the constants rather than against `22.4`, `28` and
    /// `40`, so changing the sidebar's width changes the picture without
    /// anybody editing this test.
    #[test]
    fn the_layout_diagram_draws_the_constants_sidebar_declares() {
        let root = root();
        let file =
            rust::parse(&root.join("crates/emma/src/term/sidebar.rs")).expect("sidebar.rs parses");
        let millis = rust::const_value(&file, "PROPORTION_MILLIS").expect("PROPORTION_MILLIS");
        let min = rust::const_value(&file, "MIN_WIDTH").expect("MIN_WIDTH");
        let max = rust::const_value(&file, "MAX_WIDTH").expect("MAX_WIDTH");

        let d = terminal_layout(&root).expect("the diagram builds");
        let labels: Vec<String> = d.layers.iter().flatten().map(|n| n.label.clone()).collect();

        let pct = format!("{:.1}%", millis.parse::<f64>().expect("a number") / 10.0);
        assert!(
            labels.iter().any(|l| l.contains(&pct)),
            "the measured proportion is missing: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains(&min)),
            "the floor is missing: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains(&max)),
            "the ceiling is missing: {labels:?}"
        );
        assert!(
            d.caption.contains(&pct) && d.caption.contains(&min) && d.caption.contains(&max),
            "the caption states all three: {}",
            d.caption
        );
    }

    /// **If this breaks:** the source moves and the generator draws an empty
    /// ladder rather than reporting that its source is gone.
    #[test]
    fn a_missing_source_fails_the_layout_rather_than_emptying_it() {
        let Err(e) = terminal_layout(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("sidebar.rs"), "{e:#}");
    }
}

/// Why the full-screen frame is refused, read as the guards of
/// `fallback_reason`.
///
/// The page describes a longer sequence than this: `Term::interactive` asks,
/// `Frame::install` re-asks, and the Windows VT probe sits between them. Only
/// the refusal decision is a ladder in the source, and it is the half that
/// matters to a reader asking why they got the plain path -- each rung is a
/// reason, and the first that answers is the answer.
///
/// What is not drawn, because no guard states it: the re-ask in
/// `Frame::install`, the VT proof, and the code-page change. Those are steps in
/// a sequence rather than a run of returns, and the page says so in prose.
pub fn terminal_surface(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/term.rs");
    let file = rust::parse(&path)?;
    let rungs = rust::fn_guards(&file, "fallback_reason")
        .with_context(|| format!("{} decides whether the frame is refused", path.display()))?;

    let n = rungs.len();
    let names: Vec<&str> = rungs.iter().map(|r| r.name.as_str()).collect();
    Ok(shapes::ladder(
        "surface",
        format!(
            "The {n} conditions fallback_reason checks before the frame is \
             allowed, in order: {}. The first that answers refuses the frame and \
             the run takes the plain path with the console left as found. \
             Everything after an acceptance -- the UTF-8 code page, the re-ask \
             in Frame::install, the Windows VT proof -- is a sequence rather \
             than a run of returns, and is not drawn here.",
            names.join(", ")
        ),
        &rungs,
        rungs.first().map(|r| r.name.as_str()),
    ))
}

#[cfg(test)]
mod surface_tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root")
    }

    /// **If this breaks:** the surface page lists a refusal reason the terminal
    /// does not check, or omits one it does -- and a reader who got the plain
    /// path has no way to find out why.
    ///
    /// Asserted against `fallback_reason` itself, so a fifth reason appears
    /// without this test being edited.
    #[test]
    fn the_surface_diagram_draws_every_reason_the_frame_is_refused() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/term.rs")).expect("term.rs parses");
        let rungs = rust::fn_guards(&file, "fallback_reason").expect("fallback_reason has guards");

        let d = terminal_surface(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(
            drawn,
            rungs.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            "the drawn order is the order the reasons are checked"
        );
        assert!(
            d.caption.contains(&rungs.len().to_string()),
            "the caption counts them: {}",
            d.caption
        );
        assert!(
            d.layers.iter().all(|l| l.len() == 1),
            "a ladder is one rung per row"
        );
    }

    /// **If this breaks:** the source moves and the page draws an empty ladder
    /// rather than reporting that its source is gone.
    #[test]
    fn a_missing_source_fails_the_surface_diagram_rather_than_emptying_it() {
        let Err(e) = terminal_surface(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("term.rs"), "{e:#}");
    }
}
