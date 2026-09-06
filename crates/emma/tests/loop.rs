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

use support::{
    call, cut_off, empty_harness, harness_denying, paused, registry, text, Fake, Say, TestTool,
};

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
        background: Default::default(),
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
        web_search: false,
        sampling: Default::default(),
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

/// A provider that pauses a search mid-turn hands back a turn with no text and
/// no client tool call, and asks to see it again unchanged. Before this test
/// the loop read that as the model's answer: on a fresh goal it ended
/// `Answered` after one call, and the search the user was paying for never
/// finished.
///
/// **If this breaks:** either the paused turn is judged as an answer again,
/// or it is sent back with a kick appended after it, which is not the message
/// the provider asked to see.
#[tokio::test]
async fn a_paused_turn_is_sent_back_unchanged_and_is_not_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let fake = Fake::new(vec![paused(), text("found it.\n\nGOAL COMPLETE")]);
    let log = SessionLog::open(dir.path(), "s").unwrap();

    let out = drive(
        &root,
        dir.path(),
        registry(vec![]),
        &Approvals::new(Gate::Ask, Asker::Scripted(Default::default())),
        &fake,
        budgets(),
        &goal(),
        &log,
    )
    .await;

    assert_eq!(
        out.ending,
        Ending::Done,
        "the pause was taken as the answer"
    );
    assert_eq!(fake.calls(), 2, "the paused turn was never sent back");
    // The provider's block went back exactly as it came, on the assistant
    // side, and nothing was appended after it: no kick, no user message.
    let messages = fake.last_messages();
    let last = messages.last().expect("the second request carried history");
    let rendered = last.content.to_string();
    assert!(
        rendered.contains("server_tool_use"),
        "the paused turn is not the last thing the provider saw: {rendered}"
    );
    assert!(
        !fake
            .transcript()
            .contains("The goal is not recorded as done"),
        "a kick was appended to a paused turn"
    );
}

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

/// A tool that panics **before** `invoke` is an observation too.
///
/// **The guard was one function too late, and the fixture controlled for the
/// wrong half.** `DEF-007` wraps `Tool::invoke` in `catch_unwind`, and the
/// argument in its own doc is that *"several of its crates parse input written
/// by the model, and one `unwrap` in any of them ended everything"*. The first
/// function on the call path to see that input is `validate_args`, which was
/// outside the guard — as were `network_target` and `meta`, both of which the
/// approval gate calls on the tool's own code.
///
/// There are nineteen-odd real `validate_args` implementations across
/// `tools/fs`, `tools/lsp`, `tools/tasks` and `tools/web`. An independent
/// reviewer staged a panic in one and the session died exactly the way the row
/// said it no longer could. `TestTool::panicking` panics inside `invoke`, so it
/// could never have caught this: a positive control for the other half.
#[tokio::test]
async fn a_tool_that_panics_before_it_runs_is_an_observation_too() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (boom, boom_calls) = TestTool::panicking_early("Boom");
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

    assert_eq!(
        out.ending,
        Ending::Done,
        "a panic in validate_args ended the session"
    );
    // `invoke` was never reached, which is the point: the panic happened before
    // it. A non-zero count here would mean the fixture panicked in the wrong
    // place and this test is measuring the arm that was already covered.
    assert_eq!(
        boom_calls.load(Ordering::SeqCst),
        0,
        "the tool ran; this fixture is supposed to die before invoke, so the \
         test is exercising the half that was already guarded"
    );

    let seen = fake.transcript();
    assert!(
        seen.contains("panicked"),
        "the model was not told the tool broke: {seen}"
    );
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
        background: Default::default(),
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
        web_search: false,
        sampling: Default::default(),
    });
    agent.run_goal(&goal()).await;

    let said = term.recorded().join("\n");
    assert!(
        said.contains("cut off"),
        "a truncated turn was passed off as a finished one: {said}"
    );
}

