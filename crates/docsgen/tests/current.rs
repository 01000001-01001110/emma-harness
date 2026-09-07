//! The committed diagrams must match the source they are drawn from.
//!
//! Same arrangement as `cargo fmt --check`: the generated artefact is
//! committed, and this regenerates it and fails when the two differ.
//!
//! "Diagrams" includes the three Markdown blocks in `README.md` -- the
//! provider sentence, the tool list and the budget table -- which are
//! generated through the same markers. A hand edit to a name or a number
//! inside those markers fails here the same way a stale SVG does.
//!
//! A `build.rs` would redraw on every `cargo build`, including the builds
//! where nothing a diagram is drawn from has changed, and files that appear
//! during a build get ignored. This runs under `cargo test --workspace`,
//! which `release.yml` gates on.

/// **If this breaks:** a docs page shows a diagram of a workspace that no
/// longer exists, or quotes a function that has since been edited. Run
/// `cargo run -p emma-docsgen`, look at the diff, and commit it.
///
/// "Diagrams" is the older half of the name. Since 2026-09-07 the same list
/// carries the `<!-- quote: -->` blocks, which are cut out of the source they
/// cite, and a quoted function that changes without its page being regenerated
/// fails here the same way a stale SVG does.
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

/// **If this breaks:** a page quotes a symbol that no longer exists, or one
/// whose surroundings moved far enough that the anchors naming a branch inside
/// it match nothing or match twice.
///
/// The freshness test above cannot tell those apart from an ordinary edit: it
/// reports the whole generator as failed and names the first block that could
/// not be produced. This one produces every block and says which selection
/// broke, which is the difference between "regenerate and commit" and "that
/// function is gone, decide what the page should say now".
#[test]
fn every_quoted_block_still_names_something_in_the_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root");
    let broken: Vec<String> = emma_docsgen::quote::all()
        .iter()
        .filter_map(|q| {
            emma_docsgen::quote::text(&root.join(q.file), &q.sel)
                .err()
                .map(|e| format!("{}#{} <- {}: {e:#}", q.page, q.id, q.file))
        })
        .collect();
    assert!(
        broken.is_empty(),
        "{} quoted block(s) name nothing:\n  {}",
        broken.len(),
        broken.join("\n  ")
    );
}

/// **If this breaks:** a docs page points a reader at a test that is not there.
///
/// The site's `tested` chips are promises that somebody can go and read the
/// proof. Nothing checked them until 2026-09-07, when a sweep found ten dead
/// citations — two created that same day, by a test the duplication sweep
/// deleted and one renamed when the tool surface grew. Neither was noticed, and
/// in the same session an author invented two names that had never existed.
///
/// Fix it by renaming the citation to the test that replaced it, or by deleting
/// the claim if the guarantee went with the test. Do not add the name back to
/// the source to satisfy this.
#[test]
fn every_test_a_docs_page_cites_exists() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root");
    let dead = emma_docsgen::citations::dead_citations(&root).expect("the scan runs");
    let listed: Vec<String> = dead.iter().map(ToString::to_string).collect();
    assert!(
        dead.is_empty(),
        "{} citation(s) name no test:\n  {}",
        dead.len(),
        listed.join("\n  ")
    );
}
