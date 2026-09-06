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
//! loop echoes the provider's own content blocks back on the next call because
//! thinking-block signatures do not survive reassembly, so an `assistant`
//! record carries them verbatim under `raw_content` — rebuilding a turn from its
//! `text` would produce exactly the modified blocks this model family rejects.
//! The key kept its name through the typing of those blocks so that a file
//! written by an earlier build still folds. A `tool_result` record carries the
//! `{"type":"tool_result", …}` block that was sent, including the failure
//! blocks, because the `tool_use_id` in it is what pairs a result with its call
//! and a rendered string has no way to say which call it answers. See `Agent::run_goal` and `Agent::run_tool_call` in
//! `agent.rs` for the full list of record kinds.
//!
//! It is still an audit trail as well: `text` stays on the `assistant` record
//! beside `raw_content`, duplicating bytes on purpose so a human running `grep`
//! over the file gets one plain line rather than an array of escaped blocks.
//!
//! **A finished goal is no longer collapsed here, and that is the point.** The
//! fold used to keep its own copy of `run_goal`'s two-line collapse — goal text,
//! last answer — which was one decision written twice: the moment either moved,
//! `--resume` rebuilt a conversation that was never sent, and nothing but the
//! round-trip test would have said so. A session is now one continuous
//! conversation, so a finished goal stays in it whole, and the only thing that
//! ever shortens it is compaction. Compaction writes a `compacted` record
//! carrying **both** how many messages off the front it replaced and the exact
//! messages it replaced them with, so [`fold`] replays it rather than
//! re-deriving it. There is no summarising code in this file to disagree with
//! the loop's.
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use emma_llm::{Content, ContentBlock, Message, Role, ToolResult};
use serde_json::{json, Value};

use crate::agent::{label_of, memo_key_of, Resumed};

/// The records one delegation wrote, in order — see [`SessionLog::subagent`].
pub type Records = Arc<Mutex<Vec<Value>>>;

/// Which file this log is on, and the handle open on it.
///
/// **One struct rather than three fields because they change together and must
/// never disagree.** An in-place `/resume` closes one session's file and opens
/// another's, and an id left pointing at the abandoned file would stamp the
/// resumed session's records with the wrong name — which is a transcript that
/// says it is a session it is not. See [`SessionLog::move_to`].
///
/// The file stays `Option` for the reason it always was: [`SessionLog::none`]
/// records nothing and has no handle to hold.
struct Handle {
    id: String,
    path: PathBuf,
    file: Option<File>,
}

pub struct SessionLog {
    /// A plain `std::fs::File` behind a `std::sync::Mutex`, in an async
    /// program, on purpose: a record is a few hundred bytes and is written
    /// between a model call and a subprocess. Making it async would buy
    /// nothing measurable and would make "append the record, *then* do the
    /// thing" — the ordering the whole file exists for — an await point where
    /// a cancellation can land.
    ///
    /// `Arc` because [`SessionLog::subagent`] hands out a second view of the
    /// *same* file rather than a second file, and because the process shares
    /// the log as `&SessionLog`: nothing anywhere holds a `&mut`, so moving
    /// the session onto another file has to happen through here. One process,
    /// one writer, one mutex — the module doc's "a second writer" caveat is
    /// about a second process, not a second logical run inside this one.
    handle: Arc<Mutex<Handle>>,
    /// Prefixed onto every `kind` this view writes. `None` for the session's own
    /// records; `Some("sub")` for a delegation's — see [`SessionLog::subagent`].
    prefix: Option<&'static str>,
    /// Merged into every payload this view writes, so two runs sharing one file
    /// are separable by `grep`.
    stamp: Vec<(String, Value)>,
    /// Every record this view wrote, kept in memory for whoever is composing a
    /// footer out of them.
    ///
    /// **This is what makes the delegation footer a record rather than a
    /// claim.** `Delegate` reads these back — the tool calls, their results, the
    /// denials, the ending — and writes the footer from them, so what the parent
    /// is told about what a subagent did comes from the loop rather than from the
    /// subagent. Reading the file back instead would work everywhere except
    /// where the log is [`SessionLog::none`], which is exactly where the tests
    /// are.
    tap: Option<Records>,
    /// Whether this file has already reported that it cannot be written.
    ///
    /// `Arc`, and shared with every subagent view, for the same reason `file`
    /// is: they are one file. A failing write fails on every record, so without
    /// this the run would bury itself in one complaint per append — and a
    /// delegation would repeat the complaint about a file its parent already
    /// reported.
    write_failed: Arc<AtomicBool>,
}

/// The last record an abandoned session file gets: this session moved to
/// another file, in this process, and nothing after this line belongs to it.
///
/// **The fold needs no arm for it, and that is a decision rather than an
/// omission.** Everything before it in that file is still that session's
/// conversation and a later `--resume` of it must get all of it back;
/// everything after it is in another file. [`Fold::record`] ignores a kind it
/// does not know, so an old build folds a moved session correctly too — and
/// `a_moved_record_costs_the_fold_nothing` is what stops somebody adding an
/// arm for it later on the reasonable-sounding grounds that every other record
/// has one.
pub const MOVED: &str = "session_moved";

/// The first record a session gets when it is picked back up inside a running
/// process: how many messages came back, and what was dropped to make room.
///
/// Ignored by the fold for the same reason [`MOVED`] is, and it is the sharper
/// of the two: the messages it counts are already in this file, *above* this
/// line, so an arm that replayed them would hand back the conversation twice.
/// `a_resumed_record_does_not_double_the_conversation` is that assertion.
pub const RESUMED: &str = "resumed";

