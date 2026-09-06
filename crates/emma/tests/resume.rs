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
        web_search: false,
        sampling: Default::default(),
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

    // **And in that order, on every line — not only the directory one.** The
    // order check further down was written for `cwd` alone, and the three lines
    // above it come from a single shared closure whose format string reads
    // `{was} → {now}`. Swapping those two arguments is one character of
    // movement, tells the reader the instructions and the model moved the
    // opposite way, and left every test in the crate green: both-strings-appear
    // is satisfied by a sentence that has them backwards.
    //
    // Which way round it reads is the whole content of the line. "aaaa → cccc"
    // says the transcript was written under `aaaa` and this run is using
    // `cccc`; reversed, somebody checking out the older prompt to reproduce the
    // conversation reaches for the wrong one.
    for (what, was_value, now_value) in [
        ("instructions", "aaaa", "cccc"),
        ("model", "claude-old", "claude-new"),
    ] {
        let line = lines
            .lines()
            .find(|l| l.starts_with(what))
            .unwrap_or_else(|| panic!("no line for `{what}`: {lines}"));
        let old = line.find(was_value).expect("the recorded value");
        let new = line.find(now_value).expect("the current value");
        assert!(
            old < new,
            "the `{what}` line reads `now → was`, so it tells the reader the \
             change went the other way: {line}"
        );
    }

    // The third field the closure writes, given its own case because the two
    // above happen to differ in length and this one does not: two four-character
    // hashes make an order bug invisible to anything but a position check.
    let swapped = Continuity {
        instructions_hash: "aaaa".into(),
        tool_schema_hash: "bbbb".into(),
        model: "m".into(),
        cwd: "/work/alpha".into(),
    }
    .differences("aaaa", "dddd", "m", "/work/alpha")
    .join("\n");
    assert!(
        swapped.starts_with("tool schema"),
        "the tool-schema change was not reported at all: {swapped}"
    );
    assert!(
        swapped.find("bbbb") < swapped.find("dddd"),
        "the tool-schema line reads `now → was`: {swapped}"
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

    // **And in that order.** Both-strings-appear is satisfied by a message that
    // has them the wrong way round, which is exactly what swapping the two
    // arguments produces: "now → was", a sentence that tells the reader they
    // moved in the opposite direction. A reviewer named that one-line change as
    // one the suite could not see.
    let arrow = elsewhere
        .lines()
        .find(|l| l.contains("working directory"))
        .expect("the working-directory line");
    let was = arrow.find("/work/alpha").expect("the recorded directory");
    let now = arrow.find("/work/beta").expect("the current directory");
    assert!(
        was < now,
        "the message reads `now → was`, so it tells the reader they moved the \
         other way: {arrow}"
    );
}

