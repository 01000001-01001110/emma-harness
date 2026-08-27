//! The harness reader, tested against fixture session files in tempdirs.
//!
//! **Nothing in the ordinary suite reads the real `~/.emma`.** Every session
//! file here is written by [`sess`] into a `TempDir` that dies with the test,
//! in the record shapes `agent.rs` and `delegate.rs` actually append — the same
//! keys, including the ones this module ignores, so a test cannot pass against
//! a record shape the loop does not write.
//!
//! **Fixtures agree with their author, so there is one test that does not use
//! them**: [`the_real_session_directory_reads_without_inventing_anything`] runs
//! every feed over the actual `~/.emma/sessions`. It is `#[ignore]`d, because a
//! test whose input is a home directory is a test that passes or fails for
//! reasons that have nothing to do with the change under review. Run it by hand
//! and read what it prints; the port was certified that way on 2026-08-26 and
//! the numbers are in the report.

use super::*;
use serde_json::json;
use tempfile::TempDir;

/// A record: the kind, the timestamp, and whatever else the loop puts on it.
fn rec(at_ms: u64, kind: &str, mut body: Value) -> Value {
    let obj = body.as_object_mut().expect("record body is an object");
    obj.insert("kind".into(), json!(kind));
    obj.insert("at_ms".into(), json!(at_ms));
    body
}

/// Write one session file, newline-terminated as `SessionLog::append` leaves
/// it. `lines` go down verbatim, so a test can put damage in one.
fn write_session(dir: &Path, id: &str, lines: &[String]) -> PathBuf {
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write session");
    path
}

/// The same, stopping mid-line: what a crash between a record and its newline
/// leaves behind. The distinction is a fact about the trailing byte, which is
/// why this is a second function rather than a flag.
fn write_torn_session(dir: &Path, id: &str, lines: &[String], tail: &str) -> PathBuf {
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(&path, format!("{}\n{tail}", lines.join("\n"))).expect("write session");
    path
}

/// A session directory holding one file built from records.
fn sess(id: &str, records: &[Value]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let lines: Vec<String> = records.iter().map(|r| r.to_string()).collect();
    let path = write_session(dir.path(), id, &lines);
    (dir, path)
}

fn goal_rec(at: u64, text: &str) -> Value {
    rec(
        at,
        "goal",
        json!({
            "session_id": "sess-1000000000000-1",
            "text": text,
            "opening": text,
            "cwd": "/repo",
            "instructions_hash": "abc",
            "tool_schema_hash": "def",
            "model": "claude-sonnet-5",
            "done_check": "marker_claim",
        }),
    )
}

fn model_call(at: u64, iteration: u64, total: u64) -> Value {
    rec(
        at,
        "model_call",
        json!({
            "turn_id": format!("turn-{iteration}"),
            "iteration": iteration,
            "stop_reason": "end_turn",
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 5,
            "billable_total_tokens": 125,
            "cost_tokens": 125,
            "goal_total_so_far": total,
        }),
    )
}

fn finished(at: u64, ending: &str, iterations: u64, tokens: u64, elapsed_ms: u64) -> Value {
    rec(
        at,
        "goal_finished",
        json!({
            "ending": ending,
            "detail": Value::Null,
            "tokens": tokens,
            "iterations": iterations,
            "kicks": 0,
            "elapsed_ms": elapsed_ms,
        }),
    )
}

const T0: u64 = 1_700_000_000_000;

// region: Runs

#[test]
fn a_finished_goal_is_one_completed_row_carrying_what_the_loop_recorded() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "Add a clamp helper to render.rs"),
            model_call(T0 + 1_000, 1, 125),
            finished(T0 + 2_000, "done", 1, 125, 2_000),
        ],
    );
    let feed = runs(dir.path(), T0 + 3_000).expect("runs");
    assert_eq!(feed.runs.len(), 1, "one goal is one run");
    let row = &feed.runs[0];
    assert_eq!(row.id, "sess-1700000000000-1#1");
    assert_eq!(row.kind, RunKind::Goal);
    assert_eq!(row.name, "Add a clamp helper to render.rs");
    assert_eq!(row.status, RunStatus::Completed);
    assert_eq!(row.started_ms, T0);
    assert_eq!(row.ending.as_deref(), Some("done"));
    assert_eq!(row.iterations, Some(1));
    assert_eq!(row.tokens, Some(125));
    assert_eq!(row.elapsed_ms, Some(2_000));
    assert_eq!(row.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(row.cwd.as_deref(), Some("/repo"));
    assert_eq!(row.progress, None, "a goal records no ceiling to divide by");
}

#[test]
fn an_ending_that_is_not_done_or_answered_is_a_failed_row_that_names_it() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "spend it all"),
            finished(T0 + 10, "tokens", 9, 900, 10),
        ],
    );
    let row = &runs(dir.path(), T0 + 20).expect("runs").runs[0];
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(row.ending.as_deref(), Some("tokens"));
}

#[test]
fn an_unfinished_goal_written_to_recently_is_running() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[goal_rec(T0, "still going"), model_call(T0 + 1_000, 1, 125)],
    );
    let row = &runs(dir.path(), T0 + 1_000 + STALE_AFTER_MS)
        .expect("runs")
        .runs[0];
    assert_eq!(
        row.status,
        RunStatus::Running,
        "at the threshold, still live"
    );
    assert_eq!(
        row.iterations,
        Some(1),
        "counted from the calls, not the end"
    );
    assert_eq!(row.elapsed_ms, None, "only goal_finished times a goal");
}

#[test]
fn an_unfinished_goal_gone_quiet_is_stalled_and_not_failed() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[goal_rec(T0, "killed mid-flight"), model_call(T0 + 1, 1, 10)],
    );
    let row = &runs(dir.path(), T0 + 2 + STALE_AFTER_MS)
        .expect("runs")
        .runs[0];
    assert_eq!(
        row.status,
        RunStatus::Stalled,
        "nothing said it failed, so the feed must not say so either"
    );
    assert_eq!(row.ending, None);
}

#[test]
fn a_goal_overtaken_by_a_later_goal_in_the_same_file_is_stalled_whatever_the_clock() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "the one that never finished"),
            goal_rec(T0 + 5, "the one after it"),
            model_call(T0 + 6, 1, 10),
        ],
    );
    let feed = runs(dir.path(), T0 + 7).expect("runs");
    let first = feed
        .runs
        .iter()
        .find(|r| r.name.starts_with("the one that never"))
        .expect("the abandoned goal");
    assert_eq!(
        first.status,
        RunStatus::Stalled,
        "a session moved on, so the earlier goal is not running however fresh the file is"
    );
    let second = feed
        .runs
        .iter()
        .find(|r| r.name.starts_with("the one after"))
        .expect("the live goal");
    assert_eq!(second.status, RunStatus::Running);
    assert_eq!(second.id, "sess-1700000000000-1#2", "the ordinal advanced");
}