impl SessionLog {
    /// A session id that sorts by time and cannot collide between two `emma`
    /// processes started in the same millisecond.
    pub fn new_id() -> String {
        format!("sess-{:013}-{}", now_ms(), std::process::id())
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
            handle: Arc::new(Mutex::new(Handle {
                id,
                path,
                file: Some(file),
            })),
            prefix: None,
            stamp: Vec::new(),
            tap: None,
            write_failed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Records nothing. For tests that are not about persistence, and for the
    /// case where the home directory cannot be determined — an agent that
    /// refuses to run because it cannot write a transcript would be trading a
    /// working tool for a diary.
    pub fn none() -> Self {
        Self {
            handle: Arc::new(Mutex::new(Handle {
                id: "sess-none".into(),
                path: PathBuf::new(),
                file: None,
            })),
            prefix: None,
            stamp: Vec::new(),
            tap: None,
            write_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The session this log is writing *now*, which is not always the one it
    /// opened: an in-place `/resume` moves it — see [`SessionLog::move_to`].
    ///
    /// Owned rather than borrowed for that reason, and it is the whole cost of
    /// the move being possible at all: the identity lives behind the same lock
    /// as the handle, and a borrow out of a `MutexGuard` cannot outlive it.
    pub fn id(&self) -> String {
        self.lock().id.clone()
    }

    /// The file this log is appending to now. See [`Self::id`] for why it is
    /// owned.
    pub fn path(&self) -> PathBuf {
        self.lock().path.clone()
    }

    /// The handle, with a poisoned lock recovered rather than propagated.
    ///
    /// Same ruling as [`SessionLog::append`]'s, and now in one place because
    /// four callers need it: a panic in some other thread says nothing about
    /// whether this file is still writable, and the data inside is a `File`
    /// plus two strings, which a panic elsewhere cannot leave torn.
    fn lock(&self) -> std::sync::MutexGuard<'_, Handle> {
        self.handle.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Continue a *different* session in this same process: stop writing to
    /// the file this log is on, and append to `id`'s file from here on.
    ///
    /// **The abandoned file gets the last word.** A [`MOVED`] record is written
    /// into it before the handle is swapped, so a human reading it later, or a
    /// `--resume` of it, finds a session that stops rather than a session that
    /// was truncated. The record is deliberately the last line of that file:
    /// nothing this process does afterwards belongs to that session.
    ///
    /// **The resumed file is opened in append mode**, which is exactly what
    /// `emma --resume` does to it, so an in-place resume and a process-boundary
    /// one leave the same file on disk. Nothing is rewritten and nothing is
    /// replayed; the records already in it are the conversation, and [`fold`]
    /// is what reads them back.
    ///
    /// **Nothing is renamed, and that is deliberate rather than incidental.**
    /// The obvious implementation of "move this session onto that file" is a
    /// rename, and on Windows a rename of a file this process still holds open
    /// fails with `ERROR_SHARING_VIOLATION` unless every handle was opened
    /// sharing delete — which `OpenOptions` does not do. Opening the
    /// destination and dropping the old handle needs no such favour from the
    /// filesystem and is what the operation actually means: two files, both
    /// kept, one writer moving between them.
    ///
    /// The new handle is opened **before** the old one is given up, so a
    /// destination that cannot be opened leaves the log exactly where it was
    /// rather than in a session with nowhere to write.
    ///
    /// **A log with no file at all ([`SessionLog::none`]) moves its identity
    /// and stays silent.** It is the constructor for "there is nowhere to write
    /// a transcript", and a move that opened one would make the one path in
    /// this file that promises to record nothing start recording — for the sake
    /// of a file nothing would ever read. The id still moves, so whoever asks
    /// this log what session it is gets the right answer.
    pub fn move_to(&self, dir: &Path, id: &str) -> Result<PathBuf> {
        // Bound rather than tested inline: a `MutexGuard` in an `if`
        // condition is alive for the whole `if`, and the arm below re-locks.
        let silent = {
            let handle = self.lock();
            handle.file.is_none()
        };
        if silent {
            let path = dir.join(format!("{id}.jsonl"));
            let mut handle = self.lock();
            handle.id = id.to_string();
            handle.path = path.clone();
            return Ok(path);
        }
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("{id}.jsonl"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        // Before the swap, and outside the lock, because `append` takes it.
        self.append(
            MOVED,
            json!({ "to": id, "to_path": path.display().to_string() }),
        );
        let mut handle = self.lock();
        handle.id = id.to_string();
        handle.path = path.clone();
        // The old handle is dropped here, by the assignment. On a log that
        // never had one this is the whole operation: the identity moves and
        // there is nothing to close.
        handle.file = Some(file);
        Ok(path)
    }

    /// Append one record.
    ///
    /// Failures are swallowed after the first, deliberately: a full disk must
    /// not be the thing that kills a run halfway through editing someone's
    /// source tree. The caller has already been told once by `open`.
    pub fn append(&self, kind: &str, mut payload: Value) {
        let kind = match self.prefix {
            Some(prefix) => format!("{prefix}.{kind}"),
            None => kind.to_string(),
        };
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("kind".into(), json!(kind));
            obj.insert("at_ms".into(), json!(now_ms()));
            for (key, value) in &self.stamp {
                obj.insert(key.clone(), value.clone());
            }
        }
        if let Some(tap) = &self.tap {
            if let Ok(mut seen) = tap.lock() {
                seen.push(payload.clone());
            }
        }
        // **A poisoned lock is recovered, not treated as a reason to stop
        // writing.** The lock is poisoned when some thread panicked while
        // holding it, which says nothing about whether the file is still
        // writable -- and this returned instead, silently dropping every record
        // from that moment on. Worse than the silence: `write_failed` was never
        // set on this path, so `transcript_failed` went on reporting that the
        // transcript was fine while the session stopped being recorded. The one
        // observable that exists to make this visible said the opposite.
        //
        // Recovering is what the rest of this tree does -- twenty-five
        // `unwrap_or_else(|e| e.into_inner())` sites across `term`, `agent` and
        // `input` -- and this file was the outlier. The data inside is a `File`
        // handle, which a panic elsewhere cannot leave torn.
        //
        // The recovery is `Self::lock` now rather than an `unwrap_or_else`
        // here, because `move_to` and the two accessors take the same lock and
        // one of them writing `.unwrap()` would reintroduce exactly this.
        let mut guard = self.lock();
        // `None` is the no-log constructor and is the one silence here that is
        // correct: a run with no session directory has nothing to append to.
        let Some(file) = guard.file.as_mut() else {
            return;
        };
        let mut line = payload.to_string();
        line.push('\n');
        // Not `let _ =`. This file is the only record of the run: resume folds
        // it, `emma agents` reads it, and the exit line names it as the place
        // the conversation went. A full disk or a revoked handle used to lose
        // all of that in silence, for the whole rest of the session, because
        // every append discarded its own error.
        //
        // Reported once and then never again — a failing write fails on every
        // record, and a warning per record would bury the run in its own
        // complaint. Still not fatal: losing the transcript is not a reason to
        // throw away the work the user is in the middle of.
        if let Err(e) = file.write_all(line.as_bytes()).and_then(|()| file.flush()) {
            if !self.write_failed.swap(true, Ordering::SeqCst) {
                eprintln!(
                    "emma: this session's transcript can no longer be written: {e}. The run \
                     continues, but it will not be resumable and `emma agents` will not see it."
                );
            }
        }
    }

    /// Whether this session's transcript has stopped being writable.
    ///
    /// **Returned, not only printed, which is the same correction
    /// `read_reporting` already made one screen away.** That function was
    /// deliberately changed to hand its losses back because *"a guarantee
    /// nothing can observe is not a guarantee"*; `append` kept only an
    /// `eprintln!`, so the one warning this row exists to produce went to
    /// stderr — under a full-screen alternate-screen frame, where its
    /// visibility is itself unestablished — and nothing else could see it.
    ///
    /// A reviewer found the consequence: `write_failed` appeared nowhere but
    /// this file, no test constructed a failing write, and deleting the whole
    /// error arm left the suite green.
    pub fn transcript_failed(&self) -> bool {
        self.write_failed.load(Ordering::SeqCst)
    }

    /// What to tell the operator when this session stopped being recorded, or
    /// `None` when it is being recorded fine.
    ///
    /// **The flag was right and nothing read it, which a second reviewer found
    /// after the first.** `transcript_failed` had a real unit test against a
    /// real read-only handle and zero call sites outside this file: the only
    /// signal a live run gave was one `eprintln!` at the moment of failure, and
    /// this project's own `DEF-022` records that stderr's visibility under the
    /// alternate-screen frame is unestablished. So a full disk or a revoked
    /// handle could stop the recording and the run would finish looking
    /// entirely normal — and the session file named in the exit line, which is
    /// the record this project tells people to go and read, would be missing
    /// the end of the work.
    ///
    /// Named separately from the write error itself because they answer
    /// different questions: that one says a write failed once, this one says
    /// the transcript is not what happened.
    pub fn transcript_warning(&self) -> Option<String> {
        if !self.transcript_failed() {
            return None;
        }
        Some(format!(
            "this session stopped being recorded: {} is missing part of what happened, \
             so --resume will not bring all of it back. The run itself was not affected.",
            self.path().display()
        ))
    }

    /// A second view of this same file for one delegation, and the buffer its
    /// records also land in.
    ///
    /// **Why the `kind`s are namespaced.** [`fold_records`] and
    /// [`restore_records`] match on `kind` and both have a `_ => {}` arm, so
    /// `sub.assistant` and `sub.model_call` are ignored for free — which is the
    /// whole mechanism. Written under the parent's own names the damage would
    /// not be subtle: a sub `assistant` record arriving between the parent's
    /// `tool_call` and its `tool_result` calls `close_turn`, which places the
    /// parent's held turn *without* its results, `answered()` fails, and the
    /// parent's whole turn is silently dropped from every resume of that
    /// session. `tests/delegate.rs` folds a session containing a delegation and
    /// asserts the parent's message list is unchanged, because without that test
    /// this is a convention rather than a property.
    ///
    /// The three stamped fields are what make the file greppable back apart:
    /// which delegation wrote a record, which of the parent's turns asked for
    /// it, and which agent type ran.
    pub fn subagent(&self, sub_id: &str, parent_turn_id: &str, agent: &str) -> (Self, Records) {
        let tap: Records = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                // The same handle, not a copy of it: a delegation writes into
                // the file its parent is on *now*, including after a move.
                handle: self.handle.clone(),
                write_failed: self.write_failed.clone(),
                prefix: Some("sub"),
                stamp: vec![
                    ("sub_id".into(), json!(sub_id)),
                    ("parent_turn_id".into(), json!(parent_turn_id)),
                    ("agent".into(), json!(agent)),
                ],
                tap: Some(tap.clone()),
            },
            tap,
        )
    }

    /// Every well-formed record in a session file, in order.
    ///
    /// A trailing partial line — the one thing a crash mid-write can leave — is
    /// skipped rather than failing the read, which is the property that makes
    /// this format survivable without a transaction.
    ///
    /// **A malformed line anywhere else is not that, and is not silent.** The
    /// doc above described the tail case and the code dropped *any* line that
    /// would not parse, so real corruption in the middle of a file — the case
    /// where a resumed conversation is genuinely missing turns — read as a
    /// clean success. The read still returns what it could recover, because
    /// refusing outright would make a damaged session unresumable and that is
    /// worse; but it says how many records it lost and where.
    pub fn read(path: &Path) -> Result<Vec<Value>> {
        Self::read_reporting(path).map(|(records, _lost)| records)
    }

    /// The same read, with the damage handed back rather than only printed.
    ///
    /// **A guarantee nothing can observe is not a guarantee.** The loss used to
    /// reach an `eprintln!` and nowhere else, so the test named
    /// `a_torn_tail_is_silent_and_damage_in_the_middle_is_not` asserted only
    /// that the readable records came back — which is true whether or not the
    /// damage was noticed. An adversarial reviewer removed the counting
    /// entirely and the test stayed green across the whole crate.
    ///
    /// That is the false-receipt shape this repository is most often bitten by,
    /// and the fix is not a better assertion on stderr: it is that the caller
    /// should have been told. Printing is a presentation choice; knowing is not.
    pub fn read_reporting(path: &Path) -> Result<(Vec<Value>, Vec<usize>)> {
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let lines: Vec<&str> = raw.lines().collect();
        // **A torn tail is a file that stops mid-line, and that is a fact about
        // the trailing byte rather than about the line's position.** Until
        // 2026-08-23 the last line was exempt from damage reporting whatever
        // the file looked like, which is right after a crash — the writer was
        // interrupted between the record and its newline — and wrong for a file
        // that ends cleanly, where the last line is a whole record like any
        // other. Corruption there was dropped in silence and the resume came
        // back a turn short saying nothing, which is the silent-data-loss shape
        // this file's own reporting exists to refuse. Found by mutation, in a
        // pass that was not permitted to fix it.
        //
        // `ends_with('\n')` rather than a check on the last line's contents: a
        // truncated record can be valid JSON, so nothing about the text
        // distinguishes the two cases and only the byte does.
        let torn_tail = if raw.ends_with('\n') {
            usize::MAX
        } else {
            lines.len().saturating_sub(1)
        };
        let mut out = Vec::new();
        let mut lost = Vec::new();
        for (n, line) in lines.iter().enumerate() {
            match serde_json::from_str(line) {
                Ok(v) => out.push(v),
                // A blank line is not damage, and neither is the torn tail.
                Err(_) if line.trim().is_empty() || n == torn_tail => {}
                Err(_) => lost.push(n + 1),
            }
        }
        if !lost.is_empty() {
            let shown: Vec<String> = lost.iter().take(5).map(|n| n.to_string()).collect();
            // Printed here and returned below. The print is for the human at the
            // terminal; the return is what makes the loss testable, and what
            // lets a caller decide rather than only be told.
            eprintln!(
                "emma: {} record(s) in {} could not be read and are missing from the restored \
                 conversation (line {}{}). This is damage in the middle of the file, not the \
                 partial last line a crash leaves behind.",
                lost.len(),
                path.display(),
                shown.join(", "),
                if lost.len() > shown.len() {
                    ", …"
                } else {
                    ""
                }
            );
        }
        Ok((out, lost))
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

/// Milliseconds since the Unix epoch, which is what every `at_ms` in this file
/// and in `runfacts` means.
///
/// One function rather than several copies of the same `duration_since`,
/// because two records whose timestamps come from differently-written clocks
/// cannot be ordered against each other, and ordering them is the whole point
/// of a run graph. A clock before the epoch reads as zero rather than
/// panicking: a record with a wrong timestamp is a blemish, a crash is a lost
/// session.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// [`fold`] over records already read — the testable half, and the one a caller
/// that has the records for another reason should use.
pub fn fold_records(records: &[Value]) -> Vec<Message> {
    fold_records_reporting(records).0
}

/// The fold, with what it refused to rebuild handed back.
pub fn fold_records_reporting(records: &[Value]) -> (Vec<Message>, Vec<String>) {
    let mut fold = Fold::default();
    for record in records {
        fold.record(record);
    }
    let damage = std::mem::take(&mut fold.damage);
    (fold.finish(), damage)
}

/// The state a walk over the records needs.
///
/// Two lists because the loop keeps two: `history` is the conversation up to
/// the last finished goal — the part a request sends as `Request::history` —
/// and `query` is the goal in progress. A fold that merged them would return
/// the right conversation in the wrong shape and could not apply a compaction
/// record, which counts messages in the first list only.
///
/// **Nothing here collapses a finished goal.** It used to: the loop kept two
/// lines of prose per goal and the fold kept its own copy of that rule, which
/// was two implementations of one decision and a `--resume` that reconstructed
/// a different conversation the moment either moved. A finished goal now stays
/// in the conversation in full, and the *only* thing that shortens it is
/// compaction — which writes what it replaced the messages with into the
/// record, so this reads it rather than re-deriving it.
#[derive(Default, Clone)]
struct Fold {
    /// Records this fold refused to act on because they were damaged.
    ///
    /// **The refusals were correct and unobservable.** Each printed to stderr
    /// and returned, which is the right behaviour — a damaged `compacted`
    /// record must not read as a no-op — but nothing downstream could tell it
    /// had happened, so the test guarding it asserted only that the fold did
    /// not panic. An adversarial reviewer restored the exact historical defect
    /// the test's own doc-comment describes and the test stayed green across
    /// the whole crate.
    ///
    /// Collected here so a resumed run can say the conversation it rebuilt is
    /// known not to match the one that was sent, which is the fact that
    /// matters and the one that was being thrown away.
    damage: Vec<String>,
    history: Vec<Message>,
    query: Vec<Message>,
    in_goal: bool,
    /// The ending of the goal that has just finished, from `goal_finished`.
    /// Applied when the *next* goal opens — see [`Fold::close_goal`].
    finished: Option<String>,
    /// An assistant turn that has been read but not yet placed, because what
    /// follows it decides whether it can be placed at all.
    pending: Option<Vec<ContentBlock>>,
    /// Result blocks for `pending`, accumulating until something closes them.
    results: Vec<ToolResult>,
}

impl Fold {
    fn record(&mut self, r: &Value) {
        match r["kind"].as_str().unwrap_or_default() {
            "goal" => {
                self.close_turn();
                // A goal record while another goal is open means the previous
                // one ended — closed here rather than at `goal_finished`
                // because the loop does the same: after a `goal_finished` the
                // loop still holds the finished goal's `query` and would send
                // it again unchanged if nothing else happened, so closing early
                // would make the fold disagree with the last call of a one-goal
                // session.
                self.close_goal();
                self.in_goal = true;
                self.query = vec![Message::user(string(r, "opening"))];
            }
            "assistant" => {
                self.close_turn();
                // Absent only in logs written before `raw_content` was stored.
                // The turn is dropped rather than rebuilt from `text`: a
                // rebuilt thinking block is rejected, so an approximation here
                // would be a resumed run that dies on its first call with an
                // error naming a signature nobody in the file mentions.
                self.pending = r.get("raw_content").and_then(blocks_of);
            }
            "tool_result" => {
                // Anything that is not a `tool_result` block is not pushed, and
                // the turn it was meant to answer is therefore dropped by
                // `answered` rather than half-restored. A record this cannot
                // read is a gap, and a gap is the one thing the API will not
                // take: an unanswered `tool_use` is a 400.
                if let Some(ContentBlock::ToolResult(result)) =
                    r.get("block").cloned().map(ContentBlock::from_value)
                {
                    self.results.push(result);
                }
            }
            "kick" => {
                // A kick answers a turn that made no tool calls, so there is
                // nothing to pair. If the turn itself could not be placed the
                // kick goes with it — a user message with no assistant turn
                // before it would leave two user turns in a row.
                if let Some(content) = self.pending.take() {
                    self.query.push(Message::assistant(content));
                    self.query.push(Message::user(string(r, "text")));
                }
                self.results.clear();
            }
            // Replayed rather than re-decided. The record carries both halves —
            // how many messages off the front were replaced, and exactly what
            // replaced them — so a fold cannot summarise differently from the
            // run that did it. Compaction only ever touches finished goals, so
            // the count is an index into `history` and never into `query`.
            "compacted" => {
                // **A damaged compaction record must not read as a no-op.**
                // `drop_messages` missing defaulted to 0 and `messages` missing
                // defaulted to empty, so a truncated or malformed record left
                // the conversation uncompacted — and the fold then rebuilt
                // something *different from what was sent*, which is the one
                // thing this fold exists to prevent. Silent, and visible only as
                // a resumed session that behaves unlike the one it continues.
                let drop = match r["drop_messages"].as_u64() {
                    Some(d) => d as usize,
                    None => {
                        let what = "a `compacted` record is missing `drop_messages`, so the \
                             conversation it describes cannot be rebuilt; this resumed \
                             conversation will not match the one that was sent.";
                        eprintln!("emma: {what}");
                        self.damage.push(what.to_string());
                        return;
                    }
                };
                let replacement: Vec<Message> = match serde_json::from_value(r["messages"].clone())
                {
                    Ok(m) => m,
                    Err(e) => {
                        let what = format!(
                            "a `compacted` record has unreadable replacement messages ({e}), so the \
                             conversation it describes cannot be rebuilt; this \
                             resumed conversation will not match the one that was sent."
                        );
                        eprintln!("emma: {what}");
                        self.damage.push(what);
                        return;
                    }
                };
                let drop = drop.min(self.history.len());
                self.history.splice(..drop, replacement);
            }
            // `/clear`. Everything before this point left the conversation
            // while the session was running, so a resume must not put it back.
            //
            // **Without this arm `/clear` is a lie**, and a quiet one: the
            // in-memory chapters would be empty, the next `--resume` would fold
            // the whole file, and the cleared conversation would walk back in
            // carrying its tool results. The bytes stay in the file for a human
            // to read; the fold skips them.
            //
            // Both lists go, and `in_goal` with them: `/clear` only ever happens
            // between goals, so anything still in `query` here is a goal that
            // finished and has not been moved across yet — which is exactly what
            // was cleared. `restore_records` needs no arm of its own, because it
            // already resets every counter at each `goal` record.
            "cleared" => {
                self.close_turn();
                self.history.clear();
                self.query.clear();
                self.in_goal = false;
                self.finished = None;
                self.pending = None;
                self.results.clear();
            }
            // Shedding rewrites named blocks wherever they sit, where `compacted`
            // replaces messages off the front by an index. Replayed rather than
            // re-decided, for the same reason: the record carries the replacement
            // text verbatim, so a resumed conversation is the one that was sent.
            "shed" => {
                let shed: Vec<(String, String)> = r["results_shed"]
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .map(|row| (string(row, "tool_use_id"), string(row, "content")))
                            .collect()
                    })
                    .unwrap_or_default();
                for (id, content) in shed {
                    for message in self.history.iter_mut().chain(self.query.iter_mut()) {
                        if let Content::Blocks(blocks) = &mut message.content {
                            for block in blocks.iter_mut() {
                                if let ContentBlock::ToolResult(result) = block {
                                    if result.tool_use_id == id {
                                        result.content = content.clone();
                                    }
                                }
                            }
                        }
                    }
                    for result in self.results.iter_mut() {
                        if result.tool_use_id == id {
                            result.content = content.clone();
                        }
                    }
                }
            }
            // A line typed while the goal ran, taken up at the next turn. The
            // text is the *composed* string, attribution and all, for the reason
            // the `goal` record stores `opening` rather than the user's words: a
            // fold that recomposed it would replay a conversation the model
            // never had the moment the wording changed.
            "steer" => {
                self.close_turn();
                append_user_text(&mut self.query, string(r, "text"));
            }
            "goal_finished" => self.finished = Some(string(r, "ending")),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Message> {
        self.close_turn();
        let mut out = self.history;
        out.extend(self.query);
        out
    }

    /// Place the assistant turn and its results, or drop both. See
    /// [`place_turn`], which is the rule and is shared with the loop.
    fn close_turn(&mut self) {
        let Some(content) = self.pending.take() else {
            self.results.clear();
            return;
        };
        let results = std::mem::take(&mut self.results);
        place_turn(&mut self.query, content, results);
    }

    /// Move a finished goal into the conversation, whole.
    ///
    /// The one thing added is the note on a goal that stopped mid-turn. A run
    /// killed on its token budget leaves a user turn last — the tool results
    /// nothing answered — and the next goal's opening is also a user turn, so
    /// without something between them the next request is two user turns in a
    /// row, which is a 400. The note is that something, and it is information
    /// rather than padding: without it the model reads an abandoned goal as a
    /// completed one.
    ///
    /// It is added when the *next* goal opens rather than when this one ends,
    /// because that is when the loop adds it — and the two lists have to agree
    /// at every point in the record stream, not merely at the end.
    fn close_goal(&mut self) {
        if !self.in_goal {
            return;
        }
        let mut messages = std::mem::take(&mut self.query);
        if let Some(ending) = self.finished.take() {
            if messages.last().map(|m| m.role) == Some(Role::User) {
                messages.push(Message::assistant_text(crate::agent::ended_note(&ending)));
            }
        }
        self.history.extend(messages);
        self.in_goal = false;
    }
}

/// The conversation as it stood immediately *before* each `assistant` record,
/// paired with that record's index.
///
/// The training exporter's per-turn context, and it is the same fold rather
/// than a second one: the walk is [`fold_records`] stopped early, so a
/// `compacted` record that had already been applied when a turn was sent is
/// applied here too, and one that came later is not. That is the property the
/// exporter claims, and claiming it from a re-derivation would be a second
/// implementation of one decision.
///
/// Cloning the fold at each assistant record rather than re-walking the prefix
/// keeps it linear in the records and quadratic only in the messages that are
/// copied, which for a session file is a few hundred small structs.
pub fn fold_prefixes(records: &[Value]) -> Vec<(usize, Vec<Message>)> {
    let mut fold = Fold::default();
    let mut out = Vec::new();
    for (i, record) in records.iter().enumerate() {
        if record["kind"] == "assistant" {
            // `finish` places the turn that is still pending, which is the
            // previous one, with the results that answered it. That list is
            // exactly what the request carrying turn `i` sent.
            out.push((i, fold.clone().finish()));
        }
        fold.record(record);
    }
    out
}

/// Append user text to the turn already at the end, or open a new one.
///
/// **Two user turns in a row is a 400, not a conversation**, and this is the
/// one rule that keeps steering from producing one. At the top of a loop
/// iteration the last message is always a user turn (the goal's opening, the
/// tool results of the round-trip just placed, or a kick), so almost every
/// call appends. The push arm is for the case that does not arise in the loop
/// and does in the fold: a record stream whose last placed message was an
/// assistant turn.
///
/// Shared between the loop's steering drain and the fold's `steer` arm,
/// because a fold that composed the turn differently from the run is a
/// resumed session that is not the one that was sent.
pub(crate) fn append_user_text(out: &mut Vec<Message>, text: String) {
    match out.last_mut() {
        Some(last) if last.role == Role::User => match &mut last.content {
            Content::Text(existing) => {
                existing.push_str("\n\n");
                existing.push_str(&text);
            }
            Content::Blocks(blocks) => blocks.push(ContentBlock::text(text)),
        },
        _ => out.push(Message::user(text)),
    }
}

/// Place an assistant turn and the results that answer it, or place neither.
///
/// The rule the fold and the loop both turn on: **the API rejects an assistant
/// turn carrying a `tool_use` that no `tool_result` answers**, and equally a
/// result answering nothing. A run killed between the model call and the last
/// of its tools leaves exactly that — in the file, and in the loop's own
/// in-flight list. Half a turn is not worth a message list that cannot be sent,
/// so an unmatched turn is dropped whole.
///
/// One function rather than one per caller. The loop carries tool traffic
/// across goals now, so it needs this ruling in the same place the fold does,
/// and a second copy of it would be a resumed conversation that differs from
/// the one that was sent in precisely the cases neither is tested on.
///
/// Returns whether the turn was placed.
pub(crate) fn place_turn(
    out: &mut Vec<Message>,
    content: Vec<ContentBlock>,
    results: Vec<ToolResult>,
) -> bool {
    if !answered(&content, &results) {
        return false;
    }
    out.push(Message::assistant(content));
    // A turn that called nothing is answered by nothing, and appending an empty
    // user turn to say so is a 400 of its own.
    if !results.is_empty() {
        out.push(Message::tool_results(results));
    }
    true
}

/// Whether every `tool_use` in an assistant turn has exactly one result, and
/// every result a call. Both directions, because the API refuses both ways
/// round. A turn with no calls and no results satisfies it, which is what makes
/// a plain text turn placeable.
fn answered(content: &[ContentBlock], results: &[ToolResult]) -> bool {
    let mut calls: Vec<&str> = content
        .iter()
        .filter_map(ContentBlock::tool_use_id)
        .collect();
    let mut answers: Vec<&str> = results.iter().map(|r| r.tool_use_id.as_str()).collect();
    calls.sort_unstable();
    answers.sort_unstable();
    calls == answers
}

/// A stored `raw_content` array back into blocks.
///
/// `None` rather than an empty turn for anything that is not an array, because
/// an assistant record whose content cannot be read is a turn that must be
/// dropped whole — the same ruling `place_turn` makes about a half-answered one,
/// and for the same reason.
fn blocks_of(v: &Value) -> Option<Vec<ContentBlock>> {
    Some(
        v.as_array()?
            .iter()
            .cloned()
            .map(ContentBlock::from_value)
            .collect(),
    )
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
///
/// `Default` so a test can build the damage cases without a file on disk. Every
/// field's empty value is the honest one for "nothing was restored".
#[derive(Debug, Default)]
pub struct Restored {
    /// The session id, which is the file stem — a resumed run appends to this
    /// same file rather than starting a new one, so the transcript of a goal
    /// stays in one place.
    pub id: String,
    pub path: PathBuf,
    pub resumed: Resumed,
    pub continuity: Continuity,
    /// Line numbers of records that could not be read, so the conversation that
    /// comes back is missing turns.
    ///
    /// **Carried on the value rather than only printed**, because a resume that
    /// silently dropped turns is the failure this whole path exists to make
    /// loud, and a warning on stderr is not something a caller — or a test — can
    /// act on. Empty is the ordinary case, including a torn last line, which is
    /// what a crash leaves and is not damage.
    pub lost_records: Vec<usize>,
}

/// What to tell somebody whose `--resume` passed over files it could not read,
/// or `None` when it read everything it looked at.
///
/// Separate from [`resume_damage_note`] because the two answer different
/// questions: that one says the conversation that came back is missing turns,
/// this one says a *different session* may have been chosen. Bare `--resume`
/// means "the one I was last running here", and an unreadable newer file makes
/// that quietly untrue.
pub fn skipped_sessions_note(unreadable: &[String]) -> Option<String> {
    if unreadable.is_empty() {
        return None;
    }
    Some(format!(
        "{} newer session file(s) could not be read and were passed over, so this may not be the session you meant: {}. Name one with `emma --resume <id>`.",
        unreadable.len(),
        unreadable.join(", ")
    ))
}

/// What to tell somebody resuming a session that came back damaged, or `None`.
///
/// **Both halves of this were carried on the value so a caller could act on
/// them, and no caller did.** `Restored::lost_records` and `Resumed::damage`
/// each carry a doc saying that printing is a presentation choice and knowing
/// is not — and an independent reviewer proved neither had a production reader
/// by deleting the field and building the shipping binary clean, twice. The
/// only report of fold damage was an `eprintln!`, on the stderr channel that
/// `DEF-022` itself calls unobservable under the alternate-screen frame.
///
/// So the sentence "the caller should have been told" was true of the library
/// and false of the product: the caller was told, and the caller threw it away.
///
/// Returned rather than printed, for the reason everything else in this file is
/// returned: `main` owns the surface, and a function that printed could not be
/// tested by anything short of driving the binary.
///
/// `None` is the ordinary case, including the torn last line a crash leaves,
/// which is not damage.
pub fn resume_damage_note(restored: &Restored) -> Option<String> {
    let lost = &restored.lost_records;
    let damage = &restored.resumed.damage;
    if lost.is_empty() && damage.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !lost.is_empty() {
        // The line numbers, capped: a session damaged in fifty places is a
        // session whose first few are enough to go and look at.
        let shown: Vec<String> = lost.iter().take(5).map(|n| n.to_string()).collect();
        let more = if lost.len() > shown.len() {
            format!(" and {} more", lost.len() - shown.len())
        } else {
            String::new()
        };
        parts.push(format!(
            "{} record(s) could not be read (line {}{})",
            lost.len(),
            shown.join(", "),
            more
        ));
    }
    if !damage.is_empty() {
        parts.push(format!(
            "{} turn(s) the fold refused to rebuild",
            damage.len()
        ));
    }
    Some(format!(
        "this session came back damaged: {}. The conversation restored is known not to \
         match the one that was sent, so the model is resuming with less than it had.",
        parts.join(", ")
    ))
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
        cwd: &str,
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
        // `cwd` was recorded from the beginning and never compared, so the one
        // hazard `locate` warns about in its own doc — "a conversation about the
        // wrong repository with write tools attached" — was the only drift that
        // arrived silently, while three cheaper ones all warned. Resuming a
        // session by id from another project is exactly how that happens.
        // **Compared the way the rest of this module compares a directory,
        // which is not how it was compared until now.** `check` above is raw
        // string inequality, which is right for a hash and for a model id and
        // wrong for a path: `C:\\x` and `C:\\x\\` and `c:\\x` are one
        // directory spelled three ways, and any of them raised a warning that
        // the conversation was about somewhere else. `same_dir` canonicalises
        // both sides and is what `locate` already uses to decide which session
        // belongs to this directory -- so the resume warning and the resume
        // *choice* disagreed about what "the same directory" means.
        //
        // A false warning here is not free. Its own text says the write tools
        // are pointed somewhere else, which is alarming and, in this case,
        // untrue; and a warning that cries wolf on a trailing separator is one
        // nobody reads on the day it is right.
        if !self.cwd.is_empty() && !same_dir(&self.cwd, Path::new(cwd)) {
            out.push(format!(
                "working directory changed since this session ran: {} → {cwd}. \
                 This conversation is about a different directory than the one you are in, \
                 and the tools that write files are pointed at this one.",
                self.cwd
            ));
        }
        out
    }
}

/// Read one session file into everything a resumed run needs.
pub fn restore(path: &Path) -> Result<Restored> {
    let (records, lost) = SessionLog::read_reporting(path)?;
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
        lost_records: lost,
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
    let (messages, damage) = fold_records_reporting(records);
    let mut r = Resumed {
        damage,
        messages,
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
                // A goal opens in flight and stays that way until its
                // `goal_finished` closes it. The last one to win is the last
                // one in the file, which is what a resume is continuing.
                r.in_flight = true;
                r.tokens = 0;
                r.iterations = 0;
                r.kicks = 0;
                r.failed_now.clear();
                r.failed_ever.clear();
                args.clear();
            }
            "model_call" => {
                r.iterations += 1;
                // `cost_tokens`, not `billable_total_tokens`: the budget counts
                // a cached read at what it costs rather than at its size, and a
                // resume that summed the raw field would restore a meter in
                // different units from the one the loop enforces — the same
                // record, read two ways. The raw fields are still in the record
                // beside it, and a log written before the weighting existed
                // falls back to them rather than restoring zero.
                r.tokens += record["cost_tokens"]
                    .as_i64()
                    .unwrap_or_else(|| record["billable_total_tokens"].as_i64().unwrap_or(0));
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
            // A delegation's own `sub.model_call` records are namespaced and
            // therefore invisible to the arm above, so a resumed parent would
            // restore a meter missing everything its delegations spent — a way to
            // spend past a cap, which is the exact failure `Resumed`'s doc is
            // written about. This record is the parent-level fact that closes it,
            // and it deliberately does not touch `iterations`: the parent made
            // one model call for that turn, not fifteen.
            "delegation" => r.tokens += record["cost_tokens"].as_i64().unwrap_or(0),
            "goal_finished" => {
                // **Only a goal that reached an answer is finished.** A
                // `goal_finished` record is written for every ending, including
                // the ones that cut a goal off — a budget, a nudge count, an
                // interrupt — and those are exactly what a resume exists to
                // continue. Reading this record as "not in flight" regardless
                // of its ending was the first attempt, and it turned
                // `a_resumed_run_inherits_the_nudges_the_first_one_used` red:
                // a run resumed after `KicksExhausted` stopped being treated as
                // work in progress. The suite caught it, which is the point of
                // that test. See `Resumed::in_flight`.
                if matches!(string(record, "ending").as_str(), "done" | "answered") {
                    r.in_flight = false;
                }
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
    locate_reporting(dir, id, cwd).map(|(path, _)| path)
}

/// [`locate`], with the files it could not read handed back.
///
/// **A file that will not read is passed over, and until 2026-08-23 nobody was
/// told.** The walk goes newest-first and stops at the first session belonging
/// to this directory, so an unreadable newest file means bare `--resume` --
/// which means "the one I was last running here" -- silently continues an
/// *older* conversation instead. The user gets a resume, which is what they
/// asked for, about the wrong work.
///
/// Skipping is still right: refusing to resume anything because one file in a
/// shared directory is corrupt would be worse, and the file may well belong to
/// another project. Being unable to say so was the defect. Found by an
/// independent reviewer, who also observed that changing the `continue` below
/// to a `break` left every test green, the fixtures all being clean UTF-8.
///
/// A file belonging to a different directory is **not** reported: that is the
/// rule working, not damage, and a note naming every other project's sessions
/// would be noise on every resume.
pub fn locate_reporting(
    dir: &Path,
    id: Option<&str>,
    cwd: &Path,
) -> Result<(PathBuf, Vec<String>)> {
    if let Some(id) = id {
        // Accept what `emma` prints at startup, which is a path ending in
        // `.jsonl`, as well as the bare id.
        let id = id.strip_suffix(".jsonl").unwrap_or(id);
        let path = dir.join(format!("{id}.jsonl"));
        if path.is_file() {
            return Ok((path, Vec::new()));
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
    let mut unreadable: Vec<String> = Vec::new();
    for path in candidates.iter().rev() {
        let Ok(records) = SessionLog::read(path) else {
            unreadable.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            continue;
        };
        if records
            .iter()
            .any(|r| r["kind"] == "goal" && same_dir(&string(r, "cwd"), cwd))
        {
            return Ok((path.clone(), unreadable));
        }
    }
    bail!(
        "no session recorded in {} was run from {}. Name one with `emma --resume <id>`.",
        dir.display(),
        cwd.display()
    );
}

/// Whether this is somebody's first run, and how that was decided.
///
/// **Decided from the session history rather than from a marker file.** A
/// marker is a second source of truth that can be deleted, copied between
/// machines, or written by a run that then crashed — and the question "has
/// anyone used Emma here?" already has an answer on disk, in the transcripts.
///
/// Two reasons, kept apart because they mean different things to the person
/// reading the welcome: there is no session directory at all, or there is one
/// and nothing in it was run from this working directory. The second is the
/// common one — a new project on a machine that has used Emma before — and it
/// is deliberately treated as a first run, because what the welcome lists is
/// *this project's* harness, commands and skills.
///
/// **The scan is bounded.** Sessions from every project share one directory and
/// this runs at startup on every interactive run, so only the newest
/// [`FIRST_RUN_SCAN`] files are read. That makes the answer "no *recent*
/// session from here", which is what [`FirstRun::reason`] says — an honest
/// narrower claim rather than a broad one that would cost a full directory read
/// before the first prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRun {
    /// Emma has never had anywhere to write a transcript.
    NoSessionDirectory,
    /// There are sessions, and none of the recent ones ran here.
    NoneFromHere,
}

/// How many session files back the first-run check looks. Fifty is far more
/// than a person switches between in a day and is one directory read plus fifty
/// small file reads in the worst case.
const FIRST_RUN_SCAN: usize = 50;

impl FirstRun {
    /// Said out loud in the welcome, so a returning user can tell whether
    /// something is wrong rather than wondering why they are being introduced
    /// to a tool they already use.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoSessionDirectory => {
                "there is no session directory yet, so nothing has been recorded anywhere"
            }
            Self::NoneFromHere => "no recent session was recorded from this directory",
        }
    }
}

pub fn first_run(dir: Option<&Path>, cwd: &Path) -> Option<FirstRun> {
    let Some(dir) = dir.filter(|d| d.is_dir()) else {
        return Some(FirstRun::NoSessionDirectory);
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Some(FirstRun::NoSessionDirectory);
    };
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    if candidates.is_empty() {
        return Some(FirstRun::NoSessionDirectory);
    }
    // Ids sort by time, so the newest are at the end — the same ordering
    // `locate` relies on, and for the same reason: an mtime moves when a file
    // is copied and an id does not.
    candidates.sort();
    for path in candidates.iter().rev().take(FIRST_RUN_SCAN) {
        let Ok(records) = SessionLog::read(path) else {
            continue;
        };
        if records
            .iter()
            .any(|r| r["kind"] == "goal" && same_dir(&string(r, "cwd"), cwd))
        {
            return None;
        }
    }
    Some(FirstRun::NoneFromHere)
}

/// Whether a recorded working directory is this one. Canonicalised on both
/// sides rather than compared as bytes, for the reason `harness::discover_in`
/// gives: case, a trailing separator, a short name and a symlinked path are all
/// ways two spellings name one directory. An empty recording — a session from
/// before the field existed — matches nothing rather than everything.
/// Whether a record's `cwd` names this directory.
///
/// Public so the Memory page can ask the same question `locate` asks, rather
/// than comparing path strings itself — canonicalisation is the whole of the
/// answer here, and a second comparison that skipped it would disagree about
/// the same session on a machine with a symlinked home or an 8.3 short name.
pub fn recorded_in(r: &Value, cwd: &Path) -> bool {
    same_dir(&string(r, "cwd"), cwd)
}

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

        let records = SessionLog::read(&log.path()).unwrap();
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

    /// Damage in the final record of a file that ended cleanly is damage.
    ///
    /// **The exemption above it used to fire on position rather than on the
    /// trailing byte**, so the last line of any file was excused. That is right
    /// for a crash, which stops between the record and its newline. It is wrong
    /// for a session that closed properly, where the last line is a whole record
    /// like every other -- and there the loss was silent, the resume came back a
    /// turn short, and nothing said so.
    ///
    /// The two files below differ by one byte. That is the entire distinction,
    /// and it is why the check cannot be made on the line's contents: a
    /// truncated record can be valid JSON, and this one deliberately is not, so
    /// that both cases reach the error arm and only the byte separates them.
    #[test]
    fn corruption_in_the_last_record_of_a_closed_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let good = "{\"kind\":\"goal\",\"text\":\"a\"}";
        let bad = "{\"kind\":\"assis";

        // Ends with a newline: the writer finished. The damaged record is
        // damage.
        let closed = dir.path().join("closed.jsonl");
        fs::write(&closed, format!("{good}\n{bad}\n")).unwrap();
        let (records, lost) = SessionLog::read_reporting(&closed).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            lost,
            vec![2],
            "corruption in the last record of a cleanly closed file was dropped in silence"
        );

        // The control, and the reason the exemption exists at all: the same
        // bytes without the final newline are a torn tail, and a warning on
        // every crash-resume is one nobody reads by the third time.
        let torn = dir.path().join("torn.jsonl");
        fs::write(&torn, format!("{good}\n{bad}")).unwrap();
        let (records, lost) = SessionLog::read_reporting(&torn).unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            lost.is_empty(),
            "a torn tail was reported as damage: {lost:?}"
        );
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
    // Moving a running process onto another session's file
    //
    // What an in-place `/resume` does to the log. Two files and one handle:
    // the one being left has to stop cleanly and say where the session went,
    // and the one being picked up has to be appended to rather than rewritten.
    // -----------------------------------------------------------------------