/// One directory spelled two ways is not a change, and must not warn.
///
/// **`differences` compared cwd with raw string inequality until 2026-08-23**,
/// while `same_dir` -- which `locate` uses to decide which session belongs to
/// this directory -- canonicalises both sides. So the resume *choice* and the
/// resume *warning* disagreed about what "the same directory" means, and a
/// trailing separator was enough to be told the conversation was about
/// somewhere else and the write tools were pointed at it.
///
/// A real directory, because the fix rests on `canonicalize`, which needs the
/// path to exist. The variant spellings below are the ones a person or a script
/// actually produces.
#[test]
fn one_directory_spelled_two_ways_is_not_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().to_string_lossy().into_owned();

    let continuity = Continuity {
        instructions_hash: "h".into(),
        tool_schema_hash: "t".into(),
        model: "m".into(),
        cwd: real.clone(),
    };

    for spelling in [
        real.clone(),
        format!("{real}{}", std::path::MAIN_SEPARATOR),
        real.replace('\\', "/"),
    ] {
        let notes = continuity.differences("h", "t", "m", &spelling);
        assert!(
            notes.is_empty(),
            "`{spelling}` is the same directory as `{real}` and was reported as a \
             change. A warning that fires on a trailing separator is one nobody \
             reads on the day it is right: {notes:?}"
        );
    }

    // The control: a genuinely different directory still warns, or the fix has
    // simply switched the check off.
    let other = tempfile::tempdir().unwrap();
    let notes = continuity.differences("h", "t", "m", &other.path().to_string_lossy());
    assert!(
        notes.iter().any(|n| n.contains("working directory")),
        "a real change of directory stopped being reported: {notes:?}"
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
    std::fs::write(&path, &torn).unwrap();
    let kept = SessionLog::read(&path).expect("a torn tail must not fail the read");
    assert_eq!(kept.len(), 2, "both whole records survive a torn tail");

    // The same damage, in the middle instead of at the end. This is the case
    // the doc never covered: turns are genuinely missing from what comes back.
    let damaged = [r#"{"kind":"a"}"#, "NOT JSON AT ALL", r#"{"kind":"c"}"#].join("\n");
    std::fs::write(&path, damaged).unwrap();
    let (kept, lost) =
        SessionLog::read_reporting(&path).expect("damage must still recover what it can");
    assert_eq!(kept.len(), 2, "the readable records are still returned");

    // **The half this test is named for and did not check.** Until the loss was
    // returned rather than only printed to stderr, the assertion above was the
    // whole test -- and it holds whether or not the damage is noticed at all.
    // An adversarial reviewer deleted the counting outright and this stayed
    // green across the entire crate.
    assert_eq!(lost, vec![2], "damage in the middle was not reported");

    // The other half of the name, and the reason the counting cannot simply be
    // "any line that failed to parse": a crash leaves a half-written last line,
    // and calling that corruption would make every interrupted session look
    // broken.
    std::fs::write(&path, &torn).unwrap();
    let (_, lost) = SessionLog::read_reporting(&path).unwrap();
    assert!(
        lost.is_empty(),
        "a torn tail was reported as damage: {lost:?}"
    );
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
    // **The assertion this test did not have.** Its only check was
    // `let _ = restored;` — that nothing panicked — so an adversarial reviewer
    // restoring the exact historical defect the doc-comment above describes left
    // it green across the whole crate. The refusal was real and unobservable.
    assert_eq!(
        restored.resumed.damage.len(),
        1,
        "a damaged compaction record was rebuilt without saying so: {:?}",
        restored.resumed.damage
    );
    assert!(
        restored.resumed.damage[0].contains("drop_messages"),
        "the damage does not name which field was missing: {:?}",
        restored.resumed.damage
    );
    // The conversation is still whatever survived; refusing outright would make
    // a damaged session unresumable, which is worse than resuming a short one.
    assert!(!restored.resumed.messages.is_empty());
}

// region: A finished goal is not a goal in flight
// ---------------------------------------------------------------------------
// A finished goal is not a goal in flight
//
// `worked` decides whether a model that stops without calling a tool has
// *answered* or *stalled*. A resumed run started with `worked = true` on the
// grounds — written in a comment — that "a resume only ever continues a goal
// that was already in flight". That was not true: `Resumed::messages` is the
// fold of the whole file, so it is non-empty after any goal at all, finished or
// not. An adversarial reviewer found it by reading; this is the run that
// settles it.
// ---------------------------------------------------------------------------

/// A plain question, asked after resuming a session whose goal finished
/// cleanly, is answered rather than nudged.
///
/// **What went wrong for a user:** they run a goal, it completes. They resume
/// and ask something conversational — no tool needed, no marker. The model
/// answers. The loop decides that cannot be an answer, because `worked` was
/// seeded true, and nudges. And nudges. Then ends the goal `Stalled` or
/// `KicksExhausted`, which `main.rs` reports as a failure and `-p` turns into a
/// non-zero exit code. A correct answer, delivered, reported as a failed run.
///
/// The two model calls asserted below are the real evidence: one for the
/// answer, and none after it. Counting calls rather than trusting the ending is
/// the same choice the four budget tests above make, and for the same reason.
#[tokio::test]
async fn a_question_after_a_finished_goal_is_answered_and_not_nudged() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let log = SessionLog::open(dir.path(), "sess-test").unwrap();
    // Run one: a goal that really finishes, through a tool call and the marker.
    let first = Fake::new(vec![
        call("Fine", json!({ "x": "1" })),
        text("done\n\nGOAL COMPLETE"),
    ]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![TestTool::ok("Fine", true).0]),
        &first,
        budgets(),
        &goal(),
        &log,
        None,
    )
    .await;
    assert_eq!(
        out.ending,
        Ending::Done,
        "the first goal must finish cleanly, or this test is about something else"
    );

    // Run two: resume that file and ask a question needing no tool.
    let resumed = session::restore(log.path()).unwrap();
    let second = Fake::new(vec![text("it is four.")]);
    let out = drive(
        &root,
        dir.path(),
        registry(vec![TestTool::ok("Fine", true).0]),
        &second,
        budgets(),
        &Goal::new("what is two plus two"),
        &log,
        Some(resumed.resumed),
    )
    .await;

    assert_eq!(
        second.calls(),
        1,
        "the model was asked again after it had already answered, which is the \
         nudge this test exists to refuse"
    );
    assert_eq!(
        out.ending,
        Ending::Answered,
        "a conversational turn after a finished goal was reported as a stall, \
         which `-p` turns into a non-zero exit code for a run that worked"
    );
}

