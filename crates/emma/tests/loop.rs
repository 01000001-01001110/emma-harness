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
use std::sync::Arc;
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

use support::{call, cut_off, empty_harness, harness_denying, registry, text, Fake, TestTool};

fn budgets() -> Budgets {
    Budgets {
        max_iterations: 20,
        max_tokens: 1_000_000,
        wall_clock: Duration::from_secs(60),
        max_kicks: 3,
        // Effectively off: the tests that are about compaction set it, and a
        // test that is not must not have its conversation rewritten underneath
        // the thing it is asserting on.
        max_context: 1_000_000,
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
    provider: &Arc<Fake>,
    budgets: Budgets,
    goal: &Goal,
    log: &SessionLog,
) -> Outcome {
    let (mut out, _) = drive_goals(
        root,
        cwd,
        tools,
        approvals,
        provider,
        budgets,
        std::slice::from_ref(goal),
        log,
    )
    .await;
    out.pop().unwrap()
}

/// The same thing for more than one goal on one `Agent`, which is the only way
/// to exercise the conversation it carries between them. Returns the outcomes
/// and the conversation the agent is left holding.
#[allow(clippy::too_many_arguments)]
async fn drive_goals(
    root: &Path,
    cwd: &Path,
    tools: Registry,
    approvals: &Approvals,
    provider: &Arc<Fake>,
    budgets: Budgets,
    goals: &[Goal],
    log: &SessionLog,
) -> (Vec<Outcome>, Vec<emma_llm::Message>) {
    // `load_selecting` rather than `load`: the latter reads `EMMA_PERSONA` from
    // the process environment, and a test whose result depends on the
    // developer's shell is a test that passes for the wrong reason.
    let harness = Harness::load_selecting(root, Flavor::Emma, None).unwrap();
    let term = Term::silent();
    let mut agent = Agent::new(Setup {
        provider: provider.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals,
        log,
        term: &term,
        interrupt: Interrupt::new(),
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: cwd.to_path_buf(),
        session_id: "sess-test".into(),
        budgets,
        caching: Caching::On,
        mode: Mode::Batch,
    });
    let mut out = Vec::new();
    for goal in goals {
        out.push(agent.run_goal(goal).await);
    }
    (out, agent.conversation())
}

fn goal() -> Goal {
    Goal::new("make it work")
}

// region: Failures are observations
// ---------------------------------------------------------------------------
// Failures are observations
//
// The loop's first property: no failure class ends a goal. These three cover
// the failure reaching the model at all, and both halves of the memo rule.
// ---------------------------------------------------------------------------

/// The property the whole crate is arranged around. Delete this and a
/// regression that turns a `ToolError` back into an early return is invisible:
/// the run still ends, the user still gets an ending, and the only symptom is a
/// model that never learns its call failed and a goal abandoned mid-work.
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

/// A tool that panics is an observation too, not the end of the session.
///
/// This file's own first rule is that every failure class reaches the model as
/// a `tool_result` and the turn continues. A panic was the one class that did
/// not: it unwound through the loop, past the session log and the budget
/// accounting, and took the run with it. The tool surface is large, several of
/// its crates parse input written by the model, and one `unwrap` in any of them
/// ended everything.
#[tokio::test]
async fn a_tool_that_panics_is_an_observation_and_the_goal_continues() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (boom, boom_calls) = TestTool::panicking("Boom");
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

    // The panic did not end the goal…
    assert_eq!(out.ending, Ending::Done, "a tool panic ended the session");
    assert_eq!(boom_calls.load(Ordering::SeqCst), 1);
    // …and the model was told what happened, in a block it can route on.
    let seen = fake.transcript();
    assert!(seen.contains("tool_panicked"), "{seen}");
    assert!(seen.contains("panicked on purpose"), "{seen}");
    assert!(seen.contains("is_error"), "{seen}");
    // And told not to retry it, because no argument of theirs caused it.
    assert!(seen.contains("do not retry"), "{seen}");
}

