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
pub mod citations;
pub mod coverage;
pub mod pages;
pub mod quote;
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

/// The comment pair a quoted source block lives between.
///
/// A second word rather than a second injector: `quote:` and `diagram:` are
/// injected by the same function, checked by the same `stale`, and written by
/// the same command. They are spelled differently because a reader looking at
/// the page should be able to tell a picture drawn from the source from a
/// block cut out of it, and because a quote carries one extra rule the marker
/// has to state -- see `inject_quote`.
pub const QUOTE_OPEN: &str = "<!-- quote:";
pub const QUOTE_CLOSE: &str = "<!-- /quote -->";

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
            "terminal-surface".to_string(),
            pages::terminal::terminal_surface(root)?.render(),
        ),
        (
            "config-discovery".to_string(),
            pages::consent::config_discovery(root)?.render(),
        ),
        (
            "delegation-footer".to_string(),
            pages::delegation::delegation_footer(root)?.render(),
        ),
        (
            "consent-exempt".to_string(),
            pages::consent::consent_exempt(root)?.render(),
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
        // Markdown for `README.md`, not SVG for a page. Same markers, same
        // freshness test; see `page_for` for how the name picks the file.
        (
            "readme-providers".to_string(),
            pages::readme::providers(root)?,
        ),
        ("readme-tools".to_string(), pages::readme::tools(root)?),
        ("readme-budgets".to_string(), pages::readme::budgets(root)?),
    ])
}

/// The file a generated block is written into.
///
/// `docs/{name}.html` for everything, except that a name starting with
/// `readme-` goes to `README.md`. The prefix is the seam rather than a second
/// registry because there is one list of names, one `stale`, one `write_all`
/// and one test, and a second registry would need a second of each -- or a
/// flag threaded through all four -- to say the one thing the name already
/// says. A README block is a diagram that happens to be Markdown; the
/// generator neither knows nor cares, since `inject` never reads the body.
pub fn page_for(root: &Path, name: &str) -> std::path::PathBuf {
    if name.starts_with("readme-") {
        root.join("README.md")
    } else {
        root.join("docs").join(format!("{name}.html"))
    }
}

/// Replace the marked block for `name` in `page`, returning the new text.
///
/// A missing marker is an error. Skipping silently would let a diagram stop
/// being regenerated while the generator still reported success.
pub fn inject(page: &str, name: &str, svg: &str) -> Result<String> {
    inject_between(page, OPEN, CLOSE, name, svg)
}

/// Replace the quoted block for `id` in `page`, returning the new text.
///
/// One rule a diagram does not have: **the marker must sit in column zero.**
/// `inject_between` re-indents a block to match its marker, which is right for
/// an SVG sitting inside prose and wrong for a `<pre>`, where the indent would
/// land inside the block and show up as leading spaces on every quoted line.
/// Refusing is better than silently indenting, because the damage is visible
/// only to a reader of the rendered page.
pub fn inject_quote(page: &str, id: &str, block: &str) -> Result<String> {
    let open = format!("{QUOTE_OPEN}{id} -->");
    if let Some(start) = page.find(&open) {
        let indent = page[..start]
            .chars()
            .rev()
            .take_while(|c| *c == ' ')
            .count();
        if indent > 0 {
            bail!("`{open}` is indented {indent} spaces; a quote marker sits in column zero");
        }
    }
    inject_between(page, QUOTE_OPEN, QUOTE_CLOSE, id, block)
}