#[test]
fn runs_come_back_most_recent_first_across_every_file() {
    let dir = TempDir::new().unwrap();
    for (id, at) in [
        ("sess-1700000000000-1", T0),
        ("sess-1700000009000-2", T0 + 9_000),
        ("sess-1700000004000-3", T0 + 4_000),
    ] {
        let lines = [
            goal_rec(at, id).to_string(),
            finished(at + 1, "done", 1, 1, 1).to_string(),
        ];
        write_session(dir.path(), id, &lines);
    }
    let feed = runs(dir.path(), T0 + 20_000).expect("runs");
    let order: Vec<u64> = feed.runs.iter().map(|r| r.started_ms).collect();
    assert_eq!(order, vec![T0 + 9_000, T0 + 4_000, T0]);
}

/// Two goals can share a start millisecond, and a page that reordered itself
/// between two reads of an unchanged directory would look like it was moving.
///
/// **What this does not defend is the explicit id tie-break in `runs`.**
/// Deleting that line keeps this test green, because `sort_by` is stable and
/// `session_files` already sorts the paths — so the order is total without it.
/// The comment on the sort says so and says what a defence would cost. This
/// test pins the property a reader depends on: the same directory, read twice,
/// in the same order.
#[test]
fn two_runs_that_share_a_start_millisecond_come_back_in_a_stable_order() {
    let dir = TempDir::new().unwrap();
    // Written b-then-a on disk, so a sort that only compared `started_ms`
    // would leave them in file order rather than id order.
    for id in ["sess-1700000000000-b", "sess-1700000000000-a"] {
        write_session(
            dir.path(),
            id,
            &[
                goal_rec(T0, id).to_string(),
                finished(T0 + 1, "done", 1, 1, 1).to_string(),
            ],
        );
    }
    let ids = |f: &RunsFeed| -> Vec<String> { f.runs.iter().map(|r| r.id.clone()).collect() };
    let first = ids(&runs(dir.path(), T0 + 5).expect("runs"));
    let again = ids(&runs(dir.path(), T0 + 5).expect("runs"));
    assert_eq!(
        first,
        vec![
            "sess-1700000000000-a#1".to_string(),
            "sess-1700000000000-b#1".to_string()
        ],
        "equal timestamps are broken by id, not by whatever the directory listed first"
    );
    assert_eq!(first, again, "and two reads of one directory agree");
}

#[test]
fn the_name_is_the_first_line_of_the_goal_cut_to_the_card_width() {
    let long = "x".repeat(NAME_WIDTH + 40);
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[goal_rec(
            T0,
            &format!("{long}\nand a second line nobody sees"),
        )],
    );
    let row = &runs(dir.path(), T0 + 1).expect("runs").runs[0];
    assert!(!row.name.contains('\n'), "one line per row");
    assert_eq!(row.name.chars().count(), NAME_WIDTH);
    assert!(row.name.ends_with('…'), "cut is marked, not silent");
}

/// Every optional field is absent rather than defaulted when the record does
/// not carry it. A goal written by an older build has no `model`, and a page
/// showing an empty string would be indistinguishable from a model called "".
#[test]
fn a_goal_record_missing_its_fields_leaves_them_absent_rather_than_defaulted() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[rec(T0, "goal", json!({ "text": "no model, no cwd" }))],
    );
    let row = &runs(dir.path(), T0 + 1).expect("runs").runs[0];
    assert_eq!(row.model, None);
    assert_eq!(row.cwd, None);
    assert_eq!(row.tokens, None);
    assert_eq!(row.iterations, None);
    assert_eq!(row.ending, None);
    assert_eq!(row.name, "no model, no cwd");
}

// endregion: Runs

// region: Damage

/// Damage in the middle of a cleanly-closed file is reported, **by line
/// number** — which is the thing the fork could not do, because the
/// `session.rs` it was written against handed back no numbers to report.
#[test]
fn a_corrupt_record_in_the_middle_is_reported_by_line_number() {
    let dir = TempDir::new().unwrap();
    let lines = vec![
        goal_rec(T0, "good").to_string(),
        "{not json at all".to_string(),
        finished(T0 + 1, "done", 1, 1, 1).to_string(),
        "{\"kind\":\"model_call\",".to_string(),
    ];
    write_session(dir.path(), "sess-1700000000000-1", &lines);
    let feed = runs(dir.path(), T0 + 2).expect("runs");
    assert_eq!(feed.runs.len(), 1, "the readable records still make a run");
    assert_eq!(feed.damage.len(), 1, "one file, one damage entry");
    assert_eq!(
        feed.damage[0].lines,
        vec![2, 4],
        "the line numbers, so a reader can go and look — not a count"
    );
    assert_eq!(
        feed.skipped_lines(),
        2,
        "and the fork's number still derives"
    );
    assert!(feed.unreadable.is_empty(), "damaged is not unreadable");
}

/// A file that stops mid-line is what `kill -9` between a record and its
/// newline leaves. It is not damage, and a feed that called it damage would
/// report one bad line on **every live session** it read.
#[test]
fn a_torn_tail_is_not_damage_and_the_records_before_it_still_read() {
    let dir = TempDir::new().unwrap();
    write_torn_session(
        dir.path(),
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "interrupted").to_string(),
            model_call(T0 + 1, 1, 125).to_string(),
        ],
        // Valid JSON as far as it goes, which is the point: nothing about the
        // text distinguishes a torn record from a whole one, only the byte.
        "{\"kind\":\"goal_finish",
    );
    let feed = runs(dir.path(), T0 + 2).expect("runs");
    assert!(
        feed.damage.is_empty(),
        "a torn tail is silent by design: {:?}",
        feed.damage
    );
    assert_eq!(feed.runs.len(), 1);
    assert_eq!(feed.runs[0].iterations, Some(1), "and the rest still reads");
}

#[test]
fn a_file_that_cannot_be_read_is_reported_and_the_rest_of_the_feed_survives() {
    let dir = TempDir::new().unwrap();
    let lines = [
        goal_rec(T0, "fine").to_string(),
        finished(T0 + 1, "done", 1, 1, 1).to_string(),
    ];
    write_session(dir.path(), "sess-1700000000000-1", &lines);
    // A directory wearing a session file's name: `read_to_string` fails on it,
    // which is the closest portable stand-in for an unreadable file.
    fs::create_dir(dir.path().join("sess-1700000005000-2.jsonl")).unwrap();
    let feed = runs(dir.path(), T0 + 2).expect("runs");
    assert_eq!(feed.runs.len(), 1, "one bad file does not empty the page");
    assert_eq!(feed.unreadable.len(), 1);
    assert!(feed.unreadable[0]
        .path
        .ends_with("sess-1700000005000-2.jsonl"));
    assert!(!feed.unreadable[0].problem.is_empty(), "and it says why");
}

#[test]
fn a_session_directory_that_is_not_there_is_an_empty_feed_and_not_an_error() {
    let dir = TempDir::new().unwrap();
    let feed = runs(&dir.path().join("never-ran"), T0).expect("no directory is not an error");
    assert_eq!(feed, RunsFeed::default());
}

