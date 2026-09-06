//! The records a run graph can be drawn from, and nothing it would have to
//! guess.
//!
//! **Named `runfacts` rather than `telemetry`, and the rename is the point.**
//! The fork called this `telemetry`, a word that means "measurements sent
//! somewhere". This sends nothing anywhere: every record goes to the local
//! session JSONL, there is no network, no prompt text, and no hostname. A user
//! reading `telemetry.rs` in a coding agent would reasonably assume they were
//! being reported on, and being wrong about that in the reassuring direction is
//! worse than a clumsy name.
//!
//! The rule it keeps: a screen must never claim what the store cannot back, so
//! every field is something the process measured when it wrote the record, and
//! a field that would have to be estimated is absent instead.
//!
//! Everything else in this crate writes records so a human, a resume or a
//! footer can read one run back. These four are written for a *reader that
//! draws a picture*: which agent ran under which, when it started and stopped,
//! how long it waited for its turn, and how much of a task list is ticked.
//!
//! What the absent-instead rule has already refused: no `retries` (nothing
//! retries a delegation), no `host` (nothing in this workspace asks the
//! operating system its name, and adding a crate to invent one would be
//! inventing the field — and inventing it under this module's old name would
//! have been inventing the thing the rename exists to deny), no `threads` (a
//! nested run is a future on the same runtime, not a thread of its own), and no
//! ETA (there is no completion-rate model here, and a number derived from one
//! would be the estimate wearing a measurement's clothes).
//!
//! # The addressing scheme
//!
//! A *node* is one agent run. Its id is the one the session file already uses
//! to tell two runs apart, so nothing new has to be correlated:
//!
//! - The **root** node is the session's own goal, addressed by the session id
//!   ([`crate::session::SessionLog::new_id`] — `sess-<ms>-<pid>`). It is
//!   implicit: nothing spawns it, so nothing writes a `node_spawn` for it, and
//!   its records are the un-prefixed ones.
//! - A **nested** node is one delegation, addressed by the `sub_id`
//!   [`crate::session::SessionLog::subagent`] already stamps onto every record
//!   that run writes. `delegate.rs` builds it as `sub-<parent turn id>-<n>`
//!   from a per-registry counter — unique within a session because a turn id
//!   is, and stable because it is written down rather than recomputed.
//!
//! Both shapes were re-read against the live `session.rs` and `delegate.rs`
//! when this was ported, rather than carried over from the fork's older,
//! smaller `session.rs`. They agree.
//!
//! Delegation cannot nest, because `delegate.rs` builds the child registry
//! without the `Delegate` tool at all, so every `parent_node_id` written today
//! is the root. The field is written anyway rather than implied, because the
//! day nesting becomes possible the reader should not need changing.
//!
//! # The schemas
//!
//! Every record also carries `kind` and `at_ms` from
//! [`crate::session::SessionLog::append`], where `at_ms` is milliseconds since
//! the Unix epoch, taken when the record was written.
//!
//! ```text
//! kind: "node_spawn"        one nested agent run started
//!   node_id         string  this run, "sub-<parent turn id>-<n>"
//!   parent_node_id  string  the run that asked for it; the session id at the root
//!   parent_turn_id  string  the parent turn whose tool call this was
//!   agent           string  the agent type's name, from agents/<name>.md
//!   model           string  Provider::model_id() for the provider this run
//!                           will actually be sent to
//!   pid             number  std::process::id(), see "worker identity" below
//!
//! kind: "node_done"         the same run stopped, however it stopped
//!   node_id         string  matches the node_spawn
//!   parent_node_id  string  matches the node_spawn
//!   agent           string  matches the node_spawn
//!   ending          string  Ending::as_str: done | answered | tokens | iterations |
//!                           wall_clock | interrupted | provider | ...
//!   tokens          number  what this run spent, see "token attribution" below
//!   iterations      number  model calls this run made
//!   elapsed_ms      number  wall time of the nested goal, from its own
//!                           sub.goal_finished record. ABSENT when that record
//!                           never arrived — see below.
//!
//! kind: "queue_wait"        one delegation's turn at the one-at-a-time permit
//!   node_id         string  matches the node_spawn that follows it
//!   agent           string
//!   requested_at_ms number  when the permit was asked for
//!   acquired_at_ms  number  when it was held; equals requested_at_ms when nothing waited
//!   waited_ms       number  the difference, written out so a reader need not subtract
//!
//! kind: "task_progress"     the checkbox file changed under a running agent
//!   done            number  tasks at [x]
//!   total           number  tasks in .emma/tasks/tasks.md, of any status
//! ```
//!
//! # One field the fork defaulted, and why it is now absent
//!
//! The fork wrote `elapsed_ms: 0` when the nested run left no
//! `sub.goal_finished` behind, and its own doc told the reader to treat zero as
//! unknown. That is the estimate-in-measurement's-clothes shape this module
//! exists to refuse, one field further down: a graph drawing bars from these
//! records would draw a zero-width bar for a run that in fact ran for a minute
//! and was killed, and nothing on the record distinguishes that from a
//! delegation that finished instantly. The convention was documented, which is
//! not the same as being on the record — a reader who has the file and not this
//! page sees a number.
//!
//! So [`NodeDone::elapsed_ms`] is an `Option<u64>` and the key is simply not
//! written when it is `None`. Absent is the one value a reader cannot mistake
//! for a measurement.
//!
//! # Queue depth is derived, not recorded
//!
//! Nothing counts how many delegations are waiting, because a counter is a
//! second source of truth that can disagree with the timestamps. The honest
//! record is the interval, and depth at any instant `t` is the number of
//! `queue_wait` records whose `requested_at_ms <= t < acquired_at_ms`, which a
//! reader can compute exactly. With one permit the depth is also exactly the
//! number of delegations that had started and not yet finished.
//!
//! # Worker identity
//!
//! `pid` is `std::process::id()` and it is the same number on every node,
//! because a nested run is a second `Agent::run_goal` on this process's own
//! runtime rather than a second process. That is worth writing down precisely
//! because it is the field most likely to be misread as evidence of a worker
//! pool. There is no host field: see the rule at the top.
//!
//! # Token attribution, and its one gap
//!
//! `node_done.tokens` is the nested run's own meter. [`crate::agent::Spend`]
//! gives each nested goal a child meter that charges the parent as it goes, and
//! this is what that child read when the goal ended. It is therefore a real
//! per-node number and not a share-out of a total.
//!
//! Two things it is not, both of which matter to anyone summing these:
//!
//! 1. **It is cost-weighted, not the provider's token count.** `cost_tokens`
//!    weights a cache write at 1.25x and a cache read at 0.1x, because that is
//!    what a budget should count. The provider's own counts stay in the
//!    `sub.model_call` records.
//! 2. **It excludes what the delegation cost its parent.** The brief the parent
//!    wrote, and the report plus footer it read back, are input and output
//!    tokens on the *parent's* next model call. They are charged to the parent's
//!    meter, correctly, and no field here attributes them to the child. Summing
//!    every `node_done.tokens` therefore under-counts the goal's total, and the
//!    difference is the parent's own work. `goal_finished.tokens` is the total.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::session::SessionLog;

