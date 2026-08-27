//! `index.md`: the catalog that is read first.
//!
//! Rendered from a listing and nothing else, so [`super::Wiki::rebuild_index`]
//! and any future incremental writer have one definition of what the file
//! should say. Grouped by category in [`Category::ALL`] order, with every
//! category present even when empty — a wiki that shows five headings and
//! hides the sixth reads as if the sixth is not allowed.

use super::{Category, Listing};
use std::fmt::Write;

pub fn render(all: &Listing) -> String {
    let live: Vec<_> = all.pages.iter().filter(|p| !p.archived).collect();
    let archived: Vec<_> = all.pages.iter().filter(|p| p.archived).collect();
    let pinned = live.iter().filter(|p| p.front.pinned).count();

    let mut s = String::from("# Memory index\n\n");
    let _ = writeln!(
        s,
        "{} {}, {pinned} pinned, {} archived. Read this before opening any page.",
        live.len(),
        if live.len() == 1 {
            "memory"
        } else {
            "memories"
        },
        archived.len(),
    );
    let _ = writeln!(
        s,
        "\nRegenerated from `pages/` on every change. Edit a page, not this file.\n"
    );

    for c in Category::ALL {
        let _ = writeln!(s, "## {c}\n");
        let rows: Vec<_> = live.iter().filter(|p| p.front.category == c).collect();
        if rows.is_empty() {
            let _ = writeln!(s, "_none yet_\n");
            continue;
        }
        for p in rows {
            let pin = if p.front.pinned { " · pinned" } else { "" };
            let _ = writeln!(
                s,
                "- [[{}]] — {} ({}{pin})",
                p.slug, p.title, p.front.created
            );
        }
        s.push('\n');
    }

    if !archived.is_empty() {
        let _ = writeln!(s, "## archive\n");
        for p in &archived {
            let _ = writeln!(
                s,
                "- [[{}]] — {} ({}, {})",
                p.slug, p.title, p.front.category, p.front.created
            );
        }
        s.push('\n');
    }

    // Unreadable pages are catalogued too. The alternative is a file somebody
    // edited by hand disappearing from the index with no trace of why, which
    // is indistinguishable from the memory never having been written.
    if !all.malformed.is_empty() {
        let _ = writeln!(
            s,
            "## unreadable\n\nThese files are in the wiki and could not be parsed. \
             Nothing was lost; fix the frontmatter and they return.\n"
        );
        for m in &all.malformed {
            let _ = writeln!(s, "- `{}` — {}", m.slug, m.problem.replace('\n', " "));
        }
        s.push('\n');
    }
    s
}
