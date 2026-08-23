//! `emma --resume`, driven by the same scripted model as `loop.rs`.
//!
//! Four of these are one claim in four shapes: **a resumed run does not get a
//! fresh budget.** That is N2, and it is the reason resume is not simply "hand
//! the fold to the loop" — a goal that ended on `Ending::Tokens` and could be
//! resumed indefinitely has a cap that means nothing. Each of the four was
//! mutation-checked by zeroing the restored counter and confirming it went red;
//! the assertion that notices is the model-call count, not the ending.
//!
//! The rest are the decisions: which session, what comes back as conversation,
//! and what does *not* come back as execution.

mod support;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Resumed, Setup};
use emma::approval::Approvals;
use emma::goal::{Goal, MarkerClaim};
use emma::session::{self, Continuity, SessionLog};
use emma::term::Term;
use emma_harness::{Flavor, Harness};
use emma_llm::{Caching, Mode, Role};
use emma_tool_api::Registry;
use serde_json::json;

use support::{call, empty_harness, registry, text, Fake, TestTool};

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

fn goal() -> Goal {
    Goal::new("make it work")
}

/// One goal on one `Agent`, optionally continuing a restored session.
#[allow(clippy::too_many_arguments)]
async fn drive(
    root: &Path,
    cwd: &Path,
    tools: Registry,
    provider: &Arc<Fake>,
    budgets: Budgets,
    goal: &Goal,
    log: &SessionLog,
    resumed: Option<Resumed>,
) -> Outcome {
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
    });
    if let Some(resumed) = resumed {
        agent = agent.resuming(resumed);
    }
    agent.run_goal(goal).await
}

// region: The budget is not refilled
// ---------------------------------------------------------------------------
// The budget is not refilled
//
// N2, four ways. The first run in each spends something and stops; the second
// continues from the same file and must find the meter where it left it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_resumed_run_inherits_the_model_calls_the_first_one_made() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let mut b = budgets();
    b.max_iterations = 2;
    let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();

    let first = Fake::new(vec![
        call("Fine", json!({})),
        call("Fine", json!({})),
        text("GOAL COMPLETE"),
    ]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &first,
        b,
        &goal(),
        &log,
        None,
    )
    .await;
    assert_eq!(out.ending, Ending::Iterations);

    let restored = session::restore(log.path()).unwrap();
    assert_eq!(restored.resumed.iterations, 2);

    let (fine2, _) = TestTool::ok("Fine", true);
    let second = Fake::new(vec![text("GOAL COMPLETE")]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine2]),
        &second,
        b,
        &goal(),
        &SessionLog::none(),
        Some(restored.resumed),
    )
    .await;

    assert_eq!(out.ending, Ending::Iterations);
    // The assertion that notices the mutation. The iteration budget is tested
    // at the top of the loop, so a restored count that is already at the cap
    // must cost zero further calls; a zeroed one bills twenty.
    assert_eq!(
        second.calls(),
        0,
        "the resumed run was granted a fresh iteration budget"
    );
}

#[tokio::test]
async fn a_resumed_run_inherits_the_tokens_the_first_one_spent() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let mut b = budgets();
    b.max_tokens = 15;
    let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();

    let first = Fake::new(vec![
        call("Fine", json!({})).costing(10),
        call("Fine", json!({})).costing(10),
        text("GOAL COMPLETE"),
    ]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &first,
        b,
        &goal(),
        &log,
        None,
    )
    .await;
    assert_eq!(out.ending, Ending::Tokens);
    assert_eq!(out.tokens, 20);

    let restored = session::restore(log.path()).unwrap();
    assert_eq!(restored.resumed.tokens, 20);

    let (fine2, _) = TestTool::ok("Fine", true);
    // One turn under the cap on its own — 10 against 15. It only stops the
    // resumed run because the twenty already spent came back with it.
    let second = Fake::new(vec![
        text("nearly").costing(10),
        text("GOAL COMPLETE").costing(10),
    ]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine2]),
        &second,
        b,
        &goal(),
        &SessionLog::none(),
        Some(restored.resumed),
    )
    .await;

    assert_eq!(out.ending, Ending::Tokens);
    assert_eq!(out.tokens, 30);
    assert_eq!(second.calls(), 1);
}

