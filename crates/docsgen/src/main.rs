//! Write the generated diagrams into `docs/` and the generated facts into
//! `README.md`, or report on them.
//!
//! `cargo run -p emma-docsgen` after changing anything a diagram is drawn
//! from. `tests/current.rs` fails if this was not run.
//!
//! `cargo run -p emma-docsgen -- coverage` runs a different check: which
//! source modules no page under `docs/` cites at all. See `coverage`'s module
//! doc for why that is a report a person reads rather than a gate that fails
//! the build.

use anyhow::Result;

fn main() -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize()?;

    match std::env::args().nth(1).as_deref() {
        Some("coverage") => return report_coverage(&root),
        Some("quotes") => return print_quotes(&root),
        _ => {}
    }

    let changed = emma_docsgen::write_all(&root)?;
    if changed.is_empty() {
        println!("every diagram already matches the source");
    } else {
        for name in &changed {
            println!("redrew {name}");
        }
    }
    Ok(())
}

/// Print every quoted block as plain source, one after another.
///
/// The reviewing instrument for a conversion: a hand-written block and the
/// text the source actually says are compared here, before the block is
/// replaced. A difference is a defect the page has been carrying, and it
/// should be read rather than silently overwritten.
fn print_quotes(root: &std::path::Path) -> Result<()> {
    for q in emma_docsgen::quote::all() {
        println!("===== {}#{} <- {} =====", q.page, q.id, q.file);
        println!("{}", emma_docsgen::quote::text(&root.join(q.file), &q.sel)?);
    }
    Ok(())
}

/// Print every source module no page under `docs/` cites.
///
/// Names them one per line rather than just a count, because a count invites
/// nobody to go fix any of them and a list is a punch list.
fn report_coverage(root: &std::path::Path) -> Result<()> {
    let modules = emma_docsgen::coverage::coverage(root)?;
    let total = modules.len();
    let gaps: Vec<&str> = modules
        .iter()
        .filter(|m| !m.cited)
        .map(|m| m.path.as_str())
        .collect();
    println!(
        "{} source modules, {} cited by no docs page:",
        total,
        gaps.len()
    );
    for path in &gaps {
        println!("  {path}");
    }
    Ok(())
}
