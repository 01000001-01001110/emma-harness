//! One session is one conversation.
//!
//! The defect these were written from: every `>` input started a fresh context
//! whose history was two lines of prose, so `why does it do that?` re-read the
//! file the turn before it had just read. What is asserted here is the opposite
//! — that the second turn's request *contains* the first turn's tool traffic —
//! plus the two properties that make carrying it safe: no `tool_use` reaches
//! the wire without its result, and the conversation is compacted rather than
//! allowed to grow without a bound.

mod support;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Setup};
use emma::approval::Approvals;
use emma::goal::{Goal, MarkerClaim};
use emma::session::SessionLog;
use emma::term::Term;
use emma_harness::{Flavor, Harness};
use emma_llm::{Caching, ContentBlock, Message, Mode, Role};
use emma_tool_api::{NetworkTarget, Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use support::{call, empty_harness, registry, rejected, text, Fake, Say, TestTool};

fn budgets() -> Budgets {
    Budgets {
        max_iterations: 20,
        max_tokens: 1_000_000,
        wall_clock: Duration::from_secs(60),
        max_kicks: 3,
        max_context: 1_000_000,
    }
}

/// Several goals on one `Agent`, which is the only way to exercise what the
/// session carries between them. Returns the outcomes and the conversation the
/// agent is left holding.
async fn drive_goals(
    root: &Path,
    cwd: &Path,
    tools: Registry,
    provider: &Arc<Fake>,
    budgets: Budgets,
    goals: &[Goal],
    log: &SessionLog,
) -> (Vec<Outcome>, Vec<Message>) {
    let harness = Harness::load_selecting(root, Flavor::Emma, None).unwrap();
    let approvals = Approvals::unattended();
    let term = Term::silent();
    let mut agent = Agent::new(Setup {
        background: Default::default(),
        provider: provider.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
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

/// Everything in a message list, as one string to search.
fn rendered(messages: &[Message]) -> String {
    messages
        .iter()
        .map(|m| m.content.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `tool_use` id with no `tool_result` answering it, and every result
/// answering no call. Both directions, because the API refuses both ways round.
fn unmatched(messages: &[Message]) -> Vec<String> {
    let mut calls: Vec<String> = Vec::new();
    let mut answers: Vec<String> = Vec::new();
    for m in messages {
        for b in m.content.blocks() {
            match b {
                ContentBlock::ToolUse(c) => calls.push(c.id.clone()),
                ContentBlock::ToolResult(r) => answers.push(r.tool_use_id.clone()),
                _ => {}
            }
        }
    }
    let mut out: Vec<String> = calls
        .iter()
        .filter(|id| !answers.contains(id))
        .map(|id| format!("call {id} was never answered"))
        .collect();
    out.extend(
        answers
            .iter()
            .filter(|id| !calls.contains(id))
            .map(|id| format!("result {id} answers nothing")),
    );
    out
}

/// Whether the roles alternate and the list opens on a user turn — the other
/// shape the API rejects outright.
fn alternates(messages: &[Message]) -> bool {
    if messages.first().map(|m| m.role) != Some(Role::User) {
        return messages.is_empty();
    }
    messages.windows(2).all(|w| w[0].role != w[1].role)
}

// region: The conversation
// ---------------------------------------------------------------------------
// The conversation
//
// The defect, and the mutation target: break the carry-over and the first test
// here goes red because the file contents are no longer in the second turn's
// request.
// ---------------------------------------------------------------------------

/// *"read src/auth.rs and tell me what it does"*, then *"why does it do that?"*
///
/// The second goal must be able to answer from what the first one read. The
/// assertion is on the request the second goal actually sent — not on the
/// answer, which a scripted model would give either way — because the whole
/// cost being paid here is the one in the request.
#[tokio::test]
async fn a_follow_up_sees_what_the_turn_before_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (reader, reads) = TestTool::returning("Read", "fn login() { /* THE-FILE-CONTENTS */ }");
    let fake = Fake::new(vec![
        call("Read", json!({ "file_path": "src/auth.rs" })),
        text("It validates the session cookie.\n\nGOAL COMPLETE"),
        // The second goal answers with no tool call at all, which it can only
        // do honestly if the file is still in front of it.
        text("Because the cookie is signed.\n\nGOAL COMPLETE"),
    ]);

    let (outs, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![reader]),
        &fake,
        budgets(),
        &[
            Goal::new("read src/auth.rs and tell me what it does"),
            Goal::new("why does it do that?"),
        ],
        &SessionLog::none(),
    )
    .await;

    assert_eq!(outs[0].ending, Ending::Done);
    assert_eq!(outs[1].ending, Ending::Done);
    assert_eq!(fake.calls(), 3);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "the file was read more than once"
    );

    // The request the second goal sent, in full.
    let sent = fake.last_messages();
    let seen = rendered(&sent);
    assert!(
        seen.contains("THE-FILE-CONTENTS"),
        "the file contents were dropped between the two turns: {seen}"
    );
    assert!(
        seen.contains("tool_use"),
        "the tool call itself did not survive: {seen}"
    );
    assert!(
        seen.contains("why does it do that?"),
        "the follow-up never reached the model: {seen}"
    );
    // And the same thing is what the agent is left holding.
    assert!(rendered(&conversation).contains("THE-FILE-CONTENTS"));
    assert!(unmatched(&sent).is_empty(), "{:?}", unmatched(&sent));
    assert!(alternates(&sent), "{sent:#?}");
}

/// The goal boundary is a user message, not a new context.
///
/// Stated separately from the test above because it is the property a future
/// "tidy the history" change would break first: every goal in the session is
/// still in the list, in order, as the words the user typed.
#[tokio::test]
async fn every_goal_of_the_session_is_still_in_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let fake = Fake::new(vec![
        text("one.\n\nGOAL COMPLETE"),
        text("two.\n\nGOAL COMPLETE"),
        text("three.\n\nGOAL COMPLETE"),
    ]);

    let (_, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![]),
        &fake,
        budgets(),
        &[
            Goal::new("first thing"),
            Goal::new("second thing"),
            Goal::new("third thing"),
        ],
        &SessionLog::none(),
    )
    .await;

    let seen = rendered(&conversation);
    for goal in ["first thing", "second thing", "third thing"] {
        assert!(seen.contains(goal), "{goal} is missing: {seen}");
    }
    assert!(alternates(&conversation), "{conversation:#?}");
}

// endregion: The conversation

// region: Nothing unanswered reaches the wire
// ---------------------------------------------------------------------------
// Nothing unanswered reaches the wire
//
// Carrying the tool traffic across goals is only safe while every `tool_use`
// keeps its result. A goal that dies between the model call and the tools is
// where that breaks, so that is what these drive.
// ---------------------------------------------------------------------------

/// A goal killed by its token budget on a turn that had called a tool: the
/// results were never produced, so the turn cannot be carried and is dropped
/// whole. The next goal must still be sendable.
#[tokio::test]
async fn a_turn_whose_tools_never_ran_is_not_carried_into_the_next_goal() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let mut b = budgets();
    b.max_tokens = 5;
    let fake = Fake::new(vec![
        // Crosses the budget on the way in, so the loop breaks holding an
        // assistant turn whose `tool_use` nothing will ever answer.
        call("Fine", json!({ "x": "1" })).costing(10),
        text("second goal.\n\nGOAL COMPLETE").costing(1),
    ]);

    let (outs, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![fine]),
        &fake,
        b,
        &[Goal::new("first goal"), Goal::new("second goal")],
        &SessionLog::none(),
    )
    .await;

    assert_eq!(outs[0].ending, Ending::Tokens);
    assert!(
        unmatched(&conversation).is_empty(),
        "{:?}",
        unmatched(&conversation)
    );
    assert!(alternates(&conversation), "{conversation:#?}");
    let sent = fake.last_messages();
    assert!(unmatched(&sent).is_empty(), "{:?}", unmatched(&sent));
    assert!(alternates(&sent), "{sent:#?}");
}