/// Ctrl-C reaches a tool that is already running.
///
/// **The gap this closes.** `invoke` used to be awaited outright and the
/// interrupt flag was read only at iteration boundaries and around the model
/// call — so a wrong `Bash` ran to its own timeout, 120 seconds by default and
/// up to 600, with the keyboard already asking it to stop. In the framed UI raw
/// mode means the child never sees a console signal either, so nothing else was
/// going to end it.
///
/// The assertion is on elapsed time, because that is the whole claim: a tool
/// asked to take ten seconds must not take ten seconds once cancelled.
#[tokio::test]
async fn an_interrupt_reaches_a_tool_that_is_already_running() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let term = Term::silent();
    let (slow, slow_calls) = TestTool::slow("Slow", 10);
    let tools = registry(vec![slow]);
    let approvals = Approvals::new(Gate::Ask, Asker::Scripted(Default::default()));
    let log = SessionLog::open(dir.path(), "s").unwrap();
    let fake = Fake::new(vec![
        call("Slow", json!({})),
        text("stopped.\n\nGOAL COMPLETE"),
    ]);
    let interrupt = Interrupt::new();

    // Trip it shortly after the call starts, from outside the loop.
    let trip = interrupt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        trip.trip();
    });

    let mut agent = Agent::new(Setup {
        background: Default::default(),
        provider: fake.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: interrupt.clone(),
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        mode: Mode::Batch,
        web_search: false,
        sampling: Default::default(),
    });

    let started = std::time::Instant::now();
    agent.run_goal(&goal()).await;
    let took = started.elapsed();

    assert_eq!(slow_calls.load(Ordering::SeqCst), 1, "the tool never ran");
    assert!(
        took < std::time::Duration::from_secs(5),
        "the interrupt did not reach the running tool: the goal took {took:?} \
         against a tool asked to take 10s"
    );

    // **And the conversation is still wire-legal.** An adversarial review
    // raised the worry that a cancelled call leaves a `tool_use` with no
    // `tool_result` — the shape the API refuses and the shape that breaks a
    // resumed conversation. It does not: `session::place_turn` refuses to place
    // a turn at all unless every call has exactly one result, so the cancelled
    // result travels with its call or neither travels. Asserted here rather
    // than argued, because the worry was reasonable and the answer is cheap.
    let convo = agent.conversation();
    let calls: usize = convo
        .iter()
        .flat_map(|m| m.content.blocks())
        .filter(|b| matches!(b, emma_llm::ContentBlock::ToolUse(_)))
        .count();
    let results: usize = convo
        .iter()
        .flat_map(|m| m.content.blocks())
        .filter(|b| matches!(b, emma_llm::ContentBlock::ToolResult(_)))
        .count();
    assert_eq!(
        calls, results,
        "a cancelled call left the conversation with {calls} tool_use and {results} tool_result"
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
    let records = SessionLog::read(&log.path()).unwrap();
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
    let records = SessionLog::read(&log.path()).unwrap();
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
    let folded = emma::session::fold(&log.path()).unwrap();
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
    let folded = emma::session::fold(&log.path()).unwrap();
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

// region: The guarantees nothing defended
// ---------------------------------------------------------------------------
// The guarantees nothing defended
//
// Every test in this region was written after a mutation of `agent.rs`
// survived the whole workspace suite. The mutation is named in each doc
// comment, because a test whose motivating mutation is not written down is a
// test the next reader cannot re-validate.
// ---------------------------------------------------------------------------

/// A tool that refuses the model's arguments is **not run**, and the refusal
/// reaches the model as an error block.
///
/// **The mutation that survived:** deleting the whole
/// `if let Err(e) = validated { return fail(e.kind(), ...) }` arm in
/// `run_tool_call`, so a `validate_args` rejection is computed and thrown away
/// and `invoke` is called anyway. The workspace stayed green. No fixture in the
/// support module has ever returned `Err` from `validate_args` -- the one that
/// touches that function panics instead -- so the arm had never been executed
/// by a test at all.
///
/// What that would be in production is the class this project cares about
/// most. The argument check is where `Read`, `Edit`, `Bash` and sixteen others
/// state their preconditions; running past a rejection hands a tool the input
/// it has just said it cannot take, and whatever it returns is then reported
/// to the model as a success.
#[tokio::test]
async fn a_tool_that_refuses_its_arguments_is_not_run_and_the_model_is_told() {
    use std::sync::atomic::AtomicUsize;

    struct Picky {
        refuse: bool,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl emma_tool_api::Tool for Picky {
        fn name(&self) -> &'static str {
            "Picky"
        }
        fn description(&self) -> &str {
            "a tool that checks its arguments before it runs"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({ "type": "object", "properties": { "x": { "type": "string" } } })
        }
        fn meta(&self) -> emma_tool_api::ToolMeta {
            emma_tool_api::ToolMeta {
                read_only: true,
                reaches_network: false,
                idempotent: true,
            }
        }
        fn validate_args(&self, _args: &serde_json::Value) -> Result<(), emma_tool_api::ToolError> {
            if self.refuse {
                return Err(emma_tool_api::ToolError::BadArguments(
                    "x must name a file that exists".into(),
                ));
            }
            Ok(())
        }
        async fn invoke(
            &self,
            _ctx: &emma_tool_api::ToolCtx,
            _args: serde_json::Value,
        ) -> anyhow::Result<Result<emma_tool_api::ToolOutcome, emma_tool_api::ToolError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Ok(emma_tool_api::ToolOutcome::new("Picky ran")))
        }
    }

    async fn run(refuse: bool) -> (Outcome, usize, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = empty_harness(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let tool: Arc<dyn emma_tool_api::Tool> = Arc::new(Picky {
            refuse,
            calls: calls.clone(),
        });
        let fake = Fake::new(vec![
            call("Picky", json!({ "x": "nowhere" })),
            text("understood.\n\nGOAL COMPLETE"),
        ]);
        let out = drive(
            &root,
            dir.path(),
            registry(vec![tool]),
            &Approvals::unattended(),
            &fake,
            budgets(),
            &goal(),
            &SessionLog::none(),
        )
        .await;
        let seen = fake.transcript();
        (out, calls.load(Ordering::SeqCst), seen)
    }

    let (out, ran, seen) = run(true).await;
    // The loop continued: a rejected argument is an observation like every
    // other failure class, not an abort.
    assert_eq!(out.ending, Ending::Done, "an argument check ended the goal");
    assert_eq!(
        ran, 0,
        "the tool said its arguments were wrong and was invoked anyway"
    );
    assert!(
        seen.contains("bad_arguments"),
        "the model was not told which failure class this was: {seen}"
    );
    assert!(
        seen.contains("x must name a file that exists"),
        "the tool's own reason was not passed through: {seen}"
    );
    assert!(
        seen.contains("is_error"),
        "the refusal reached the model as an ordinary result: {seen}"
    );

    // The control, and the half that makes the assertions above mean
    // something: the same call with the same arguments against a tool that
    // does not object. It runs, and none of the above is said.
    let (out, ran, seen) = run(false).await;
    assert_eq!(out.ending, Ending::Done);
    assert_eq!(ran, 1, "the ordinary call did not reach the tool");
    assert!(
        !seen.contains("bad_arguments"),
        "an accepted call was reported to the model as a rejected one: {seen}"
    );
    assert!(
        !seen.contains("is_error"),
        "an ordinary successful call carried the error flag: {seen}"
    );
}

