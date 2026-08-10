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
//! resume could fold back into a message list. (There is no resume flag today;
//! `cli.rs` has no such option.) That is a file with one JSON
//! object per line: a truncated final line is the only damage a crash can do,
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
//! **Resume is not built, and the records as written today would not support
//! it.** What is stored is a human-readable audit trail, not a replayable
//! transcript: an `assistant` record carries `turn.text` only, and a
//! `tool_result` record carries the rendered content string rather than the
//! `{"type":"tool_result", …}` block that was actually sent. See
//! `Agent::run_goal` and `Agent::run_tool_call` in `agent.rs` for the full list
//! of record kinds.
//!
//! The gap that matters for a future resume is `raw_content`. The loop echoes
//! the provider's own content array back on the next call because thinking-block
//! signatures do not survive reassembly — and that array is never written here,
//! so a fold over this file cannot reproduce it. Storing `raw_content` verbatim
//! is therefore the first change resume needs, not an optimisation on top of
//! one; reconstructing an assistant turn from its text would produce exactly
//! the modified blocks this model family rejects.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
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
}