/// A completed tool round-trip is carried whole — the positive control for the
/// test above, which a loop that dropped every turn would otherwise pass.
#[tokio::test]
async fn a_completed_round_trip_is_carried_whole() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let fake = Fake::new(vec![
        call("Fine", json!({ "x": "1" })),
        text("done.\n\nGOAL COMPLETE"),
        text("and again.\n\nGOAL COMPLETE"),
    ]);

    let (_, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![fine]),
        &fake,
        budgets(),
        &[Goal::new("first goal"), Goal::new("second goal")],
        &SessionLog::none(),
    )
    .await;

    let seen = rendered(&conversation);
    assert!(seen.contains("Fine ran"), "{seen}");
    assert!(unmatched(&conversation).is_empty());
    assert!(alternates(&conversation), "{conversation:#?}");
}

// endregion: Nothing unanswered reaches the wire

// region: Compaction
// ---------------------------------------------------------------------------
// Compaction
//
// What keeps a growing conversation from being a slow leak. It is lossy on
// purpose, so both halves are asserted: what survives, and what does not.
// ---------------------------------------------------------------------------

/// Over the context cap, the oldest goals are replaced by the goal and the
/// answer — which is what the loop used to do to *every* goal, immediately.
/// The tool traffic goes; the words do not.
///
/// **The summariser is refused on purpose here, and that is what makes this
/// test the control it was written to be.** Compaction asks the provider first
/// now, and its answer would be the replacement — so a fixture that let the
/// call succeed would assert about the model's words and not about the collapse
/// this test exists for. The model path has four tests of its own further down.
#[tokio::test]
async fn compaction_drops_the_oldest_tool_traffic_and_keeps_the_words() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    // A file-sized result, because that is what compaction exists to shed and
    // because the replacement text is not free: compacting a goal whose whole
    // traffic is shorter than the note saying it is gone would make the
    // conversation longer, and the loop refuses to do it. See the test below.
    let body = format!(
        "AAAA-FIRST-FILE-BODY\n{}",
        "fn f() { /* body */ }\n".repeat(200)
    );
    let (reader, _) = TestTool::returning("Read", body);
    let mut b = budgets();
    // Well under the first goal's traffic and well over what is left of it
    // after compaction.
    b.max_context = 400;
    let fake = Fake::new(vec![
        call("Read", json!({ "file_path": "a.rs" })),
        text("the first file parses cookies.\n\nGOAL COMPLETE"),
        rejected("no summariser in this fixture"),
        text("the second answer.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "compacting").unwrap();

    let (_, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![reader]),
        &fake,
        b,
        &[Goal::new("read a.rs"), Goal::new("and now the second goal")],
        &log,
    )
    .await;

    let sent = rendered(&fake.last_messages());
    assert!(
        !sent.contains("AAAA-FIRST-FILE-BODY"),
        "the file body was not compacted away: {sent}"
    );
    assert!(
        sent.contains("read a.rs"),
        "the goal was thrown away with the traffic: {sent}"
    );
    assert!(
        sent.contains("the first file parses cookies"),
        "the answer was thrown away with the traffic: {sent}"
    );
    assert!(
        sent.contains("Read anything you need again"),
        "the model was not told the results are gone: {sent}"
    );
    assert!(alternates(&conversation), "{conversation:#?}");
    assert!(unmatched(&conversation).is_empty());

    // It is recorded, with the replacement in the record, so a resume of this
    // session rebuilds the conversation that was actually sent.
    let records = SessionLog::read(log.path()).unwrap();
    let compacted: Vec<&Value> = records
        .iter()
        .filter(|r| r["kind"] == "compacted")
        .collect();
    assert_eq!(compacted.len(), 1, "{records:#?}");
    assert!(compacted[0]["drop_messages"].as_u64().unwrap() > 0);
    assert_eq!(
        emma::session::fold(log.path()).unwrap(),
        conversation,
        "the folded session is not the conversation that was held"
    );
}

/// Compaction that cannot win does nothing rather than churning: a session
/// whose whole conversation is already a summary has nothing left to give, and
/// a compactor that kept trying would log a record per model call forever.
///
/// **Since compaction asks a model, churn costs money and not only records**,
/// and the script is what proves the cost is bounded. Three goals over a cap of
/// one token: the summariser is asked exactly twice, once for each *new*
/// chapter count, and never again while the answer cannot change. Every
/// summarisation call is scripted as a refusal, so the deterministic path is
/// what runs and the entries left over are the three goals' own answers. Five
/// entries, five calls — a build that dropped `Agent::no_summary_at` asks on
/// every request instead, runs the script out, and the third goal fails.
#[tokio::test]
async fn compaction_stops_when_there_is_nothing_left_to_win() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let mut b = budgets();
    b.max_context = 1;
    let fake = Fake::new(vec![
        text("one.\n\nGOAL COMPLETE"),
        rejected("the summariser is not part of this test"),
        text("two.\n\nGOAL COMPLETE"),
        rejected("the summariser is not part of this test"),
        text("three.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "floor").unwrap();

    let (outs, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![]),
        &fake,
        b,
        &[Goal::new("a"), Goal::new("b"), Goal::new("c")],
        &log,
    )
    .await;

    assert!(outs.iter().all(|o| o.ending == Ending::Done));
    assert!(alternates(&conversation), "{conversation:#?}");
    let records = SessionLog::read(log.path()).unwrap();
    let compactions = records.iter().filter(|r| r["kind"] == "compacted").count();
    assert!(compactions <= 2, "compaction churned: {compactions}");
    // Three goals and two summarisation calls, and no more: the cost memo is
    // what stops a session pinned against its cap paying for an answer it
    // throws away on every single request.
    assert_eq!(
        fake.calls(),
        5,
        "the summariser was asked more often than the conversation changed"
    );
    assert_eq!(emma::session::fold(log.path()).unwrap(), conversation);
}

// endregion: Compaction

// region: Model-summarised compaction
// ---------------------------------------------------------------------------
// Model-summarised compaction
//
// Compaction asks the provider for the summary and writes the answer into the
// conversation and into the `compacted` record. What these tests hold down is
// that the model path is an improvement on the deterministic one and never a
// dependency of it: a compaction that needed the model to be healthy would fail
// exactly when the model is being leaned on hardest.
//
// Every provider here is scripted. Nothing in this file reaches a network.
// ---------------------------------------------------------------------------

/// The fixture the four tests below share: one goal that reads a file-sized
/// result, then a second goal whose opening is over the cap and compacts the
/// first. `script` is what the provider answers, in order, starting from the
/// summarisation call the second goal's compaction makes.
async fn compacting_fixture(
    dir: &Path,
    log: &SessionLog,
    max_tokens: i64,
    script: Vec<Say>,
) -> (Arc<Fake>, Vec<Message>) {
    let root = empty_harness(dir);
    let body = format!(
        "AAAA-FIRST-FILE-BODY\n{}",
        "fn f() { /* body */ }\n".repeat(200)
    );
    let (reader, _) = TestTool::returning("Read", body);
    let mut b = budgets();
    b.max_context = 400;
    b.max_tokens = max_tokens;
    let mut full = vec![
        call("Read", json!({ "file_path": "a.rs" })),
        text("the first file parses cookies.\n\nGOAL COMPLETE"),
    ];
    full.extend(script);
    let fake = Fake::new(full);
    let (_, conversation) = drive_goals(
        &root,
        dir,
        registry(vec![reader]),
        &fake,
        b,
        &[Goal::new("read a.rs"), Goal::new("and now the second goal")],
        log,
    )
    .await;
    (fake, conversation)
}

/// The `compacted` record, of which these tests produce exactly one.
fn only_compacted(log: &SessionLog) -> Value {
    let records = SessionLog::read(log.path()).unwrap();
    let found: Vec<&Value> = records
        .iter()
        .filter(|r| r["kind"] == "compacted")
        .collect();
    assert_eq!(found.len(), 1, "{records:#?}");
    found[0].clone()
}

/// The headline. A reachable provider is asked to summarise the goals being
/// folded, and its answer is what goes into the conversation and into the
/// record — word for word, because `session::fold` replays the record rather
/// than summarising again, and a resume that re-derived the text would rebuild
/// a conversation the run never held.
#[tokio::test]
async fn compaction_records_the_model_summary_and_a_resume_replays_it() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "model-summary").unwrap();
    let (fake, conversation) = compacting_fixture(
        dir.path(),
        &log,
        1_000_000,
        vec![
            text("ZZZZ-SUMMARY: a.rs parses cookies. Nothing is outstanding."),
            text("the second answer.\n\nGOAL COMPLETE"),
        ],
    )
    .await;

    let sent = rendered(&fake.last_messages());
    assert!(
        sent.contains("ZZZZ-SUMMARY: a.rs parses cookies"),
        "the model's summary is not in the conversation: {sent}"
    );
    assert!(
        !sent.contains("AAAA-FIRST-FILE-BODY"),
        "the file body survived a compaction: {sent}"
    );
    // The note stays under a model-written summary for the same reason it stays
    // under a preserved answer: a model that does not know the tool results are
    // gone answers from a memory of them.
    assert!(
        sent.contains("re-read files rather"),
        "the model was not told the results are gone: {sent}"
    );
    // But it is a different note. `COMPACTED_NOTE` opens by promising the text
    // above it is preserved word for word, and over a summary that is false.
    assert!(
        sent.contains("a summary of earlier goals"),
        "a model-written summary was not labelled as one: {sent}"
    );
    assert!(
        !sent.contains("preserved word for word"),
        "the note claims a summary is a quotation: {sent}"
    );

    let record = only_compacted(&log);
    assert_eq!(record["summary_source"], "model", "{record:#?}");
    assert!(record["summary_fallback"].is_null(), "{record:#?}");
    assert!(
        record["summary_tokens"].as_i64().unwrap() > 0,
        "a model call was made and the record priced it at nothing: {record:#?}"
    );
    let recorded = format!("{}", record["messages"]);
    assert!(
        recorded.contains("ZZZZ-SUMMARY: a.rs parses cookies"),
        "the record does not carry the text that was sent: {recorded}"
    );

    // The property the whole design turns on. The fold splices the recorded
    // messages in and never calls a model, so a resumed session is the session
    // that ran — and it reads this record through the same two keys it always
    // read, `drop_messages` and `messages`, neither of which moved.
    assert_eq!(
        emma::session::fold(log.path()).unwrap(),
        conversation,
        "the folded session is not the conversation that was held"
    );
}

