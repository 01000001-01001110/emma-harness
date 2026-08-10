//! Session persistence: one JSONL file per session under `~/.emma/sessions/`.
//!
//! **The choice, and the argument for it.** SQLite is the right answer for a
//! system with an HTTP server, concurrent turns from concurrent requests, and a
//! crash-recovery fold that has to decide whether a tool with a side effect had
//! already run. Rebuilding that fold on a text file would be a mistake.
//!
//! Emma has none of those. One process, one user, one writer, no recovery
//! requirement — a crashed run is re-run, because the thing it was doing is in
//! the user's working tree where they can see it. What is actually needed is an
//! append-only record that survives `kill -9` mid-write and that a future
//! resume can fold back into a message list — see [`fold`]. That is a file with
//! one JSON object per line: a truncated final line is the only damage a crash can do,
//! and it is skipped on read. SQLite would add a dependency, a schema, a
//! migration story and a binary file the user cannot `grep`, to buy durability
//! guarantees against writers that do not exist.
//!
//! **What would change the recommendation.** Any one of these, and it should
//! change the same day:
//!
//! - A second writer. A daemon, a watch mode, or two `emma` processes sharing
//!   one session id. Append-with-`O_APPEND` survives concurrent small writes on
//!   unix and does not on Windows; the moment that matters, this is wrong.
//! - Exactly-once semantics for tool side effects across a crash. The instant
//!   resume must *not* re-run something, a journalled result needs a
//!   transaction, not a line.
//! - Queries across sessions — "which session touched this file", "what did I
//!   spend last week". Folding every JSONL file to answer that is fine at a
//!   hundred sessions and absurd at ten thousand.
//!
//! **The record is sufficient for resume; the command is not built.** Every
//! value the loop appends to `query` is written here at the moment it is
//! appended, so [`fold`] returns the message list that was sent rather than a
//! reconstruction of it. That is the distinction the whole format turns on: the
//! loop echoes the provider's own content array back on the next call because
//! thinking-block signatures do not survive reassembly, so an `assistant`
//! record carries `raw_content` verbatim — rebuilding a turn from its `text`
//! would produce exactly the modified blocks this model family rejects. A
//! `tool_result` record carries the `{"type":"tool_result", …}` block that was
//! sent, including the failure blocks, because the `tool_use_id` in it is what
//! pairs a result with its call and a rendered string has no way to say which
//! call it answers. See `Agent::run_goal` and `Agent::run_tool_call` in
//! `agent.rs` for the full list of record kinds.
//!
//! It is still an audit trail as well: `text` stays on the `assistant` record
//! beside `raw_content`, duplicating bytes on purpose so a human running `grep`
//! over the file gets one plain line rather than an array of escaped blocks.
//!
//! What [`fold`] does not do is decide what a resumed run should *send* — that
//! list has a new user turn on the end, and its shape is the `--resume`
//! command's decision. There is no such flag today; `cli.rs` has no such
//! option. Nor does anything here address exactly-once tool side effects: a
//! resumed run re-runs from the last complete turn, and a turn whose tool calls
//! were not all answered is dropped rather than half-restored, because the API
//! rejects an unanswered `tool_use`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use emma_llm::Message;
use serde_json::{json, Value};

pub struct SessionLog {
    id: String,
    path: PathBuf,
    /// A plain `std::fs::File` behind a `std::sync::Mutex`, in an async
    /// program, on purpose: a record is a few hundred bytes and is written
    /// between a model call and a subprocess. Making it async would buy
    /// nothing measurable and would make "append the record, *then* do the
    /// thing" — the ordering the whole file exists for — an await point where
    /// a cancellation can land.
    file: Mutex<Option<File>>,
}

impl SessionLog {
    /// A session id that sorts by time and cannot collide between two `emma`
    /// processes started in the same millisecond.
    pub fn new_id() -> String {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("sess-{ms:013}-{}", std::process::id())
    }