    #[test]
    fn moving_ends_one_file_and_appends_to_the_other() {
        let dir = tempfile::tempdir().unwrap();
        // The session being picked up already has a conversation in it, written
        // by an earlier run. Nothing here may touch those bytes.
        let old = SessionLog::open(dir.path(), "sess-old").unwrap();
        old.append("goal", json!({ "text": "the earlier work" }));
        drop(old);

        let log = SessionLog::open(dir.path(), "sess-here").unwrap();
        log.append("goal", json!({ "text": "what I was doing" }));
        let moved = log.move_to(dir.path(), "sess-old").unwrap();

        // The identity moved with the handle, which is the point: every record
        // written from here on, and every reader asking this log what session
        // it is, gets the resumed one.
        assert_eq!(log.id(), "sess-old");
        assert_eq!(log.path(), moved);

        log.append("goal", json!({ "text": "what I am doing now" }));

        let left = SessionLog::read(&dir.path().join("sess-here.jsonl")).unwrap();
        assert_eq!(left.len(), 2, "{left:?}");
        assert_eq!(left[0]["text"], "what I was doing");
        // The last word, and it is the last line: nothing this process does
        // after the move belongs to the session it left.
        assert_eq!(left[1]["kind"], MOVED);
        assert_eq!(left[1]["to"], "sess-old");

        let picked = SessionLog::read(&moved).unwrap();
        assert_eq!(picked.len(), 2, "the earlier conversation was not kept");
        assert_eq!(picked[0]["text"], "the earlier work");
        assert_eq!(picked[1]["text"], "what I am doing now");
    }

