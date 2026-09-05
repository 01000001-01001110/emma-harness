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
//! 15 of the 31 are generated today. The rest are still hand-drawn, and each
//! of those is either waiting for an extractor or is an argument a person made
//! rather than a structure the source states.
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
pub mod pages;
pub mod rust;
pub mod shapes;
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
    Ok(vec![
        (
            "architecture".to_string(),
            architecture::diagram(&crates).render(),
        ),
        (
            "tools-containment".to_string(),
            architecture::containment(&crates).render(),
        ),
        ("tools-lsp".to_string(), pages::tools_lsp(root)?.render()),
        (
            "terminal-layout".to_string(),
            pages::terminal::terminal_layout(root)?.render(),
        ),
        (
            "consent-egress".to_string(),
            pages::consent::consent_egress(root)?.render(),
        ),
        (
            "consent-ladder".to_string(),
            pages::consent::consent_ladder(root)?.render(),
        ),
        (
            "loop-endings".to_string(),
            pages::loop_::loop_endings(root)?.render(),
        ),
        (
            "loop-memo".to_string(),
            pages::loop_::loop_memo(root)?.render(),
        ),
        (
            "loop-failure".to_string(),
            pages::loop_::loop_failure(root)?.render(),
        ),
        (
            "loop-one-turn".to_string(),
            pages::loop_::loop_one_turn(root)?.render(),
        ),
        (
            "session-compaction".to_string(),
            pages::loop_::session_compaction(root)?.render(),
        ),
        (
            "session-fold".to_string(),
            pages::loop_::session_fold(root)?.render(),
        ),
        (
            "providers-boundary".to_string(),
            pages::providers::providers_boundary(root)?.render(),
        ),
        (
            "providers-credentials".to_string(),
            pages::providers::providers_credentials(root)?.render(),
        ),
        (
            "providers-models".to_string(),
            pages::providers::providers_models(root)?.render(),
        ),
        (
            "providers-caching".to_string(),
            pages::providers::providers_caching(root)?.render(),
        ),
        (
            "providers-content".to_string(),
            pages::providers::providers_content(root)?.render(),
        ),
        (
            "providers-floor".to_string(),
            pages::providers::providers_floor(root)?.render(),
        ),
        (
            "providers-turn".to_string(),
            pages::providers::providers_turn(root)?.render(),
        ),
        (
            "tools-edit".to_string(),
            pages::tools::tools_edit(root)?.render(),
        ),
        (
            "tools-tasks".to_string(),
            pages::tools::tools_tasks(root)?.render(),
        ),
        (
            "tools-truncation".to_string(),
            pages::tools::tools_truncation(root)?.render(),
        ),
        (
            "tools-web".to_string(),
            pages::tools::tools_web(root)?.render(),
        ),
        (
            "tools-ctx".to_string(),
            pages::tools::tools_ctx(root)?.render(),
        ),
        (
            "tools-trait".to_string(),
            pages::tools::tools_trait(root)?.render(),
        ),
    ])
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
    // **The page's line endings decide, not the generator's.** Seven pages
    // under `docs/` are CRLF and the other 82 are LF; writing an LF into a CRLF
    // file leaves a file holding two conventions.
    //
    // Only in the working tree: `.gitattributes` carries `* text=auto eol=lf`,
    // so git normalises on the way in and the stored blob is LF either way.
    // The first version of this comment claimed every later diff would show the
    // whole block as changed, and `git show HEAD:docs/tools-edit.html` came
    // back with no CRLF at all.
    let nl = if page.contains("\r\n") { "\r\n" } else { "\n" };
    let body: String = svg
        .lines()
        .map(|l| {
            if l.is_empty() {
                nl.to_string()
            } else {
                format!("{indent}{l}{nl}")
            }
        })
        .collect();
    Ok(format!(
        "{}{nl}{body}{indent}{}",
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

    /// **If this breaks:** a diagram written into one of the seven CRLF pages
    /// under `docs/` leaves an island of LF inside it. Nothing renders
    /// differently and git normalises it away on commit, so the only place it
    /// is visible is the working tree -- which is where anything reading the
    /// file raw would see it.
    #[test]
    fn a_crlf_page_keeps_its_line_endings() {
        let page = PAGE.replace('\n', "\r\n");
        let out = inject(&page, "x", "<svg>\nnew\n</svg>").expect("markers");
        assert!(out.contains("<svg>"), "{out:?}");
        assert_eq!(
            out.replace("\r\n", "").matches('\n').count(),
            0,
            "a lone LF reached a CRLF page: {out:?}"
        );
    }

    /// **If this breaks:** an LF page gains carriage returns because the
    /// generator assumed the platform rather than reading the file.
    #[test]
    fn an_lf_page_does_not_gain_carriage_returns() {
        let out = inject(PAGE, "x", "<svg>\nnew\n</svg>").expect("markers");
        assert!(!out.contains('\r'), "{out:?}");
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