/// A file with nothing in it — `SessionLog` opened it and the process died
/// before the first record. No runs, no damage, no error.
#[test]
fn an_empty_file_is_no_runs_and_no_damage() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sess-1700000000000-1.jsonl");
    fs::write(&path, "").unwrap();
    let feed = runs(dir.path(), T0).expect("runs");
    assert_eq!(feed, RunsFeed::default());
    let tail = events(&path, EVENT_TAIL).expect("events");
    assert_eq!(tail, EventFeed::default());
}

/// Blank lines are not damage. A file hand-edited by somebody looking for a
/// record is the ordinary way one gets in.
#[test]
fn a_blank_line_is_not_damage() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sess-1700000000000-1.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n\n\n{}\n",
            goal_rec(T0, "spaced out"),
            model_call(T0 + 1, 1, 5)
        ),
    )
    .unwrap();
    let feed = runs(dir.path(), T0 + 2).expect("runs");
    assert!(feed.damage.is_empty(), "{:?}", feed.damage);
    assert_eq!(feed.runs.len(), 1);
}

/// Records in an order the one-pass walk was not built around: an ending with
/// no beginning, and a result with no call. Neither invents a run or a row.
#[test]
fn records_that_arrive_without_what_they_answer_are_ignored_rather_than_guessed() {
    let (dir, path) = sess(
        "sess-1700000000000-1",
        &[
            finished(T0, "done", 1, 1, 1),
            rec(
                T0 + 1,
                "tool_result",
                json!({ "id": "orphan", "tool": "Read",
                        "block": { "type": "tool_result", "tool_use_id": "orphan",
                                   "content": "x" } }),
            ),
            model_call(T0 + 2, 1, 125),
        ],
    );
    let feed = runs(dir.path(), T0 + 10).expect("runs");
    assert!(
        feed.runs.is_empty(),
        "an ending with no goal is not a run: {:?}",
        feed.runs
    );
    // The records are still rendered — a stream shows what is in the file.
    let tail = events(&path, EVENT_TAIL).expect("events");
    assert_eq!(tail.lines.len(), 3);
}

// endregion: Damage

// region: Delegations

fn sub_goal(at: u64, sub_id: &str, agent: &str, text: &str) -> Value {
    rec(
        at,
        "sub.goal",
        json!({
            "session_id": "sess-1700000000000-1",
            "text": text,
            "opening": text,
            "cwd": "/repo",
            "model": "claude-sonnet-5",
            "done_check": "subagent_claim",
            "sub_id": sub_id,
            "parent_turn_id": "turn-1",
            "agent": agent,
        }),
    )
}

fn delegation(at: u64, sub_id: &str, agent: &str, ending: &str) -> Value {
    rec(
        at,
        "delegation",
        json!({
            "sub_id": sub_id,
            "parent_turn_id": "turn-1",
            "agent": agent,
            "task": "read the survey",
            "ending": ending,
            "cost_tokens": 4_200,
            "iterations": 3,
            "elapsed_ms": 12_000,
            "tool_calls": 4,
            "files_read": 2,
            "commands": 0,
            "failed_commands": 0,
            "denied": 0,
            "max_tokens": 60_000,
            "max_iterations": 12,
            "wall_clock_ms": 600_000,
            "max_kicks": 1,
        }),
    )
}

#[test]
fn a_delegation_is_its_own_run_with_its_agent_and_a_ceiling_backed_progress() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "parent goal"),
            sub_goal(T0 + 100, "sub-turn-1-1", "explorer", "read the survey"),
            rec(
                T0 + 200,
                "sub.goal_finished",
                json!({ "ending": "done", "detail": Value::Null, "tokens": 4200,
                        "iterations": 3, "kicks": 0, "elapsed_ms": 12000,
                        "sub_id": "sub-turn-1-1", "parent_turn_id": "turn-1", "agent": "explorer" }),
            ),
            delegation(T0 + 210, "sub-turn-1-1", "explorer", "done"),
            finished(T0 + 300, "done", 2, 9_000, 300),
        ],
    );
    let feed = runs(dir.path(), T0 + 400).expect("runs");
    let sub = feed
        .runs
        .iter()
        .find(|r| r.kind == RunKind::Delegation)
        .expect("the delegation is a run of its own");
    assert_eq!(sub.id, "sub-turn-1-1");
    assert_eq!(sub.agent.as_deref(), Some("explorer"));
    assert_eq!(sub.name, "read the survey");
    assert_eq!(sub.status, RunStatus::Completed);
    assert_eq!(
        sub.progress,
        Some(Progress { done: 3, total: 12 }),
        "the delegation record is the one place a ceiling is written down"
    );
    assert_eq!(sub.tokens, Some(4_200));
    assert_eq!(feed.runs.len(), 2, "the parent goal is still its own row");
}

#[test]
fn a_delegation_still_running_has_no_progress_because_no_ceiling_is_recorded_yet() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "parent goal"),
            sub_goal(T0 + 100, "sub-turn-1-1", "explorer", "read the survey"),
            sub_model_call(T0 + 150, "sub-turn-1-1", "explorer"),
        ],
    );
    let feed = runs(dir.path(), T0 + 200).expect("runs");
    let sub = feed
        .runs
        .iter()
        .find(|r| r.kind == RunKind::Delegation)
        .unwrap();
    assert_eq!(sub.status, RunStatus::Running);
    assert_eq!(
        sub.progress, None,
        "the ceiling lands with the delegation record at the end, so live progress has no \
         honest denominator"
    );
    assert_eq!(sub.iterations, Some(1));
}

fn sub_model_call(at: u64, sub_id: &str, agent: &str) -> Value {
    rec(
        at,
        "sub.model_call",
        json!({ "turn_id": "turn-1", "iteration": 1, "goal_total_so_far": 500,
                "input_tokens": 400, "output_tokens": 100,
                "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
                "billable_total_tokens": 500, "cost_tokens": 500,
                "stop_reason": "tool_use",
                "sub_id": sub_id, "parent_turn_id": "turn-1", "agent": agent }),
    )
}

/// A subagent charges the parent's meter once, in the `delegation` record. Its
/// own `sub.model_call` records must not be added to the parent's totals as
/// well, or the parent's TOKENS card bills the same tokens twice.
#[test]
fn a_delegations_own_calls_are_in_the_parents_stream_and_out_of_its_totals() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "parent goal"),
            model_call(T0 + 10, 1, 125),
            rec(
                T0 + 20,
                "tool_call",
                json!({ "turn_id": "turn-1", "id": "p1", "tool": "Grep", "args": {} }),
            ),
            sub_goal(T0 + 100, "sub-turn-1-1", "explorer", "read the survey"),
            sub_model_call(T0 + 150, "sub-turn-1-1", "explorer"),
            rec(
                T0 + 160,
                "sub.tool_call",
                json!({ "turn_id": "turn-1", "id": "s1", "tool": "Read", "args": {},
                        "sub_id": "sub-turn-1-1", "agent": "explorer" }),
            ),
            delegation(T0 + 210, "sub-turn-1-1", "explorer", "done"),
            finished(T0 + 300, "done", 1, 4_325, 300),
        ],
    );
    let parent = detail(dir.path(), "sess-1700000000000-1#1", T0 + 400)
        .expect("detail")
        .expect("the parent run");
    assert_eq!(parent.tokens.calls, 1, "only the parent's own model call");
    assert_eq!(parent.tokens.input, 100, "not 500 — the sub's is the sub's");
    assert_eq!(
        parent
            .calls
            .iter()
            .map(|c| c.tool.as_str())
            .collect::<Vec<_>>(),
        vec!["Grep"],
        "the parent's own Grep, and not the sub's Read: that is not a call this run made"
    );
    assert!(
        parent.events.iter().any(|e| e.line.contains("[explorer]")),
        "but it is in the stream, tagged with who did it"
    );

    let sub = detail(dir.path(), "sub-turn-1-1", T0 + 400)
        .expect("detail")
        .expect("the sub run");
    assert_eq!(sub.tokens.calls, 1);
    assert_eq!(sub.tokens.input, 400, "and they are counted exactly once");
    assert_eq!(sub.calls.len(), 1);
}