/// The provider refuses the summarisation call and compaction happens anyway,
/// deterministically, with the record naming the path and the reason. This is
/// the property that keeps compaction from becoming a thing that needs the
/// model to be healthy.
#[tokio::test]
async fn a_refused_summarisation_falls_back_and_the_record_says_which_path_ran() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "fallback").unwrap();
    let (fake, conversation) = compacting_fixture(
        dir.path(),
        &log,
        1_000_000,
        vec![
            rejected("the summariser is unreachable"),
            text("the second answer.\n\nGOAL COMPLETE"),
        ],
    )
    .await;

    let sent = rendered(&fake.last_messages());
    assert!(
        sent.contains("the first file parses cookies"),
        "the deterministic replacement did not keep the answer: {sent}"
    );
    assert!(
        !sent.contains("AAAA-FIRST-FILE-BODY"),
        "the fallback did not compact anything: {sent}"
    );

    let record = only_compacted(&log);
    assert_eq!(record["summary_source"], "deterministic", "{record:#?}");
    let why = record["summary_fallback"].as_str().unwrap_or_default();
    assert!(
        why.contains("failed"),
        "the record does not say why the model path was not used: {record:#?}"
    );
    assert_eq!(emma::session::fold(log.path()).unwrap(), conversation);
}

/// Under the budget floor the model is not asked at all, so the summarisation
/// call cannot be the thing that ends the goal it was shortening the
/// conversation for. The provider is scripted with no summary to give, which is
/// how the test proves the call was never made: if it were, the script would
/// hand over the second goal's answer and that goal would fail.
#[tokio::test]
async fn a_goal_with_little_budget_left_skips_the_summariser_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "poor").unwrap();
    let (fake, conversation) = compacting_fixture(
        dir.path(),
        &log,
        // Under the floor from the first call, so the second goal's compaction
        // never reaches the provider.
        1_000,
        vec![text("the second answer.\n\nGOAL COMPLETE")],
    )
    .await;

    let record = only_compacted(&log);
    assert_eq!(record["summary_source"], "deterministic", "{record:#?}");
    assert_eq!(
        record["summary_tokens"], 0,
        "a skipped call was charged for: {record:#?}"
    );
    let why = record["summary_fallback"].as_str().unwrap_or_default();
    assert!(
        why.contains("budget"),
        "the record does not say the budget was the reason: {record:#?}"
    );
    assert!(
        rendered(&fake.last_messages()).contains("the first file parses cookies"),
        "the deterministic replacement did not run"
    );
    assert_eq!(emma::session::fold(log.path()).unwrap(), conversation);
}