// region: A node in the graph
// ---------------------------------------------------------------------------
// A node in the graph
//
// Structs rather than argument lists, because the fields are the schema: a
// reader of the module doc above should be able to find each documented field
// once, spelled the same way, in the type that writes it.
// ---------------------------------------------------------------------------

/// One nested agent run starting. See the module doc for the field meanings.
pub struct NodeSpawn<'a> {
    pub node_id: &'a str,
    pub parent_node_id: &'a str,
    pub parent_turn_id: &'a str,
    pub agent: &'a str,
    pub model: &'a str,
}

impl NodeSpawn<'_> {
    /// Write it. On the *parent's* log view, so the kind is un-prefixed: this
    /// is the parent recording what it started, not the child narrating itself.
    pub fn write(&self, log: &SessionLog) {
        log.append(
            "node_spawn",
            json!({
                "node_id": self.node_id,
                "parent_node_id": self.parent_node_id,
                "parent_turn_id": self.parent_turn_id,
                "agent": self.agent,
                "model": self.model,
                // Real, and the same on every node. See the module doc.
                "pid": std::process::id(),
            }),
        );
    }
}

/// The same run stopping, whatever the ending.
pub struct NodeDone<'a> {
    pub node_id: &'a str,
    pub parent_node_id: &'a str,
    pub agent: &'a str,
    pub ending: &'a str,
    pub tokens: i64,
    pub iterations: u32,
    /// From the nested run's own `sub.goal_finished`, and `None` when that
    /// record never arrived — a run that did not reach its own ending.
    ///
    /// `None` is written by leaving the key out rather than by writing a
    /// sentinel; the module doc argues why at length. A caller that has the
    /// record has the number; a caller that does not must not invent one here,
    /// where it would be indistinguishable from a measurement.
    pub elapsed_ms: Option<u64>,
}

