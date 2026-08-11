//! The tests for the failure this crate exists to prevent.
//!
//! A language server answers `references` before it has indexed anything, and
//! the answer is `[]`. "No references" and "not indexed yet" are the same shape,
//! and a model told the first will delete the function. Every test here is one
//! way that could go wrong, driven against a fake server that can be made to
//! index, to never finish indexing, or to say nothing at all — states a real
//! rust-analyzer produces on its own schedule and never on request.
//!
//! The readiness ceiling is overridden to three seconds, because the real one is
//! three minutes. See [`short_ceiling`] for why it is one value for the file
//! rather than one per test.

mod support;

use std::time::{Duration, Instant};

use emma_tools_lsp::client::{Readiness, READY_TIMEOUT_ENV};
use serde_json::json;
use support::{Fake, Indexing, Sandbox};

/// One ceiling for the whole file, set once.
///
/// Not a per-test value: the override is an environment variable, environment
/// variables are process-global, and the tests in one integration file run in
/// parallel threads of one process — so two tests disagreeing about it is a race
/// that fails whichever one loses. Three seconds is long enough for the 1.5s
/// settle window and short enough that a test which is supposed to give up
/// does so promptly.
fn short_ceiling() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var(READY_TIMEOUT_ENV, "3000"));
}

/// The happy path, and the baseline the rest are measured against: when the
/// server says it has finished, Emma says so, and an empty answer is allowed to
/// mean "there are none".
///
/// The fake emits the real server's shape, so this also pins that the three
/// moments where no progress token is open are *not* mistaken for the end.
#[tokio::test]
async fn a_server_that_finishes_indexing_is_ready() {
    short_ceiling();
    let sandbox = Sandbox::new();
    let client = Fake::new(Indexing::Finishes).start(sandbox.root()).await;
    assert_eq!(client.wait_ready().await, Readiness::Ready);
    assert!(Readiness::Ready.caveat().is_none());
}

/// The fallback, for a server with no `experimental/serverStatus`: progress
/// tokens only, with the same misleading gaps. Readiness must come from the
/// settle window and not from the first moment the count hits zero.
///
/// It also has to actually arrive. A settle that never fires would be safe and
/// useless — every answer permanently caveated — so both halves are asserted.
#[tokio::test]
async fn progress_alone_settles_rather_than_believing_the_first_gap() {
    short_ceiling();
    let sandbox = Sandbox::new();
    let client = Fake::new(Indexing::GapsThenSettles)
        .start(sandbox.root())
        .await;

    let started = Instant::now();
    assert_eq!(client.wait_ready().await, Readiness::Ready);
    // It cannot have believed the first gap: the settle window is 1.5s and the
    // gaps are microseconds apart in the fake.
    assert!(
        started.elapsed() >= Duration::from_millis(1_400),
        "readiness arrived in {:?} — the settle window was skipped",
        started.elapsed()
    );
}

/// A server that has finished everything and cannot load the project is
/// perfectly `Ready` and answers nothing — the one case readiness alone gets
/// exactly backwards. The health note is the only thing standing between that
/// and an empty answer read as fact.
#[tokio::test]
async fn a_quiescent_but_broken_server_carries_its_own_bad_news() {
    short_ceiling();
    let sandbox = Sandbox::new();
    let client = Fake::new(Indexing::Unhealthy)
        .answers("textDocument/references", json!([]))
        .start(sandbox.root())
        .await;

    let answer = client
        .request("textDocument/references", json!({}))
        .await
        .expect("it answers, emptily");
    assert_eq!(answer.readiness, Readiness::Ready, "it really has finished");
    let health = answer.health.expect("a broken server owes a note");
    assert!(health.contains("cargo metadata failed"), "{health}");
    assert!(health.contains("missing or wrong"), "{health}");
}

/// The dangerous one. The server is still working, the answer comes back empty,
/// and Emma must not let that be read as "there are none".
#[tokio::test]
async fn an_answer_from_a_still_indexing_server_is_marked_as_one() {
    short_ceiling();
    let sandbox = Sandbox::new();
    let client = Fake::new(Indexing::NeverFinishes)
        .answers("textDocument/references", json!([]))
        .start(sandbox.root())
        .await;

    let started = Instant::now();
    let answer = client
        .request("textDocument/references", json!({}))
        .await
        .expect("the request still runs");
    assert_eq!(answer.readiness, Readiness::Indexing);
    assert_eq!(answer.value, json!([]));

    // It waited, rather than answering instantly from a half-built index — and
    // then gave up rather than hanging, which is the pair of properties that
    // makes the ceiling worth having.
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "it did not wait"
    );
    assert!(started.elapsed() < Duration::from_secs(30), "it hung");
}

/// A server that never reports progress is `Unknown`, not `Ready`.
///
/// This is the subtlety that makes a naive implementation wrong: "no indexing is
/// currently in progress" is true both of a finished index and of one that has
/// not been announced yet. Guessing `Ready` here is exactly the confidently
/// wrong empty result, and it would be the *common* case, since a request in the
/// first fifty milliseconds of a session beats the first `begin` notification.
#[tokio::test]
async fn a_server_that_never_reports_progress_is_unknown_and_never_ready() {
    let sandbox = Sandbox::new();
    let client = Fake::new(Indexing::Silent).start(sandbox.root()).await;
    let readiness = client.wait_ready().await;
    assert_eq!(readiness, Readiness::Unknown);
    assert!(
        readiness.caveat().is_some(),
        "Unknown owes the model a sentence"
    );
    assert!(!readiness.is_ready());

    // And the second call does not pay the grace period again: burning ten
    // seconds per tool call for the rest of the session would make the crate
    // unusable against any server that does not speak progress.
    let started = Instant::now();
    assert_eq!(client.wait_ready().await, Readiness::Unknown);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the grace was paid twice"
    );
}

/// Readiness is not asked once at startup and cached — it is asked per request,
/// which is what lets an answer say what state *it* was produced in. A request
/// issued while indexing and one issued after must not report the same thing.
#[tokio::test]
async fn readiness_travels_with_the_answer_rather_than_the_session() {
    short_ceiling();
    let sandbox = Sandbox::new();
    let client = Fake::new(Indexing::NeverFinishes)
        .answers("textDocument/hover", json!({ "contents": "x" }))
        .start(sandbox.root())
        .await;
    let first = client
        .request("textDocument/hover", json!({}))
        .await
        .unwrap();
    assert_eq!(first.readiness, Readiness::Indexing);
    assert!(first.readiness.caveat().is_some());
}