/// A summariser that answers with more words than the conversation it was
/// given has produced a plausible sentence and no saving, and compaction exists
/// for the saving. The answer is paid for and then dropped, and the record says
/// so — which is the only way anybody would ever find out it happened.
#[tokio::test]
async fn a_summary_larger_than_what_it_replaces_is_paid_for_and_not_used() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "verbose").unwrap();
    let (fake, conversation) = compacting_fixture(
        dir.path(),
        &log,
        1_000_000,
        vec![
            text(&format!(
                "ZZZZ-BLOATED-SUMMARY {}",
                "and then, and then ".repeat(3_000)
            )),
            text("the second answer.\n\nGOAL COMPLETE"),
        ],
    )
    .await;

    let record = only_compacted(&log);
    assert_eq!(record["summary_source"], "deterministic", "{record:#?}");
    assert!(
        record["summary_fallback"]
            .as_str()
            .unwrap_or_default()
            .contains("not smaller"),
        "the record does not say the summary was dropped for its size: {record:#?}"
    );
    assert!(
        record["summary_tokens"].as_i64().unwrap() > 0,
        "the call was made and the record prices it at nothing: {record:#?}"
    );
    let sent = rendered(&fake.last_messages());
    assert!(
        !sent.contains("ZZZZ-BLOATED-SUMMARY"),
        "a summary larger than the conversation was sent anyway: {sent}"
    );
    assert!(
        sent.contains("the first file parses cookies"),
        "the deterministic replacement did not run: {sent}"
    );
    assert_eq!(emma::session::fold(log.path()).unwrap(), conversation);
}

