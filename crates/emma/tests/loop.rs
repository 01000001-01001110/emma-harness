//! The loop's properties, driven by a scripted model.
//!
//! Every test here corresponds to something that would otherwise only be true
//! by inspection. They were each validated by mutation — the guarantee was
//! removed from `agent.rs`, the test was confirmed to fail, and the guarantee
//! was restored. A test that cannot fail proves nothing, and a mutation that
//! silently did not apply proves less than nothing.

mod support;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Setup};
use emma::approval::{Answer, Approvals, Asker, Gate};
use emma::goal::{Goal, MarkerClaim};
use emma::session::SessionLog;
use emma::term::Term;
use emma_harness::{Flavor, Harness};
use emma_llm::{Caching, Mode};
use emma_tool_api::Registry;
use serde_json::json;

use support::{call, text, empty_harness, harness_denying, registry, Fake, TestTool};

fn budgets() -> Budgets {
    Budgets {
        max_iterations: 20,
        max_tokens: 1_000_000,
        wall_clock: Duration::from_secs(60),
        max_kicks: 3,
    }
}

/// Eight arguments because `Setup` has eight things a test may want to vary,
/// and a builder here would be a second way to construct the thing under test.
#[allow(clippy::too_many_arguments)]
async fn drive(
    root: &Path,
    cwd: &Path,
    tools: Registry,
    approvals: &Approvals,
    provider: &Fake,
    budgets: Budgets,
    goal: &Goal,
    log: &SessionLog,
) -> Outcome {
    // `load_selecting` rather than `load`: the latter reads `EMMA_PERSONA` from
    // the process environment, and a test whose result depends on the
    // developer's shell is a test that passes for the wrong reason.
    let harness = Harness::load_selecting(root, Flavor::Emma, None).unwrap();
    let term = Term::silent();
    let mut agent = Agent::new(Setup {
        provider,
        harness: &harness,
        tools: &tools,
        approvals,
        log,
        term: &term,
        interrupt: Interrupt::new(),
        done: &MarkerClaim,
        cwd: cwd.to_path_buf(),
        session_id: "sess-test".into(),
        budgets,
        caching: Caching::On,
        mode: Mode::Batch,
    });
    agent.run_goal(goal).await
}

fn goal() -> Goal {
    Goal::new("make it work")
}

// ---------------------------------------------------------------------------
// Failures are observations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tool_failure_reaches_the_model_and_the_goal_continues() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (boom, boom_calls) = TestTool::failing("Boom", true);
    let fake = Fake::new(vec![
        call("Boom", json!({ "x": "1" })),
        text("I will take the other route.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "s").unwrap();

    let out = drive(
        &root,
        dir.path(),
        registry(vec![boom]),
        &Approvals::new(Gate::Ask, Asker::Scripted(Default::default())),
        &fake,
        budgets(),
        &goal(),
        &log,
    )
    .await;

    // The failure did not end the goal…
    assert_eq!(out.ending, Ending::Done);
    assert_eq!(boom_calls.load(Ordering::SeqCst), 1);
    // …and the model was told, in a block it can route on.
    let seen = fake.transcript();
    assert!(seen.contains("tool_failed"), "{seen}");
    assert!(seen.contains("Boom broke on purpose"), "{seen}");
    assert!(seen.contains("is_error"), "{seen}");
}

#[tokio::test]
async fn a_failed_call_is_not_repeated_with_the_same_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (boom, boom_calls) = TestTool::failing("Boom", true);
    let fake = Fake::new(vec![
        call("Boom", json!({ "x": "same" })),
        call("Boom", json!({ "x": "same" })),
        text("fine.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::none();

    let out = drive(
        &root,
        dir.path(),
        registry(vec![boom]),
        &Approvals::new(Gate::Ask, Asker::Scripted(Default::default())),
        &fake,
        budgets(),
        &goal(),
        &log,
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        boom_calls.load(Ordering::SeqCst),
        1,
        "the identical retry reached the tool"
    );
    assert!(fake.transcript().contains("already_failed"));
}

/// The other half of the same rule, and the reason it is not "never again this
/// turn". Run the tests, see them fail, fix a file, run them again — the second
/// run is the one that proves the fix, and a memo keyed on the call alone would
/// forbid it.
#[tokio::test]
async fn a_failed_call_may_be_repeated_once_something_else_has_succeeded() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (boom, boom_calls) = TestTool::failing("Boom", true);
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Boom", json!({ "x": "same" })),
        call("Fine", json!({})),
        call("Boom", json!({ "x": "same" })),
        text("ok\n\nGOAL COMPLETE"),
    ]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![boom, fine]),
        &Approvals::new(Gate::Ask, Asker::Scripted(Default::default())),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(boom_calls.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_iteration_budget_stops_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Fine", json!({})),
        call("Fine", json!({})),
        call("Fine", json!({})),
        text("GOAL COMPLETE"),
    ]);
    let mut b = budgets();
    b.max_iterations = 2;

    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &Approvals::new(Gate::Ask, Asker::Scripted(Default::default())),
        &fake,
        b,
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Iterations);
    assert_eq!(out.iterations, 2);
    assert_eq!(fake.calls(), 2, "the model was called past the budget");
}