/// Ctrl-C during a model call ends the goal, and ends it as `Interrupted`.
///
/// **The mutation that survived:** replacing `self.s.interrupt.wait()` in
/// `call_model`'s `select!` with a future that never completes, so a keypress
/// can no longer cut a call in flight. Nothing went red. The one interrupt test
/// this file had covers a running *tool*; the model call -- the branch a person
/// actually hits, because a long answer is where the waiting happens -- was
/// asserted nowhere.
///
/// The assertion is a timeout rather than an ending alone, because the shape of
/// the regression is a hang: without that branch `run_goal` waits on a provider
/// that will never answer, and a test that only compared endings would never
/// reach the comparison.
#[tokio::test]
async fn a_ctrl_c_during_a_model_call_ends_the_goal_rather_than_hanging() {
    use std::sync::atomic::AtomicUsize;

    /// A provider that answers nothing, ever, having first pressed Ctrl-C.
    /// `Fake` cannot express this: every script entry returns.
    struct Hanging {
        interrupt: Arc<Interrupt>,
        calls: Arc<AtomicUsize>,
        press: bool,
    }

    #[async_trait::async_trait]
    impl emma_llm::Provider for Hanging {
        fn model_id(&self) -> &str {
            "hanging"
        }
        async fn send(
            &self,
            _request: emma_llm::Request,
            _mode: Mode,
            _events: Option<tokio::sync::mpsc::Sender<emma_llm::Event>>,
        ) -> Result<emma_llm::AssistantTurn, emma_llm::LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.press {
                self.interrupt.trip();
            }
            std::future::pending::<()>().await;
            unreachable!("the pending future resolved")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let term = Term::silent();
    let tools = registry(vec![]);
    let approvals = Approvals::unattended();
    let log = SessionLog::none();
    let interrupt = Interrupt::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn emma_llm::Provider> = Arc::new(Hanging {
        interrupt: interrupt.clone(),
        calls: calls.clone(),
        press: true,
    });

    let mut agent = Agent::new(Setup {
        background: Default::default(),
        provider,
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: interrupt.clone(),
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        mode: Mode::Batch,
        web_search: false,
        sampling: Default::default(),
    });

    let out = tokio::time::timeout(Duration::from_secs(5), agent.run_goal(&goal()))
        .await
        .expect(
            "Ctrl-C did not reach the model call: run_goal was still waiting on a provider \
             that never answers",
        );
    assert_eq!(
        out.ending,
        Ending::Interrupted,
        "the abandoned call was reported as something other than an interrupt"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Nothing came back, so nothing is billed and no iteration is counted --
    // which is the claim `Ending::Interrupted`'s own message makes about an
    // abandoned call.
    assert_eq!(out.tokens, 0, "an abandoned call was charged for");
    assert_eq!(
        out.iterations, 0,
        "an abandoned call counted as an iteration"
    );

    // The control. The same provider and the same silence, with no keypress:
    // the goal must now fail to end, which is what makes the ending above
    // evidence about the interrupt rather than about the loop giving up on its
    // own. Asserted as a timeout that is expected to expire.
    let interrupt = Interrupt::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn emma_llm::Provider> = Arc::new(Hanging {
        interrupt: interrupt.clone(),
        calls: calls.clone(),
        press: false,
    });
    let mut agent = Agent::new(Setup {
        background: Default::default(),
        provider,
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt,
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        mode: Mode::Batch,
        web_search: false,
        sampling: Default::default(),
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(400), agent.run_goal(&goal()))
            .await
            .is_err(),
        "the goal ended without an interrupt and without an answer, so the ending above \
         proves nothing about Ctrl-C"
    );
}

/// A cancelled tool is recorded as cancelled, and a tool that finished is not.
///
/// **The mutation that survived:** deleting the `tool_cancelled` record from
/// the interrupt arm of `run_tool_call`. The session file is the only account
/// of a run nobody watched, and "the tool was stopped part-way" and "the tool
/// was never reached" are different runs -- the first may have left half a file
/// on disk.
///
/// The existing interrupt test asserts elapsed time and that the conversation
/// stays wire-legal. It does not read the log, and it does not check the
/// ending.
#[tokio::test]
async fn a_cancelled_tool_is_recorded_as_cancelled_and_a_finished_one_is_not() {
    async fn run(interrupt_it: bool) -> (Outcome, String, Vec<emma_llm::Message>) {
        let dir = tempfile::tempdir().unwrap();
        let root = empty_harness(dir.path());
        let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
        let term = Term::silent();
        // Ten seconds when it is going to be cancelled, none when it is not:
        // the same tool both times would make the control take ten seconds.
        let (slow, _) = if interrupt_it {
            TestTool::slow("Slow", 10)
        } else {
            TestTool::ok("Slow", true)
        };
        let tools = registry(vec![slow]);
        let approvals = Approvals::unattended();
        let log = SessionLog::open(dir.path(), "cancel").unwrap();
        let fake = Fake::new(vec![
            call("Slow", json!({})),
            text("stopped.\n\nGOAL COMPLETE"),
        ]);
        let interrupt = Interrupt::new();
        if interrupt_it {
            let trip = interrupt.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                trip.trip();
            });
        }

        let mut agent = Agent::new(Setup {
            background: Default::default(),
            provider: fake.clone(),
            harness: &harness,
            instructions: &harness.instructions,
            tools: &tools,
            approvals: &approvals,
            log: &log,
            term: &term,
            interrupt,
            spend: emma::agent::Spend::new(),
            done: &MarkerClaim,
            cwd: dir.path().to_path_buf(),
            session_id: "sess-test".into(),
            budgets: budgets(),
            caching: Caching::On,
            mode: Mode::Batch,
            web_search: false,
            sampling: Default::default(),
        });
        let out = agent.run_goal(&goal()).await;
        let kinds = SessionLog::read(&log.path())
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
            .join(",");
        (out, kinds, agent.conversation())
    }

    let (out, kinds, convo) = run(true).await;
    assert_eq!(
        out.ending,
        Ending::Interrupted,
        "a goal cut off at a tool boundary reported some other ending"
    );
    assert!(
        kinds.contains("tool_cancelled"),
        "the log does not say the tool was stopped part-way through, so a reader cannot \
         tell it from a tool that was never reached: {kinds}"
    );
    // And the model was told, in the conversation a resume will start from --
    // the cancelled call is never sent again in this run, so the transcript
    // cannot be where this is checked.
    let said = convo
        .iter()
        .map(|m| m.content.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        said.contains("cancelled"),
        "the cancelled call is a hole in the transcript rather than an observation: {said}"
    );

    // The control: the same script and the same tool name, with nothing
    // pressed.
    let (out, kinds, _) = run(false).await;
    assert_eq!(out.ending, Ending::Done);
    assert!(
        !kinds.contains("tool_cancelled"),
        "a tool that ran to completion was recorded as cancelled: {kinds}"
    );
}

