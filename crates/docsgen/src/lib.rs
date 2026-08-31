//! Diagrams for `docs/`, generated from the source they describe.
//!
//! # What this replaces
//!
//! `CLAUDE.md` requires diagrams to be drawn from the source at the time of
//! drawing. Nothing enforced it. 31 pages under `docs/` carry an inline
//! `<svg>` with coordinates written by hand, and a hand-drawn diagram stops
//! matching the code on the commit after it is drawn without changing
//! appearance.
//!
//! One of the 31 is generated today: `architecture.html`. The other 30 are
//! still hand-drawn.
//!
//! # Where the generated SVG lives
//!
//! Committed to the page, not produced by a `build.rs`. A build script would
//! redraw on builds that changed nothing a diagram is drawn from, and files
//! that appear during a build get ignored in `git status`.
//!
//! `tests/current.rs` regenerates every diagram and fails when the committed
//! copy differs, which is the same arrangement as `cargo fmt --check`. It runs
//! under `cargo test --workspace`, which `release.yml` gates on.
//! `cargo run -p emma-docsgen` writes the update.
//!
//! # What a diagram may be drawn from
//!
//! Something a machine can read: a manifest, a registry, an enum, a match. A
//! picture that can only be drawn by a person who has read the module and
//! formed an opinion belongs in prose, where a reader can see it is an
//! opinion.

use std::path::Path;

use anyhow::{bail, Context, Result};

pub mod architecture;
pub mod svg;
pub mod theme;

/// The comment pair a generated diagram lives between, inside a docs page.
///
/// A docs page is mostly prose a person wrote, so the generator owns only what
/// is between these two lines.
pub const OPEN: &str = "<!-- diagram:";
pub const CLOSE: &str = "<!-- /diagram -->";

/// Every diagram this generator knows how to draw, by the name in its marker.
pub fn render_all(root: &Path) -> Result<Vec<(String, String)>> {
    let crates = architecture::read(root)?;
    Ok(vec![(
        "architecture".to_string(),
        architecture::diagram(&crates).render(),
    )])
}

/// Replace the marked block for `name` in `page`, returning the new text.
///
/// A missing marker is an error. Skipping silently would let a diagram stop
/// being regenerated while the generator still reported success.
pub fn inject(page: &str, name: &str, svg: &str) -> Result<String> {
    let open = format!("{OPEN}{name} -->");
    let Some(start) = page.find(&open) else {
        bail!("no `{open}` marker on the page");
    };
    let after = start + open.len();
    let Some(rel) = page[after..].find(CLOSE) else {
        bail!("`{open}` is never closed by `{CLOSE}`");
    };
    let end = after + rel;
    // The indentation of the opening marker is reused, so a generated block
    // sits at the same depth as the prose around it and a diff stays readable.
    let indent: String = page[..start]
        .chars()
        .rev()
        .take_while(|c| *c == ' ')
        .collect();
    let body: String = svg
        .lines()
        .map(|l| {
            if l.is_empty() {
                String::from("\n")
            } else {
                format!("{indent}{l}\n")
            }
        })
        .collect();
    Ok(format!(
        "{}\n{body}{indent}{}",
        &page[..after],
        &page[end..]
    ))
}

/// Write every diagram into its page. Returns the pages that changed.
pub fn write_all(root: &Path) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for (name, svg) in render_all(root)? {
        let page = root.join("docs").join(format!("{name}.html"));
        let before = std::fs::read_to_string(&page)
            .with_context(|| format!("{} carries the {name} diagram", page.display()))?;
        let after = inject(&before, &name, &svg)
            .with_context(|| format!("injecting {name} into {}", page.display()))?;
        if before != after {
            std::fs::write(&page, &after)?;
            changed.push(name);
        }
    }
    Ok(changed)
}

/// Every page that is out of date with respect to the source, with a diff-ish
/// note. Empty means the committed diagrams match the code.
pub fn stale(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (name, svg) in render_all(root)? {
        let page = root.join("docs").join(format!("{name}.html"));
        let before = std::fs::read_to_string(&page)?;
        let after = inject(&before, &name, &svg)?;
        if before != after {
            out.push(name);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<p>before</p>\n  <!-- diagram:x -->\n  <svg>old</svg>\n  <!-- /diagram -->\n<p>after</p>\n";

    #[test]
    fn the_block_between_the_markers_is_replaced_and_nothing_else_is() {
        let out = inject(PAGE, "x", "<svg>new</svg>").expect("markers are present");
        assert!(out.contains("<svg>new</svg>"), "{out}");
        assert!(!out.contains("old"), "{out}");
        assert!(out.contains("<p>before</p>"), "{out}");
        assert!(out.contains("<p>after</p>"), "{out}");
    }

    /// **If this breaks:** a page loses its markers in an edit and the
    /// generator reports success while regenerating nothing, so the diagram
    /// silently freezes at whatever it last said.
    #[test]
    fn a_page_with_no_marker_is_an_error_rather_than_a_no_op() {
        let e = inject("<p>nothing here</p>", "x", "<svg/>").expect_err("no marker");
        assert!(e.to_string().contains("diagram:x"), "{e}");
    }

    /// **If this breaks:** an unclosed marker eats the rest of the page, and
    /// the generator writes a file that ends mid-document.
    #[test]
    fn an_unclosed_marker_is_an_error_rather_than_swallowing_the_page() {
        let e = inject("<!-- diagram:x -->\n<p>rest</p>", "x", "<svg/>").expect_err("unclosed");
        assert!(e.to_string().contains("never closed"), "{e}");
    }

    /// **If this breaks:** running the generator twice produces two different
    /// files, and the freshness test fails on a tree nobody changed.
    #[test]
    fn injecting_the_same_diagram_twice_changes_nothing_the_second_time() {
        let once = inject(PAGE, "x", "<svg>new</svg>").expect("first");
        let twice = inject(&once, "x", "<svg>new</svg>").expect("second");
        assert_eq!(once, twice);
    }
}