#[tokio::test]
async fn the_token_budget_stops_the_loop_and_the_run_is_charged_for_what_it_spent() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Fine", json!({})).costing(10),
        call("Fine", json!({})).costing(10),
        text("GOAL COMPLETE"),
    ]);
    let mut b = budgets();
    b.max_tokens = 15;
    let log = SessionLog::open(dir.path(), "tokens").unwrap();

    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &Approvals::new(Gate::Ask, Asker::Scripted(Default::default())),
        &fake,
        b,
        &goal(),
        &log,
    )
    .await;

    assert_eq!(out.ending, Ending::Tokens);
    assert_eq!(out.tokens, 20);

    // The scar this is guarding: in tustle-agent a token count only reached the
    // log on a *completed* turn, so every abort was recorded at zero and
    // aborting became the cheapest way to spend money.
    let records = SessionLog::read(log.path()).unwrap();
    let spent: i64 = records
        .iter()
        .filter(|r| r["kind"] == "model_call")
        .map(|r| r["billable_total_tokens"].as_i64().unwrap_or(0))
        .sum();
    assert_eq!(spent, 20, "the aborted run was recorded as free");
    let finished = records.iter().find(|r| r["kind"] == "goal_finished").unwrap();
    assert_eq!(finished["ending"], "tokens");
    assert_eq!(finished["tokens"], 20);
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_gate_denies_a_writer_with_nobody_to_ask_and_the_model_is_told_why() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (writer, writer_calls) = TestTool::ok("Writer", false);
    let fake = Fake::new(vec![
        call("Writer", json!({ "x": "1" })),
        text("understood.\n\nGOAL COMPLETE"),
    ]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![writer]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        writer_calls.load(Ordering::SeqCst),
        0,
        "a tool that needed approval ran without it"
    );
    let seen = fake.transcript();
    assert!(seen.contains("not_approved"), "{seen}");
    assert!(seen.contains("-p"), "the model was not told why: {seen}");
}