/// The wall clock ends a goal, and ends it before spending anything.
///
/// **The mutation that survived:** deleting the `started.elapsed() >
/// wall_clock` break. `Ending::Deadline` has a unit test for its *wording* and
/// had nothing anywhere for its *behaviour* -- no test in the workspace had
/// ever produced one.
#[tokio::test]
async fn the_wall_clock_ends_the_goal_before_the_first_call() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let fake = Fake::new(vec![text("GOAL COMPLETE")]);
    let mut b = budgets();
    b.wall_clock = Duration::ZERO;

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

    assert_eq!(out.ending, Ending::Deadline);
    assert_eq!(
        fake.calls(),
        0,
        "the deadline was checked after the call it was supposed to prevent"
    );

    // The control: the identical script under an ordinary clock finishes, so
    // the ending above is the deadline rather than anything else about this
    // run.
    let fake = Fake::new(vec![text("GOAL COMPLETE")]);
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
}

/// An ending that lands on a turn full of tool calls still reports the last
/// thing the assistant actually said.
///
/// **The mutation that survived:** dropping the `if !text.trim().is_empty()`
/// guard around `last_text`, so a final turn that is nothing but tool calls
/// blanks the answer. `Outcome::text` is what `-p` prints and what the exit
/// line carries, so the symptom is a run that did work, hit a budget, and
/// reported an empty string -- a stopped run that reads as a quiet successful
/// one.
#[tokio::test]
async fn an_ending_on_a_tool_call_turn_still_reports_the_last_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        // A turn that both says something and calls a tool, which `text` and
        // `call` cannot express on their own.
        Say {
            text: "I have read the file and it is a router.".into(),
            calls: vec![("Fine".into(), json!({ "x": "1" }))],
            tokens: 10,
            truncated: false,
            paused: false,
            fail: None,
        },
        // ...and then a turn that is only tool calls. This is the one the
        // budget stops on.
        call("Fine", json!({ "x": "2" })),
        text("never reached"),
    ]);
    let mut b = budgets();
    b.max_iterations = 2;

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

    assert_eq!(out.ending, Ending::Iterations);
    assert_eq!(
        out.text, "I have read the file and it is a router.",
        "the run reported {:?} instead of the last thing the assistant said",
        out.text
    );
}

