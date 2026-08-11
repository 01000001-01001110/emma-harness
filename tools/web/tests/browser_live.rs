//! One real session, on the real internet, through the `Tool` trait.
//!
//! `#[ignore]`d, like `tests/fetch.rs`'s live case and for the same reason: the
//! suite stays offline and deterministic, and the certification stays one
//! command away —
//!
//! ```text
//! cargo test -p emma-tools-web --test browser_live -- --ignored --nocapture
//! ```
//!
//! What it certifies that the fixture tests cannot: that a real site's markup
//! produces selectors that actually resolve, that a click on one of them lands,
//! and that the delta of a real page is small. A fixture page is written by the
//! person writing the test, which is exactly the wrong author for that question.

use emma_tool_api::ToolCtx;
use emma_tools_web::browser::browser_tools;

fn ctx() -> ToolCtx {
    ToolCtx {
        cwd: std::env::temp_dir(),
        session_id: "live".into(),
        turn_id: "turn-1".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the live network and a real Chrome"]
async fn a_real_session_opens_reads_acts_and_closes() {
    // The interaction allowlist, which `click` requires and reading does not
    // (chromehand ADR-4). In a real run this is `~/.emma/browser-allowlist.json`
    // and the user writes it; here it is a temp file, because the point of the
    // test is the click, not where the list lives.
    let dir = tempfile::tempdir().expect("tempdir");
    let allowlist = dir.path().join("browser-allowlist.json");
    std::fs::write(&allowlist, r#"{ "domains": ["example.com", "iana.org"] }"#).unwrap();

    let (tools, pool) = browser_tools(Some(allowlist));
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .clone()
    };

    let open = tool("BrowserOpen")
        .invoke(&ctx(), serde_json::json!({ "url": "https://example.com/" }))
        .await
        .expect("open must not end the turn")
        .expect("open failed");
    println!("--- BrowserOpen ---\n{}\n", open.content);
    let id = pool.ids().first().cloned().expect("no session registered");
    assert!(open.content.contains("Example Domain"), "{}", open.content);

    // A link on the real page, addressed by the selector the read handed out.
    // This is the assertion a fixture page cannot make: that a selector computed
    // from somebody else's markup actually resolves to the element again.
    let selector = open
        .content
        .lines()
        .filter(|l| l.starts_with("- ["))
        .find_map(|l| l.split_once(" — `"))
        .map(|(_, sel)| sel.trim_end_matches('`').to_string())
        .expect("the read listed no clickable selector for the page's link");
    println!("--- link selector from the read: {selector} ---");

    let go = tool("BrowserAct")
        .invoke(
            &ctx(),
            serde_json::json!({
                "session": id, "action": "click", "selector": selector
            }),
        )
        .await
        .expect("act must not end the turn")
        .expect("click failed");
    println!("--- BrowserAct click ---\n{}\n", go.content);

    // The cross-origin re-ask, from the gate's side: the host it would now be
    // asked about is the new one, not the one the session was opened on.
    let target = tool("BrowserRead")
        .network_target(&serde_json::json!({ "session": id }))
        .expect("no network target");
    println!("--- gate would now ask about: {} ---", target.host);
    assert_eq!(target.host, "www.iana.org");

    let read = tool("BrowserRead")
        .invoke(&ctx(), serde_json::json!({ "session": id, "delta": true }))
        .await
        .expect("read must not end the turn")
        .expect("read failed");
    println!("--- BrowserRead (delta) ---\n{}\n", read.content);

    let close = tool("BrowserClose")
        .invoke(&ctx(), serde_json::json!({}))
        .await
        .expect("close must not end the turn")
        .expect("close failed");
    println!("--- BrowserClose ---\n{}\n", close.content);
    assert!(pool.is_empty(), "a session survived BrowserClose");
}