/// A `delegation` record from a build that did not write `max_iterations` —
/// the field was added to that record after the first delegations shipped.
/// Without a recorded ceiling there is no denominator, and a bar drawn against
/// a zero total is a bar drawn against nothing.
///
/// **This is the case that makes the two-`Some` guard load-bearing**, and it
/// was found by mutation: replacing the guard with `unwrap_or(0)` on both
/// halves left the whole suite green, because every other fixture in this file
/// carries the key.
#[test]
fn a_delegation_record_with_no_recorded_ceiling_has_no_progress() {
    let mut old = delegation(T0 + 210, "sub-turn-1-1", "explorer", "done");
    old.as_object_mut().unwrap().remove("max_iterations");
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "parent goal"),
            sub_goal(T0 + 100, "sub-turn-1-1", "explorer", "read the survey"),
            old,
            finished(T0 + 300, "done", 2, 9_000, 300),
        ],
    );
    let feed = runs(dir.path(), T0 + 400).expect("runs");
    let sub = feed
        .runs
        .iter()
        .find(|r| r.kind == RunKind::Delegation)
        .expect("the delegation is still a run");
    assert_eq!(
        sub.progress, None,
        "no ceiling in the record, so no fraction — not a fraction over zero"
    );
    assert_eq!(sub.iterations, Some(3), "and what was recorded still reads");
}

// endregion: Delegations

// region: Detail

#[test]
fn detail_gives_the_totals_the_calls_and_the_stream_for_one_run() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "read a file"),
            model_call(T0 + 1_000, 1, 125),
            rec(
                T0 + 1_100,
                "tool_call",
                json!({ "turn_id": "turn-1", "id": "toolu_1", "tool": "Read",
                        "args": { "path": "src/lib.rs" } }),
            ),
            rec(
                T0 + 1_600,
                "tool_result",
                json!({ "turn_id": "turn-1", "id": "toolu_1", "tool": "Read",
                        "block": { "type": "tool_result", "tool_use_id": "toolu_1",
                                   "content": "fn main() {}" },
                        "truncated": false, "exit_code": Value::Null }),
            ),
            model_call(T0 + 2_000, 2, 300),
            finished(T0 + 2_100, "done", 2, 300, 2_100),
        ],
    );
    let d = detail(dir.path(), "sess-1700000000000-1#1", T0 + 3_000)
        .expect("detail")
        .expect("the run is there");
    assert_eq!(d.row.status, RunStatus::Completed);
    assert_eq!(d.tokens.calls, 2);
    assert_eq!(d.tokens.input, 200);
    assert_eq!(d.tokens.output, 40);
    assert_eq!(d.tokens.cache_read, 10);
    assert_eq!(d.tokens.billable, 250);
    assert_eq!(d.kicks, Some(0));
    assert_eq!(d.calls.len(), 1);
    let call = &d.calls[0];
    assert_eq!(call.tool, "Read");
    assert_eq!(call.status, CallStatus::Ok);
    assert!(
        call.args.contains("src/lib.rs"),
        "args summarised, not dropped"
    );
    assert_eq!(
        call.elapsed_ms,
        Some(500),
        "result time minus call time — the only elapsed the log can back"
    );
    assert!(
        d.events.iter().any(|e| e.line.contains("Read")),
        "the stream carries the tool traffic"
    );
    assert!(
        d.events.windows(2).all(|w| w[0].at_ms <= w[1].at_ms),
        "oldest first"
    );
}

#[test]
fn a_failed_call_a_denied_call_and_an_unanswered_call_are_told_apart() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "three calls"),
            rec(
                T0 + 10,
                "tool_call",
                json!({ "turn_id": "turn-1", "id": "a", "tool": "Bash", "args": { "cmd": "ls" } }),
            ),
            // The failure class is on the fixture exactly as it is on disk:
            // `agent.rs` writes `e.kind()` under the key `kind` and
            // `SessionLog::append` overwrites it with the record's own kind
            // before the line is written, so every `tool_failed` on disk says
            // `tool_failed` where the class should be. Certified against the two
            // real `tool_failed` records in `~/.emma/sessions`, 2026-08-26.
            rec(
                T0 + 20,
                "tool_failed",
                json!({ "turn_id": "turn-1", "id": "a", "tool": "Bash",
                        "detail": "no such file" }),
            ),
            rec(
                T0 + 30,
                "tool_call",
                json!({ "turn_id": "turn-1", "id": "b", "tool": "Write", "args": { "path": "x" } }),
            ),
            rec(
                T0 + 40,
                "denied",
                json!({ "turn_id": "turn-1", "id": "b", "by": "user",
                        "reason": "the user said no" }),
            ),
            rec(
                T0 + 50,
                "tool_call",
                json!({ "turn_id": "turn-1", "id": "c", "tool": "Read", "args": { "path": "y" } }),
            ),
        ],
    );
    let d = detail(dir.path(), "sess-1700000000000-1#1", T0 + 60)
        .expect("detail")
        .unwrap();
    let by = |id: &str| d.calls.iter().find(|c| c.id == id).expect("call").clone();
    assert_eq!(by("a").status, CallStatus::Error);
    assert_eq!(by("a").detail.as_deref(), Some("no such file"));
    assert_eq!(by("b").status, CallStatus::Denied);
    assert_eq!(by("b").detail.as_deref(), Some("the user said no"));
    assert_eq!(
        by("c").status,
        CallStatus::Unanswered,
        "a call with no answer in the file is not a success"
    );
    assert_eq!(by("c").elapsed_ms, None);
}

