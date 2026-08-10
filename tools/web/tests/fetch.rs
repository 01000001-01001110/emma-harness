//! `WebFetch`, through the `Tool` trait, on the paths that never need a page.
//!
//! Every case here is decided by policy before Chrome is launched, which is
//! what makes them fast and deterministic — and they are the cases that matter
//! most, because they are where a wrong answer is *plausible*: a refusal that
//! reads as a browser failure tells the model the web is broken when the truth
//! is that it asked for something it may not have.
//!
//! The rendering path itself is covered against a local fixture server by
//! `tests/integration.rs` — the same code, one layer down. The single test
//! here that touches the real internet is `#[ignore]`d, so the suite stays
//! offline and deterministic while the certification stays one command away.

use emma_tool_api::{Tool, ToolCtx};
use emma_tools_web::WebFetch;
use serde_json::json;

fn ctx() -> ToolCtx {
    ToolCtx {
        cwd: std::env::temp_dir(),
        session_id: "test-session".into(),
        turn_id: "turn-1".into(),
    }
}

async fn err(args: serde_json::Value) -> emma_tool_api::ToolError {
    WebFetch::new()
        .invoke(&ctx(), args)
        .await
        .expect("a refusal must not end the turn")
        .expect_err("this URL must not be fetchable")
}

#[tokio::test]
async fn a_refused_scheme_is_the_arguments_not_the_browser() {
    // `bad_arguments` and not `tool_failed`: the model can fix a URL, and
    // telling it the browser broke sends it to retry the exact same call.
    let e = err(json!({ "url": "file:///etc/passwd" })).await;
    assert_eq!(e.kind(), "bad_arguments", "{e}");
    assert!(e.detail().contains("scheme"), "{e}");
}

#[tokio::test]
async fn loopback_is_refused() {
    // The agent's own machine is not the web. Reaching it would turn a page
    // read into a way to probe services bound to localhost.
    for url in [
        "http://localhost:8080/admin",
        "http://127.0.0.1:9/x",
        "http://[::1]:9/x",
    ] {
        let e = err(json!({ "url": url })).await;
        assert_eq!(e.kind(), "bad_arguments", "{url}: {e}");
    }
}

#[tokio::test]
async fn a_url_that_is_not_a_url_is_an_argument_error() {
    // Unparseable input must never reach the launcher. If it did, the model
    // would be told Chrome failed for a string that was never a URL.
    let e = err(json!({ "url": "not a url at all" })).await;
    assert_eq!(e.kind(), "bad_arguments", "{e}");
}

#[tokio::test]
async fn an_unknown_parameter_is_refused_rather_than_ignored() {
    // Delete this and a misspelled parameter is silently dropped, which the
    // model reads as "that option had no effect" and concludes the behaviour
    // is impossible rather than that it typed the key wrong.
    let e = err(json!({ "url": "https://example.com", "depth": 3 })).await;
    assert_eq!(e.kind(), "bad_arguments", "{e}");
    assert!(e.detail().contains("depth"), "{e}");
}

/// The one test that reaches the real internet, and the reason it is
/// `#[ignore]`d rather than absent: everything above proves the *mapping*, and
/// a mapping can be perfect while the thing it maps has never once rendered a
/// page. Run it by hand — `cargo test -p emma-tools-web --test fetch --
/// --ignored` — to certify a machine, an upgrade, or a Chrome that has just
/// changed under you.
#[tokio::test]
#[ignore = "reaches the live network and launches Chrome; run with --ignored"]
async fn a_real_page_renders_as_markdown() {
    let outcome = WebFetch::new()
        .invoke(&ctx(), json!({ "url": "https://example.com" }))
        .await
        .expect("no turn-ending fault")
        .expect("example.com is a result");

    assert!(
        outcome.content.starts_with("# Example Domain"),
        "{}",
        outcome.content
    );
    assert!(
        outcome.content.contains("HTTP 200, verified"),
        "{}",
        outcome.content
    );
    assert!(
        outcome.content.contains("## Content"),
        "{}",
        outcome.content
    );
    assert!(
        outcome.content.contains("illustrative examples")
            || outcome.content.contains("documentation examples"),
        "{}",
        outcome.content
    );
    // Markdown, not JSON: the model must never be handed the raw digest.
    assert!(
        !outcome.content.contains("\"digest\""),
        "{}",
        outcome.content
    );
    assert!(!outcome.truncated);
    assert!(outcome.display.is_some());
}

#[test]
fn webfetch_declares_itself_read_only_and_writes_nothing_here() {
    // `emma::approval` decides whether to interrupt a human purely from this
    // field, so it has to be true rather than merely declared — and no gate
    // has ever consulted it for this tool, because WebFetch is not registered.
    // The claim is checked here so that flipping the declaration is a
    // deliberate act with a test to answer to, rather than something noticed
    // the first time a page read runs unattended. What makes it true: WebFetch
    // reaches the network and drives a browser but has no path that writes
    // inside the working directory — the Chrome profile lives in the system
    // temp directory and is removed on teardown.
    let meta = WebFetch::new().meta();
    assert!(meta.read_only);
    assert!(meta.idempotent);
}