    /// The move is not a rename, and this is the assertion that says so.
    ///
    /// **Windows is why.** A rename of a file this process still holds open
    /// fails with a sharing violation unless every handle on it was opened
    /// sharing delete, which `OpenOptions` does not do — so the obvious
    /// implementation of "move the session onto that file" is one that works on
    /// the developer's Mac and fails on the owner's box. Both files exist
    /// afterwards, both keep their own history, and the only thing that moved
    /// is which of them this process is writing to.
    #[test]
    fn both_files_are_still_there_afterwards_and_neither_was_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-here").unwrap();
        log.append("goal", json!({ "text": "here" }));
        let moved = log.move_to(dir.path(), "sess-there").unwrap();
        log.append("goal", json!({ "text": "there" }));

        let left = dir.path().join("sess-here.jsonl");
        assert!(left.is_file(), "the abandoned file was renamed away");
        assert!(moved.is_file());
        // And a third session can still be opened on the abandoned file, which
        // a rename or a still-exclusive handle would refuse.
        let again = SessionLog::open(dir.path(), "sess-here").unwrap();
        again.append("goal", json!({ "text": "picked up again" }));
        let records = SessionLog::read(&left).unwrap();
        assert_eq!(records.len(), 3, "{records:?}");
        assert_eq!(records[2]["text"], "picked up again");
    }

    /// A move onto a directory that cannot be opened leaves the log where it
    /// was, rather than in a session with nowhere to write.
    ///
    /// The destination handle is taken before the old one is given up for this
    /// reason alone: the failure mode being refused is a `/resume` that reports
    /// an error *and* silently stops recording the session the user is still
    /// sitting in.
    #[test]
    fn a_move_that_cannot_open_its_destination_keeps_the_session_it_had() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-here").unwrap();
        // A file where the destination directory would be: `create_dir_all`
        // fails on it on every platform.
        let blocked = dir.path().join("not-a-dir");
        fs::write(&blocked, "").unwrap();

        assert!(log.move_to(&blocked, "sess-there").is_err());
        assert_eq!(log.id(), "sess-here");
        log.append("goal", json!({ "text": "still recording" }));
        let records = SessionLog::read(&log.path()).unwrap();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0]["text"], "still recording");
    }

    /// The no-log constructor moves its identity and stays silent.
    ///
    /// `none()` is "there is nowhere to write a transcript". A move that opened
    /// a file for it would make the one path that promises to record nothing
    /// start recording, for a file nothing would ever read.
    #[test]
    fn moving_a_log_that_records_nothing_moves_only_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::none();
        let path = log.move_to(dir.path(), "sess-elsewhere").unwrap();
        assert_eq!(log.id(), "sess-elsewhere");
        assert_eq!(log.path(), path);
        log.append("goal", json!({ "text": "x" }));
        assert!(!path.exists(), "a silent log started writing a file");
    }

    #[test]
    fn a_moved_record_costs_the_fold_nothing() {
        // The arm that is deliberately absent. Everything before the move is
        // still that session's conversation, so a `--resume` of the file that
        // was left has to get all of it back.
        let msgs = fold_records(&[
            opened(),
            assistant_with(&["tu_1"]),
            result_for("tu_1"),
            json!({ "kind": MOVED, "to": "sess-elsewhere" }),
        ]);
        assert_eq!(msgs.len(), 3, "{msgs:?}");
        assert_eq!(msgs[0], Message::user("work on g"));
    }

    #[test]
    fn a_resumed_record_does_not_double_the_conversation() {
        // The other half: the record lands in the file whose messages it
        // counts, above them in the file it describes. An arm that replayed it
        // would put the conversation in twice.
        let with = fold_records(&[
            opened(),
            assistant_with(&["tu_1"]),
            result_for("tu_1"),
            json!({ "kind": RESUMED, "messages": 3 }),
        ]);
        let without = fold_records(&[opened(), assistant_with(&["tu_1"]), result_for("tu_1")]);
        assert_eq!(with, without);
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

    /// A transcript that cannot be written says so, once, and the session
    /// continues.
    ///
    /// **Nothing tested this until a reviewer pointed out that nothing did.**
    /// `write_failed` appeared in one file and no test constructed a failing
    /// write, so deleting the whole error arm left the suite green — the
    /// "guarantee nothing can observe" shape this file corrected for
    /// `read_reporting` one screen away and not for `append`.
    ///
    /// The failure is produced rather than mocked: the handle is opened
    /// read-only, so `write_all` really fails the way a revoked handle or a full
    /// disk fails. Constructing the struct directly is the only way in — `open`
    /// hands back a writable handle by construction, which is exactly why this
    /// path had never been reached.
    #[test]
    fn a_transcript_that_cannot_be_written_is_reported_once_and_the_run_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "").unwrap();
        // Read-only: every write through this handle fails.
        let handle = OpenOptions::new().read(true).open(&path).unwrap();

        let log = SessionLog {
            handle: Arc::new(Mutex::new(Handle {
                id: "s".into(),
                path: path.clone(),
                file: Some(handle),
            })),
            prefix: None,
            stamp: Vec::new(),
            tap: None,
            write_failed: Arc::new(AtomicBool::new(false)),
        };

        assert!(
            !log.transcript_failed(),
            "a fresh log reported itself dead before anything was written"
        );

        log.append("goal", json!({ "text": "one" }));
        assert!(
            log.transcript_failed(),
            "the write failed and nothing recorded it — which is the silence this row exists to end"
        );

        // The second append must not re-report. A failing write fails on every
        // record, and a warning per record buries the run in its own complaint.
        // The flag having latched is what makes that true, and it is the only
        // part observable from here.
        log.append("goal", json!({ "text": "two" }));
        assert!(
            log.transcript_failed(),
            "the flag was cleared by a later write"
        );

        // And the run goes on: losing the transcript is not a reason to throw
        // away the work the user is in the middle of. `append` returns `()` and
        // must not panic.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "",
            "a read-only handle somehow wrote, so this test proves nothing"
        );

        // And the operator is told. The flag above was correct from the day it
        // was written and had no call site outside this file, so a run whose
        // recording stopped finished looking entirely normal.
        let warning = log
            .transcript_warning()
            .expect("the transcript stopped and there was nothing to say about it");
        assert!(
            warning.contains(&path.display().to_string()),
            "the warning does not name the file that is now incomplete: {warning}"
        );
        assert!(
            warning.contains("--resume"),
            "the warning does not say what is lost: {warning}"
        );
        assert!(
            warning.contains("run itself was not affected"),
            "the warning reads as though the work failed: {warning}"
        );
    }

    /// An ordinary session says nothing about its transcript.
    ///
    /// The control. A warning that fires on every run is one nobody reads, and
    /// this one has to be believed the once it matters.
    #[test]
    fn a_session_that_is_being_recorded_says_nothing_about_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-fine").unwrap();
        log.append("goal", json!({ "text": "one" }));
        assert!(
            log.transcript_warning().is_none(),
            "an ordinary run warned that its transcript was incomplete"
        );
    }

    /// A panic elsewhere must not stop the session being recorded.
    ///
    /// **The lock was the second silent drop in this function and the worse
    /// one.** `append` reports a failed *write*, once, and `transcript_failed`
    /// makes that observable. A poisoned lock reached neither: it returned
    /// early, so every record from that moment was discarded, and
    /// `transcript_failed` went on saying the transcript was fine. The one
    /// observable built to make this visible reported the opposite of what had
    /// happened.
    ///
    /// A lock is poisoned when a thread panics while holding it, which says
    /// nothing about whether the file is writable — and the data behind it is a
    /// `File` handle, which cannot be left half-updated by somebody else's
    /// panic. Recovering is what the rest of this tree does at twenty-five
    /// other sites.
    ///
    /// The poisoning is real rather than simulated: a thread takes the lock and
    /// panics while holding it. Its panic message is suppressed, because a test
    /// that prints a backtrace on the happy path teaches the next reader to
    /// ignore backtraces.
    /// A damaged resume says so, and an undamaged one says nothing.
    ///
    /// **The two fields this reads had no production reader at all.** Their
    /// docs each said a caller should be able to act on the damage rather than
    /// only see it printed, and an independent reviewer proved neither was read
    /// by deleting the field and building the shipping binary clean -- twice.
    /// The only report was an `eprintln!` on the channel `DEF-022` itself calls
    /// unobservable under the frame.
    ///
    /// The quiet case is the load-bearing half, for the reason it always is
    /// here: a warning on every resume is one nobody reads by the third time,
    /// and a torn last line -- what a crash leaves -- is not damage.
    #[test]
    fn a_damaged_resume_says_so_and_a_clean_one_says_nothing() {
        let clean = Restored::default();
        assert!(
            resume_damage_note(&clean).is_none(),
            "an undamaged resume warned anyway: {:?}",
            resume_damage_note(&clean)
        );

        let torn = Restored {
            lost_records: vec![12, 40],
            ..Default::default()
        };
        let note = resume_damage_note(&torn).expect("unreadable records were not reported");
        assert!(note.contains("2 record"), "{note}");
        assert!(
            note.contains("12"),
            "the line numbers are what somebody goes and looks at: {note}"
        );

        let refused = Restored {
            resumed: Resumed {
                damage: vec!["a turn with no result".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let note = resume_damage_note(&refused).expect("refused turns were not reported");
        assert!(note.contains("1 turn"), "{note}");

        // Both at once, and the sentence still names both rather than the first.
        let both = Restored {
            lost_records: vec![7],
            resumed: Resumed {
                damage: vec!["a".into(), "b".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let note = resume_damage_note(&both).expect("nothing reported");
        assert!(note.contains("record") && note.contains("turn"), "{note}");

        // And it says what the damage MEANS, not only how much there was. A
        // count on its own does not tell a reader whether to resume.
        assert!(
            note.contains("known not to match"),
            "the note counts the damage and never says what it costs: {note}"
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_silently_stop_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let handle = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();

        let log = SessionLog {
            handle: Arc::new(Mutex::new(Handle {
                id: "s".into(),
                path: path.clone(),
                file: Some(handle),
            })),
            prefix: None,
            stamp: Vec::new(),
            tap: None,
            write_failed: Arc::new(AtomicBool::new(false)),
        };

        log.append("goal", json!({ "text": "before" }));

        // Poison it for real.
        let file = log.handle.clone();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = std::thread::spawn(move || {
            let _held = file.lock().unwrap();
            panic!("a thread died holding the transcript lock");
        })
        .join();
        std::panic::set_hook(hook);
        assert!(panicked.is_err(), "the fixture thread did not panic");
        assert!(
            log.handle.lock().is_err(),
            "the lock is not poisoned, so this test would pass without testing anything"
        );

        log.append("goal", json!({ "text": "after" }));

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("before"),
            "the first record never landed: {written:?}"
        );
        assert!(
            written.contains("after"),
            "a poisoned lock silently swallowed every record after it, and \
             transcript_failed still reported the transcript was fine: {written:?}"
        );
        // The write itself succeeded, so the failure flag must NOT be set —
        // reporting a write failure that did not happen would be its own lie.
        assert!(
            !log.transcript_failed(),
            "recovering a poisoned lock was reported as a transcript failure"
        );
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
        assert_eq!(msgs[2].content.blocks().len(), 2);
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

    // -----------------------------------------------------------------------
    // The first run
    //
    // Decided from the transcripts rather than from a marker file, so what is
    // asserted is that the two reasons are told apart and that a directory with
    // history in it is not mistaken for a fresh one.
    // -----------------------------------------------------------------------

    /// A session file that records one goal run from `cwd`.
    fn session_from(dir: &Path, id: &str, cwd: &Path) {
        let log = SessionLog::open(dir, id).unwrap();
        log.append(
            "goal",
            json!({ "text": "g", "cwd": cwd.display().to_string() }),
        );
    }

    #[test]
    fn a_machine_that_has_never_run_emma_is_a_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        // No directory at all, and a directory with nothing in it, are the same
        // answer: there is no history anywhere.
        assert_eq!(
            first_run(Some(&cwd.join("nope")), cwd),
            Some(FirstRun::NoSessionDirectory)
        );
        assert_eq!(first_run(None, cwd), Some(FirstRun::NoSessionDirectory));
        let empty = dir.path().join("sessions");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            first_run(Some(&empty), cwd),
            Some(FirstRun::NoSessionDirectory)
        );
    }

    #[test]
    fn a_new_project_on_a_machine_with_history_is_still_a_first_run() {
        // The common case, and the one that decides whether the welcome is
        // worth having: sessions from every project share one directory, so
        // "has Emma run anywhere" is the wrong question. What the welcome
        // lists — the harness, this project's commands and skills — is a fact
        // about *here*.
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        session_from(&sessions, "sess-1", elsewhere.path());
        assert_eq!(
            first_run(Some(&sessions), here.path()),
            Some(FirstRun::NoneFromHere)
        );
        // …and once something has run here, it is not shown again.
        session_from(&sessions, "sess-2", here.path());
        assert_eq!(first_run(Some(&sessions), here.path()), None);
    }

    #[test]
    fn each_reason_says_which_one_it_was() {
        // The welcome prints this. A returning user seeing an introduction
        // needs to be able to tell whether something is wrong, and "we found
        // no session directory" and "nothing recent ran here" point at
        // different things.
        assert!(FirstRun::NoSessionDirectory
            .reason()
            .contains("no session directory"));
        assert!(FirstRun::NoneFromHere.reason().contains("this directory"));
    }

    #[test]
    fn the_scan_is_bounded_so_startup_does_not_read_a_whole_history() {
        // Only the newest `FIRST_RUN_SCAN` files are read, so a session that
        // ran here long enough ago falls off the end and the welcome is shown
        // again. That is the trade the doc states rather than hides: a full
        // directory read before every prompt is a worse bug than one extra
        // welcome.
        let home = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        session_from(&sessions, "sess-0000", here.path());
        for i in 1..=(FIRST_RUN_SCAN + 1) {
            session_from(&sessions, &format!("sess-{i:04}"), elsewhere.path());
        }
        assert_eq!(
            first_run(Some(&sessions), here.path()),
            Some(FirstRun::NoneFromHere)
        );
    }

    // -----------------------------------------------------------------------
    // What is written can be read back, and nothing else is written
    //
    // The file is the only record of the run, so two questions are worth
    // asking of `append`, and nothing asked either until now: can a value
    // survive the trip, and does a record carry anything the caller did not
    // hand it. Both were true and neither was defended — an adversarial pass
    // added a whole `std::env::vars()` dump to every record and the suite
    // stayed green.
    // -----------------------------------------------------------------------

    /// A record carries what was written, plus `kind` and `at_ms`, and nothing
    /// else.
    ///
    /// **The leak this refuses is not hypothetical in shape.** `append` takes a
    /// `Value` and merges into it, so any future line reaching for ambient
    /// context — the environment, the argv, a header map — lands in a file that
    /// `emma agents` reads, that `--resume` folds, and that a user is told to go
    /// and `grep`. `ANTHROPIC_API_KEY` and every other credential the process
    /// was started with are one `env::vars()` away. An exact key set is the only
    /// assertion that notices, because a "does not contain a secret" check
    /// passes on any machine that has no secret set.
    #[test]
    fn a_record_carries_what_was_written_and_nothing_ambient() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-keys").unwrap();
        log.append("goal", json!({ "text": "g", "cwd": "/work" }));

        let records = SessionLog::read(&log.path()).unwrap();
        let mut keys: Vec<&str> = records[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["at_ms", "cwd", "kind", "text"],
            "a record grew a field the caller never wrote. Whatever it holds is \
             now in a file the user is told to grep and `emma agents` reads: {:?}",
            records[0]
        );

        // A delegation's view adds exactly the three fields that make the file
        // greppable back apart, and still nothing else.
        let (sub, _tap) = log.subagent("sub-1", "turn-1", "Explore");
        sub.append("goal", json!({ "text": "g" }));
        let records = SessionLog::read(&log.path()).unwrap();
        let mut keys: Vec<&str> = records[1]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["agent", "at_ms", "kind", "parent_turn_id", "sub_id", "text"],
            "a delegation's record grew a field nobody wrote: {:?}",
            records[1]
        );
    }

    /// Everything a value can contain survives the write and comes back
    /// unchanged — including the bytes that would end the line early.
    ///
    /// **The format is one JSON object per line and the reader splits on
    /// newlines**, so the failure mode is not "the value comes back wrong". It
    /// is "the value ends the record and the rest of it becomes a *different*
    /// record": a tool result carrying a newline and a plausible `{"kind":…}`
    /// forges a turn into somebody's transcript, and, far more ordinarily, a
    /// bare CR from a CRLF file loses a byte per line on the way back.
    ///
    /// `serde_json` escapes all of this correctly, which is the reason to write
    /// the test rather than a reason not to: nothing asserted it, so the day
    /// `payload.to_string()` becomes a cheaper formatter the suite has no
    /// opinion. Mutating the write to emit the payload with newlines unescaped
    /// turns this red and nothing else in the crate.
    #[test]
    fn every_byte_a_value_can_hold_survives_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-hostile").unwrap();

        // A megabyte on one line: what a `Read` of a large file writes.
        let long = "x".repeat(1024 * 1024);
        // The bytes that end a line and forge a record.
        let forgery = "ok\n{\"kind\":\"goal\",\"text\":\"forged\"}\n";
        // CR alone is what a CRLF file puts in a tool result, and `str::lines`
        // strips a trailing one — so a value that survives as text can still
        // come back a byte short per line.
        let crlf = "line one\r\nline two\r";
        // A NUL, a tab, a backslash, a quote, an escape byte, a non-BMP scalar,
        // and the *text* of a lone-surrogate escape — which is not a scalar
        // value and can therefore only ever arrive as those six characters.
        let odd = "nul:\u{0}\ttab \\ \"quote\" \u{1b}[31m emoji:\u{1F600} surrogate-text:\\ud800";

        log.append(
            "tool_result",
            json!({ "long": long, "forgery": forgery, "crlf": crlf, "odd": odd }),
        );

        let records = SessionLog::read(&log.path()).unwrap();
        assert_eq!(
            records.len(),
            1,
            "a value containing a newline became more than one record, which is \
             how a tool result forges a turn into a transcript"
        );
        assert_eq!(records[0]["long"].as_str().unwrap().len(), long.len());
        assert_eq!(records[0]["forgery"], json!(forgery));
        assert_eq!(
            records[0]["crlf"],
            json!(crlf),
            "a CR inside a value did not come back. `str::lines` strips a \
             trailing CR, so an unescaped one silently shortens every file Emma \
             reads on Windows"
        );
        assert_eq!(records[0]["odd"], json!(odd));
        // And the forged record really is not there — the count above notices
        // an extra record, this names what would have been in it.
        assert!(
            records.iter().all(|r| r["text"] != json!("forged")),
            "a value forged a record: {records:?}"
        );
    }

    // -----------------------------------------------------------------------
    // What a resumed run inherits
    //
    // `tests/resume.rs` drives the four budget claims through the real loop.
    // These are the arithmetic underneath them, on record shapes the loop
    // cannot be asked to produce on demand: several goals in one file, a log
    // written before a field existed, and a goal whose own totals disagree
    // with the running ones.
    // -----------------------------------------------------------------------

    #[test]
    fn every_counter_starts_again_at_each_goal() {
        // Budgets are per goal. A session with a finished goal in it must not
        // hand the next one the first one's spend — a resume that did would
        // report a fresh goal as already over budget and refuse to work.
        //
        // Deleting the four resets left every test in the crate green.
        let records = vec![
            opened(),
            json!({ "kind": "model_call", "cost_tokens": 100 }),
            json!({ "kind": "kick", "n": 2, "text": "keep going" }),
            json!({ "kind": "tool_call", "id": "t1", "tool": "Bash", "args": { "cmd": "a" } }),
            json!({ "kind": "tool_result", "id": "t1",
                    "block": { "type": "tool_result", "tool_use_id": "t1", "is_error": true } }),
            json!({ "kind": "goal_finished", "ending": "done",
                    "tokens": 100, "iterations": 1, "kicks": 2 }),
            opened(),
            json!({ "kind": "model_call", "cost_tokens": 7 }),
        ];
        let r = restore_records(&records);
        assert_eq!(
            r.tokens, 7,
            "the second goal inherited the first goal's tokens"
        );
        assert_eq!(
            r.iterations, 1,
            "the second goal inherited the first goal's model calls"
        );
        assert_eq!(
            r.kicks, 0,
            "the second goal inherited the first goal's nudges"
        );
        assert!(
            r.failed_now.is_empty(),
            "a failure from a finished goal followed the next one: {:?}",
            r.failed_now
        );
        assert!(
            r.failed_ever.is_empty(),
            "a failure from a finished goal followed the next one: {:?}",
            r.failed_ever
        );

        // The control, and the half that keeps this honest: within *one* goal
        // everything accumulates, so a fold that simply zeroed the counters at
        // the end would pass all five assertions above.
        let r = restore_records(&records[..5]);
        assert_eq!(r.tokens, 100);
        assert_eq!(r.iterations, 1);
        assert_eq!(r.kicks, 2);
        assert_eq!(r.failed_now.len(), 1);
    }

    #[test]
    fn a_log_written_before_the_weighting_existed_still_restores_its_spend() {
        // `cost_tokens` is what the budget counts; a log old enough not to have
        // it carries only the raw fields. Reading zero there is the exact
        // failure this region exists to prevent — a run resumed from an older
        // session with the budget spent and a meter saying nothing has been.
        let r = restore_records(&[
            opened(),
            json!({ "kind": "model_call", "billable_total_tokens": 55 }),
        ]);
        assert_eq!(
            r.tokens, 55,
            "an old log restored a fresh budget, which makes the cap that \
             stopped the first run mean nothing"
        );

        // The control: where both are present the weighted one wins, because a
        // cached read costs less than its size and the loop's meter is in those
        // units. Summing the raw field instead would restore a meter in
        // different units from the one the loop enforces.
        let r = restore_records(&[
            opened(),
            json!({ "kind": "model_call", "cost_tokens": 3, "billable_total_tokens": 55 }),
        ]);
        assert_eq!(r.tokens, 3, "the resumed meter is not in the loop's units");
    }

    #[test]
    fn a_goals_own_totals_win_over_the_running_ones() {
        // `goal_finished` carries the loop's own arithmetic and is the better
        // number where it exists; the running totals answer the case the file
        // exists for, which is a run killed before it wrote one. Dropping the
        // override left the suite green.
        let r = restore_records(&[
            opened(),
            json!({ "kind": "model_call", "cost_tokens": 10 }),
            json!({ "kind": "goal_finished", "ending": "tokens",
                    "tokens": 999, "iterations": 40, "kicks": 3 }),
        ]);
        assert_eq!(r.tokens, 999, "the loop's own token total was ignored");
        assert_eq!(
            r.iterations, 40,
            "the loop's own iteration count was ignored"
        );
        assert_eq!(r.kicks, 3, "the loop's own nudge count was ignored");
        assert!(
            r.in_flight,
            "a goal cut off by its budget is exactly what a resume continues"
        );

        // The control: a `goal_finished` missing its totals falls back to the
        // running ones rather than to zero, which is the same fresh-budget
        // failure arriving through a truncated record.
        let r = restore_records(&[
            opened(),
            json!({ "kind": "model_call", "cost_tokens": 10 }),
            json!({ "kind": "goal_finished", "ending": "tokens" }),
        ]);
        assert_eq!(
            r.tokens, 10,
            "a `goal_finished` with no totals in it zeroed the meter"
        );
    }

    #[test]
    fn a_success_re_allows_the_calls_that_failed_before_it() {
        // The loop's rule, replayed by the fold: something changed, so every
        // earlier failure is worth trying again. A resume that only ever
        // accumulated failures hands the model a memo forbidding calls the
        // original run had already re-allowed, and the model has no way to see
        // why it is being refused.
        //
        // Deleting the `failed_now.clear()` in `restore_records` left every
        // test in the crate green: the rule is exercised live by `tests/loop.rs`
        // and its replay here was exercised by nothing.
        let call_1 =
            json!({ "kind": "tool_call", "id": "t1", "tool": "Bash", "args": { "cmd": "a" } });
        let failed = json!({ "kind": "tool_result", "id": "t1",
            "block": { "type": "tool_result", "tool_use_id": "t1", "is_error": true } });
        let call_2 =
            json!({ "kind": "tool_call", "id": "t2", "tool": "Bash", "args": { "cmd": "b" } });
        let ok = json!({ "kind": "tool_result", "id": "t2",
            "block": { "type": "tool_result", "tool_use_id": "t2", "content": "fine" } });

        let r = restore_records(&[opened(), call_1.clone(), failed.clone()]);
        assert_eq!(r.failed_now.len(), 1, "the failure was not restored at all");
        assert_eq!(r.failed_ever.len(), 1);

        let r = restore_records(&[opened(), call_1, failed, call_2, ok]);
        assert!(
            r.failed_now.is_empty(),
            "a call that failed before something else succeeded is still \
             forbidden after the resume: {:?}",
            r.failed_now
        );
        assert_eq!(
            r.failed_ever.len(),
            1,
            "what failed during the goal is what a nudge quotes back, and a \
             later success does not un-happen it"
        );
    }

    #[test]
    fn a_goal_that_stopped_mid_turn_is_separated_from_the_next_one() {
        // A run killed on its budget leaves a user turn last — the tool results
        // nothing answered — and the next goal's opening is also a user turn.
        // Two user turns in a row is a 400, so the whole restored conversation
        // is unsendable, which is the most expensive thing this fold can
        // produce. The note is information as well as separation: without it
        // the model reads an abandoned goal as a completed one.
        //
        // Deleting it left every test in the crate green.
        let msgs = fold_records(&[
            opened(),
            assistant_with(&["tu_1"]),
            result_for("tu_1"),
            json!({ "kind": "goal_finished", "ending": "the token budget" }),
            json!({ "kind": "goal", "text": "h", "opening": "work on h" }),
        ]);
        assert!(
            msgs.windows(2)
                .all(|w| !(w[0].role == Role::User && w[1].role == Role::User)),
            "two user turns in a row: the API refuses this list outright, so the \
             whole resumed session is unsendable. {:?}",
            msgs.iter().map(|m| m.role).collect::<Vec<_>>()
        );
        let note = crate::agent::ended_note("the token budget");
        assert!(
            msgs.iter().any(|m| m.content.to_string().contains(&note)),
            "the abandoned goal is not named, so the model reads it as finished: {msgs:?}"
        );

        // The control: a goal whose last message is already an assistant turn
        // needs no separating and gets no note. Padding every goal boundary is
        // noise the model pays for and a sentence that is not true.
        let msgs = fold_records(&[
            opened(),
            json!({ "kind": "assistant", "raw_content": [{ "type": "text", "text": "done" }] }),
            json!({ "kind": "goal_finished", "ending": "the token budget" }),
            json!({ "kind": "goal", "text": "h", "opening": "work on h" }),
        ]);
        assert!(
            !msgs.iter().any(|m| m
                .content
                .to_string()
                .contains("stopped before it was finished")),
            "a note was added after an assistant turn, where nothing needed \
             separating: {msgs:?}"
        );
    }

    #[test]
    fn a_file_that_records_no_goal_is_refused_rather_than_resumed_empty() {
        // `--resume` on a file with no goal in it has nothing to continue, and
        // succeeding with an empty conversation would look to the user like
        // their session came back. The refusal names the file, because the next
        // thing they will do is go and look at it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess-nogoal.jsonl");
        fs::write(&path, "{\"kind\":\"assistant\",\"raw_content\":[]}\n").unwrap();
        let err = restore(&path).expect_err("a file with no goal was resumed anyway");
        assert!(
            err.to_string().contains(&path.display().to_string()),
            "the refusal does not name the file: {err}"
        );

        // The control: one goal record is enough.
        let ok = dir.path().join("sess-goal.jsonl");
        fs::write(&ok, "{\"kind\":\"goal\",\"opening\":\"work on g\"}\n").unwrap();
        assert!(restore(&ok).is_ok(), "a file with a goal in it was refused");
    }

    #[test]
    fn the_path_emma_prints_is_accepted_as_a_session_id() {
        // The exit line names a path ending in `.jsonl`, and the obvious thing
        // to do with it is paste it after `--resume`. Nothing tested that it
        // works, and the failure would be a flat "no session `…jsonl`" against
        // a file sitting right there.
        let dir = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();
        let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();
        log.append(
            "goal",
            json!({ "text": "g", "cwd": here.path().display().to_string() }),
        );

        let bare = locate(dir.path(), Some("sess-0000000000001-1"), here.path()).unwrap();
        let printed = locate(dir.path(), Some("sess-0000000000001-1.jsonl"), here.path()).unwrap();
        assert_eq!(
            printed, bare,
            "the path emma prints is not accepted as an id"
        );

        // The control: a name that is simply wrong is still refused, so the
        // stripping above is not an accident of accepting anything at all.
        assert!(locate(dir.path(), Some("sess-nope.jsonl"), here.path()).is_err());
    }
}

#[cfg(test)]
mod steer_fold_tests {
    use super::*;

    fn text_of(m: &Message) -> String {
        match &m.content {
            Content::Text(t) => t.clone(),
            Content::Blocks(b) => panic!("these tests build text turns only: {b:?}"),
        }
    }

    /// A `steer` record joins the user turn already at the end rather than
    /// opening a second one: two user turns in a row is a 400.
    #[test]
    fn a_steer_record_folds_into_the_open_user_turn() {
        let records = vec![
            json!({"kind": "goal", "goal_id": "g1", "opening": "do the thing"}),
            json!({"kind": "steer", "text": "and also this"}),
        ];
        let out = fold_records(&records);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].role, Role::User);
        assert_eq!(text_of(&out[0]), "do the thing\n\nand also this");
    }

    /// `append_user_text` opens a new turn only when the last one is not the
    /// user's. Removing the role check makes this push twice and fails.
    #[test]
    fn append_user_text_opens_a_turn_only_after_an_assistant() {
        let mut out = vec![Message::assistant_text("done")];
        append_user_text(&mut out, "more".into());
        append_user_text(&mut out, "and more".into());
        assert_eq!(out.len(), 2);
        assert_eq!(text_of(&out[1]), "more\n\nand more");
    }

    /// Each prefix is the conversation as sent with that assistant turn: the
    /// first turn sees only the opening, the second sees the first turn too.
    #[test]
    fn fold_prefixes_is_the_fold_stopped_before_each_assistant_record() {
        let records = vec![
            json!({"kind": "goal", "goal_id": "g1", "opening": "q"}),
            json!({"kind": "assistant", "raw_content": [{"type": "text", "text": "a1"}]}),
            json!({"kind": "kick", "text": "go on"}),
            json!({"kind": "assistant", "raw_content": [{"type": "text", "text": "a2"}]}),
        ];
        let prefixes = fold_prefixes(&records);
        assert_eq!(prefixes.iter().map(|p| p.0).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(prefixes[0].1.len(), 1);
        assert!(
            prefixes[1].1.len() > prefixes[0].1.len(),
            "{:?}",
            prefixes[1].1
        );
    }
}