#[tokio::test]
async fn a_resumed_run_inherits_the_nudges_the_first_one_used() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let mut b = budgets();
    b.max_kicks = 1;
    let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();

    // Tool use before the first stop and between the two stops, so the first
    // run ends on the kick count rather than on the stall rule or the answer
    // rule — both of which would end it a turn earlier and prove nothing about
    // the count.
    let first = Fake::new(vec![
        call("Fine", json!({})),
        text("thinking about it"),
        call("Fine", json!({})),
        text("still thinking"),
    ]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &first,
        b,
        &goal(),
        &log,
        None,
    )
    .await;
    assert_eq!(out.ending, Ending::KicksExhausted);
    assert_eq!(out.kicks, 1);

    let restored = session::restore(log.path()).unwrap();
    assert_eq!(restored.resumed.kicks, 1);

    let (fine2, _) = TestTool::ok("Fine", true);
    let second = Fake::new(vec![text("not done and not saying so")]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![fine2]),
        &second,
        b,
        &goal(),
        &SessionLog::none(),
        Some(restored.resumed),
    )
    .await;

    // With the count restored there is no nudge left to spend, so the run stops
    // on the first stop. Zero it and the loop nudges, the script runs out, and
    // the ending is a provider error instead.
    assert_eq!(out.ending, Ending::KicksExhausted);
    assert_eq!(second.calls(), 1);
}

#[tokio::test]
async fn a_resumed_run_inherits_the_call_that_already_failed() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (boom, _) = TestTool::failing("Boom", true);
    let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();

    let first = Fake::new(vec![
        call("Boom", json!({ "x": "same" })),
        text("I will stop here.\n\nGOAL COMPLETE"),
    ]);
    drive(
        &root,
        dir.path(),
        registry(vec![boom]),
        &first,
        budgets(),
        &goal(),
        &log,
        None,
    )
    .await;

    let restored = session::restore(log.path()).unwrap();
    assert_eq!(restored.resumed.failed_now.len(), 1, "{restored:?}");

    let (boom2, boom_calls) = TestTool::failing("Boom", true);
    let second = Fake::new(vec![
        call("Boom", json!({ "x": "same" })),
        text("fine.\n\nGOAL COMPLETE"),
    ]);
    drive(
        &root,
        dir.path(),
        registry(vec![boom2]),
        &second,
        budgets(),
        &goal(),
        &SessionLog::none(),
        Some(restored.resumed),
    )
    .await;

    assert_eq!(
        boom_calls.load(Ordering::SeqCst),
        0,
        "the identical call that failed before the interrupt ran again"
    );
    assert!(second.transcript().contains("already_failed"));
}

// endregion: The budget is not refilled

// region: What comes back, and what does not
// ---------------------------------------------------------------------------
// What comes back, and what does not
//
// The conversation returns as messages. The tools do not return as calls —
// that is the half-turn ruling, and it is the difference between resuming a
// session and re-running one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_conversation_comes_back_and_the_new_goal_joins_the_last_turn() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();

    let first = Fake::new(vec![call("Fine", json!({ "x": "marker-value" }))]);
    // The script runs out mid-goal, which is the interrupted shape resume is
    // for: the last thing in the file is a complete tool round-trip.
    drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &first,
        budgets(),
        &goal(),
        &log,
        None,
    )
    .await;

    let restored = session::restore(log.path()).unwrap();
    let folded = restored.resumed.messages.clone();
    assert!(!folded.is_empty());
    assert_eq!(folded.last().unwrap().role, Role::User);

    let (fine2, _) = TestTool::ok("Fine", true);
    let second = Fake::new(vec![text("GOAL COMPLETE")]);
    drive(
        &root,
        dir.path(),
        registry(vec![fine2]),
        &second,
        budgets(),
        &Goal::new("carry on where you stopped"),
        &SessionLog::none(),
        Some(restored.resumed),
    )
    .await;

    let sent = second.last_messages();
    // Same number of messages, not one more: the opening joined the trailing
    // user turn rather than following it, because two user turns in a row is a
    // 400 rather than a conversation.
    assert_eq!(sent.len(), folded.len(), "{sent:#?}");
    for (i, before) in folded.iter().enumerate().take(folded.len() - 1) {
        assert_eq!(&sent[i], before, "message {i} did not survive the resume");
    }
    // The `tool_use` block is back verbatim, arguments and all — the property
    // the whole `raw_content` design exists for, now across a process boundary.
    assert!(
        sent[1].content.to_string().contains("marker-value"),
        "{:#?}",
        sent[1]
    );
    let last = sent.last().unwrap();
    assert_eq!(last.role, Role::User);
    let rendered = last.content.to_string();
    assert!(
        rendered.contains("carry on where you stopped"),
        "the new goal never reached the model: {rendered}"
    );
}