/// A turn cut off at the output limit is reported to the person paying for it.
///
/// `stop_reason: max_tokens` means the model stopped mid-sentence. It was
/// recorded in the session log and read by nothing, so the loop treated a
/// truncated turn exactly like a finished one — and the user, who is the only
/// party that cannot see the wire, was told nothing.
///
/// Built inline rather than through `drive`, because `drive` uses a silent
/// terminal and the whole assertion is about what reached one.
#[tokio::test]
async fn a_turn_cut_off_at_the_output_limit_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let term = Term::recording();
    let tools = registry(vec![]);
    let approvals = Approvals::new(Gate::Ask, Asker::Scripted(Default::default()));
    let log = SessionLog::open(dir.path(), "s").unwrap();
    let fake = Fake::new(vec![
        cut_off("I was part way through explaining when"),
        text("done now.\n\nGOAL COMPLETE"),
    ]);

    let mut agent = Agent::new(Setup {
        provider: fake.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: Interrupt::new(),
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        mode: Mode::Batch,
    });
    agent.run_goal(&goal()).await;

    let said = term.recorded().join("\n");
    assert!(
        said.contains("cut off"),
        "a truncated turn was passed off as a finished one: {said}"
    );
}

/// One half of the memo rule. Without it, a model that has found a call it
/// likes and an argument it does not can spend the entire iteration budget
/// re-issuing the identical call, and every symptom points at the budget rather
/// than at the loop.
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

// endregion: Failures are observations

// region: Budgets
// ---------------------------------------------------------------------------
// Budgets
//
// The only things allowed to end a goal, besides done-detection and the user.
// Both tests here assert the arithmetic as well as the ending, because an
// ending with the wrong number behind it still looks like success.
// ---------------------------------------------------------------------------

/// The budget has to be checked *before* the call, not after it. An
/// off-by-one that tests it afterwards still terminates, so nothing looks
/// broken — it just bills one extra model call on every run that hits the cap.
/// The `fake.calls()` assertion is what notices; `out.iterations` alone would
/// not.
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
    let finished = records
        .iter()
        .find(|r| r["kind"] == "goal_finished")
        .unwrap();
    assert_eq!(finished["ending"], "tokens");
    assert_eq!(finished["tokens"], 20);
}

// endregion: Budgets

// region: The gate
// ---------------------------------------------------------------------------
// The gate
//
// Approval as the loop sees it, rather than as `approval.rs` tests it in
// isolation: the ordering of hook, gate and tool, driven end to end. The last
// two are the pair — a hook denial that cannot be approved away, and the same
// hook not touching a call it does not match.
// ---------------------------------------------------------------------------

/// Two failures at once, and the second is the quiet one. If `-p` ever starts
/// letting writers through, `emma -p` becomes a way to get unattended writes
/// without saying so. If it denies them without telling the model why, the tool
/// looks to the model like it silently did nothing, and the usual response to
/// that is to try it again.
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

/// The other side of the gate, and the reason the gate is usable at all. If
/// reads ever start needing approval, the interactive prompt fires several
/// times a minute and gets answered without being read — which costs the
/// prompts on `Write`, `Edit` and `Bash` too.
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

/// The positive control for the three tests above it. Without this one, a gate
/// that denied absolutely everything would pass every other approval test in
/// this file.
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

/// The second axis, driven through the real loop rather than through `decide`.
/// A tool that changes nothing locally still cannot reach a host nobody
/// approved, and under `-p` there is nobody — so the model has to be told which
/// host it was, or its only options are to guess or to give up silently.
#[tokio::test]
async fn the_gate_denies_a_network_read_with_nobody_to_ask_and_the_model_is_told_why() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fetcher, fetch_calls) = TestTool::reaching("Fetcher", "docs.rs");
    let fake = Fake::new(vec![
        call("Fetcher", json!({ "x": "1" })),
        text("understood.\n\nGOAL COMPLETE"),
    ]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![fetcher]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        fetch_calls.load(Ordering::SeqCst),
        0,
        "a read-only tool reached the network with nobody to approve the host"
    );
    let seen = fake.transcript();
    assert!(seen.contains("docs.rs"), "the host was not named: {seen}");
    assert!(seen.contains("-p"), "the model was not told why: {seen}");
}