/// A kick quotes what has already failed, once each.
///
/// **Two mutations survived here.** Passing `&[]` instead of
/// `tail(&failed_ever, 5)`, so the nudge never tells the model what it just
/// tried; and replacing the `if !failed_ever.contains(&label)` guard with
/// `true`, so one repeated call is listed as many times as it was attempted.
/// The kick is the loop's only chance to stop a model repeating itself, and a
/// nudge that says "keep going" and nothing else is the nudge that produces the
/// same call again.
#[tokio::test]
async fn a_kick_quotes_what_already_failed_once_each() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (boom, _) = TestTool::failing("Boom", true);
    let fake = Fake::new(vec![
        // The same call twice: the second is refused by the memo, which is a
        // second failure carrying the *same* label -- the case the dedupe
        // guard is about.
        call("Boom", json!({ "x": "a" })),
        call("Boom", json!({ "x": "a" })),
        text("I think that is everything."),
        text("all done.\n\nGOAL COMPLETE"),
    ]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![boom]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &goal(),
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        out.kicks, 1,
        "the nudge never fired, so this proves nothing"
    );
    let seen = fake.transcript();
    assert!(
        seen.contains("These calls failed earlier in this goal"),
        "the nudge did not carry the failure list at all: {seen}"
    );
    // `Boom({` rather than the whole label: the transcript renders each
    // message as JSON, so the label's own quotes come back escaped and a
    // literal written the obvious way would match nothing at all — which is
    // exactly how this assertion failed the first time it ran.
    let listed = seen.matches("Boom({").count();
    assert_eq!(
        listed, 1,
        "one repeated call is listed {listed} times in the nudge: {seen}"
    );

    // The control: the same shape with nothing failing. A nudge with no
    // failures behind it must not grow a list, or the assertion above is
    // satisfied by a sentence that is there either way.
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Fine", json!({ "x": "a" })),
        text("I think that is everything."),
        text("all done.\n\nGOAL COMPLETE"),
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
    assert_eq!(out.kicks, 1);
    assert!(
        !fake.transcript().contains("These calls failed earlier"),
        "a goal in which nothing failed was nudged with a list of failures: {}",
        fake.transcript()
    );
}