impl NodeDone<'_> {
    pub fn write(&self, log: &SessionLog) {
        let mut payload = json!({
            "node_id": self.node_id,
            "parent_node_id": self.parent_node_id,
            "agent": self.agent,
            "ending": self.ending,
            "tokens": self.tokens,
            "iterations": self.iterations,
        });
        if let Some(elapsed_ms) = self.elapsed_ms {
            // `expect` on a literal object this function just built: the only
            // way this is not an object is an edit to the three lines above.
            payload
                .as_object_mut()
                .expect("json! literal above is an object")
                .insert("elapsed_ms".into(), json!(elapsed_ms));
        }
        log.append("node_done", payload);
    }
}

// endregion: A node in the graph

// region: The queue
// ---------------------------------------------------------------------------
// The queue
//
// Two timestamps and their difference. Everything a queue display wants (depth,
// the longest wait, who was blocking whom) falls out of intervals, and none of
// it needs a counter that could be wrong.
//
// The timestamps come from the caller rather than from a clock in here, on
// purpose: the two instants worth recording are "just before `acquire().await`"
// and "just after", and only the caller is at those two points. It also leaves
// this module with exactly one clock read, `stamp`, which is what makes every
// test below assertable without a sleep.
// ---------------------------------------------------------------------------

/// One delegation's wait for the one-at-a-time permit.
pub struct QueueWait<'a> {
    pub node_id: &'a str,
    pub agent: &'a str,
    pub requested_at_ms: u64,
    pub acquired_at_ms: u64,
}

impl QueueWait<'_> {
    /// Written *after* the permit is held, because until then there is no
    /// second timestamp to write and a record with one honest field and one
    /// blank is worse than a record a few milliseconds late.
    pub fn write(&self, log: &SessionLog) {
        log.append(
            "queue_wait",
            json!({
                "node_id": self.node_id,
                "agent": self.agent,
                "requested_at_ms": self.requested_at_ms,
                "acquired_at_ms": self.acquired_at_ms,
                // Saturating, so a clock that steps backwards mid-wait reads as
                // zero rather than as an enormous wait. `at_ms` comes from
                // `SystemTime`, which an NTP correction can move backwards
                // under a running process; the subtraction is the one place in
                // this module where that would become a visible lie rather than
                // two timestamps a reader can see are out of order.
                "waited_ms": self.acquired_at_ms.saturating_sub(self.requested_at_ms),
            }),
        );
    }
}

// endregion: The queue