    /// `~/.emma/sessions/`, or `EMMA_SESSION_DIR` when set — which is how a
    /// test writes somewhere other than the developer's real history.
    pub fn default_dir(home: Option<&Path>) -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("EMMA_SESSION_DIR") {
            return Some(PathBuf::from(dir));
        }
        home.map(|h| h.join(".emma").join("sessions"))
    }

    pub fn open(dir: &Path, id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("{id}.jsonl"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            id,
            path,
            file: Mutex::new(Some(file)),
        })
    }

    /// Records nothing. For tests that are not about persistence, and for the
    /// case where the home directory cannot be determined — an agent that
    /// refuses to run because it cannot write a transcript would be trading a
    /// working tool for a diary.
    pub fn none() -> Self {
        Self {
            id: "sess-none".into(),
            path: PathBuf::new(),
            file: Mutex::new(None),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record.
    ///
    /// Failures are swallowed after the first, deliberately: a full disk must
    /// not be the thing that kills a run halfway through editing someone's
    /// source tree. The caller has already been told once by `open`.
    pub fn append(&self, kind: &str, mut payload: Value) {
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        let Some(file) = guard.as_mut() else { return };
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("kind".into(), json!(kind));
            obj.insert(
                "at_ms".into(),
                json!(SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)),
            );
        }
        let mut line = payload.to_string();
        line.push('\n');
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }

    /// Every well-formed record in a session file, in order.
    ///
    /// A trailing partial line — the one thing a crash mid-write can leave — is
    /// skipped rather than failing the read, which is the property that makes
    /// this format survivable without a transaction.
    pub fn read(path: &Path) -> Result<Vec<Value>> {
        let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(raw
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }
}

// region: The fold
// ---------------------------------------------------------------------------
// The fold
//
// One session file back into the message list that was last sent. This is the
// half of resume that can be built and tested without deciding anything about
// a command: what comes out of here is what would go into `Request::history`
// and `Request::query` concatenated, and `tests/loop.rs` asserts it against
// what a real run actually sent.
// ---------------------------------------------------------------------------

/// The messages a session's last model call carried, in order.
///
/// Not "the conversation so far, tidied": every value here was written by the
/// loop at the moment it appended the same value to `query`, so what comes back
/// is the list itself rather than a reconstruction of it. That distinction is
/// the whole point — see the module doc on thinking-block signatures.
///
/// What it is *not*: the list a resumed run should send. That one has a new
/// user turn on the end and possibly a re-stated goal, and deciding its shape
/// is the `--resume` command's job, not this function's.
pub fn fold(path: &Path) -> Result<Vec<Message>> {
    Ok(fold_records(&SessionLog::read(path)?))
}

/// [`fold`] over records already read — the testable half, and the one a caller
/// that has the records for another reason should use.
pub fn fold_records(records: &[Value]) -> Vec<Message> {
    let mut fold = Fold::default();
    for record in records {
        fold.record(record);
    }
    fold.finish()
}

/// The state a walk over the records needs.
///
/// Two lists because the loop keeps two: `Agent::history` holds finished goals
/// collapsed to a goal and an answer, and `query` holds the goal in progress in
/// full. A fold that merged them would return the right conversation in the
/// wrong shape and would not equal what was sent.
#[derive(Default)]
struct Fold {
    history: Vec<Message>,
    query: Vec<Message>,
    goal_text: String,
    in_goal: bool,
    last_text: String,
    /// An assistant turn that has been read but not yet placed, because what
    /// follows it decides whether it can be placed at all.
    pending: Option<Value>,
    /// Result blocks for `pending`, accumulating until something closes them.
    results: Vec<Value>,
}