/// Compaction fires on the size the provider **measured**, not on the local
/// estimate -- and it leaves the new goal room to work in.
///
/// **Two mutations survived here.** Setting `last_input = None` after every
/// call, so the threshold falls back to `estimate`, which `emma-llm` documents
/// as under-counting; and widening the compaction target from `cap / 2` to
/// `cap`, so a conversation is compacted to exactly the limit it just crossed
/// and crosses it again on the next call.
///
/// The conversation here is built so the two numbers disagree in the direction
/// that matters: the estimate sits comfortably under the cap while the measured
/// request is well over it. That is the ordinary case rather than a contrived
/// one -- the request also carries the system prompt and every tool schema, and
/// `chars / 4` under-counts what is left.
#[tokio::test]
async fn compaction_fires_on_the_measured_request_rather_than_the_estimate() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let term = Term::recording();
    // ~240,000 characters is ~60,000 estimated tokens: over half the cap, and
    // well under the cap itself.
    let (reader, _) = TestTool::returning("Read", "x".repeat(240_000));
    let tools = registry(vec![reader]);
    let approvals = Approvals::unattended();
    let log = SessionLog::none();
    let fake = Fake::new(vec![
        call("Read", json!({ "x": "1" })).costing(200_000),
        text("read it.\n\nGOAL COMPLETE").costing(200_000),
        text("and the second.\n\nGOAL COMPLETE").costing(200_000),
    ]);
    let mut b = budgets();
    b.max_context = 100_000;
    b.max_tokens = i64::MAX;

    let mut agent = Agent::new(Setup {
        background: Default::default(),
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
        budgets: b,
        caching: Caching::On,
        mode: Mode::Batch,
        web_search: false,
        sampling: Default::default(),
    });
    agent.run_goal(&Goal::new("first goal")).await;
    // Nothing has been compacted yet: within the first goal there is no
    // finished goal behind it to summarise. This is the control for the
    // assertion below -- it is what makes "it compacted" evidence about the
    // second goal's measurement rather than about the conversation being large.
    assert!(
        !term.recorded().iter().any(|s| s.contains("compacted")),
        "something was compacted before the second goal opened, so the next assertion \
         proves nothing: {:?}",
        term.recorded()
    );

    // Only what the second goal said. The first goal warned once that it had
    // nothing behind it to compact — correctly, and that warning is `said once`
    // rather than `said again`, so reading the whole recording would mix the
    // two goals' accounts together.
    let before = term.recorded().len();
    agent.run_goal(&Goal::new("second goal")).await;
    let said = term.recorded()[before..].join("\n");
    assert!(
        said.contains("compacted"),
        "the request the provider measured at 200,000 tokens against a 100,000 cap was \
         not compacted, so the trigger is reading the estimate: {said}"
    );
    assert!(
        !said.contains("compaction cannot"),
        "compaction reported that it had nothing to give, which is what happens when the \
         target is the whole cap rather than half of it: {said}"
    );
}