/// Two kinds the fork's reader never saw, because its `agent.rs` predates them.
/// Both answer a call and both are failures; neither may leave the call reading
/// as unanswered, which is what "the run stopped here" means.
#[test]
fn a_cancelled_call_and_a_panicked_call_are_errors_that_say_which() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "two ways to not finish"),
            rec(
                T0 + 10,
                "tool_call",
                json!({ "turn_id": "t", "id": "a", "tool": "Bash", "args": { "cmd": "sleep 90" } }),
            ),
            rec(
                T0 + 20,
                "tool_cancelled",
                json!({ "turn_id": "t", "id": "a", "tool": "Bash" }),
            ),
            rec(
                T0 + 30,
                "tool_call",
                json!({ "turn_id": "t", "id": "b", "tool": "Edit", "args": { "path": "x" } }),
            ),
            rec(
                T0 + 40,
                "tool_panicked",
                json!({ "turn_id": "t", "id": "b", "tool": "Edit",
                        "detail": "index out of bounds" }),
            ),
        ],
    );
    let d = detail(dir.path(), "sess-1700000000000-1#1", T0 + 50)
        .expect("detail")
        .unwrap();
    let by = |id: &str| d.calls.iter().find(|c| c.id == id).expect("call").clone();
    assert_eq!(by("a").status, CallStatus::Error);
    assert_eq!(by("a").detail.as_deref(), Some("cancelled by the user"));
    assert_eq!(by("b").status, CallStatus::Error);
    assert_eq!(by("b").detail.as_deref(), Some("index out of bounds"));
    let level = |needle: &str| {
        d.events
            .iter()
            .find(|l| l.line.contains(needle))
            .unwrap_or_else(|| panic!("no line mentioning {needle}"))
            .level
    };
    assert_eq!(
        level("cancelled"),
        Level::Warn,
        "the loop carried on from it, so it is not the harness failing"
    );
    assert_eq!(level("panicked"), Level::Warn);
}

#[test]
fn detail_of_an_id_no_file_holds_is_none_rather_than_an_error() {
    let (dir, _) = sess("sess-1700000000000-1", &[goal_rec(T0, "one")]);
    assert_eq!(
        detail(dir.path(), "sess-nope#7", T0 + 1).expect("no error"),
        None
    );
}

// endregion: Detail

// region: Events

#[test]
fn the_tail_renders_records_as_leveled_lines() {
    let (_dir, path) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "do a thing"),
            rec(
                T0 + 10,
                "tool_call",
                json!({ "turn_id": "t", "id": "a", "tool": "Bash", "args": { "cmd": "ls" } }),
            ),
            rec(
                T0 + 20,
                "tool_failed",
                json!({ "turn_id": "t", "id": "a", "tool": "Bash",
                        "detail": "no such file" }),
            ),
            rec(
                T0 + 30,
                "tool_fault",
                json!({ "turn_id": "t", "id": "a", "tool": "Bash",
                        "detail": "the tool says the session cannot continue" }),
            ),
            finished(T0 + 40, "done", 1, 1, 40),
        ],
    );
    let feed = events(&path, EVENT_TAIL).expect("events");
    let level = |needle: &str| {
        feed.lines
            .iter()
            .find(|l| l.line.contains(needle))
            .unwrap_or_else(|| panic!("no line mentioning {needle}"))
            .level
    };
    assert_eq!(level("do a thing"), Level::Info);
    assert_eq!(
        level("no such file"),
        Level::Warn,
        "a tool failure is carried on from"
    );
    assert_eq!(
        level("cannot continue"),
        Level::Error,
        "a fault is the harness"
    );
    assert_eq!(feed.lines.first().unwrap().at_ms, T0, "oldest first");
    assert!(feed.damaged_lines.is_empty());
    assert_eq!(feed.older, 0);
}

#[test]
fn the_tail_is_the_last_n_and_says_how_many_it_left_behind() {
    let mut records = vec![goal_rec(T0, "long run")];
    for i in 1..=10u64 {
        records.push(model_call(T0 + i * 10, i, i * 100));
    }
    let (_dir, path) = sess("sess-1700000000000-1", &records);
    let feed = events(&path, 3).expect("events");
    assert_eq!(feed.lines.len(), 3);
    assert_eq!(feed.older, 8, "11 records, 3 shown");
    assert_eq!(feed.lines.last().unwrap().at_ms, T0 + 100);
}

#[test]
fn a_corrupt_line_in_the_tail_is_reported_by_line_number() {
    let dir = TempDir::new().unwrap();
    let path = write_session(
        dir.path(),
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "good").to_string(),
            "}{".to_string(),
            finished(T0 + 1, "done", 1, 1, 1).to_string(),
        ],
    );
    let feed = events(&path, EVENT_TAIL).expect("events");
    assert_eq!(feed.lines.len(), 2);
    assert_eq!(feed.damaged_lines, vec![2]);
}

#[test]
fn a_turn_that_only_called_tools_says_so_rather_than_rendering_an_empty_line() {
    let (_dir, path) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "read it"),
            rec(
                T0 + 5,
                "assistant",
                json!({ "turn_id": "turn-1", "text": "", "raw_content": [] }),
            ),
        ],
    );
    let feed = events(&path, EVENT_TAIL).expect("events");
    assert_eq!(feed.lines[1].line, "assistant: (tool calls only, no prose)");
}

/// A record kind this build has never heard of still renders as its own name.
/// A reader shown the name can grep the file; a reader shown a blank cannot,
/// and a session written by a newer Emma is the ordinary way one arrives.
#[test]
fn a_record_kind_this_build_does_not_know_is_still_a_line() {
    let (_dir, path) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "from the future"),
            rec(T0 + 1, "quantum_entangled", json!({ "spooky": true })),
        ],
    );
    let feed = events(&path, EVENT_TAIL).expect("events");
    assert_eq!(feed.lines[1].line, "quantum_entangled");
}

/// Control bytes in a record are somebody else's, not Emma's, and a rendered
/// line carrying a bare CR overwrites the line above it in anything that is
/// redirected. They are flattened at the reader.
///
/// **The `hook` record is the fixture, and that is the whole point of this
/// test.** A `tool_call`'s `args` is an object, and `Value::to_string`
/// re-serialises it — serde escapes a control character into `` on the
/// way out, so the flattening is unreachable through that arm and a test using
/// one passes with the flattening deleted. Proven: that was the first version
/// of this test, and it survived the mutation. A `hook` record's `run` is a
/// plain string, which [`args_summary`] hands through verbatim.
#[test]
fn control_bytes_in_a_record_never_reach_a_rendered_line() {
    let (_dir, path) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "sneaky"),
            rec(
                T0 + 1,
                "hook",
                json!({ "turn_id": "t", "run": "echo \u{1b}[31mred\rover the top" }),
            ),
            rec(
                T0 + 2,
                "tool_call",
                json!({ "id": "a", "tool": "Bash",
                        "args": { "cmd": "echo \u{1b}[31mred\r" } }),
            ),
        ],
    );
    let feed = events(&path, EVENT_TAIL).expect("events");
    for line in feed.lines.iter().map(|l| &l.line) {
        assert!(
            !line.chars().any(char::is_control),
            "no control bytes reach a line: {line:?}"
        );
    }
    assert!(
        feed.lines[1].line.contains("red") && feed.lines[1].line.contains("over the top"),
        "and the readable text survives: {:?}",
        feed.lines[1].line
    );
}

// endregion: Events

// region: Gates

use crate::approval::Gate;