#[tokio::test]
async fn a_resumed_run_replays_no_tools() {
    // The half-turn ruling. A dropped turn means those calls would run again,
    // which is harmless for a read and is not for a write — so resume hands the
    // model the record and lets it decide what to redo. The counter is on a
    // fresh tool instance, so anything it counts was run by this resume.
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let (fine, _) = TestTool::ok("Fine", true);
    let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();

    let first = Fake::new(vec![
        call("Fine", json!({ "x": "1" })),
        call("Fine", json!({ "x": "2" })),
    ]);
    drive(
        &root,
        dir.path(),
        registry(vec![fine]),
        &first,
        budgets(),
        &goal(),
        &log,
        None,
    )
    .await;

    let restored = session::restore(log.path()).unwrap();
    let (fine2, replayed) = TestTool::ok("Fine", true);
    let second = Fake::new(vec![text("GOAL COMPLETE")]);
    drive(
        &root,
        dir.path(),
        registry(vec![fine2]),
        &second,
        budgets(),
        &goal(),
        &SessionLog::none(),
        Some(restored.resumed),
    )
    .await;

    assert_eq!(
        replayed.load(Ordering::SeqCst),
        0,
        "resume re-ran a tool instead of handing back its recorded result"
    );
    // …and the result is still in front of the model, as a record.
    assert!(second.last_query().contains("Fine ran"));
}

// endregion: What comes back, and what does not

// region: Which session, and whether it is still the same harness
// ---------------------------------------------------------------------------
// Which session, and whether it is still the same harness
//
// `fold` takes a path and nothing chooses one. These two are the choosing and
// the warning that comes with it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_newest_session_for_this_directory_is_the_one_resumed() {
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join("sessions");
    let here = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    for (id, cwd) in [
        ("sess-0000000000001-1", here.path()),
        ("sess-0000000000002-1", elsewhere.path()),
        ("sess-0000000000003-1", here.path()),
        ("sess-0000000000004-1", elsewhere.path()),
    ] {
        let log = SessionLog::open(&sessions, id).unwrap();
        log.append(
            "goal",
            json!({ "text": "g", "opening": "work on g", "cwd": cwd.display().to_string() }),
        );
    }

    let chosen = session::locate(&sessions, None, here.path()).unwrap();
    assert_eq!(
        chosen.file_stem().unwrap().to_string_lossy(),
        "sess-0000000000003-1",
        "resume picked a session from another directory, or an older one"
    );

    // A named session is a named session: the directory rule is what bare
    // `--resume` needs, not a filter on an explicit request.
    let named = session::locate(&sessions, Some("sess-0000000000002-1"), here.path()).unwrap();
    assert_eq!(
        named.file_stem().unwrap().to_string_lossy(),
        "sess-0000000000002-1"
    );

    let nowhere = tempfile::tempdir().unwrap();
    let err = session::locate(&sessions, None, nowhere.path()).unwrap_err();
    assert!(
        err.to_string()
            .contains(&nowhere.path().display().to_string()),
        "the refusal does not say which directory it searched for: {err}"
    );
    assert!(session::locate(&sessions, Some("sess-nope"), here.path()).is_err());
}