/// `Agent::recover_from_a_model_change` compacts to escape a provider that has
/// rejected signed content, and that only works because a replacement carries
/// none. A model-written summary is assistant text and nothing else, exactly as
/// the deterministic answer text is, so the property survives the change.
/// Asserted rather than assumed, because the failure mode is a 400 on the next
/// call.
#[tokio::test]
async fn a_model_summary_carries_no_provider_bound_content() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "unsigned").unwrap();
    let (_, conversation) = compacting_fixture(
        dir.path(),
        &log,
        1_000_000,
        vec![
            text("ZZZZ-SUMMARY: a.rs parses cookies."),
            text("the second answer.\n\nGOAL COMPLETE"),
        ],
    )
    .await;

    // Only the replacement is checked, because a live assistant turn is
    // *supposed* to carry its thinking block.
    let record = only_compacted(&log);
    let replacement: Vec<Message> = serde_json::from_value(record["messages"].clone()).unwrap();
    assert_eq!(record["summary_source"], "model", "{record:#?}");
    assert!(
        !replacement
            .iter()
            .any(|m| m.content.blocks().iter().any(ContentBlock::is_model_bound)),
        "the recorded replacement carries content bound to the model that wrote it: \
         {replacement:#?}"
    );
    assert!(alternates(&conversation), "{conversation:#?}");
    assert!(unmatched(&conversation).is_empty());
}