/// The grant, end to end: one `y`, two fetches to the same host, both run. A
/// second prompt would find the scripted queue empty and fail this — which is
/// the point, because a prompt per page is the thing that teaches somebody to
/// answer without reading.
#[tokio::test]
async fn one_yes_covers_a_host_for_the_rest_of_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fetcher, fetch_calls) = TestTool::reaching("Fetcher", "docs.rs");
    let fake = Fake::new(vec![
        call("Fetcher", json!({ "x": "1" })),
        call("Fetcher", json!({ "x": "2" })),
        text("GOAL COMPLETE"),
    ]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![fetcher]),
        &Approvals::new(Gate::Ask, Asker::Scripted(vec![Answer::Yes].into())),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 2);
}

/// The safety inversion again, on the new axis. A hook is policy; a session
/// grant — even the blanket one `--dangerously-skip-permissions` gives — is
/// convenience, and convenience does not outrank policy on either axis.
#[tokio::test]
async fn a_hook_denial_outranks_a_network_grant() {
    let dir = tempfile::tempdir().unwrap();
    let root = harness_denying(dir.path(), "Fetcher");
    let (fetcher, fetch_calls) = TestTool::reaching("Fetcher", "docs.rs");
    let fake = Fake::new(vec![
        call("Fetcher", json!({ "x": "1" })),
        text("understood.\n\nGOAL COMPLETE"),
    ]);

    drive(
        &root,
        dir.path(),
        registry(vec![fetcher]),
        // The loudest possible approval, which also skips the host question.
        &Approvals::new(Gate::SkipAll, Asker::Scripted(Default::default())),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(
        fetch_calls.load(Ordering::SeqCst),
        0,
        "a PreToolUse denial was approved away on the network axis"
    );
    assert!(fake.transcript().contains("blocked_by_policy"));
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

// endregion: The gate

// region: The goal, and the kick
// ---------------------------------------------------------------------------
// The goal, and the kick
//
// What makes this a loop rather than a conversation, and the two independent
// bounds that stop the kick firing forever — the count, and the stall rule.
// ---------------------------------------------------------------------------

/// The difference between Emma and a chat client, asserted. Delete it and the
/// loop can regress to stopping when the model stops — which is not a crash,
/// not an error, and looks exactly like success. The transcript assertions
/// matter as much as the ending: a kick that does not restate the goal leaves
/// "continue" meaning "try the last thing again".
#[tokio::test]
async fn the_kick_fires_when_the_model_stops_without_claiming_completion() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    // The tool call is load-bearing, not scenery: a goal in which the model
    // never touched a tool is a conversation, and the loop now ends it as one
    // rather than nudging it. What is under test here is the *other* case —
    // work started, then stopped short of the completion line.
    let fake = Fake::new(vec![
        call("Fine", json!({})),
        text("Here is what I found. The middleware calls the legacy helper."),
        text("Ported it and the tests pass.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "kick").unwrap();

    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine]),
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

/// The kick is the one mechanism here that can argue back, so it needs a bound
/// that does not depend on the model cooperating. Without this the failure is a
/// goal that never ends until the iteration or token budget catches it — paid
/// for in full, and reported as the wrong limit.
#[tokio::test]
async fn the_kick_is_bounded_by_its_budget() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    // Tool use before the first stop and between every pair of stops, so
    // neither the answer rule nor the stall rule is what ends this — the count
    // is. That is what makes the `kicks == 2` assertion below mean something.
    let fake = Fake::new(vec![
        call("Fine", json!({})),
        text("thinking about it"),
        call("Fine", json!({})),
        text("still thinking"),
        call("Fine", json!({})),
        text("nearly there"),
        text("this one would be the third kick"),
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
    // The bound is real: the seventh scripted turn was never asked for.
    assert_eq!(fake.calls(), 6);
}

/// The second bound, and the one that catches a model arguing with the loop
/// without spending the kick budget on it. What breaks without it is not a
/// hang but a waste: three full model calls to be told the same thing three
/// times, on the one case where the model has already said everything it has.
/// The `kicks == 1` assertion is the load-bearing one — it proves the stall
/// rule fired rather than the count running out.
///
/// **This is the real stall and it must survive the answer rule below.** The
/// tool call at the top is what makes it one: work was started and then
/// abandoned mid-goal. Take the tool call away and this becomes a
/// conversational turn, which is a different ending and a different test.
#[tokio::test]
async fn a_model_that_abandons_started_work_twice_is_believed() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Fine", json!({})),
        text("I think this is already done."),
        text("As I said, there is nothing to change."),
    ]);
    let b = budgets(); // max_kicks is 3; the stall rule fires first.

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

    assert_eq!(out.ending, Ending::Stalled);
    assert_eq!(out.kicks, 1);
    assert_eq!(fake.calls(), 3);
}

/// The owner typed `hello` and was told his exchange stalled.
///
/// A turn that used no tool, claimed no completion, and followed no tool use
/// anywhere in the goal is an answer. Kicking it costs a second model call to
/// discover that nobody was working, and then reports a perfectly good reply as
/// a failure — wrong twice, and wrong in the direction that makes a person
/// distrust every other ending the loop reports.
///
/// The two counters are the assertion. `calls == 1` proves no kick was sent —
/// the fix has to be *not asking*, not asking and then relabelling the answer.
#[tokio::test]
async fn a_conversational_turn_is_answered_not_stalled() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, fine_calls) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        text("Hello. I am Emma. Tell me what you want changed and I will work on it."),
        text("this turn must never be asked for"),
    ]);

    let out = drive(
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

    assert_eq!(out.ending, Ending::Answered);
    assert_eq!(out.kicks, 0, "a turn that attempted no work was kicked");
    assert_eq!(fake.calls(), 1, "the kick was sent anyway");
    assert_eq!(fine_calls.load(Ordering::SeqCst), 0);
    // The answer survives as the outcome text, because it is the whole point of
    // the turn — an ending that threw it away would be the same defect quieter.
    assert!(out.text.contains("I am Emma"), "{}", out.text);
}