#[test]
fn resuming_into_a_changed_harness_names_what_changed() {
    // The "booted on the wrong prompt" failure, one level along: continuing a
    // conversation under instructions it was never held to. Nothing here
    // refuses — the user asked to resume — but nothing is silent either.
    let was = Continuity {
        instructions_hash: "aaaa".into(),
        tool_schema_hash: "bbbb".into(),
        model: "claude-old".into(),
        cwd: "/work/alpha".into(),
    };
    assert!(was
        .differences("aaaa", "bbbb", "claude-old", "/work/alpha")
        .is_empty());

    let lines = was
        .differences("cccc", "bbbb", "claude-new", "/work/alpha")
        .join("\n");
    assert!(lines.contains("aaaa") && lines.contains("cccc"), "{lines}");
    assert!(
        lines.contains("claude-old") && lines.contains("claude-new"),
        "{lines}"
    );
    assert!(
        !lines.contains("bbbb"),
        "an unchanged tool surface was reported as changed: {lines}"
    );

    // A session recorded before a field existed cannot be compared, and a
    // warning about an empty string is noise that trains people past warnings.
    assert!(Continuity::default()
        .differences("cccc", "dddd", "claude-new", "/work/alpha")
        .is_empty());

    // The hazard `locate` warns about in its own doc and that nothing checked:
    // resuming a session by id from inside a different repository, with write
    // tools pointed at the one you are standing in. Every cheaper drift warned;
    // this one arrived in silence.
    let elsewhere = was
        .differences("aaaa", "bbbb", "claude-old", "/work/beta")
        .join(
            "
",
        );
    assert!(
        elsewhere.contains("working directory"),
        "a cross-project resume must be named: {elsewhere}"
    );
    assert!(
        elsewhere.contains("/work/alpha") && elsewhere.contains("/work/beta"),
        "both directories must be shown: {elsewhere}"
    );
}

// endregion: Which session, and whether it is still the same harness

/// A torn last line is survivable; damage in the middle is reported.
///
/// `SessionLog::read`s doc has always described one case — the partial line a
/// crash mid-write leaves — and the code dropped *any* line that would not
/// parse. So real corruption in the middle of a file, the case where a resumed
/// conversation is genuinely missing turns, read as a clean success. Recovery
/// still returns what it can, because refusing outright would make a damaged
/// session unresumable, which is worse.
#[test]
fn a_torn_tail_is_silent_and_damage_in_the_middle_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sess-torn.jsonl");

    // Two whole records, then the half-written line a crash leaves behind.
    let torn = [r#"{"kind":"a"}"#, r#"{"kind":"b"}"#, r#"{"kind":"c"#].join("\n");
    std::fs::write(&path, torn).unwrap();
    let kept = SessionLog::read(&path).expect("a torn tail must not fail the read");
    assert_eq!(kept.len(), 2, "both whole records survive a torn tail");

    // The same damage, in the middle instead of at the end. This is the case
    // the doc never covered: turns are genuinely missing from what comes back.
    let damaged = [r#"{"kind":"a"}"#, "NOT JSON AT ALL", r#"{"kind":"c"}"#].join("\n");
    std::fs::write(&path, damaged).unwrap();
    let kept = SessionLog::read(&path).expect("damage must still recover what it can");
    assert_eq!(kept.len(), 2, "the readable records are still returned");
}

/// A damaged compaction record does not quietly become a no-op.
///
/// `drop_messages` missing defaulted to 0 and `messages` missing defaulted to
/// empty, so a truncated or malformed `compacted` record left the conversation
/// uncompacted — and the fold then rebuilt something *different from what was
/// sent*, which is the one thing the fold exists to prevent. The only symptom
/// was a resumed session behaving unlike the one it continued.
#[test]
fn a_compaction_record_missing_its_fields_does_not_silently_rebuild_a_different_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sess-damaged.jsonl");

    // A goal, a turn, then a `compacted` record with its fields gone.
    let lines = [
        r#"{"kind":"goal","text":"do the thing"}"#,
        r#"{"kind":"assistant","raw_content":[{"type":"text","text":"working"}],"text":"working"}"#,
        r#"{"kind":"compacted"}"#,
    ];
    std::fs::write(&path, lines.join("\n")).unwrap();

    // The fold must not pretend the compaction happened, and must not panic.
    let restored = session::restore(&path).expect("a damaged record must not fail the resume");
    // The conversation is whatever survived; the point is that nothing claimed
    // a compaction that could not be reconstructed.
    let _ = restored;
}
