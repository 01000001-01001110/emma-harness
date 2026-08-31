//! The committed diagrams must match the source they are drawn from.
//!
//! Same arrangement as `cargo fmt --check`: the generated artefact is
//! committed, and this regenerates it and fails when the two differ.
//!
//! A `build.rs` would redraw on every `cargo build`, including the builds
//! where nothing a diagram is drawn from has changed, and files that appear
//! during a build get ignored. This runs under `cargo test --workspace`,
//! which `release.yml` gates on.

/// **If this breaks:** a docs page shows a diagram of a workspace that no
/// longer exists. Run `cargo run -p emma-docsgen`, look at the diff, and
/// commit it.
#[test]
fn the_committed_diagrams_match_the_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root");
    let stale = emma_docsgen::stale(&root).expect("the generator runs");
    assert!(
        stale.is_empty(),
        "these diagrams no longer match the source: {stale:?}. \
         Run `cargo run -p emma-docsgen` and commit the result."
    );
}
