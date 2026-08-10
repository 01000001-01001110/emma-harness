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
//! **The record is sufficient for resume, and `emma --resume` is what spends
//! it.** Every
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
//! What [`fold`] does not do is decide what a resumed run should *send*, nor
//! what it has already spent. Both are [`restore`], further down: the messages
//! plus the goal's token, iteration and nudge counters and its failed-call
//! memo, because a resumed run that restarted those at zero would hand back a
//! fresh budget and make the cap that stopped it meaningless.
//!
//! Nothing here addresses exactly-once tool side effects, and nothing here
//! replays a tool. A resumed run restarts from the last complete turn; a turn
//! whose tool calls were not all answered is dropped rather than half-restored,
//! because the API rejects an unanswered `tool_use`, and the calls in it are
//! handed to the model as a gap in the record rather than re-run behind its
//! back.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use emma_llm::Message;
use serde_json::{json, Value};

use crate::agent::{label_of, memo_key_of, Resumed};

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
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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
            self.history
                .push(Message::assistant(Value::String(std::mem::take(
                    &mut self.last_text,
                ))));
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

// region: Resume
// ---------------------------------------------------------------------------
// Resume
//
// Three questions the fold deliberately does not answer: which file, what was
// already spent, and whether the harness that spent it is still the one in
// front of us. The fold is about the conversation; this is about everything
// else a run carried that a message list cannot express.
//
// What is *not* here, stated because its absence is a decision: replay.
// Nothing in this section runs a tool. A turn dropped for unmatched tool ids
// means those calls never happened as far as the restored conversation is
// concerned, and re-running them to close the gap would re-run a `Write` or a
// `Bash` against a working tree that has moved on since. Handing the model the
// record and letting it decide what to redo costs a turn; replaying costs
// whatever the tool did the second time.
// ---------------------------------------------------------------------------

/// Everything `--resume` needs out of one session file.
#[derive(Debug)]
pub struct Restored {
    /// The session id, which is the file stem — a resumed run appends to this
    /// same file rather than starting a new one, so the transcript of a goal
    /// stays in one place.
    pub id: String,
    pub path: PathBuf,
    pub resumed: Resumed,
    pub continuity: Continuity,
}

/// What the file says the interrupted run was booted against.
///
/// Every field is a string that may be empty, because a session written before
/// a field existed simply does not have it, and a warning derived from a
/// missing value is noise that trains people to ignore warnings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Continuity {
    pub instructions_hash: String,
    pub tool_schema_hash: String,
    pub model: String,
    pub cwd: String,
}

impl Continuity {
    /// What changed between the run that wrote the file and the run about to
    /// continue it, in sentences a user can act on.
    ///
    /// **Warn, do not refuse.** The user asked to resume; declining would leave
    /// them with a transcript and no way to use it, and the thing that is
    /// actually dangerous is not the change but the change being invisible. A
    /// conversation continuing under instructions it was never held to is the
    /// "booted on the wrong prompt" failure this project has already paid for,
    /// arriving one level along — so each line names the field, the old value
    /// and the new one.
    pub fn differences(
        &self,
        instructions_hash: &str,
        tool_schema_hash: &str,
        model: &str,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let mut check = |what: &str, was: &str, now: &str, cost: &str| {
            if !was.is_empty() && was != now {
                out.push(format!(
                    "{what} changed since this session ran: {was} → {now}. {cost}"
                ));
            }
        };
        check(
            "instructions",
            &self.instructions_hash,
            instructions_hash,
            "The restored conversation was produced under a prompt this run is not using.",
        );
        check(
            "tool schema",
            &self.tool_schema_hash,
            tool_schema_hash,
            "A tool the transcript calls may not exist now, or may take different arguments.",
        );
        check(
            "model",
            &self.model,
            model,
            "The turns being handed back were written by a different model.",
        );
        out
    }
}

/// Read one session file into everything a resumed run needs.
pub fn restore(path: &Path) -> Result<Restored> {
    let records = SessionLog::read(path)?;
    if !records.iter().any(|r| r["kind"] == "goal") {
        bail!(
            "{} records no goal, so there is nothing to resume",
            path.display()
        );
    }
    Ok(Restored {
        id: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.to_path_buf(),
        resumed: restore_records(&records),
        continuity: continuity_of(&records),
    })
}

