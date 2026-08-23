//! `WebSearch` against a loopback stub.
//!
//! A real HTTP server on 127.0.0.1 rather than an injectable transport, the
//! same choice `crates/llm` made and for the same reason: the claims worth
//! proving are about the wire — that the key travels in
//! `X-Subscription-Token` and not a query parameter, that `count` is clamped
//! before it is sent, that a 401 is classified differently from a 500 — and a
//! transport trait would stub out exactly the layer those claims live in.

use std::sync::{Arc, Mutex};

use emma_llm::ApiKey;
use emma_tool_api::{Tool, ToolCtx};
use emma_tools_web::WebSearch;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// region: The loopback stub
// ---------------------------------------------------------------------------
// The loopback stub
//
// A real socket serving canned replies, recording the request line and headers
// so the tests can assert on what actually went out. `connection: close` and
// one reply per connection keep each request its own socket, which is what
// makes `last()` unambiguous.
// ---------------------------------------------------------------------------

struct Seen {
    target: String,
    headers: Vec<(String, String)>,
}

struct Stub {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    fn last(&self) -> (String, Vec<(String, String)>) {
        let seen = self.seen.lock().unwrap();
        let last = seen.last().expect("the stub saw a request");
        (last.target.clone(), last.headers.clone())
    }
}

/// Serve one canned reply per connection, recording the request line and
/// headers. `connection: close` so each request is its own socket.
async fn stub(status: u16, body: String, replies: usize) -> Stub {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = seen.clone();
    tokio::spawn(async move {
        for _ in 0..replies {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut raw: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&raw).to_string();
            let mut lines = head.lines();
            let target = lines
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or_default()
                .to_string();
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                .collect();
            captured.lock().unwrap().push(Seen { target, headers });

            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });
    Stub {
        url: format!("http://{addr}/res/v1/web/search"),
        seen,
    }
}

fn ctx() -> ToolCtx {
    ToolCtx {
        cwd: std::env::temp_dir(),
        session_id: "test-session".into(),
        turn_id: "turn-1".into(),
        background: Default::default(),
    }
}

const TEST_KEY: &str = "BSA-TESTKEYTESTKEYTESTKEY";

fn results_body() -> String {
    json!({
        "web": { "results": [
            { "title": "Async in Rust", "url": "https://rust-lang.org/async",
              "description": "How <strong>async</strong> works &amp; why" },
            { "title": "Tokio", "url": "https://tokio.rs", "description": "A runtime" }
        ] }
    })
    .to_string()
}

// endregion: The loopback stub

// region: What goes out, and what comes back
// ---------------------------------------------------------------------------
// What goes out, and what comes back
//
// The request side — the key in a header and never the URL, the count clamped
// before it is sent — and the response side, where every status has to land in
// the class that routes the caller correctly. The secret-leak assertions are
// negative ones: the key must appear in neither the target nor any error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn results_come_back_as_markdown_and_the_key_travels_in_the_header() {
    let s = stub(200, results_body(), 1).await;
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY)).with_base_url(s.url.clone());

    let outcome = tool
        .invoke(&ctx(), json!({ "query": "rust async", "count": 2 }))
        .await
        .unwrap()
        .expect("a 200 with results is a success");

    assert!(
        outcome.content.contains("Async in Rust"),
        "{}",
        outcome.content
    );
    assert!(
        outcome.content.contains("https://tokio.rs"),
        "{}",
        outcome.content
    );
    // Markup stripped, entities decoded — the snippet is prose, not HTML.
    assert!(
        outcome.content.contains("How async works & why"),
        "{}",
        outcome.content
    );
    assert!(!outcome.truncated);

    let (target, headers) = s.last();
    assert!(
        target.contains("q=rust+async") || target.contains("q=rust%20async"),
        "{target}"
    );
    assert!(target.contains("count=2"), "{target}");
    // A key in the query string ends up in every proxy log between here and
    // Brave. It belongs in the header and nowhere else.
    assert!(
        !target.contains(TEST_KEY),
        "the key leaked into the URL: {target}"
    );
    let token = headers
        .iter()
        .find(|(k, _)| k == "x-subscription-token")
        .expect("no subscription header");
    assert_eq!(token.1, TEST_KEY);
}