// region: Progress
// ---------------------------------------------------------------------------
// Progress
//
// "6 / 11" is the one number on a run graph that is about the work rather than
// about the machinery, and the only thing in this process that knows what the
// work is, is the task file.
//
// **Why it is sampled rather than emitted by the task tools.** `tools/tasks`
// does not depend on this crate and must not: it is a `Tool` over a markdown
// file, and giving it a `SessionLog` would invert the dependency for the same
// reason `delegate.rs` cannot live under `tools/`. Sampling here also catches
// the other writer the design explicitly allows, a human with the file open in
// an editor, which a record written inside `TaskUpdate` never would.
//
// **Why the loop does not branch on a tool's name to do it.** `agent.rs` states
// the invariant that nothing there branches on a tool's identity. So the sample
// is taken after *every* tool call and a record is written only when the
// numbers moved, which needs no knowledge of which tools exist.
// ---------------------------------------------------------------------------

/// How much of the task list is ticked: `(done, total)`.
///
/// A missing or unreadable file is `(0, 0)`. A project with no task list is a
/// fact about the project, not an error, and `store::load` already rules that
/// way for the tools.
///
/// That conflation is safe only because of [`worth_recording`], which drops
/// every sample whose total is zero: an unreadable file therefore never reaches
/// a record claiming the project has no tasks. If that rule is ever relaxed,
/// this function has to start distinguishing the two cases first.
///
/// `done` counts what the task tools call closed — `[x]`. An in-progress `[~]`
/// is still open, which is `Status::is_open`'s ruling and not a second opinion
/// held here.
pub fn sample_tasks(cwd: &Path) -> (usize, usize) {
    let ctx = emma_tool_api::ToolCtx {
        cwd: cwd.to_path_buf(),
        session_id: String::new(),
        turn_id: String::new(),
        background: Default::default(),
    };
    let Ok(file) = emma_tools_tasks::store::tasks_path(&ctx) else {
        return (0, 0);
    };
    let Ok((doc, _)) = emma_tools_tasks::store::load(&file) else {
        return (0, 0);
    };
    let tasks = doc.tasks();
    let done = tasks.iter().filter(|t| !t.status.is_open()).count();
    (done, tasks.len())
}

/// Whether a sample is worth a record, given the last one written.
///
/// Two rules, both about not filling a session file with nothing:
///
/// - An unchanged pair says nothing a reader cannot already see, so it is
///   dropped. The record stream is the changes; the current state is the last
///   record.
/// - A list with no tasks in it at all is dropped even the first time. Almost
///   every session never opens a task list, and `0 / 0` written after every
///   tool call would be a progress bar for work nobody planned.
pub fn worth_recording(previous: Option<(usize, usize)>, now: (usize, usize)) -> bool {
    now.1 > 0 && previous != Some(now)
}

/// The record itself: `done` of `total`, at the moment it was read.
pub fn task_progress(log: &SessionLog, done: usize, total: usize) {
    log.append("task_progress", json!({ "done": done, "total": total }));
}

// endregion: Progress

// region: Reading them back
// ---------------------------------------------------------------------------
// Reading them back
//
// One accessor, used by the tests here and available to whatever draws the
// graph, so that "which records are the graph's" is answered in this file
// rather than by a string literal somewhere else.
// ---------------------------------------------------------------------------

/// The record kinds this module writes, for a reader that wants to filter.
///
/// **This is a wire format.** These four strings are in every session JSONL
/// ever written with this module compiled in, and anything that reads those
/// files back matches on them. Changing one is not a rename.
pub const KINDS: &[&str] = &["node_spawn", "node_done", "queue_wait", "task_progress"];