// endregion: Model-summarised compaction

// region: Compaction inside a running goal
// ---------------------------------------------------------------------------
// Compaction inside a running goal
//
// The defect these were written from, observed live: a single goal that read
// twenty-five files ran a 129,254 token request under a 96,000 token cap and
// never wrote a `compacted` record. Chapter compaction could not help, because
// chapters hold only *finished* goals and every one of those reads was in the
// goal still running. The cure is shedding: the oldest tool results of the
// in-flight goal are replaced by a note, in place, so the pairing the API
// checks is untouched.
// ---------------------------------------------------------------------------

/// The wire bytes of a message list, which is what the cap is really about.
fn wire_bytes(messages: &[Message]) -> usize {
    messages.iter().map(|m| m.content.to_string().len()).sum()
}

/// Four reads inside one goal, over the cap. The oldest bodies must go and the
/// newest must stay.
///
/// Every read is a differently named tool so the bodies are distinguishable:
/// the claim is about *which* results were shed, and one shared body could not
/// tell the oldest from the newest.
#[tokio::test]
async fn a_single_long_goal_sheds_its_own_oldest_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let body = |mark: &str| format!("{mark}\n{}", "fn f() { /* body */ }\n".repeat(400));
    let (r1, _) = TestTool::returning("ReadOne", body("AAAA-FIRST-BODY"));
    let (r2, _) = TestTool::returning("ReadTwo", body("BBBB-SECOND-BODY"));
    let (r3, _) = TestTool::returning("ReadThree", body("CCCC-THIRD-BODY"));
    let (r4, _) = TestTool::returning("ReadFour", body("DDDD-FOURTH-BODY"));
    let mut b = budgets();
    b.max_context = 1_500;
    // The provider's own input count for each call, which is what the threshold
    // reads. It crosses the cap on the third call, exactly as the live run did.
    let fake = Fake::new(vec![
        call("ReadOne", json!({ "file_path": "a.rs" })).costing(100),
        call("ReadTwo", json!({ "file_path": "b.rs" })).costing(2_000),
        call("ReadThree", json!({ "file_path": "c.rs" })).costing(4_000),
        call("ReadFour", json!({ "file_path": "d.rs" })).costing(6_000),
        text("the audit is done.\n\nGOAL COMPLETE").costing(6_000),
    ]);
    let log = SessionLog::open(dir.path(), "shedding").unwrap();

    let (outs, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![r1, r2, r3, r4]),
        &fake,
        b,
        &[Goal::new("audit the auth stack")],
        &log,
    )
    .await;

    assert_eq!(outs[0].ending, Ending::Done);
    let sent = fake.last_messages();
    let seen = rendered(&sent);
    assert!(
        !seen.contains("AAAA-FIRST-BODY"),
        "the oldest read of the running goal was never shed: {seen}"
    );
    assert!(
        seen.contains("DDDD-FOURTH-BODY"),
        "the newest read was shed, which is the one result the model has not \
         acted on yet: {seen}"
    );
    assert!(
        seen.contains("audit the auth stack"),
        "the goal itself was thrown away with the traffic: {seen}"
    );
    assert!(
        seen.contains("dropped to save context"),
        "the model was not told a result had gone: {seen}"
    );
    assert!(
        seen.contains("ReadOne"),
        "the tool call the shed result answers is gone, which is a 400: {seen}"
    );
    // The two shapes the API refuses, on the shed path.
    assert!(unmatched(&sent).is_empty(), "{:?}", unmatched(&sent));
    assert!(alternates(&sent), "{sent:#?}");
    assert!(unmatched(&conversation).is_empty());
    assert!(alternates(&conversation), "{conversation:#?}");

    let records = SessionLog::read(log.path()).unwrap();
    let shed: Vec<&Value> = records.iter().filter(|r| r["kind"] == "shed").collect();
    assert!(!shed.is_empty(), "nothing was recorded: {records:#?}");
    assert!(
        shed[0]["estimated_before"].as_i64().unwrap()
            > shed[0]["estimated_after"].as_i64().unwrap()
    );
    assert!(shed[0]["results"].as_u64().unwrap() > 0);
    // No churn. Every later call is over the cap too, so a shed that could not
    // tell an already-shed result from a live one would rewrite the same block
    // and log a record on every single call for the rest of the session.
    let mut ids: Vec<String> = shed
        .iter()
        .flat_map(|r| r["results_shed"].as_array().cloned().unwrap_or_default())
        .map(|row| row["tool_use_id"].as_str().unwrap_or_default().to_string())
        .collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "a result was shed twice: {shed:#?}");

    // The `shed` arm in `session.rs` replays the record: a resumed session is
    // the one that was sent, shed results and all. Every other edit to the
    // conversation has the same guarantee, so this test asserts it too, and
    // separately that the record is complete enough to replay from (a
    // `tool_use_id` and the replacement text for every block that went).
    assert_eq!(
        emma::session::fold(log.path()).unwrap(),
        conversation,
        "the folded session is not the conversation that was held"
    );
    for row in shed
        .iter()
        .flat_map(|r| r["results_shed"].as_array().cloned().unwrap_or_default())
    {
        assert!(
            !row["tool_use_id"].as_str().unwrap_or_default().is_empty(),
            "a shed row names no block, so no fold could replay it: {row}"
        );
        assert!(
            row["content"]
                .as_str()
                .unwrap_or_default()
                .starts_with("[Context note: this tool result was dropped"),
            "a shed row does not carry the text that replaced the result: {row}"
        );
    }

    // The same script with the cap out of reach, so the saving is measured
    // against the request this build would otherwise have sent rather than
    // against an assertion that it is smaller than nothing.
    let (r1, _) = TestTool::returning("ReadOne", body("AAAA-FIRST-BODY"));
    let (r2, _) = TestTool::returning("ReadTwo", body("BBBB-SECOND-BODY"));
    let (r3, _) = TestTool::returning("ReadThree", body("CCCC-THIRD-BODY"));
    let (r4, _) = TestTool::returning("ReadFour", body("DDDD-FOURTH-BODY"));
    let unshed = Fake::new(vec![
        call("ReadOne", json!({ "file_path": "a.rs" })).costing(100),
        call("ReadTwo", json!({ "file_path": "b.rs" })).costing(2_000),
        call("ReadThree", json!({ "file_path": "c.rs" })).costing(4_000),
        call("ReadFour", json!({ "file_path": "d.rs" })).costing(6_000),
        text("the audit is done.\n\nGOAL COMPLETE").costing(6_000),
    ]);
    drive_goals(
        &root,
        dir.path(),
        registry(vec![r1, r2, r3, r4]),
        &unshed,
        budgets(),
        &[Goal::new("audit the auth stack")],
        &SessionLog::none(),
    )
    .await;

    let with_shedding = wire_bytes(&sent);
    let without = wire_bytes(&unshed.last_messages());
    assert!(
        with_shedding * 2 < without,
        "shedding saved less than half the request: {with_shedding} against {without}"
    );
    println!("last request: {without} bytes unshed, {with_shedding} shed");
}