#[test]
fn the_gates_view_reports_the_posture_and_this_sessions_refusals() {
    let (_dir, path) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "write a file"),
            rec(
                T0 + 10,
                "tool_call",
                json!({ "turn_id": "t", "id": "a", "tool": "Write", "args": { "path": "x" } }),
            ),
            // No `tool` key: the real record names the call id only, so the
            // view has to join it back to the call.
            rec(
                T0 + 20,
                "denied",
                json!({ "turn_id": "t", "id": "a", "by": "user",
                        "reason": "the user declined" }),
            ),
            rec(
                T0 + 30,
                "tool_call",
                json!({ "turn_id": "t", "id": "b", "tool": "Bash", "args": { "cmd": "rm -rf /" } }),
            ),
            rec(
                T0 + 40,
                "denied",
                json!({ "turn_id": "t", "id": "b", "by": "hook",
                        "reason": "policy forbids it" }),
            ),
        ],
    );
    let view = gates(Gate::Ask, Some(&path)).expect("gates");
    assert_eq!(view.mode_label, "ASK");
    assert!(!view.mode_about.is_empty(), "and it says what that means");
    assert_eq!(view.calls, 2);
    assert_eq!(view.denials.len(), 2);
    assert_eq!(view.denials[0].by, "hook", "most recent first");
    assert_eq!(view.denials[0].tool, "Bash", "joined back to its call");
    assert_eq!(view.denials[1].reason, "the user declined");
}

/// A denial whose `tool_call` is not in this file — a log that starts mid-run,
/// or one the damage report says lost the call's own line. The id stands in
/// rather than a name being invented for it.
#[test]
fn a_denial_whose_call_is_missing_keeps_the_id_rather_than_naming_a_tool() {
    let (_dir, path) = sess(
        "sess-1700000000000-1",
        &[rec(
            T0,
            "denied",
            json!({ "id": "toolu_orphan", "by": "hook", "reason": "policy" }),
        )],
    );
    let view = gates(Gate::Ask, Some(&path)).expect("gates");
    assert_eq!(view.denials[0].tool, "toolu_orphan");
    assert_eq!(view.calls, 0);
}

#[test]
fn a_pending_approval_is_not_readable_today_and_the_view_says_none_rather_than_guessing() {
    // Stated as a test so the gap cannot be forgotten: `Approvals::request`
    // asks its question inside the call that needs the answer and awaits it
    // there, so the question a human is looking at exists only in a future's
    // state. There is no cell to read it out of, and inventing one from the log
    // would mean reporting a prompt from a `tool_call` with no result yet —
    // which is also what an interrupted run looks like.
    let (_dir, path) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "write a file"),
            rec(
                T0 + 10,
                "tool_call",
                json!({ "turn_id": "t", "id": "a", "tool": "Write", "args": { "path": "x" } }),
            ),
        ],
    );
    assert_eq!(gates(Gate::Ask, Some(&path)).expect("gates").pending, None);
}

#[test]
fn the_posture_answers_with_no_session_file_at_all() {
    let view = gates(Gate::Unattended, None).expect("gates");
    assert_eq!(view.mode_label, "UNATTENDED");
    assert!(view.denials.is_empty());
    assert_eq!(view.calls, 0);
    assert!(view.damaged_lines.is_empty());
}

/// Three postures, three words, three sentences. A card that showed the same
/// label for `--dangerously-skip-permissions` as for the default gate would be
/// telling somebody they are being asked when they are not.
///
/// **Distinctness alone is not enough, and mutation proved it**: swapping
/// `Gate::Ask`'s sentence for a generic one kept all three distinct and left
/// the earlier version of this test green. A sentence that is wrong about which
/// posture the run is in is exactly as bad as one that is missing, so each is
/// checked for the thing it has to say.
#[test]
fn the_three_postures_are_told_apart_by_both_the_label_and_the_sentence() {
    let all = [Gate::Ask, Gate::SkipAll, Gate::Unattended];
    let labels: Vec<&str> = all.iter().map(|&g| gate_label(g)).collect();
    let abouts: Vec<&str> = all.iter().map(|&g| gate_about(g)).collect();
    for i in 0..all.len() {
        assert!(!labels[i].is_empty() && !abouts[i].is_empty());
        for j in i + 1..all.len() {
            assert_ne!(labels[i], labels[j], "two postures, one word");
            assert_ne!(abouts[i], abouts[j], "two postures, one sentence");
        }
    }
    // The default: somebody is asked.
    assert!(
        gate_about(Gate::Ask).contains("asks you"),
        "{}",
        gate_about(Gate::Ask)
    );
    // The bypass has to name the flag that produces it, because "everything
    // runs" without the flag beside it reads as a description of the default.
    assert!(
        gate_about(Gate::SkipAll).contains("--dangerously-skip-permissions"),
        "{}",
        gate_about(Gate::SkipAll)
    );
    // And the unattended posture has to say the word `denied`: its whole
    // difference from the default is that nothing waits for an answer.
    assert!(
        gate_about(Gate::Unattended).contains("denied"),
        "{}",
        gate_about(Gate::Unattended)
    );
}

// endregion: Gates

// region: Sessions

/// A `goal` record that ran somewhere in particular. The fixed [`goal_rec`]
/// always says `/repo`, and the whole point of these tests is the cwd.
fn goal_in(at: u64, cwd: &str, text: &str) -> Value {
    let mut record = goal_rec(at, text);
    record["cwd"] = json!(cwd);
    record
}

/// A session directory with several files in it.
fn sessions(files: &[(&str, Vec<Value>)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (id, records) in files {
        let lines: Vec<String> = records.iter().map(|r| r.to_string()).collect();
        write_session(dir.path(), id, &lines);
    }
    dir
}

const DAY: u64 = 86_400_000;

/// A directory name that exists nowhere, so `canonicalize` fails on it and
/// `recorded_in` falls back to comparing the paths as written — which is what
/// makes these fixtures portable between Windows and unix.
const ELSEWHERE: &str = "/not-a-real-place/elsewhere";
const HERE: &str = "/not-a-real-place/repo";

#[test]
fn only_the_sessions_that_ran_in_this_repo_are_listed_newest_first() {
    let dir = sessions(&[
        (
            "sess-1700000000000-1",
            vec![
                goal_in(T0, HERE, "the older one"),
                finished(T0 + 10, "done", 1, 10, 10),
            ],
        ),
        (
            "sess-1700000000000-2",
            vec![goal_in(T0 + DAY, ELSEWHERE, "someone else's repo")],
        ),
        (
            "sess-1700000000000-3",
            vec![goal_in(T0 + 2 * DAY, HERE, "the newer one")],
        ),
    ]);
    let feed = sessions_for(dir.path(), Path::new(HERE), T0 + 2 * DAY + 1).expect("sessions");
    let names: Vec<&str> = feed.sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["the newer one", "the older one"]);
    assert!(feed.unreadable.is_empty(), "{:?}", feed.unreadable);
}

#[test]
fn a_session_that_visited_this_repo_once_counts_even_though_it_moved_on() {
    let dir = sessions(&[(
        "sess-1700000000000-1",
        vec![
            goal_in(T0, HERE, "started here"),
            finished(T0 + 10, "done", 1, 10, 10),
            goal_in(T0 + 60, ELSEWHERE, "carried on over there"),
        ],
    )]);
    let feed = sessions_for(dir.path(), Path::new(HERE), T0 + 100).expect("sessions");
    assert_eq!(feed.sessions.len(), 1);
    // The name is the session's latest goal, not the goal that matched: the
    // row names the session, and the session's last subject is what it is.
    assert_eq!(feed.sessions[0].name, "carried on over there");
    assert_eq!(feed.sessions[0].goals, 2);
}