/// A timestamp taken the same way every `at_ms` is, for callers that need one
/// before they have a record to write it into — the `queue_wait` pair, both of
/// which are read outside any `append`.
///
/// **A duplicate of the arithmetic inside [`crate::session::SessionLog::append`]
/// rather than a call to it**, because that computation is inline there and
/// this module does not own `session.rs`. The duplication is the whole hazard:
/// the two must agree about the epoch and the unit, or a `queue_wait` interval
/// and the `at_ms` on the record carrying it would be in different scales and a
/// graph would place the wait somewhere it did not happen. That agreement is
/// what `stamp_is_in_the_same_units_and_epoch_as_a_record_it_brackets` holds,
/// by bracketing a real `append` between two `stamp` calls — an ordering, not a
/// magnitude, so a slow machine cannot make it wrong. Lifting `now_ms` back
/// into `session.rs` and calling it from here would delete the hazard outright,
/// and is the right fix the day that file is open for editing.
pub fn stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// endregion: Reading them back

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The writers against a real session file, because the deliverable is the file
// and a test that asserted against an in-memory value would be asserting about
// the wrong artefact. Every assertion below reads back through
// `SessionLog::read`, so what is checked is what a reader of the JSONL will
// find, not what was handed to `write`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;

    fn records(log: &SessionLog) -> Vec<Value> {
        SessionLog::read(&log.path()).unwrap()
    }

    fn of_kind<'a>(records: &'a [Value], kind: &str) -> Vec<&'a Value> {
        records.iter().filter(|r| r["kind"] == kind).collect()
    }

    /// The keys actually present on a record, so a test can assert a whole
    /// shape rather than the fields it happened to think of.
    fn keys(record: &Value) -> BTreeSet<&str> {
        record
            .as_object()
            .expect("a record is an object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn set(names: &[&'static str]) -> BTreeSet<&'static str> {
        names.iter().copied().collect()
    }

    #[test]
    fn a_spawn_and_its_done_address_the_same_node_and_name_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();
        NodeSpawn {
            node_id: "sub-turn-1-1",
            parent_node_id: "sess-0000000000001-1",
            parent_turn_id: "turn-1",
            agent: "coder",
            model: "fake",
        }
        .write(&log);
        NodeDone {
            node_id: "sub-turn-1-1",
            parent_node_id: "sess-0000000000001-1",
            agent: "coder",
            ending: "done",
            tokens: 1_234,
            iterations: 3,
            elapsed_ms: Some(4_000),
        }
        .write(&log);

        let records = records(&log);
        let spawn = of_kind(&records, "node_spawn");
        let done = of_kind(&records, "node_done");
        assert_eq!(spawn.len(), 1, "{records:?}");
        assert_eq!(done.len(), 1, "{records:?}");
        assert_eq!(spawn[0]["node_id"], done[0]["node_id"]);
        // The link the graph draws an edge along, and the root's own address.
        assert_eq!(spawn[0]["parent_node_id"], "sess-0000000000001-1");
        assert_eq!(spawn[0]["agent"], "coder");
        assert_eq!(spawn[0]["model"], "fake");
        assert_eq!(spawn[0]["pid"], std::process::id());
        assert_eq!(done[0]["tokens"], 1_234);
        assert_eq!(done[0]["ending"], "done");
        assert_eq!(done[0]["elapsed_ms"], 4_000);
        // Stamped by `append`, and what everything here is ordered by.
        assert!(spawn[0]["at_ms"].as_u64().is_some(), "{records:?}");
        assert!(
            spawn[0]["at_ms"].as_u64() <= done[0]["at_ms"].as_u64(),
            "{records:?}"
        );
    }

    /// The absent-instead rule, at the one field where a value was available
    /// and was refused. A sentinel would satisfy every other assertion in this
    /// file, so this is the only place the rule is enforced rather than
    /// described.
    #[test]
    fn an_unmeasured_elapsed_is_missing_from_the_record_rather_than_zero() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000007-1").unwrap();
        NodeDone {
            node_id: "sub-turn-1-1",
            parent_node_id: "sess-0000000000007-1",
            agent: "coder",
            // A run killed before it wrote its own `sub.goal_finished`: there
            // is an ending, a token count and an iteration count, and no
            // elapsed time anywhere in the process to read.
            ending: "interrupted",
            tokens: 900,
            iterations: 2,
            elapsed_ms: None,
        }
        .write(&log);

        let records = records(&log);
        let done = of_kind(&records, "node_done")[0];
        assert!(
            done.get("elapsed_ms").is_none(),
            "elapsed_ms must be absent, not a sentinel: {done:?}"
        );
        // The rest of the record is unaffected: this is a missing measurement,
        // not a degraded record.
        assert_eq!(done["ending"], "interrupted");
        assert_eq!(done["tokens"], 900);
    }

    /// The whole-shape assertion, and the one that would catch a `host`, a
    /// `retries` or an ETA being added to any of the four — the class of field
    /// the module doc refuses by name. An assertion naming only the fields it
    /// expects to find cannot see an extra one.
    #[test]
    fn each_record_carries_exactly_the_fields_the_schema_names() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000008-1").unwrap();
        NodeSpawn {
            node_id: "n",
            parent_node_id: "p",
            parent_turn_id: "turn-1",
            agent: "coder",
            model: "fake",
        }
        .write(&log);
        NodeDone {
            node_id: "n",
            parent_node_id: "p",
            agent: "coder",
            ending: "done",
            tokens: 1,
            iterations: 1,
            elapsed_ms: Some(5),
        }
        .write(&log);
        QueueWait {
            node_id: "n",
            agent: "coder",
            requested_at_ms: 1,
            acquired_at_ms: 2,
        }
        .write(&log);
        task_progress(&log, 1, 2);

        let records = records(&log);
        assert_eq!(
            keys(of_kind(&records, "node_spawn")[0]),
            set(&[
                "kind",
                "at_ms",
                "node_id",
                "parent_node_id",
                "parent_turn_id",
                "agent",
                "model",
                "pid"
            ])
        );
        assert_eq!(
            keys(of_kind(&records, "node_done")[0]),
            set(&[
                "kind",
                "at_ms",
                "node_id",
                "parent_node_id",
                "agent",
                "ending",
                "tokens",
                "iterations",
                "elapsed_ms"
            ])
        );
        assert_eq!(
            keys(of_kind(&records, "queue_wait")[0]),
            set(&[
                "kind",
                "at_ms",
                "node_id",
                "agent",
                "requested_at_ms",
                "acquired_at_ms",
                "waited_ms"
            ])
        );
        assert_eq!(
            keys(of_kind(&records, "task_progress")[0]),
            set(&["kind", "at_ms", "done", "total"])
        );
    }

    /// [`KINDS`] is what a reader filters on, so it is only true if it is both
    /// complete and exact. Asserting it against the kinds a file actually
    /// received catches a renamed kind string, a fifth record kind added
    /// without registering it, and an entry in the list that nothing writes.
    #[test]
    fn kinds_is_exactly_what_the_writers_put_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000009-1").unwrap();
        NodeSpawn {
            node_id: "n",
            parent_node_id: "p",
            parent_turn_id: "turn-1",
            agent: "coder",
            model: "fake",
        }
        .write(&log);
        NodeDone {
            node_id: "n",
            parent_node_id: "p",
            agent: "coder",
            ending: "done",
            tokens: 1,
            iterations: 1,
            elapsed_ms: None,
        }
        .write(&log);
        QueueWait {
            node_id: "n",
            agent: "coder",
            requested_at_ms: 1,
            acquired_at_ms: 1,
        }
        .write(&log);
        task_progress(&log, 1, 2);

        let written: BTreeSet<String> = records(&log)
            .iter()
            .filter_map(|r| r["kind"].as_str())
            .map(String::from)
            .collect();
        let declared: BTreeSet<String> = KINDS.iter().map(|k| (*k).to_string()).collect();
        assert_eq!(written, declared, "KINDS and the file disagree");
    }

    #[test]
    fn a_queue_wait_carries_both_timestamps_in_order_and_the_difference() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000002-1").unwrap();
        QueueWait {
            node_id: "sub-turn-1-2",
            agent: "reviewer",
            requested_at_ms: 1_000,
            acquired_at_ms: 1_750,
        }
        .write(&log);

        let records = records(&log);
        let wait = of_kind(&records, "queue_wait");
        assert_eq!(wait.len(), 1, "{records:?}");
        assert_eq!(wait[0]["requested_at_ms"], 1_000);
        assert_eq!(wait[0]["acquired_at_ms"], 1_750);
        assert_eq!(wait[0]["waited_ms"], 750);
    }

    #[test]
    fn a_clock_that_steps_backwards_reads_as_no_wait_rather_than_a_huge_one() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000003-1").unwrap();
        QueueWait {
            node_id: "sub-turn-1-1",
            agent: "planner",
            requested_at_ms: 2_000,
            acquired_at_ms: 1_000,
        }
        .write(&log);
        let records = records(&log);
        assert_eq!(of_kind(&records, "queue_wait")[0]["waited_ms"], 0);
    }

    /// Depth is the reader's arithmetic over intervals, and this is the shape
    /// that arithmetic runs on: two waits that overlap, one of which was still
    /// waiting when the other was granted.
    #[test]
    fn overlapping_waits_are_what_a_depth_of_two_looks_like() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000004-1").unwrap();
        QueueWait {
            node_id: "a",
            agent: "coder",
            requested_at_ms: 100,
            acquired_at_ms: 100,
        }
        .write(&log);
        QueueWait {
            node_id: "b",
            agent: "coder",
            requested_at_ms: 105,
            acquired_at_ms: 400,
        }
        .write(&log);

        let records = records(&log);
        let waits = of_kind(&records, "queue_wait");
        let depth_at = |t: u64| {
            waits
                .iter()
                .filter(|w| {
                    let from = w["requested_at_ms"].as_u64().unwrap();
                    let until = w["acquired_at_ms"].as_u64().unwrap();
                    from <= t && t < until
                })
                .count()
        };
        assert_eq!(depth_at(50), 0);
        assert_eq!(depth_at(200), 1, "one delegation was waiting at t=200");
        assert_eq!(depth_at(500), 0);
    }

    #[test]
    fn task_counts_come_from_the_file_a_human_can_also_edit() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join(".emma/tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(
            tasks.join("tasks.md"),
            "# Tasks\n\n- [x] one\n- [~] two\n- [ ] three\n",
        )
        .unwrap();
        // Three tasks, one of them closed. `[~]` is in progress, which is open:
        // counting it as done would draw a bar past the work.
        assert_eq!(super::sample_tasks(dir.path()), (1, 3));
    }

    #[test]
    fn a_project_with_no_task_file_reads_as_no_tasks_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(super::sample_tasks(dir.path()), (0, 0));
        // And nothing is written for it, ever: see `worth_recording`. This is
        // the half that makes the `(0, 0)`-for-unreadable conflation safe.
        assert!(!worth_recording(None, (0, 0)));
    }

    #[test]
    fn only_a_change_is_recorded_so_the_stream_is_the_changes() {
        assert!(worth_recording(None, (0, 3)));
        assert!(!worth_recording(Some((0, 3)), (0, 3)));
        assert!(worth_recording(Some((0, 3)), (1, 3)));
        // A task added is progress moving too, in the direction that matters
        // most for a display that would otherwise show 3 of 3 and stop.
        assert!(worth_recording(Some((1, 3)), (1, 4)));
    }

    #[test]
    fn the_progress_record_says_both_halves_of_the_fraction() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000005-1").unwrap();
        task_progress(&log, 6, 11);
        let records = records(&log);
        let progress = of_kind(&records, "task_progress");
        assert_eq!(progress[0]["done"], 6);
        assert_eq!(progress[0]["total"], 11);
    }

    /// A delegation's records must be the parent's, not the child's: written on
    /// a sub view they would come out as `sub.node_spawn` and a reader
    /// filtering on [`KINDS`] would see no graph at all.
    ///
    /// **What this cannot catch, stated because the omission is the kind this
    /// repository calls a false receipt.** Which view a record is written on is
    /// decided by the caller, so no edit inside this module can turn it red —
    /// eleven mutations were run against this file and none of them reached it.
    /// It is here as a worked example of the convention and as a guard against
    /// a future writer in this module reaching for a sub view; the property
    /// itself has to be defended where the call is made, in `delegate.rs`.
    #[test]
    fn the_graph_records_are_written_unprefixed_by_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000006-1").unwrap();
        let (sub, _tap) = log.subagent("sub-turn-1-1", "turn-1", "coder");
        NodeSpawn {
            node_id: "sub-turn-1-1",
            parent_node_id: "sess-0000000000006-1",
            parent_turn_id: "turn-1",
            agent: "coder",
            model: "fake",
        }
        .write(&log);
        // What the child writes, for contrast: same file, namespaced kind.
        sub.append("goal", json!({ "text": "a brief" }));

        let records = records(&log);
        let kinds: Vec<&str> = records.iter().filter_map(|r| r["kind"].as_str()).collect();
        assert!(kinds.contains(&"node_spawn"), "{kinds:?}");
        assert!(kinds.contains(&"sub.goal"), "{kinds:?}");
        assert!(!kinds.contains(&"sub.node_spawn"), "{kinds:?}");
    }

    /// The duplicated clock, held to the one thing that matters about it.
    ///
    /// A magnitude here would be a test that passes on this machine and fails
    /// on a loaded one, so this asserts an ordering: a stamp taken before a
    /// record cannot be after that record's `at_ms`, and one taken after cannot
    /// be before it. Both hold for any elapsed time and neither holds if
    /// `stamp` and `append` disagree about the unit or the epoch — seconds
    /// would fail the upper bound, nanoseconds and a non-Unix epoch the lower.
    #[test]
    fn stamp_is_in_the_same_units_and_epoch_as_a_record_it_brackets() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000010-1").unwrap();
        let before = stamp();
        task_progress(&log, 1, 2);
        let after = stamp();

        let at_ms = records(&log)[0]["at_ms"].as_u64().unwrap();
        assert!(before <= at_ms, "stamp {before} is after at_ms {at_ms}");
        assert!(at_ms <= after, "at_ms {at_ms} is after stamp {after}");
    }

    /// The claim the module's name is about, checked against the source rather
    /// than asserted in prose: nothing in here reaches a network, a process or
    /// the environment.
    ///
    /// A source-grep test is a weak instrument and this repository has been
    /// bitten by one before — it passed while the thing it guarded was disabled
    /// entirely. It is used here because the property is an *absence*, and an
    /// absence has no code path to exercise. It cannot prove the module sends
    /// nothing; it can only fail loudly the moment someone adds the shape of a
    /// sender, which is the day the name would stop being true.
    #[test]
    fn nothing_here_opens_a_socket_a_process_or_the_environment() {
        let source = include_str!("runfacts.rs");
        // Only the module's own code, for two reasons that both make the test
        // meaningless otherwise: the doc above discusses the absent `host`
        // field in prose, and the needle list below is itself a run of the very
        // strings being searched for. So: cut at the test module, then drop
        // comment lines.
        let (module, _tests) = source
            .split_once("\n#[cfg(test)]")
            .expect("this file has a test module");
        let code: String = module
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("//!")
            })
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "reqwest",
            "TcpStream",
            "UdpSocket",
            "Command::new",
            "std::env",
            "hostname",
        ] {
            assert!(
                !code.contains(forbidden),
                "`{forbidden}` in runfacts.rs: this module writes to the local \
                 session file and nowhere else, which is what its name promises"
            );
        }
    }
}

// endregion: Tests