/// The control: under the cap, nothing is shed and every body is still there.
///
/// Without this the test above passes just as well against a build that sheds
/// unconditionally, which would be the same amnesia the compactor was written
/// to stop being.
#[tokio::test]
async fn a_goal_under_the_cap_keeps_every_result() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (r1, _) = TestTool::returning("ReadOne", "AAAA-FIRST-BODY");
    let (r2, _) = TestTool::returning("ReadTwo", "BBBB-SECOND-BODY");
    let fake = Fake::new(vec![
        call("ReadOne", json!({ "file_path": "a.rs" })).costing(10),
        call("ReadTwo", json!({ "file_path": "b.rs" })).costing(10),
        text("done.\n\nGOAL COMPLETE").costing(10),
    ]);
    let log = SessionLog::open(dir.path(), "no-shedding").unwrap();

    let (_, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![r1, r2]),
        &fake,
        budgets(),
        &[Goal::new("read both")],
        &log,
    )
    .await;

    let seen = rendered(&conversation);
    assert!(seen.contains("AAAA-FIRST-BODY"), "{seen}");
    assert!(seen.contains("BBBB-SECOND-BODY"), "{seen}");
    let records = SessionLog::read(log.path()).unwrap();
    assert_eq!(records.iter().filter(|r| r["kind"] == "shed").count(), 0);
}

