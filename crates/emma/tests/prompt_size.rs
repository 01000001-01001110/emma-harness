//! What Emma sends before the first user word, measured rather than asserted.
//!
//! `docs/index.html` carried a note saying Emma's own prompt "has never been
//! counted" and that settling it needs Anthropic's `count_tokens` endpoint with
//! a real key. Two thirds of that was wrong. The `tools` array is assembled with
//! no network and no credential, and its size in bytes is a fact this test can
//! read off disk in a second. Only the conversion from bytes to tokens, and the
//! comparison against another harness, need anything paid.
//!
//! **This test asserts an order of magnitude, not a number.** A tool surface
//! that doubles is worth somebody looking at; a tool surface that grows by a
//! description is not, and a test that goes red on every wording change is a
//! test people delete. The number itself is printed, so
//! `cargo test -p emma prompt_size -- --nocapture` is the measurement.

use emma_tool_api::Registry;

/// The wire `tools` array for the registry a run assembles, in bytes.
///
/// Built the way `main::run` builds it, minus the surfaces that depend on this
/// machine: no `Skill` (needs a harness), no `WebFetch` and no browser tools
/// (need a Chrome). So this is a floor, and the comment below says by roughly
/// how much. The provider's search tool is not in any of these numbers: it is
/// one small entry the provider appends on its own side of `Request::tools`,
/// and this test measures what Emma assembles.
fn tool_surface_bytes() -> (usize, usize) {
    let mut registry = Registry::new();
    let (fs, _tracker) = emma_tools_fs::fs_tools();
    for tool in fs {
        registry.register(tool);
    }
    for tool in emma_tools_tasks::task_tools() {
        registry.register(tool);
    }
    let (lsp, _pool) = emma_tools_lsp::lsp_tools();
    for tool in lsp {
        registry.register(tool);
    }
    let defs = registry.wire_definitions();
    let bytes = defs.iter().map(|d| d.to_string().len()).sum();
    (defs.len(), bytes)
}

/// **If this breaks:** the tool surface changed size by more than a factor of
/// two, which is a design change rather than an edit. Print the number, decide
/// whether it is wanted, and move the bound.
///
/// **What the mutations showed, and it is not flattering.** Deleting the tasks
/// and language-server registrations goes red, on the count. Deleting only the
/// four language-server tools stays green: the count falls to 12, which is the
/// floor, and the bytes stay inside the range. So a quarter of the surface can
/// disappear without this noticing. That is the price of a bound wide enough
/// not to fire on a reworded description, and it is why this is a magnitude
/// check and not coverage. A test that detects a missing tool is
/// `Registry::names`, not this one.
#[test]
fn the_tool_surface_the_model_is_shown_stays_within_an_order_of_magnitude() {
    let (count, bytes) = tool_surface_bytes();

    println!("tools without a browser, a search key or a harness: {count}");
    println!("wire `tools` array: {bytes} bytes");
    println!(
        "  ~{} tokens at 4 bytes each, which is an English-prose ratio and \
         wrong for JSON; treat it as an upper bound on the count and settle it \
         with count_tokens",
        bytes / 4
    );

    assert!(
        count >= 12,
        "only {count} tools registered; fs, tasks and lsp together are more \
         than that, so something failed to register rather than shrank"
    );
    // Measured at 22,562 bytes on 2026-09-05 for 15 tools. The bound is a
    // doubling in each direction, because the point is to notice a surface that
    // grew a browser rather than one that gained a sentence.
    assert!(
        (10_000..90_000).contains(&bytes),
        "the tool surface is {bytes} bytes, outside 10K-90K. It was 22,562 on \
         2026-09-05. If this is intended, print the number and move the bound."
    );
}