/// The narrowness of the answer rule, stated as a test. One tool call anywhere
/// in the goal is enough to make every later stop a stall again, however
/// conversational the wording of it is.
///
/// Without this assertion the obvious simplification — "did *this turn* use a
/// tool" instead of "has *this goal* used one" — passes every other test in
/// this file while quietly deleting the stall rule.
#[tokio::test]
async fn one_tool_call_earlier_in_the_goal_is_enough_to_make_a_stop_a_stall() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Fine", json!({})),
        text("Hello. There is nothing for me to do here."),
        text("As I said."),
    ]);

    let out = drive(
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

    assert_eq!(out.ending, Ending::Stalled);
    assert_eq!(out.kicks, 1);
}

/// A model that claims completion on its first breath is done, not "answered".
/// The answer rule is checked after the verdict for exactly this reason, and an
/// ordering mistake here would swallow every one-shot goal into a new ending
/// nothing downstream treats as success.
#[tokio::test]
async fn a_first_turn_that_claims_completion_is_still_done() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let fake = Fake::new(vec![text("Nothing needed changing.\n\nGOAL COMPLETE")]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(out.kicks, 0);
}

// endregion: The goal, and the kick

// region: The record
// ---------------------------------------------------------------------------
// The record
//
// What goes back to the provider on the next call, byte for byte.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_assistant_turn_is_echoed_back_exactly_as_it_arrived() {
    // Thinking-block signatures do not survive reassembly, so the content array
    // that comes out of the provider is the content array that goes back in.
    //
    // The failure this catches is a helpful refactor: rebuilding the assistant
    // message from `text` and `tool_calls`, which reads as tidier and produces
    // an array the API rejects on the *next* call — so the symptom lands one
    // step away from the change that caused it. Asserting on `last_query`
    // rather than the whole transcript is what makes it specific: the block has
    // to be intact in the message that was actually sent back.
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