#[test]
fn a_trailing_slash_on_the_cwd_is_the_same_directory() {
    let dir = sessions(&[(
        "sess-1700000000000-1",
        vec![goal_in(T0, &format!("{HERE}/"), "written with a slash")],
    )]);
    let feed = sessions_for(dir.path(), Path::new(HERE), T0 + 1).expect("sessions");
    assert_eq!(feed.sessions.len(), 1);
}

/// A `sub.goal` carries a cwd too, and a delegation is not a session's own
/// visit to a directory. Only an un-prefixed `goal` puts a session in a
/// repository's list.
#[test]
fn a_delegations_cwd_does_not_put_a_session_in_this_repos_list() {
    let dir = sessions(&[(
        "sess-1700000000000-1",
        vec![goal_in(T0, ELSEWHERE, "ran over there"), {
            let mut s = sub_goal(T0 + 10, "sub-1", "explorer", "but delegated");
            s["cwd"] = json!(HERE);
            s
        }],
    )]);
    let feed = sessions_for(dir.path(), Path::new(HERE), T0 + 20).expect("sessions");
    assert!(feed.sessions.is_empty(), "{:?}", feed.sessions);
}

#[test]
fn a_corrupt_line_is_reported_by_number_and_the_rest_of_the_directory_still_lists() {
    let dir = TempDir::new().expect("tempdir");
    write_session(
        dir.path(),
        "sess-1700000000000-1",
        &[
            goal_in(T0, HERE, "survives the damage").to_string(),
            "{ this was half-written when the process died".to_string(),
            finished(T0 + 1, "done", 1, 1, 1).to_string(),
        ],
    );
    let feed = sessions_for(dir.path(), Path::new(HERE), T0 + 2).expect("sessions");
    assert_eq!(feed.damage.len(), 1);
    assert_eq!(feed.damage[0].lines, vec![2]);
    assert_eq!(feed.skipped_lines(), 1);
    assert_eq!(feed.sessions.len(), 1);
    assert_eq!(feed.sessions[0].name, "survives the damage");
}

#[test]
fn a_session_whose_goal_carried_no_text_falls_back_to_the_tail_of_its_id() {
    let dir = sessions(&[("sess-1787712345678-48282", vec![goal_in(T0, HERE, "")])]);
    let feed = sessions_for(dir.path(), Path::new(HERE), T0 + 1).expect("sessions");
    assert_eq!(feed.sessions[0].name, "345678-48282");
}

#[test]
fn a_directory_that_is_not_there_is_no_sessions_rather_than_an_error() {
    let dir = TempDir::new().expect("tempdir");
    let feed = sessions_for(&dir.path().join("never-ran"), Path::new(HERE), T0).expect("sessions");
    assert!(feed.sessions.is_empty());
}

// endregion: Sessions

// region: The relative time column

/// 2026-05-18 12:42:00 in whatever zone the offset names.
const MAY_18: u64 = 1_779_108_120_000;

#[test]
fn today_is_a_clock_yesterday_is_a_word_and_older_is_a_date() {
    // Same day, five hours later.
    assert_eq!(relative_time(MAY_18, MAY_18 + 5 * 3_600_000, 0), "12:42");
    assert_eq!(relative_time(MAY_18, MAY_18 + DAY, 0), "Yesterday");
    assert_eq!(relative_time(MAY_18, MAY_18 + 3 * DAY, 0), "May 18");
}

#[test]
fn the_year_shows_only_when_it_is_not_this_one() {
    assert_eq!(relative_time(MAY_18, MAY_18 + 400 * DAY, 0), "May 18 2026");
}

#[test]
fn the_offset_moves_the_day_boundary_and_not_just_the_clock() {
    // 23:42 UTC is the next day in Sydney (+10); in Los Angeles (-7) it is
    // still the same afternoon.
    let late = MAY_18 + 11 * 3_600_000;
    assert_eq!(relative_time(late, late + 60_000, 10 * 3_600), "09:42");
    assert_eq!(relative_time(late, late + 60_000, -7 * 3_600), "16:42");
}

#[test]
fn a_clock_that_ran_backwards_shows_a_time_rather_than_a_date_in_the_future() {
    assert_eq!(relative_time(MAY_18 + DAY, MAY_18, 0), "12:42");
}

/// The calendar arithmetic against dates checked outside this file, including
/// the epoch, an ordinary leap day, and — the case a first pass missed — the
/// last day of a 400-year era.
///
/// **`doe == 146096` is the only input the `- doe / 146_096` term changes**, so
/// every date that is not the day before an era boundary passes with that term
/// deleted. Proven by mutation: with the four dates above this line and none
/// below it, dropping the correction left the suite green. 2000-02-29,
/// 1600-02-29 and 2400-02-29 are the boundary days in reach, and 1600 is also
/// the negative-`days` branch of the `era` floor-division.
#[test]
fn the_calendar_conversion_agrees_with_dates_checked_elsewhere() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    // A leap day in a century that is a leap year by the 400 rule.
    assert_eq!(civil_from_days(1_709_164_800 / 86_400), (2024, 2, 29));
    assert_eq!(civil_from_days(1_779_108_120 / 86_400), (2026, 5, 18));
    // The last day of an era, in both directions from the epoch.
    assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    assert_eq!(civil_from_days(-135_081), (1600, 2, 29));
    assert_eq!(civil_from_days(157_113), (2400, 2, 29));
    // And the first day of one, which is where the era shift puts its origin.
    assert_eq!(civil_from_days(11_017), (2000, 3, 1));
}

// endregion: The relative time column

// region: Traces

/// The step walk keeps what the flat lists throw away: what followed what.
/// A completed run is a turn, then the calls that turn issued, in the order
/// `agent.rs` appended them, with each call's status its own record's answer.
#[test]
fn a_trace_is_the_records_in_order_with_each_calls_own_answer() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "Fix the flaky retry test"),
            model_call(T0 + 100, 1, 125),
            rec(
                T0 + 200,
                "tool_call",
                json!({ "id": "t1", "tool": "Read", "args": {} }),
            ),
            rec(
                T0 + 700,
                "tool_result",
                json!({ "id": "t1", "tool": "Read",
                        "block": { "type": "tool_result", "tool_use_id": "t1", "content": "ok" } }),
            ),
            rec(
                T0 + 800,
                "tool_call",
                json!({ "id": "t2", "tool": "Bash", "args": {} }),
            ),
            rec(
                T0 + 900,
                "tool_failed",
                json!({ "id": "t2", "tool": "Bash", "detail": "exited with 1" }),
            ),
            rec(
                T0 + 950,
                "tool_call",
                json!({ "id": "t3", "tool": "Write", "args": {} }),
            ),
            rec(
                T0 + 960,
                "denied",
                json!({ "id": "t3", "by": "user", "reason": "the user said no" }),
            ),
            finished(T0 + 2_000, "done", 1, 125, 2_000),
        ],
    );
    let t = trace(dir.path(), "sess-1700000000000-1#1", T0 + 3_000)
        .expect("trace")
        .expect("the run is in the file");
    assert_eq!(t.turns(), 1);
    assert_eq!(t.calls(), 3);
    assert_eq!(t.tokens.calls, 1);
    let shape: Vec<(String, Option<CallStatus>)> = t
        .steps
        .iter()
        .map(|s| match s {
            Step::Turn(t) => (format!("turn {}", t.iteration), None),
            Step::Call(c) => (c.tool.clone(), Some(c.status)),
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("turn 1".to_string(), None),
            ("Read".to_string(), Some(CallStatus::Ok)),
            ("Bash".to_string(), Some(CallStatus::Error)),
            ("Write".to_string(), Some(CallStatus::Denied)),
        ],
    );
    // The elapsed is call to answer, and only where both records exist.
    let Step::Call(read) = &t.steps[1] else {
        panic!("step 1 is the Read call");
    };
    assert_eq!(read.elapsed_ms, Some(500));
    assert_eq!(
        t.denials.len(),
        1,
        "the refusal is kept for the detail pane"
    );
    assert_eq!(
        t.denials[0].tool, "Write",
        "joined back to the call it answers"
    );
    assert_eq!(t.row.ending.as_deref(), Some("done"));
}