#[tokio::test]
async fn a_read_only_tool_is_never_gated() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (reader, reader_calls) = TestTool::ok("Reader", true);
    let fake = Fake::new(vec![call("Reader", json!({})), text("GOAL COMPLETE")]);

    // Unattended denies anything needing approval — and a read must not need it,
    // or `-p` becomes useless and the interactive gate becomes unusable.
    let out = drive(
        &root,
        dir.path(),
        registry(vec![reader]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(reader_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_human_yes_lets_a_writer_through() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (writer, writer_calls) = TestTool::ok("Writer", false);
    let fake = Fake::new(vec![call("Writer", json!({})), text("GOAL COMPLETE")]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![writer]),
        &Approvals::new(Gate::Ask, Asker::Scripted(vec![Answer::Yes].into())),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(writer_calls.load(Ordering::SeqCst), 1);
}

/// The safety inversion, stated as a test: a hook is policy and the person at
/// the keyboard is convenience. Here the human has said yes to everything — the
/// gate is entirely off — and the call is still refused.
#[tokio::test]
async fn a_hook_denial_overrides_every_approval() {
    let dir = tempfile::tempdir().unwrap();
    let root = harness_denying(dir.path(), "Writer");
    let (writer, writer_calls) = TestTool::ok("Writer", false);
    let fake = Fake::new(vec![
        call("Writer", json!({ "x": "1" })),
        text("understood.\n\nGOAL COMPLETE"),
    ]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![writer]),
        // The loudest possible approval. It must not matter.
        &Approvals::new(Gate::SkipAll, Asker::Scripted(Default::default())),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        writer_calls.load(Ordering::SeqCst),
        0,
        "a PreToolUse denial was approved away"
    );
    let seen = fake.transcript();
    assert!(seen.contains("blocked_by_policy"), "{seen}");
    assert!(
        seen.contains("cannot be approved away"),
        "the model was not told the denial is not negotiable: {seen}"
    );
}

/// …and the same hook must not block a tool it does not match, or the test
/// above would pass for a harness that denies everything.
#[tokio::test]
async fn the_hook_matcher_is_what_decides_which_calls_are_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let root = harness_denying(dir.path(), "Writer");
    let (other, other_calls) = TestTool::ok("Reader", true);
    let fake = Fake::new(vec![call("Reader", json!({})), text("GOAL COMPLETE")]);

    drive(
        &root,
        dir.path(),
        registry(vec![other]),
        &Approvals::new(Gate::SkipAll, Asker::Scripted(Default::default())),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(other_calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// The goal, and the kick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_kick_fires_when_the_model_stops_without_claiming_completion() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let fake = Fake::new(vec![
        text("Here is what I found. The middleware calls the legacy helper."),
        text("Ported it and the tests pass.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "kick").unwrap();

    let out = drive(
        &root,
        dir.path(),
        registry(vec![]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &goal(),
        &log,
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(out.kicks, 1);
    let seen = fake.transcript();
    assert!(seen.contains("not recorded as done"), "{seen}");
    assert!(
        seen.contains("make it work"),
        "the kick did not restate the goal: {seen}"
    );
    let records = SessionLog::read(log.path()).unwrap();
    assert!(records.iter().any(|r| r["kind"] == "kick"));
}

#[tokio::test]
async fn the_kick_is_bounded_by_its_budget() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    // Tool use between the stops, so the stall rule is not what stops this —
    // the count is.
    let fake = Fake::new(vec![
        text("thinking about it"),
        call("Fine", json!({})),
        text("still thinking"),
        call("Fine", json!({})),
        text("nearly there"),
        text("this one would be the fourth kick"),
    ]);
    let mut b = budgets();
    b.max_kicks = 2;

    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &Approvals::unattended(),
        &fake,
        b,
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::KicksExhausted);
    assert_eq!(out.kicks, 2);
    // The bound is real: the sixth scripted turn was never asked for.
    assert_eq!(fake.calls(), 5);
}

/// The second bound, and the one that catches a model arguing with the loop
/// without spending the kick budget on it.
#[tokio::test]
async fn a_model_that_stops_twice_without_touching_a_tool_is_believed() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let fake = Fake::new(vec![
        text("I think this is already done."),
        text("As I said, there is nothing to change."),
    ]);
    let b = budgets(); // max_kicks is 3; the stall rule fires first.

    let out = drive(
        &root,
        dir.path(),
        registry(vec![]),
        &Approvals::unattended(),
        &fake,
        b,
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Stalled);
    assert_eq!(out.kicks, 1);
    assert_eq!(fake.calls(), 2);
}

// ---------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_assistant_turn_is_echoed_back_exactly_as_it_arrived() {
    // Thinking-block signatures do not survive reassembly, so the content array
    // that comes out of the provider is the content array that goes back in.
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Fine", json!({ "x": "marker-value" })),
        text("GOAL COMPLETE"),
    ]);

    drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    let last = fake.last_query();
    assert!(last.contains("\"type\":\"tool_use\""), "{last}");
    assert!(last.contains("marker-value"), "{last}");
}