impl Fold {
    fn record(&mut self, r: &Value) {
        match r["kind"].as_str().unwrap_or_default() {
            "goal" => {
                self.close_turn();
                // A goal record while another goal is open means the previous
                // one ended — collapsing it here rather than at
                // `goal_finished` is deliberate: after a `goal_finished` the
                // loop still holds the finished goal's `query` and sends it
                // again if nothing else happens, so collapsing early would make
                // the fold disagree with the last call of a one-goal session.
                self.close_goal();
                self.in_goal = true;
                self.goal_text = string(r, "text");
                self.last_text.clear();
                self.query = vec![Message::user(string(r, "opening"))];
            }
            "assistant" => {
                self.close_turn();
                // Absent only in logs written before `raw_content` was stored.
                // The turn is dropped rather than rebuilt from `text`: a
                // rebuilt thinking block is rejected, so an approximation here
                // would be a resumed run that dies on its first call with an
                // error naming a signature nobody in the file mentions.
                self.pending = r.get("raw_content").cloned();
                let text = string(r, "text");
                if !text.trim().is_empty() {
                    self.last_text = text;
                }
            }
            "tool_result" => {
                if let Some(block) = r.get("block") {
                    self.results.push(block.clone());
                }
            }
            "kick" => {
                // A kick answers a turn that made no tool calls, so there is
                // nothing to pair. If the turn itself could not be placed the
                // kick goes with it — a user message with no assistant turn
                // before it would leave two user turns in a row.
                if let Some(raw) = self.pending.take() {
                    self.query.push(Message::assistant(raw));
                    self.query.push(Message::user(string(r, "text")));
                }
                self.results.clear();
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Message> {
        self.close_turn();
        let mut out = self.history;
        out.extend(self.query);
        out
    }

    /// Place the assistant turn and its results, or drop both.
    ///
    /// The rule the whole fold turns on: **the API rejects an assistant turn
    /// carrying a `tool_use` that no `tool_result` answers**, and equally a
    /// result answering nothing. A run killed between the model call and the
    /// last of its tools leaves exactly that in the file. Half a turn is not
    /// worth a message list that cannot be sent, so an unmatched turn is
    /// dropped whole and resume restarts one turn earlier.
    fn close_turn(&mut self) {
        let Some(raw) = self.pending.take() else {
            self.results.clear();
            return;
        };
        let results = std::mem::take(&mut self.results);
        if results.is_empty() || !answered(&raw, &results) {
            return;
        }
        self.query.push(Message::assistant(raw));
        self.query.push(Message::tool_results(results));
    }

    /// Collapse a finished goal the way `Agent::run_goal` does — the goal, and
    /// the last thing the assistant said, if it said anything.
    fn close_goal(&mut self) {
        if !self.in_goal {
            return;
        }
        self.history
            .push(Message::user(std::mem::take(&mut self.goal_text)));
        if !self.last_text.trim().is_empty() {
            self.history.push(Message::assistant(Value::String(
                std::mem::take(&mut self.last_text),
            )));
        }
        self.query.clear();
        self.in_goal = false;
    }
}

/// Whether every `tool_use` in an assistant turn has exactly one result, and
/// every result a call. Both directions, because the API refuses both ways
/// round.
fn answered(raw: &Value, results: &[Value]) -> bool {
    let mut calls: Vec<&str> = raw
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| b["id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut answers: Vec<&str> = results
        .iter()
        .filter_map(|b| b["tool_use_id"].as_str())
        .collect();
    calls.sort_unstable();
    answers.sort_unstable();
    calls == answers
}

fn string(r: &Value, key: &str) -> String {
    r[key].as_str().unwrap_or_default().to_string()
}

// endregion: The fold

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_carry_their_kind() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-test").unwrap();
        log.append("goal", json!({ "text": "do the thing" }));
        log.append("assistant", json!({ "text": "done" }));

        let records = SessionLog::read(log.path()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["kind"], "goal");
        assert_eq!(records[0]["text"], "do the thing");
        assert!(records[1]["at_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn a_torn_final_line_costs_only_that_line() {
        // The crash this format is chosen to survive: the process died between
        // two write syscalls. Everything before it must still read.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torn.jsonl");
        fs::write(&path, "{\"kind\":\"goal\",\"text\":\"a\"}\n{\"kind\":\"assis").unwrap();
        let records = SessionLog::read(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["text"], "a");
    }

    #[test]
    fn a_log_with_no_file_is_not_an_error() {
        SessionLog::none().append("goal", json!({ "text": "x" }));
    }

    #[test]
    fn ids_sort_by_time() {
        let a = SessionLog::new_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = SessionLog::new_id();
        assert!(a < b, "{a} !< {b}");
    }

    // -----------------------------------------------------------------------
    // The fold, on records a crash or an interrupt left half-written.
    //
    // The end-to-end round trip lives in `tests/loop.rs`, driven by the real
    // loop. These are the cases the loop cannot be made to produce on demand:
    // a file that stops mid-turn. Every one of them is about the same rule —
    // the API rejects an assistant turn carrying a `tool_use` that no
    // `tool_result` answers, so the fold must never hand one back.
    // -----------------------------------------------------------------------

    fn assistant_with(calls: &[&str]) -> Value {
        let blocks: Vec<Value> = calls
            .iter()
            .map(|id| json!({ "type": "tool_use", "id": id, "name": "Fine", "input": {} }))
            .collect();
        json!({ "kind": "assistant", "turn_id": "turn-1", "raw_content": blocks })
    }

    fn result_for(id: &str) -> Value {
        json!({
            "kind": "tool_result",
            "turn_id": "turn-1",
            "id": id,
            "tool": "Fine",
            "block": { "type": "tool_result", "tool_use_id": id, "content": "Fine ran" },
        })
    }

    fn opened() -> Value {
        json!({ "kind": "goal", "text": "g", "opening": "work on g" })
    }

    #[test]
    fn a_turn_whose_tool_calls_were_never_answered_is_dropped() {
        // The interrupt case: Ctrl-C between the model call and the first tool
        // result. Handing this turn back would produce a message list the API
        // refuses, which is a worse failure than resuming one turn earlier.
        let msgs = fold_records(&[opened(), assistant_with(&["tu_1"])]);
        assert_eq!(msgs, vec![Message::user("work on g")]);
    }

    #[test]
    fn a_partly_answered_turn_is_dropped_whole() {
        // Two calls, one result: the surviving `tool_use` is unanswered, so the
        // pair goes together or not at all. Keeping the half that is present is
        // the tempting wrong answer — it looks like more of the record survived
        // and produces exactly the 400 this rule exists to prevent.
        let msgs = fold_records(&[
            opened(),
            assistant_with(&["tu_1", "tu_2"]),
            result_for("tu_1"),
        ]);
        assert_eq!(msgs, vec![Message::user("work on g")]);
    }

    #[test]
    fn a_fully_answered_turn_survives() {
        // The positive control: without it, a fold that dropped every turn
        // would pass both tests above.
        let msgs = fold_records(&[
            opened(),
            assistant_with(&["tu_1", "tu_2"]),
            result_for("tu_1"),
            result_for("tu_2"),
        ]);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1].role, emma_llm::Role::Assistant);
        assert_eq!(msgs[2].content.as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_record_written_before_raw_content_existed_costs_its_turn_and_no_more() {
        // Logs written by an older build have no `raw_content`, and there is no
        // honest way to rebuild one: reassembling the turn from `text` is the
        // precise thing that invalidates a thinking-block signature. So the turn
        // is dropped rather than approximated, and the kick that answered it
        // goes with it — a lone user message after a dropped assistant turn
        // would leave two user turns in a row.
        let msgs = fold_records(&[
            opened(),
            json!({ "kind": "assistant", "turn_id": "turn-1", "text": "hello" }),
            json!({ "kind": "kick", "turn_id": "turn-1", "n": 1, "text": "keep going" }),
        ]);
        assert_eq!(msgs, vec![Message::user("work on g")]);
    }
}