#[tokio::test]
async fn an_empty_result_set_is_a_success() {
    // The governing rule, end to end: a query that finds nothing is an answer.
    let s = stub(200, json!({ "web": { "results": [] } }).to_string(), 1).await;
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY)).with_base_url(s.url.clone());

    let outcome = tool
        .invoke(&ctx(), json!({ "query": "asdkjhasdkjh" }))
        .await
        .unwrap()
        .expect("zero results must not be an error");
    assert!(
        outcome.content.contains("No results"),
        "{}",
        outcome.content
    );
}

#[tokio::test]
async fn a_rejected_key_is_unavailable_not_a_failure() {
    let s = stub(401, json!({ "error": "invalid token" }).to_string(), 1).await;
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY)).with_base_url(s.url.clone());

    let err = tool
        .invoke(&ctx(), json!({ "query": "x" }))
        .await
        .unwrap()
        .expect_err("401 must not read as success");
    // Unavailable, not Failed: no retry and no rephrasing fixes a bad key, and
    // the message must say what to do about it.
    assert_eq!(err.kind(), "tool_unavailable", "{err}");
    assert!(err.detail().contains("BRAVE_SEARCH_API_KEY"), "{err}");
    assert!(
        !err.detail().contains(TEST_KEY),
        "the key leaked into the error"
    );
}

#[tokio::test]
async fn a_server_error_is_a_failure_the_model_can_route_around() {
    // The counterpart to the 401 above, and the pair is the point: if both
    // landed in the same class the model would either retry a bad key forever
    // or give up on a Brave outage that would have cleared on the next call.
    let s = stub(500, "upstream exploded".into(), 1).await;
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY)).with_base_url(s.url.clone());

    let err = tool
        .invoke(&ctx(), json!({ "query": "x" }))
        .await
        .unwrap()
        .expect_err("500 must not read as success");
    assert_eq!(err.kind(), "tool_failed", "{err}");
}

#[tokio::test]
async fn count_is_clamped_before_it_reaches_the_api() {
    let s = stub(200, results_body(), 1).await;
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY)).with_base_url(s.url.clone());

    let _ = tool
        .invoke(&ctx(), json!({ "query": "x", "count": 500 }))
        .await
        .unwrap();
    let (target, _) = s.last();
    // Brave silently clamps, which would leave the request and the result
    // disagreeing about how many were asked for.
    assert!(target.contains("count=20"), "{target}");
}

#[tokio::test]
async fn a_search_reaching_nothing_at_all_is_a_failure_not_a_crash() {
    // Nothing is listening on this port: the transport error must arrive as a
    // ToolError the model sees, not as an `Err` that ends the turn.
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY))
        .with_base_url("http://127.0.0.1:1/res/v1/web/search");
    let err = tool
        .invoke(&ctx(), json!({ "query": "x" }))
        .await
        .expect("a dead endpoint must not be a turn-ending fault")
        .expect_err("a dead endpoint is not a result");
    assert_eq!(err.kind(), "tool_failed", "{err}");
}

#[test]
fn websearch_declares_egress_and_names_the_provider_and_the_query() {
    // `read_only` is honest — nothing on this machine changes — and it is
    // exactly why the second axis has to be true: a search is an arbitrary
    // string the model chose, sent to a third party, and it reads as a read.
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY));
    assert!(tool.meta().read_only);
    assert!(tool.meta().reaches_network);

    let t = tool
        .network_target(&json!({ "query": "  tokio select  " }))
        .expect("a query has a destination");
    assert_eq!(t.host, "api.search.brave.com");
    // The query, because the query *is* what leaves. A prompt naming only the
    // provider cannot tell a search for a crate from a search for a secret.
    assert!(t.detail.contains("tokio select"), "{}", t.detail);
}

#[tokio::test]
async fn the_host_gated_is_the_host_contacted() {
    // The target comes from `base_url` rather than a constant, so a run
    // pointed at a stub is approved for the stub. Gating the host it *would*
    // have contacted would mean the grant and the connection disagree — which
    // is the one property a per-host grant cannot afford to lose.
    let s = stub(200, results_body(), 1).await;
    let tool = WebSearch::with_key(ApiKey::new(TEST_KEY)).with_base_url(s.url.clone());
    let t = tool
        .network_target(&json!({ "query": "x" }))
        .expect("a query has a destination");
    assert_eq!(t.host, "127.0.0.1");
}

// endregion: What goes out, and what comes back
