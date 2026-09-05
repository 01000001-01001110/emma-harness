//! `WebSearch` against the real engine, in a real Chrome.
//!
//! `#[ignore]`d, like `tests/fetch.rs`'s live case and for the same reason: the
//! suite stays offline and deterministic, and the certification stays one
//! command away. Everything decidable without a browser is in `search.rs`'s
//! own tests.
//!
//! This is the test that settles which engine the constant names. It prints
//! what came back, because the assertion it makes is deliberately weak: the
//! call returned, and either produced hits or said in words that the engine
//! blocked it. Which of those happened on a given day and machine is the
//! observation, and it goes on `docs/tools-web.html` with the date.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p emma-tools-web --test search -- --ignored --nocapture
//! ```

use emma_tool_api::{Tool, ToolCtx};
use emma_tools_web::WebSearch;
use serde_json::json;

fn ctx() -> ToolCtx {
    ToolCtx {
        cwd: std::env::temp_dir(),
        session_id: "test-session".into(),
        turn_id: "turn-1".into(),
        background: Default::default(),
    }
}

#[tokio::test]
#[ignore = "needs the live network and a real Chrome"]
async fn a_real_search_returns_places_to_look_or_says_it_was_blocked() {
    let out = WebSearch::new()
        .invoke(
            &ctx(),
            json!({ "query": "ratatui alternate screen rust", "count": 5 }),
        )
        .await
        .expect("a search must not end the turn")
        .expect("the engine could not be reached or refused the URL");

    println!(
        "--- display ---\n{}\n--- content ---\n{}",
        out.display.as_deref().unwrap_or("(none)"),
        out.content
    );

    let blocked = out.content.contains("challenge page");
    let numbered = out.content.contains("1. **");
    assert!(
        blocked || numbered,
        "neither results nor a named block came back:\n{}",
        out.content
    );
    if numbered {
        // A result leaves the engine: no bing.com link may be in the list.
        assert!(
            !out.content.to_ascii_lowercase().contains("bing.com/"),
            "the engine's own pages were returned as results:\n{}",
            out.content
        );
        // **Relevance, not just liveness.** The first version of this test
        // accepted any results page, and Bing served three runs of unrelated
        // results to a headless User-Agent with HTTP 200 and no challenge. A
        // results page that returned is not evidence the search worked; a
        // result about the thing asked for is.
        assert!(
            out.content.to_ascii_lowercase().contains("ratatui"),
            "results came back and none of them is about the query:\n{}",
            out.content
        );
    }
}