/// The shared body of both injectors.
fn inject_between(
    page: &str,
    open_prefix: &str,
    close: &str,
    name: &str,
    svg: &str,
) -> Result<String> {
    let open = format!("{open_prefix}{name} -->");
    let Some(start) = page.find(&open) else {
        bail!("no `{open}` marker on the page");
    };
    let after = start + open.len();
    let Some(rel) = page[after..].find(close) else {
        bail!("`{open}` is never closed by `{close}`");
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

/// One generated block, resolved: the file it lives in, the marker that
/// bounds it, and the text the source says it should hold now.
///
/// Diagrams and quotes share this so that `write_all`, `stale` and the
/// freshness test each exist once. The alternative was a second registry, a
/// second writer and a second test, which is how one of the two ends up
/// running in CI and the other does not.
struct Block {
    page: std::path::PathBuf,
    /// The marker name, prefixed by its kind for the messages a failing test
    /// prints: `terminal-layout` or `quote:config-themes#role-names`.
    label: String,
    body: String,
    quote: bool,
    name: String,
}

impl Block {
    /// The page's text with this block's contents replaced.
    fn applied(&self, before: &str) -> Result<String> {
        if self.quote {
            inject_quote(before, &self.name, &self.body)
        } else {
            inject(before, &self.name, &self.body)
        }
    }
}

/// Every generated block: the diagrams, the README's Markdown, and the quoted
/// source blocks.
fn blocks(root: &Path) -> Result<Vec<Block>> {
    let mut out: Vec<Block> = render_all(root)?
        .into_iter()
        .map(|(name, svg)| Block {
            page: page_for(root, &name),
            label: name.clone(),
            body: svg,
            quote: false,
            name,
        })
        .collect();
    for q in quote::all() {
        out.push(Block {
            page: root.join("docs").join(format!("{}.html", q.page)),
            label: format!("quote:{}#{}", q.page, q.id),
            body: quote::render(root, &q)?,
            quote: true,
            name: q.id.to_string(),
        });
    }
    Ok(out)
}

/// Write every generated block into its page. Returns the ones that changed.
pub fn write_all(root: &Path) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for block in blocks(root)? {
        let before = std::fs::read_to_string(&block.page).with_context(|| {
            format!("{} carries the {} block", block.page.display(), block.label)
        })?;
        let after = block
            .applied(&before)
            .with_context(|| format!("injecting {} into {}", block.label, block.page.display()))?;
        if before != after {
            std::fs::write(&block.page, &after)?;
            changed.push(block.label);
        }
    }
    Ok(changed)
}

/// Every generated block that is out of date with respect to the source.
/// Empty means the committed pages match the code.
pub fn stale(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for block in blocks(root)? {
        let before = std::fs::read_to_string(&block.page).with_context(|| {
            format!("{} carries the {} block", block.page.display(), block.label)
        })?;
        let after = block
            .applied(&before)
            .with_context(|| format!("injecting {} into {}", block.label, block.page.display()))?;
        if before != after {
            out.push(block.label);
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

    /// **If this breaks:** a README block is looked for under `docs/`, or a
    /// docs diagram is written into the README. Every name that existed before
    /// the README joined must still resolve to `docs/`, which is the second
    /// assertion.
    #[test]
    fn a_readme_prefixed_name_targets_the_readme_and_nothing_else_moves() {
        let root = Path::new("r");
        assert_eq!(page_for(root, "readme-tools"), root.join("README.md"));
        assert_eq!(
            page_for(root, "tools-edit"),
            root.join("docs").join("tools-edit.html")
        );
        assert_eq!(
            page_for(root, "architecture"),
            root.join("docs").join("architecture.html")
        );
    }

    /// **If this breaks:** a quoted block is written under an indented marker,
    /// and every line of the `<pre>` on the rendered page gains leading spaces
    /// that are not in the file it quotes. The damage shows up only in a
    /// browser, which is the worst place for it to show up first.
    #[test]
    fn an_indented_quote_marker_is_refused_rather_than_indenting_the_block() {
        let page = "<div>
  <!-- quote:x -->
  <pre>old</pre>
  <!-- /quote -->
</div>";
        let e = inject_quote(page, "x", "<pre>new</pre>").expect_err("the marker is indented");
        assert!(e.to_string().contains("column zero"), "{e}");
    }

    /// **If this breaks:** a quote marker is closed by `<!-- /diagram -->` or
    /// the other way round, and one injector eats the other's block.
    #[test]
    fn a_quote_and_a_diagram_do_not_answer_to_each_other_s_markers() {
        let quoted = "<!-- quote:x -->
old
<!-- /quote -->
";
        assert!(
            inject(quoted, "x", "<svg/>").is_err(),
            "diagram found a quote"
        );
        assert!(
            inject_quote(PAGE, "x", "<pre/>").is_err(),
            "quote found a diagram"
        );
        let out = inject_quote(quoted, "x", "<pre>new</pre>").expect("its own markers");
        assert!(
            out.contains("<pre>new</pre>") && !out.contains("old"),
            "{out}"
        );
    }

    /// **If this breaks:** a Markdown block is written with an indent, which
    /// turns a table row into a code line. README markers sit in column zero,
    /// and this pins that a zero-indent marker gives a zero-indent body.
    #[test]
    fn a_markdown_block_at_column_zero_stays_at_column_zero() {
        let page = "# Title\n\n<!-- diagram:readme-x -->\nold\n<!-- /diagram -->\n\nafter\n";
        let out = inject(page, "readme-x", "| a | b |\n| - | - |").expect("markers");
        assert_eq!(
            out,
            "# Title\n\n<!-- diagram:readme-x -->\n| a | b |\n| - | - |\n<!-- /diagram -->\n\nafter\n"
        );
    }
}
