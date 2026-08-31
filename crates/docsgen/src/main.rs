//! Write the generated diagrams into `docs/`.
//!
//! `cargo run -p emma-docsgen` after changing anything a diagram is drawn
//! from. `tests/current.rs` fails if this was not run.

use anyhow::Result;

fn main() -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize()?;
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