/// `max_context` of zero turns compaction off, and says nothing about it.
///
/// **The mutation that survived:** deleting the `if cap <= 0 { return; }`
/// guard. With it gone, every request is over a cap of zero, so an ordinary run
/// is told on its first call that "this conversation is over the context limit
/// and compaction cannot shrink it" -- a warning about a limit the user
/// switched off.
#[tokio::test]
async fn a_context_cap_of_zero_is_off_rather_than_a_cap_of_zero() {
    async fn run(cap: i64) -> String {
        let dir = tempfile::tempdir().unwrap();
        let root = empty_harness(dir.path());
        let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
        let term = Term::recording();
        let tools = registry(vec![]);
        let approvals = Approvals::unattended();
        let log = SessionLog::none();
        // Two goals, because the check that has to stay silent is the one a
        // *second* goal makes: with only one goal there is no conversation
        // behind it and every cap is under-run, so a one-goal run is silent
        // whatever the guard does.
        let fake = Fake::new(vec![
            text("done.\n\nGOAL COMPLETE"),
            text("done again.\n\nGOAL COMPLETE"),
        ]);
        let mut b = budgets();
        b.max_context = cap;

        let mut agent = Agent::new(Setup {
            background: Default::default(),
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
            budgets: b,
            caching: Caching::On,
            mode: Mode::Batch,
            web_search: false,
            sampling: Default::default(),
        });
        agent.run_goal(&Goal::new("first goal")).await;
        agent.run_goal(&Goal::new("second goal")).await;
        term.recorded().join("\n")
    }

    let off = run(0).await;
    assert!(
        !off.contains("context limit"),
        "a run with compaction switched off was warned about the context limit: {off}"
    );

    // The control, and the reason the assertion above is not satisfied by a
    // terminal that records nothing: a cap of one is a real cap this
    // conversation is over, and there the warning does appear.
    let on = run(1).await;
    assert!(
        on.contains("context limit"),
        "a conversation over a cap of one said nothing, so the silence above is the \
         terminal rather than the guard: {on}"
    );
}