/// [`restore`] over records already read — the testable half, and the half with
/// the reasoning in it.
///
/// **Every counter resets at a `goal` record.** Budgets are per goal in the
/// loop, so a session with three finished goals in it must not hand the fourth
/// the sum of the first three; what a resume inherits is the spend of the goal
/// it is continuing. `goal_finished` overrides the running totals where it is
/// present because it is the loop's own arithmetic; the running totals are what
/// answer the case the file exists for, which is a run that was killed and
/// never wrote one.
pub fn restore_records(records: &[Value]) -> Resumed {
    let mut r = Resumed {
        messages: fold_records(records),
        ..Default::default()
    };
    // `tool_call` carries the arguments and `tool_result` carries the verdict,
    // and they are two records joined by the call id. Neither alone is enough:
    // the memo is keyed on the arguments, and whether the call failed is only
    // in the block.
    let mut args: HashMap<String, (String, Value)> = HashMap::new();
    for record in records {
        match record["kind"].as_str().unwrap_or_default() {
            "goal" => {
                r.tokens = 0;
                r.iterations = 0;
                r.kicks = 0;
                r.failed_now.clear();
                r.failed_ever.clear();
                args.clear();
            }
            "model_call" => {
                r.iterations += 1;
                r.tokens += record["billable_total_tokens"].as_i64().unwrap_or(0);
            }
            "kick" => {
                r.kicks = record["n"].as_u64().unwrap_or(u64::from(r.kicks) + 1) as u32;
            }
            "tool_call" => {
                if let (Some(id), Some(tool)) = (str_of(record, "id"), str_of(record, "tool")) {
                    args.insert(id, (tool, record["args"].clone()));
                }
            }
            "tool_result" => {
                let failed = record["block"]["is_error"] == Value::Bool(true);
                if !failed {
                    // The loop's rule, replayed: something changed, so every
                    // earlier failure is worth trying again. A fold that only
                    // accumulated failures would restore a memo that forbids
                    // calls the original run had already re-allowed.
                    r.failed_now.clear();
                    continue;
                }
                // Only the failures that got as far as being dispatched are
                // recoverable, because only those wrote a `tool_call` with the
                // arguments in it. An unknown tool or a schema violation fails
                // before that record exists, and is not restored — the cost is
                // that the resumed model may re-issue one call that cannot run
                // and gets told so again, which is a wasted turn rather than a
                // side effect.
                let Some(id) = str_of(record, "id") else {
                    continue;
                };
                let Some((tool, input)) = args.get(&id) else {
                    continue;
                };
                let key = memo_key_of(tool, input);
                if !r.failed_now.contains(&key) {
                    r.failed_now.push(key);
                }
                let label = label_of(tool, input);
                if !r.failed_ever.contains(&label) {
                    r.failed_ever.push(label);
                }
            }
            "goal_finished" => {
                r.tokens = record["tokens"].as_i64().unwrap_or(r.tokens);
                r.iterations = record["iterations"]
                    .as_u64()
                    .unwrap_or(u64::from(r.iterations)) as u32;
                r.kicks = record["kicks"].as_u64().unwrap_or(u64::from(r.kicks)) as u32;
            }
            _ => {}
        }
    }
    r
}

/// The last `goal` record's account of the harness, because it is the one the
/// restored messages were actually produced under.
fn continuity_of(records: &[Value]) -> Continuity {
    let mut out = Continuity::default();
    for record in records {
        if record["kind"] == "goal" {
            out = Continuity {
                instructions_hash: string(record, "instructions_hash"),
                tool_schema_hash: string(record, "tool_schema_hash"),
                model: string(record, "model"),
                cwd: string(record, "cwd"),
            };
        }
    }
    out
}

/// Which session file `--resume` means.
///
/// **Named wins outright.** `--resume <id>` is a request for one session and is
/// not filtered by the working directory: a user who names a session has
/// already answered the question the directory rule exists to answer, and
/// second-guessing them would refuse a file that is plainly there.
///
/// **Bare `--resume` means "the one I was last running here".** Sessions from
/// every project share one directory, so the newest file overall is routinely
/// somebody else's work — resuming that into this tree is a conversation about
/// the wrong repository with write tools attached. Ids sort by time, so newest
/// is the last id rather than the newest mtime: an mtime moves when a file is
/// copied and an id does not.
///
/// Files are opened newest-first and the walk stops at the first match, so the
/// usual case reads one file. It is still a scan, and it is the "queries across
/// sessions" cost the module doc names as a thing that would change the format
/// if it ever mattered at scale.
pub fn locate(dir: &Path, id: Option<&str>, cwd: &Path) -> Result<PathBuf> {
    if let Some(id) = id {
        // Accept what `emma` prints at startup, which is a path ending in
        // `.jsonl`, as well as the bare id.
        let id = id.strip_suffix(".jsonl").unwrap_or(id);
        let path = dir.join(format!("{id}.jsonl"));
        if path.is_file() {
            return Ok(path);
        }
        bail!("no session `{id}` in {}", dir.display());
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    for path in candidates.iter().rev() {
        let Ok(records) = SessionLog::read(path) else {
            continue;
        };
        if records
            .iter()
            .any(|r| r["kind"] == "goal" && same_dir(&string(r, "cwd"), cwd))
        {
            return Ok(path.clone());
        }
    }
    bail!(
        "no session recorded in {} was run from {}. Name one with `emma --resume <id>`.",
        dir.display(),
        cwd.display()
    );
}

/// Whether a recorded working directory is this one. Canonicalised on both
/// sides rather than compared as bytes, for the reason `harness::discover_in`
/// gives: case, a trailing separator, a short name and a symlinked path are all
/// ways two spellings name one directory. An empty recording — a session from
/// before the field existed — matches nothing rather than everything.
fn same_dir(recorded: &str, cwd: &Path) -> bool {
    if recorded.is_empty() {
        return false;
    }
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    real(Path::new(recorded)) == real(cwd)
}

fn str_of(r: &Value, key: &str) -> Option<String> {
    r[key].as_str().map(str::to_string)
}

// endregion: Resume

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
        fs::write(
            &path,
            "{\"kind\":\"goal\",\"text\":\"a\"}\n{\"kind\":\"assis",
        )
        .unwrap();
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