/// A call with no answer in the file stays unanswered. The trace never
/// decides what a missing record would have said.
#[test]
fn a_trace_leaves_a_call_with_no_answer_unanswered() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "still going"),
            model_call(T0 + 100, 1, 125),
            rec(
                T0 + 200,
                "tool_call",
                json!({ "id": "t1", "tool": "Bash", "args": {} }),
            ),
        ],
    );
    let t = trace(dir.path(), "sess-1700000000000-1#1", T0 + 1_000)
        .expect("trace")
        .expect("the run is in the file");
    assert_eq!(t.row.status, RunStatus::Running);
    let Step::Call(call) = &t.steps[1] else {
        panic!("step 1 is the call");
    };
    assert_eq!(call.status, CallStatus::Unanswered);
    assert_eq!(call.elapsed_ms, None, "no answer, no elapsed");
}

/// An id no file holds is `None`, not an empty trace: the caller has to be
/// able to tell "this run is gone" from "this run did nothing".
#[test]
fn a_trace_for_a_run_that_is_not_there_is_none() {
    let (dir, _) = sess("sess-1700000000000-1", &[goal_rec(T0, "one")]);
    assert_eq!(
        trace(dir.path(), "sess-nope#1", T0 + 10).expect("trace"),
        None
    );
}

/// `detail` and `trace` slice the same records the same way and answer calls
/// through the same function, so their call tables cannot disagree.
#[test]
fn the_trace_and_the_detail_agree_about_every_call() {
    let (dir, _) = sess(
        "sess-1700000000000-1",
        &[
            goal_rec(T0, "one of each"),
            model_call(T0 + 10, 1, 125),
            rec(
                T0 + 20,
                "tool_call",
                json!({ "id": "t1", "tool": "Read", "args": {} }),
            ),
            rec(
                T0 + 30,
                "tool_unknown",
                json!({ "id": "t1", "tool": "Read" }),
            ),
            rec(
                T0 + 40,
                "tool_call",
                json!({ "id": "t2", "tool": "Bash", "args": {} }),
            ),
            rec(
                T0 + 50,
                "tool_cancelled",
                json!({ "id": "t2", "tool": "Bash" }),
            ),
        ],
    );
    let id = "sess-1700000000000-1#1";
    let d = detail(dir.path(), id, T0 + 60).expect("detail").unwrap();
    let t = trace(dir.path(), id, T0 + 60).expect("trace").unwrap();
    let from_trace: Vec<ToolCallRow> = t
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Call(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(d.calls, from_trace);
    assert_eq!(d.tokens, t.tokens);
}

// endregion: Traces

// region: The real session directory

/// Every feed, over the real `~/.emma/sessions`, asserting only what must hold
/// of any file the loop wrote.
///
/// **`#[ignore]`d on purpose.** Its input is a home directory: it passes or
/// fails for reasons that have nothing to do with the change under review, and
/// a suite that depends on one is a suite nobody can trust on a fresh machine.
/// It is here because fixtures agree with their author and this repository has
/// been bitten three times by that — so the port was certified by running this
/// by hand, with `--nocapture`, and reading what it printed.
///
/// ```text
/// cargo test -p emma --lib harness_state -- --ignored --nocapture
/// ```
#[test]
#[ignore = "reads the real ~/.emma/sessions; run by hand"]
fn the_real_session_directory_reads_without_inventing_anything() {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("a home directory");
    let dir = Path::new(&home).join(".emma").join("sessions");
    if !dir.is_dir() {
        println!("no {} on this machine — nothing to certify", dir.display());
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_millis() as u64;

    let feed = runs(&dir, now).expect("the real directory reads");
    println!(
        "{} runs, {} damaged lines across {} file(s), {} unreadable",
        feed.runs.len(),
        feed.skipped_lines(),
        feed.damage.len(),
        feed.unreadable.len()
    );
    for d in &feed.damage {
        println!("  damaged {}: lines {:?}", d.path.display(), d.lines);
    }
    for u in &feed.unreadable {
        println!("  unreadable {}: {}", u.path.display(), u.problem);
    }
    assert!(!feed.runs.is_empty(), "the machine has run Emma");

    let mut statuses = std::collections::BTreeMap::new();
    for row in &feed.runs {
        *statuses
            .entry(format!("{:?}", row.status))
            .or_insert(0usize) += 1;
        assert!(row.started_ms > 0, "every run has a start: {row:?}");
        assert!(
            row.last_event_ms >= row.started_ms,
            "a run's last record is not before its first: {row:?}"
        );
        // The one inference, checked against the file rather than assumed.
        match row.status {
            RunStatus::Running | RunStatus::Stalled => assert!(
                row.ending.is_none(),
                "an unfinished run must not carry an ending: {row:?}"
            ),
            RunStatus::Completed | RunStatus::Failed => assert!(
                row.ending.is_some(),
                "a finished run names its ending: {row:?}"
            ),
        }
    }
    println!("statuses: {statuses:?}");

    // Every run round-trips through both readers, which is where a slicing bug
    // in `records_of` would show up on a shape no fixture has.
    for row in &feed.runs {
        let d = detail(&dir, &row.id, now)
            .expect("detail reads")
            .unwrap_or_else(|| panic!("a run `runs` listed is a run `detail` finds: {}", row.id));
        let t = trace(&dir, &row.id, now)
            .expect("trace reads")
            .expect("same");
        assert_eq!(d.tokens, t.tokens, "two readers, one total: {}", row.id);
        assert_eq!(d.calls.len(), t.calls(), "two readers, one call list");
        for call in &d.calls {
            if call.status == CallStatus::Unanswered {
                assert_eq!(call.elapsed_ms, None, "no answer, no elapsed: {call:?}");
            }
        }
    }
    println!(
        "{} runs round-tripped through detail and trace",
        feed.runs.len()
    );
}

// endregion: The real session directory
