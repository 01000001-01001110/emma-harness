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
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Setup};
use emma::approval::Approvals;
use emma::goal::{Goal, MarkerClaim};
use emma::session::SessionLog;
use emma::term::Term;
use emma_harness::{Flavor, Harness};
use emma_llm::{Caching, ContentBlock, Message, Mode, Role};
use emma_tool_api::Registry;
use serde_json::{json, Value};

use support::{call, empty_harness, registry, text, Fake, TestTool};

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
    provider: &Fake,
    budgets: Budgets,
    goals: &[Goal],
    log: &SessionLog,
) -> (Vec<Outcome>, Vec<Message>) {
    let harness = Harness::load_selecting(root, Flavor::Emma, None).unwrap();
    let approvals = Approvals::unattended();
    let term = Term::silent();
    let mut agent = Agent::new(Setup {
        provider,
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
#[tokio::test]
async fn compaction_stops_when_there_is_nothing_left_to_win() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let mut b = budgets();
    b.max_context = 1;
    let fake = Fake::new(vec![
        text("one.\n\nGOAL COMPLETE"),
        text("two.\n\nGOAL COMPLETE"),
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
    assert_eq!(emma::session::fold(log.path()).unwrap(), conversation);
}

// endregion: Compaction