// endregion: A finished goal is not a goal in flight

/// An unreadable newer session is passed over **and said out loud**.
///
/// **Bare `--resume` means "the one I was last running here", and this is where
/// that quietly stopped being true.** The walk goes newest-first and skips a
/// file it cannot read, so a corrupt newest file resumed an *older*
/// conversation — the user got a resume, which is what they asked for, about
/// the wrong work. Skipping remains right: refusing to resume anything because
/// one file in a shared directory is broken would be worse, and the file may
/// belong to another project. Being unable to say so was the defect.
///
/// A reviewer found it by noting that changing the `continue` to a `break` left
/// every test green, every fixture being clean UTF-8. This one is not: the
/// newest file here holds bytes that are not UTF-8 at all.
#[test]
fn an_unreadable_newer_session_is_named_rather_than_silently_skipped() {
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join("sessions");
    let here = tempfile::tempdir().unwrap();

    for id in ["sess-0000000000001-1", "sess-0000000000002-1"] {
        let log = SessionLog::open(&sessions, id).unwrap();
        log.append(
            "goal",
            json!({ "text": "g", "opening": "work on g", "cwd": here.path().display().to_string() }),
        );
    }
    // The newest file, and not text at all. A lone 0xFF is invalid UTF-8 in
    // every encoding of it, so `read_to_string` fails for a reason the standard
    // library supplies rather than one this test arranged.
    let broken = sessions.join("sess-0000000000003-1.jsonl");
    std::fs::write(&broken, [0xFFu8, 0xFE, 0xFD]).unwrap();

    let (chosen, skipped) = session::locate_reporting(&sessions, None, here.path()).unwrap();
    assert_eq!(
        chosen.file_stem().unwrap().to_string_lossy(),
        "sess-0000000000002-1",
        "the readable newest session was not the one chosen"
    );
    assert_eq!(
        skipped,
        vec!["sess-0000000000003-1.jsonl".to_string()],
        "the file that could not be read was passed over in silence"
    );

    let note = session::skipped_sessions_note(&skipped).expect("nothing was said about it");
    assert!(
        note.contains("sess-0000000000003-1"),
        "the note does not name the file, so nobody can go and look at it: {note}"
    );
    assert!(
        note.contains("--resume"),
        "the note says the wrong session may have been chosen and not how to \
         choose the right one: {note}"
    );

    // The control, and it is the half that keeps this worth reading: an
    // ordinary resume says nothing. A file belonging to another directory is
    // the rule working, not damage, and naming every other project's sessions
    // on every resume is how a warning gets ignored.
    //
    // A separate directory, deliberately. Reusing the one above would prove
    // nothing: the walk stops at the first match, so a newest file belonging to
    // the directory being asked about returns before the broken one is ever
    // reached, and the silence would be an accident of ordering rather than the
    // rule under test.
    let clean = home.path().join("clean");
    let elsewhere = tempfile::tempdir().unwrap();
    for (id, cwd) in [
        ("sess-0000000000001-1", elsewhere.path()),
        ("sess-0000000000002-1", here.path()),
        ("sess-0000000000003-1", elsewhere.path()),
    ] {
        let log = SessionLog::open(&clean, id).unwrap();
        log.append(
            "goal",
            json!({ "text": "g", "opening": "g", "cwd": cwd.display().to_string() }),
        );
    }
    // Asked about `here`, so the walk really does pass over two of another
    // directory's sessions on its way to the answer.
    let (chosen, skipped) = session::locate_reporting(&clean, None, here.path()).unwrap();
    assert_eq!(
        chosen.file_stem().unwrap().to_string_lossy(),
        "sess-0000000000002-1"
    );
    assert!(
        session::skipped_sessions_note(&skipped).is_none(),
        "a resume that passed over another directory's sessions warned about them, which would put a warning on almost every resume: {skipped:?}"
    );
}