// endregion: Compaction inside a running goal

// region: What a truncated result says
// ---------------------------------------------------------------------------
// What a truncated result says
//
// Once. The `tools/fs` tools write `[truncated: reason]` into their content and
// flag the outcome, and the runtime appended the same sentence again on top of
// it: every one of the 24 truncated `Read` results and both truncated `Glob`s
// in the audited session logs carried it twice.
// ---------------------------------------------------------------------------

/// A tool that both writes the truncation line into its content and sets the
/// flag, which is what `tools/fs` does.
///
/// Local to this file rather than a `TestTool` knob, because `support/mod.rs`
/// is shared with five other suites and this is the only one that needs it.
struct Truncating {
    body: String,
    reason: String,
}

#[async_trait::async_trait]
impl Tool for Truncating {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &str {
        "a tool that truncates and says so, the way tools/fs does"
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "file_path": { "type": "string" } } })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            reaches_network: false,
            idempotent: true,
        }
    }

    fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
        Ok(())
    }

    fn network_target(&self, _args: &Value) -> Option<NetworkTarget> {
        None
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        _args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        // Both halves, exactly as `tools/fs` does it: the line in the content
        // and the flag on the outcome.
        let content = format!("{}\n[truncated: {}]\n", self.body, self.reason);
        Ok(Ok(
            ToolOutcome::new(content).truncated_because(self.reason.clone())
        ))
    }
}

#[tokio::test]
async fn a_truncated_result_says_so_once_rather_than_twice() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let reader: Arc<dyn Tool> = Arc::new(Truncating {
        body: "BBBB-PARTIAL-BODY".into(),
        reason: "the file is longer than the read limit".into(),
    });
    let fake = Fake::new(vec![
        call("Read", json!({ "file_path": "a.rs" })),
        text("read it.\n\nGOAL COMPLETE"),
    ]);
    let log = SessionLog::open(dir.path(), "truncation").unwrap();

    let (_, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![reader]),
        &fake,
        budgets(),
        &[Goal::new("read a.rs")],
        &log,
    )
    .await;

    let seen = rendered(&conversation);
    assert_eq!(
        seen.matches("the file is longer than the read limit")
            .count(),
        1,
        "the truncation sentence reached the model more than once: {seen}"
    );
    // Said at all, which is the half the guard must not break: a tool that sets
    // the flag without writing the line still gets the runtime's note.
    assert!(seen.contains("[truncated:"), "{seen}");
}

/// The other half of the guard, as its own test rather than as a comment: a
/// tool that reports the flag and writes nothing still gets the note.
///
/// `TestTool::returning` is not that tool — it never truncates — so this uses
/// the local one with an empty line of its own.
#[tokio::test]
async fn a_tool_that_only_sets_the_flag_still_gets_the_note() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let reader: Arc<dyn Tool> = Arc::new(Silent {
        reason: "50 of 70 hits shown".into(),
    });
    let fake = Fake::new(vec![
        call("Read", json!({ "file_path": "a.rs" })),
        text("read it.\n\nGOAL COMPLETE"),
    ]);

    let (_, conversation) = drive_goals(
        &root,
        dir.path(),
        registry(vec![reader]),
        &fake,
        budgets(),
        &[Goal::new("read a.rs")],
        &SessionLog::none(),
    )
    .await;

    let seen = rendered(&conversation);
    assert_eq!(
        seen.matches("50 of 70 hits shown").count(),
        1,
        "the note was suppressed for a tool that never wrote one: {seen}"
    );
}

/// A tool that sets the truncation flag and writes no line of its own.
struct Silent {
    reason: String,
}

#[async_trait::async_trait]
impl Tool for Silent {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &str {
        "a tool that truncates and does not say so"
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "file_path": { "type": "string" } } })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            reaches_network: false,
            idempotent: true,
        }
    }

    fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
        Ok(())
    }

    fn network_target(&self, _args: &Value) -> Option<NetworkTarget> {
        None
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        _args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(Ok(
            ToolOutcome::new("CCCC-PARTIAL-BODY").truncated_because(self.reason.clone())
        ))
    }
}

// endregion: What a truncated result says
