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
        background: Default::default(),
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
    assert!(outcome.truncation.is_none());
    assert!(outcome.display.is_some());
}

/// The page the owner hit, and the message they got: `… output was truncated
/// by the tool` on a result whose prose was 4100 characters against an 8000
/// cap. Nothing about the text was cut — the link list was — and neither the
/// human nor the model was told which, how much, or what to pass instead.
///
/// Live, and `#[ignore]`d for the same reason as the test above: the
/// guarantee is proven offline in `digest_md`'s unit tests, and this is the
/// certification that a real hub page still exercises it. Run with
/// `cargo test -p emma-tools-web --test fetch -- --ignored`.
#[tokio::test]
#[ignore = "reaches the live network and launches Chrome; run with --ignored"]
async fn a_link_heavy_hub_page_names_the_cap_that_cut_it() {
    let outcome = WebFetch::new()
        .invoke(
            &ctx(),
            json!({ "url": "https://apnews.com/hub/artificial-intelligence" }),
        )
        .await
        .expect("no turn-ending fault")
        .expect("AP News is a result");

    let reason = outcome
        .truncation
        .as_deref()
        .expect("a hub page this size must report which cap bound");
    assert!(reason.contains("links"), "{reason}");
    assert!(reason.contains("max_links"), "{reason}");
    // The specific wrong turn this fix exists to prevent: sending a reader to
    // `max_chars` for a page whose text was never near its limit.
    assert!(
        !reason.contains("max_chars"),
        "pointed at the cap that did not bind: {reason}"
    );
    // And the model's copy carries the same sentence, not a summary of it.
    assert!(outcome.content.contains("max_links"), "{}", outcome.content);
}

/// The complaint this branch answers, certified end to end: a long page is
/// readable past its first window, and the *only* thing the reader needs in
/// order to keep reading is the sentence the tool already handed it.
///
/// Run with `cargo test -p emma-tools-web --test fetch -- --ignored`.
#[tokio::test]
#[ignore = "reaches the live network and launches Chrome twice; run with --ignored"]
async fn a_long_page_can_be_read_past_its_first_window() {
    const URL: &str = "https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html";
    const WINDOW: u64 = 2_000;

    let first = WebFetch::new()
        .invoke(&ctx(), json!({ "url": URL, "max_chars": WINDOW }))
        .await
        .expect("no turn-ending fault")
        .expect("the Rust book is a result");

    let reason = first
        .truncation
        .as_deref()
        .expect("a book chapter must not fit in 2000 characters")
        .to_string();
    assert!(reason.contains("continue with offset="), "{reason}");

    let next: u64 = reason
        .split("continue with offset=")
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .unwrap_or_else(|| panic!("no offset to continue from: {reason}"));
    assert_eq!(next, WINDOW, "the next window is not where this one ended");

    let second = WebFetch::new()
        .invoke(
            &ctx(),
            json!({ "url": URL, "max_chars": WINDOW, "offset": next }),
        )
        .await
        .expect("no turn-ending fault")
        .expect("the continuation is a result");

    let head = content_body(&first.content);
    let tail = content_body(&second.content);
    assert!(
        !tail.is_empty(),
        "the second window was empty: {}",
        second.content
    );
    assert_ne!(
        head, tail,
        "the continuation returned the first window again"
    );
}

fn content_body(markdown: &str) -> String {
    markdown
        .split("## Content")
        .nth(1)
        .unwrap_or("")
        .split("\n[truncated:")
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[test]
fn webfetch_declares_itself_read_only_and_writes_nothing_here() {
    // `emma::approval` decides whether to interrupt a human from these two
    // fields, so they have to be true rather than merely declared. What makes
    // `read_only` true: WebFetch drives a browser but has no path that writes
    // inside the working directory — the Chrome profile lives in the system
    // temp directory and is removed on teardown. What makes `reaches_network`
    // true is the entire tool.
    //
    // The second one is the load-bearing claim now. Flip it to false and every
    // page this tool reads happens without anybody being asked, because
    // `read_only: true` is the arm the gate reaches next.
    let meta = WebFetch::new().meta();
    assert!(meta.read_only);
    assert!(meta.reaches_network);
    assert!(meta.idempotent);
}

#[test]
fn webfetch_names_the_host_the_gate_will_grant_against() {
    // The gate asks the tool because it must not learn that this tool keeps
    // its destination in an argument called `url`. This is the answering half,
    // and it is the only place the URL is parsed for that purpose.
    let t = WebFetch::new()
        .network_target(&json!({ "url": " https://Docs.RS/tokio/latest " }))
        .expect("a well-formed URL has a host");
    // Lowercased and trimmed, because the grant is a `HashSet` key: two
    // spellings of one host would prompt twice for a host already approved.
    assert_eq!(t.host, "docs.rs");
    // …and the human is shown the errand, not just the destination.
    assert!(
        t.detail.contains("https://Docs.RS/tokio/latest"),
        "{}",
        t.detail
    );
}

#[test]
fn a_url_with_no_host_names_no_target_and_is_therefore_refused() {
    // `None` is fail-closed at the gate: a tool that declares egress and names
    // nothing is denied. These are the same URLs chromehand refuses anyway —
    // the wrong outcome would be for them to be the ones that slip past unasked.
    for bad in ["not a url", "file:///etc/passwd", ""] {
        assert!(
            WebFetch::new()
                .network_target(&json!({ "url": bad }))
                .is_none(),
            "{bad} named a host"
        );
    }
}