/// Context a `UserPromptSubmit` hook injected rides into the model with the
/// goal, attributed rather than merged into the user's sentence.
///
/// **The mutation that survived:** `goal.opening_turn()` -> `goal.text`, which
/// drops every injected block on the floor. `goal.rs` has unit tests for the
/// composition; nothing checked that the loop sends the composed string rather
/// than the raw one, and the two are identical for every other goal in this
/// file.
#[tokio::test]
async fn injected_context_reaches_the_model_in_front_of_the_users_words() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let fake = Fake::new(vec![text("noted.\n\nGOAL COMPLETE")]);
    let with_context =
        Goal::new("make it work").with_injected(vec!["branch: main, 3 files dirty".into()]);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![]),
        &Approvals::unattended(),
        &fake,
        budgets(),
        &with_context,
        &SessionLog::none(),
    )
    .await;

    assert_eq!(out.ending, Ending::Done);
    let sent = fake.last_query();
    assert!(
        sent.contains("branch: main, 3 files dirty"),
        "the hook's context never reached the model: {sent}"
    );
    assert!(
        sent.contains("The user did not write it"),
        "the injected context was passed off as the user's own words: {sent}"
    );
    assert!(sent.contains("make it work"), "{sent}");

    // The control: a goal with nothing injected sends the user's words alone,
    // so the attribution line above is evidence of the injection rather than
    // something every turn carries.
    let fake = Fake::new(vec![text("noted.\n\nGOAL COMPLETE")]);
    drive(
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
    assert!(
        !fake.last_query().contains("The user did not write it"),
        "an ordinary goal was labelled as hook-injected: {}",
        fake.last_query()
    );
}

// endregion: The guarantees nothing defended

/// Plan mode is a posture the gate applies, and a refusal under it is an
/// observation like any other denial: the goal goes on, the model is told in a
/// block it can route on, and the session log names the mode as the decider.
///
/// The loop's arm is shared by every decider and branches on none of them, so
/// this is the construction being tested rather than a new path: nothing else
/// drives a goal in plan mode and reads `"by": "mode"` back out of the log.
#[tokio::test]
async fn a_plan_mode_refusal_is_an_observation_and_the_log_names_the_mode() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (writer, writer_calls) = TestTool::ok("Writer", false);
    let fake = Fake::new(vec![
        call("Writer", json!({ "path": "a.txt" })),
        text(
            "I will not write in plan mode.

GOAL COMPLETE",
        ),
    ]);
    let log = SessionLog::open(dir.path(), "plan").unwrap();
    let approvals = Approvals::new(Gate::Ask, Asker::Scripted(Default::default()));
    approvals.set_mode(emma::approval::Mode::Plan);

    let out = drive(
        &root,
        dir.path(),
        registry(vec![writer]),
        &approvals,
        &fake,
        budgets(),
        &goal(),
        &log,
    )
    .await;

    assert_eq!(out.ending, Ending::Done, "a refusal must not end the goal");
    assert_eq!(
        writer_calls.load(Ordering::SeqCst),
        0,
        "plan mode let a write run"
    );
    let seen = fake.transcript();
    assert!(seen.contains("is_error"), "{seen}");
    let records = std::fs::read_to_string(log.path()).unwrap();
    let denied = records
        .lines()
        .find(|l| l.contains("\"kind\":\"denied\"") || l.contains("\"kind\": \"denied\""))
        .unwrap_or_else(|| {
            panic!(
                "no denied record in the log:
{records}"
            )
        });
    assert!(
        denied.contains("\"mode\""),
        "the log does not name the mode as the decider: {denied}"
    );
    assert!(
        denied.contains("/mode assist"),
        "the refusal does not say how to lift it: {denied}"
    );
}