/// The claim the session format makes: what was held can be read back out of
/// the file. Byte equality, because the thing resume needs is not "roughly this
/// conversation" — a reassembled thinking block is rejected, so anything short
/// of the same values is a run that dies on its first call.
///
/// **Two comparisons, and they are different claims.** The fold equals the
/// conversation the agent is left holding, which is what a resume continues;
/// and every message the last request carried is still the first N of it
/// unchanged, which is the byte-for-byte property the `raw_content` design
/// exists for. The two differ by exactly one message — the final answer, which
/// the loop places into the conversation *after* the call that produced it, and
/// which is the whole reason a follow-up question can be asked.
///
/// The script covers every shape the query can take: a successful call, a
/// failure block, a kick, and a second round of tool use.
#[tokio::test]
async fn the_log_folds_back_to_the_conversation_that_was_held() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let (boom, _) = TestTool::failing("Boom", true);
    let fake = Fake::new(vec![
        call("Fine", json!({ "x": "1" })),
        call("Boom", json!({ "x": "2" })),
        text("I am not sure this is finished."),
        call("Fine", json!({ "x": "3" })),
        text("done\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "roundtrip").unwrap();

    let (out, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![fine, boom]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        std::slice::from_ref(&goal()),
        &log,
    )
    .await;

    assert_eq!(out[0].ending, Ending::Done);
    assert_eq!(out[0].kicks, 1);
    let folded = emma::session::fold(log.path()).unwrap();
    // Stated as a number as well as compared, so a fold that returned nothing
    // against a provider that was sent nothing could not pass.
    assert_eq!(folded.len(), 10, "{folded:#?}");
    assert_eq!(folded, conversation);
    let sent = fake.last_messages();
    assert_eq!(sent.len(), folded.len() - 1, "{sent:#?}");
    assert_eq!(folded[..sent.len()], sent[..]);
}

/// A session is more than one goal, and the conversation runs straight through
/// them. A fold that rebuilt only the current goal — or that collapsed a
/// finished one the way the loop used to — would hand back a different list
/// from the one that was sent, every time somebody typed a second goal.
#[tokio::test]
async fn the_fold_carries_a_finished_goal_forward_the_way_the_loop_does() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        text("the first one was already true.\n\nGOAL COMPLETE"),
        call("Fine", json!({ "x": "1" })),
        text("and now the second.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "two-goals").unwrap();

    let (out, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![fine]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &[Goal::new("first goal"), Goal::new("second goal")],
        &log,
    )
    .await;

    assert_eq!(out[0].ending, Ending::Done);
    assert_eq!(out[1].ending, Ending::Done);
    let folded = emma::session::fold(log.path()).unwrap();
    // The first goal in full — opening and answer — then the second goal's
    // opening, its tool round-trip, and its answer.
    assert_eq!(folded.len(), 6, "{folded:#?}");
    assert_eq!(folded, conversation);
    // The tool traffic of goal two is in there, which is the thing the old
    // collapse threw away before goal three could ever see it.
    assert!(folded
        .iter()
        .any(|m| m.content.to_string().contains("tool_result")));
    let sent = fake.last_messages();
    assert_eq!(folded[..sent.len()], sent[..]);
}

// endregion: The record
