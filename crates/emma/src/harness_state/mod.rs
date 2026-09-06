//! What the session file says about runs, events and approval posture.
//!
//! Read-only derivation over records `session.rs` already writes — it adds no
//! new record and decides nothing. Ported from the fork reviewed on
//! 2026-08-27; its discipline is the reason it was ranked second: `Option` on
//! nearly every field with the reason it is optional, one named inference with
//! its measurement, and a per-feed account of the lines it could not read, so a
//! page built on this can say how much of its own input was unreadable rather
//! than drawing a confident picture over a gap.
//!
//! **This is a reader, and only a reader.** Nothing here starts a run, ends
//! one, or asks the model anything. Every number comes off the session JSONL
//! that `session.rs` appends to — the same bytes `emma agents` counts and
//! `--resume` folds — so a value on a harness page is a value the loop wrote,
//! not one this module inferred and dressed up.
//!
//! **The harness the fork's mocks draw does not exist.** There is no worker
//! pool, no task queue and no DAG: Emma's real runs are goals in a session and
//! delegations inside them, and those are what [`runs`] returns. A card the log
//! cannot feed stays empty rather than being filled with a plausible number —
//! see the `Option` on nearly every field below, each one carrying the reason
//! it is optional.
//!
//! **One inference lives here, and it is named.** A session file records that a
//! goal started and, later, that it finished. It never records that the process
//! is alive. So "running" is read off the clock: an unfinished goal whose file
//! has been written to within [`STALE_AFTER_MS`] is [`RunStatus::Running`], and
//! one that has gone quiet longer than that is [`RunStatus::Stalled`] — not
//! failed, because nothing said it failed; not running, because nothing has
//! said anything. The threshold's measurement is on the constant.
//!
//! # The damage account, and why the live source is better than the fork's
//!
//! The fork counted its own unreadable lines, because the `session.rs` it was
//! written against read with a bare `filter_map(ok)` and had nothing to hand
//! back. That reader is not in this tree.
//! [`SessionLog::read_reporting`](crate::session::SessionLog::read_reporting)
//! is, and it answers a strictly better question than a count:
//!
//! - It returns the **line numbers**, so a page can say *which* lines to go and
//!   look at rather than only how many there were. Every feed here carries them
//!   through; a caller that wants the fork's number calls `.len()`.
//! - It **exempts a torn tail** — a file that stops mid-line, keyed on the
//!   trailing byte rather than on the line's position (`190dd7b`). That is what
//!   a crash mid-write leaves and it is not damage. The fork's own counter had
//!   no such notion, so every live session being written to at the moment of
//!   the read would have reported one damaged line on a page whose whole point
//!   is that the number means something.
//!
//! What that costs, stated because it is a real cost and not a footnote:
//! `read_reporting` also prints to stderr when it finds damage. Under the
//! full-screen frame stderr is not somewhere a person is looking, and a page
//! that polls a damaged file will call it again on every poll. Nothing here can
//! fix that from inside this module — the print is in `session.rs` — and no
//! caller should treat this module's return value as *also* having been
//! reported to the user.
//!
//! # What was left behind from the fork
//!
//! The fork's [`RunTrace`] also read four record kinds its own `telemetry.rs`
//! wrote — `node_spawn`, `node_done`, `queue_wait`, `task_progress` — and
//! exposed a delegation graph and a queue-depth derivation over them. That
//! module is not in this tree (audit §5 item 3 ranks it separately), so nothing
//! writes any of those four kinds here: a grep over the 156 real session files
//! on the machine this was ported on found none of them, and the record
//! inventory in `agent.rs` and `delegate.rs` does not contain them. A reader
//! for records no writer emits can only ever be exercised by fixtures its own
//! author invented, which is the false-receipt shape this repository has been
//! bitten by. It comes back with `telemetry.rs`, in the same change, tested
//! against what that module actually appends.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::approval::Gate;
use crate::session::{recorded_in, SessionLog};

/// How long an unfinished goal may go without a record before the feed stops
/// calling it running.
///
/// **Measured, not chosen for roundness — and measured twice.**
///
/// The fork stated 300s from 16 session files, with a worst intra-goal gap of
/// 135s (a slow model call) and a runner-up of 125s (a tool that failed after
/// two minutes). Those 16 files are not in the archive, so that measurement
/// cannot be re-run and is repeated here as somebody else's reading rather than
/// as a fact of this tree.
///
/// It was re-derived on this machine on 2026-08-26, over the **156** session
/// files in `~/.emma/sessions`, by walking every adjacent pair of records and
/// splitting them into gaps *inside* an open goal and gaps in front of a `goal`
/// record. The result: 130 intra-goal gaps, worst 55.8s (a `tool_call` waiting
/// on its `tool_result`), runner-up 24.5s; **none over 300s**. Every gap longer
/// than 300s in the whole corpus sat in front of a `goal` record — the longest
/// 423.5s — which is a human at a prompt, not a run.
///
/// So 300s is a little over five times the observed worst case here and a
/// little over twice the fork's: long enough that a slow local model or a
/// `cargo build` does not make a live run look dead, short enough that a
/// session killed with `kill -9` stops claiming the CPU on the page within five
/// minutes.
///
/// The honest fix is a heartbeat or a pid in the log, and neither exists today.
/// Until one does, this number is the whole of "is it running".
pub const STALE_AFTER_MS: u64 = 300_000;

/// How wide a run name is cut. The ACTIVE RUNS card shows one line per run.
pub const NAME_WIDTH: usize = 72;

/// How many events [`events`] hands back when the caller does not say.
pub const EVENT_TAIL: usize = 200;

// region: Damage

/// One file's unreadable lines, for a feed that read a whole directory.
///
/// The line numbers are
/// [`SessionLog::read_reporting`](crate::session::SessionLog::read_reporting)'s,
/// 1-based, and exclude the torn tail a crash leaves. A file with nothing wrong
/// with it never appears in a feed's list at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDamage {
    pub path: PathBuf,
    pub lines: Vec<usize>,
}

/// A session file that could not be read at all, and why.
///
/// Distinct from [`FileDamage`]: that one is a file we read and could not fully
/// parse, this one is a file we never got the bytes of. A page must be able to
/// tell "this session is missing three turns" from "this session is missing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub path: PathBuf,
    pub problem: String,
}

// endregion: Damage

// region: Runs

/// Which of Emma's two kinds of run a row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    /// One goal in a session: what the person at the keyboard asked for.
    Goal,
    /// One delegation inside a goal — the `sub.*` records a subagent wrote.
    Delegation,
}

/// What the feed can honestly say about a run's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Unfinished, and written to within [`STALE_AFTER_MS`]. An inference; see
    /// the module doc.
    Running,
    /// Finished with `done` or `answered`.
    Completed,
    /// Finished with any other ending. [`RunRow::ending`] names which.
    Failed,
    /// Unfinished and quiet, or overtaken by a later goal in the same file.
    /// Nothing said it failed and nothing says it lives.
    Stalled,
}

/// A fraction the log can actually back: work done against a ceiling that was
/// recorded, never an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

/// One row of the ACTIVE RUNS card.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    /// `<session-id>#<n>` for a goal, the `sub_id` for a delegation. Stable
    /// across reads, and what [`detail`] takes.
    pub id: String,
    /// The session file this came out of, without the `.jsonl`.
    pub session: String,
    pub kind: RunKind,
    /// First line of the goal text, cut at [`NAME_WIDTH`].
    pub name: String,
    /// From the `goal` record's `at_ms`. Always known: a run with no start
    /// record is not a run this module can see.
    ///
    /// **One exception, and the fork's doc did not have it.** A `delegation`
    /// record whose `sub.goal` is not in the file — a log that starts mid-run —
    /// still becomes a row, and its start is `at_ms` minus the recorded
    /// `elapsed_ms` rather than a timestamp anybody wrote down. That is a
    /// derivation, and it is the only value in this struct that is one. It is
    /// not an `Option` because a row with no start cannot be sorted or placed,
    /// and being off by whatever the loop did not count is a smaller lie than
    /// dropping the delegation entirely.
    pub started_ms: u64,
    /// The `at_ms` of the last record belonging to this run — what
    /// [`RunStatus::Running`] is decided against.
    pub last_event_ms: u64,
    pub status: RunStatus,
    /// `None` for a goal, and for a delegation that is still running.
    ///
    /// The loop records a run's *ceiling* nowhere except the `delegation`
    /// record `delegate.rs` writes when the delegation is over, so there is no
    /// honest denominator for live work. Anything else this field could hold
    /// would be a bar moving at a rate nobody measured.
    pub progress: Option<Progress>,
    /// The model named on the `goal` record.
    pub model: Option<String>,
    /// The agent type, for a delegation.
    pub agent: Option<String>,
    /// Model calls this run made, counted from `model_call` records rather
    /// than trusted from the ending, so a live run has one too.
    pub iterations: Option<u64>,
    /// `goal_total_so_far` off the last `model_call`, or the ending's total.
    pub tokens: Option<u64>,
    /// Only from `goal_finished`: the loop times a goal and reports the
    /// elapsed at the end. A live run's clock is the caller's to compute from
    /// [`RunRow::started_ms`], because only the caller knows what time it is.
    pub elapsed_ms: Option<u64>,
    /// `done`, `answered`, `tokens`, `provider_error`… `None` while unfinished.
    pub ending: Option<String>,
    /// Where the goal ran. Recorded once per goal; absent on a delegation,
    /// whose `sub.goal` record carries the sub's own cwd only if it had one.
    pub cwd: Option<String>,
}

/// Every run the session directory knows about, recent first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunsFeed {
    pub runs: Vec<RunRow>,
    /// Lines that were not records, per file. Never fatal, never silent.
    pub damage: Vec<FileDamage>,
    pub unreadable: Vec<Unreadable>,
}

impl RunsFeed {
    /// How many lines this feed could not read, across every file it opened.
    pub fn skipped_lines(&self) -> usize {
        self.damage.iter().map(|d| d.lines.len()).sum()
    }
}

/// Token totals for one run, added up from its `model_call` records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    /// What the provider billed, summed. `goal_total_so_far` on the last call
    /// is the loop's own running total and is on [`RunRow::tokens`].
    pub billable: u64,
}

/// Whether a tool call reached its tool, and what came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStatus {
    Ok,
    /// The tool ran and failed, panicked, was cancelled, did not exist, or the
    /// result block was flagged an error.
    Error,
    /// A hook or a human refused it. It never ran.
    Denied,
    /// A `tool_call` with no result, no failure and no denial in the file —
    /// the run stopped between the call and its answer.
    Unanswered,
}

/// One row of the TOOL CALLS card.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRow {
    pub id: String,
    pub tool: String,
    /// The arguments on one line, cut. The full object is in the log.
    pub args: String,
    pub at_ms: u64,
    pub status: CallStatus,
    /// The failure kind, the denial reason, or `None`.
    pub detail: Option<String>,
    /// Result time minus call time, when both records are present.
    ///
    /// This is wall time from "the loop logged the call" to "the loop logged
    /// the answer", so it contains the approval prompt when there was one. It
    /// is not the tool's own execution time and must not be labelled as one.
    pub elapsed_ms: Option<u64>,
}

/// Everything an inspect page shows for one run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunDetail {
    pub row: RunRow,
    pub tokens: TokenTotals,
    pub calls: Vec<ToolCallRow>,
    /// This run's records as leveled lines, oldest first.
    pub events: Vec<EventLine>,
    /// Nudges the loop sent. `None` until `goal_finished`.
    pub kicks: Option<u64>,
    /// The `detail` on `goal_finished` — the provider's error text, when the
    /// ending was one.
    pub ending_detail: Option<String>,
    /// Lines of the file this run's records came from that were not records.
    /// The whole file's, not this run's: a line that would not parse has no
    /// run.
    pub damaged_lines: Vec<usize>,
}

/// One step of a run, in the order the log recorded it.
///
/// A detail page wants the tool calls as a table and the records as a stream; a
/// graph wants neither. It wants to know what followed what, which is the one
/// thing the flat lists throw away. This enum keeps it.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// A `model_call`: one model turn, carrying what that call cost.
    ///
    /// The record is appended after the provider answered, so a turn that is
    /// in this list is a turn that returned.
    Turn(TurnRow),
    /// A `tool_call` and whatever answered it, joined by call id.
    Call(ToolCallRow),
}

/// One model turn, off its `model_call` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRow {
    pub iteration: u64,
    pub at_ms: u64,
    pub stop_reason: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One run as an ordered walk, for the callers that need the sequence.
///
/// **No parallelism is recorded and none is inferred.** `agent.rs` appends
/// `model_call`, then `assistant`, then one `tool_call` per call and its
/// answer, in execution order; the steps below are that order and nothing
/// else. A drawing of this run is a chain because the log is a chain.
#[derive(Debug, Clone, PartialEq)]
pub struct RunTrace {
    pub row: RunRow,
    /// Oldest first.
    pub steps: Vec<Step>,
    pub tokens: TokenTotals,
    /// Refusals inside this run, oldest first.
    pub denials: Vec<Denial>,
    /// As [`RunDetail::damaged_lines`].
    pub damaged_lines: Vec<usize>,
}

impl RunTrace {
    /// Model turns in the trace.
    pub fn turns(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, Step::Turn(_)))
            .count()
    }

    /// Tool calls in the trace.
    pub fn calls(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, Step::Call(_)))
            .count()
    }
}

// endregion: Runs

// region: Events

/// How loud a line is. Three levels, because the log distinguishes three
/// things: what happened, what went wrong and was carried on from, and what
/// the harness itself could not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// One rendered record: when, how loud, and one line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLine {
    pub at_ms: u64,
    pub level: Level,
    pub line: String,
}

/// The tail of one session log, rendered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFeed {
    /// Oldest first, at most the requested count.
    pub lines: Vec<EventLine>,
    pub damaged_lines: Vec<usize>,
    /// Records in the file the tail did not include.
    pub older: usize,
}

// endregion: Events

// region: Gates

/// One refusal, off a `denied` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    pub at_ms: u64,
    /// The tool the refused call named. The `denied` record carries only the
    /// call id, so this is joined back to the `tool_call` it answers; when that
    /// call is not in the file, the id stands in for the name.
    pub tool: String,
    /// Who refused, verbatim from the record.
    ///
    /// **The fork's doc said "`hook` or `user`" and that is false here.** This
    /// tree writes seven: `hook` from the `PreToolUse` denial in `agent.rs`, and
    /// `Decider::logged`'s six — `user`, `rule`, `unattended`, `end-of-input`,
    /// `unevaluable`, and `mode` since plan mode was ported on 2026-09-06. That enum's own doc gives the reason they are distinct
    /// strings rather than a bool: "the user said no" and "there was no user"
    /// are different runs. So this is carried through as written rather than
    /// mapped onto a two-valued enum here, which would throw away exactly the
    /// distinction `approval.rs` went to the trouble of recording.
    pub by: String,
    pub reason: String,
}

/// What a tool-gates card can say today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatesView {
    /// The posture this process was started in, as a word.
    pub mode_label: &'static str,
    /// The one-line description of that posture.
    pub mode_about: &'static str,
    /// A call waiting on a human right now.
    ///
    /// **Always `None` today, and that is a fact about the gate, not about
    /// this module.** `Approvals::request` asks its question inside the call
    /// that needs the answer and awaits it there; the pending question lives in
    /// a future's state, and there is nothing to read it out of. Filling this
    /// in means the gate publishing what it is asking. Inventing it from the
    /// log is not an option either: a `tool_call` with no answer yet is also
    /// exactly what an interrupted run looks like.
    pub pending: Option<PendingApproval>,
    /// Refusals recorded in this session, most recent first.
    pub denials: Vec<Denial>,
    /// Tool calls the session logged, and how many of them were refused.
    pub calls: u64,
    pub damaged_lines: Vec<usize>,
}

/// The question a human is being asked. Nothing constructs one yet; see
/// [`GatesView::pending`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub tool: String,
    pub preview: String,
    pub since_ms: u64,
}

/// The posture as a word.
///
/// **These are the gate's words, not the posture's.** The status bar's MODE
/// cell says `ASSIST`, `AUTO` or `PLAN` from `approval::current_mode_label`,
/// the posture ported on 2026-09-06. This card draws the gate the run started
/// with, and under `/mode plan` that gate still reads `ASK`, which is the
/// started gate and not the refusal in force. The Harness page package that
/// makes the card live should draw the posture beside the gate word; until it
/// does, this label understates a plan-mode session.
///
/// Exhaustive on purpose: a fourth [`Gate`] variant should stop the build here
/// rather than reach a page as a blank.
pub fn gate_label(gate: Gate) -> &'static str {
    match gate {
        Gate::Ask => "ASK",
        Gate::SkipAll => "SKIP-ALL",
        Gate::Unattended => "UNATTENDED",
    }
}

/// One sentence on what that posture means, in the gate's own terms.
pub fn gate_about(gate: Gate) -> &'static str {
    match gate {
        Gate::Ask => "anything that writes or leaves the machine asks you first",
        Gate::SkipAll => "--dangerously-skip-permissions: nothing is asked, everything runs",
        Gate::Unattended => {
            "nobody to ask, so anything needing approval is denied and the model \
                             is told why"
        }
    }
}

// endregion: Gates

// region: Reading

/// The `.jsonl` files in a session directory, oldest id first. A directory that
/// is not there is an empty one: a machine that has never run Emma has no runs,
/// which is not an error.
fn session_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    // Ids sort by time — the ordering `emma agents` and `session::locate` use.
    files.sort();
    Ok(files)
}

fn session_id_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// endregion: Reading

// region: Reading records

fn text_of(record: &Value, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn num_of(record: &Value, key: &str) -> Option<u64> {
    record.get(key).and_then(Value::as_u64)
}

/// The record's kind with any `sub.` prefix taken off, and whether it had one.
///
/// [`SessionLog::subagent`](crate::session::SessionLog::subagent) namespaces a
/// delegation's records so the fold ignores them. Here they are wanted — they
/// are the delegation's whole life — so the prefix is a flag rather than a
/// different vocabulary of kinds.
fn kind_of(record: &Value) -> (&str, bool) {
    let kind = record.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind.strip_prefix("sub.") {
        Some(rest) => (rest, true),
        None => (kind, false),
    }
}

/// The first line of a text, cut to `width` with the cut marked.
fn one_line(text: &str, width: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.chars().count() <= width {
        return first.to_string();
    }
    let kept: String = first.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// How wide a tool call's arguments are rendered.
const ARGS_WIDTH: usize = 96;

/// The arguments on one line. Compact JSON rather than a prose preview: the
/// gate's `approval::preview` is written for a human deciding whether to allow
/// a call, and a row in a table wants the values, cut.
fn args_summary(args: Option<&Value>) -> String {
    let Some(args) = args else {
        return String::new();
    };
    let text = match args {
        Value::Object(_) | Value::Array(_) => args.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // Control characters would be escape bytes on a page that is drawn, and a
    // bare CR in a piped record can overwrite the line above it. Neither is
    // this reader's to emit — see `CLAUDE.md` on redirected output.
    //
    // **Which branch above this actually matters was found by mutation.**
    // Deleting this map left the whole suite green, because the first test
    // written for it used an object: `Value::to_string` re-serialises, and
    // serde escapes a control character into `` or `\r` on the way out,
    // so nothing raw ever reaches here through the object or array arm. The
    // `Value::String` arm is the one that carries bytes verbatim — a `hook`
    // record's `run` is a plain string — and it is the arm the test now uses.
    // The map stays on all three: which arm a caller lands in is not this
    // function's to know, and a sanitiser that is correct only for the input
    // its test happened to pick is the false receipt over again.
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    one_line(&flat, ARGS_WIDTH)
}

// endregion: Reading records

// region: Runs, built

/// `done` and `answered` are the two endings that mean the work finished — the
/// same pair `main` and `emma agents` treat together, because a brief that
/// needed no tools was still answered. Every other ending stopped short, and
/// [`RunRow::ending`] carries which one so the page never has to guess.
fn status_of(ending: &str) -> RunStatus {
    match ending {
        "done" | "answered" => RunStatus::Completed,
        _ => RunStatus::Failed,
    }
}

/// A row still being built, and whether its run has ended.
struct Building {
    row: RunRow,
    open: bool,
}

/// Every run in one session file's records.
///
/// The walk is one pass and holds two things: the goal currently open, and the
/// delegations open inside it. A record touches the goal's clock whether or not
/// it came from a delegation, because a parent waiting on a subagent is a
/// parent that is still running.
fn rows_from_records(session: &str, records: &[Value], now_ms: u64) -> Vec<RunRow> {
    let mut out: Vec<Building> = Vec::new();
    let mut goal_ordinal = 0usize;
    let mut current_goal: Option<usize> = None;
    let mut subs: HashMap<String, usize> = HashMap::new();

    for record in records {
        let (kind, is_sub) = kind_of(record);
        let at = num_of(record, "at_ms").unwrap_or(0);
        let sub_id = text_of(record, "sub_id");

        // The goal's clock, first and unconditionally: a delegation's record is
        // still evidence its parent is alive.
        if let Some(i) = current_goal {
            if out[i].open {
                out[i].row.last_event_ms = out[i].row.last_event_ms.max(at);
            }
        }
        let target = match &sub_id {
            Some(id) => subs.get(id).copied(),
            None => current_goal,
        };
        if let Some(i) = target {
            out[i].row.last_event_ms = out[i].row.last_event_ms.max(at);
        }

        match kind {
            "goal" if is_sub => {
                let Some(id) = sub_id.clone() else { continue };
                let text = text_of(record, "text").unwrap_or_default();
                out.push(Building {
                    row: RunRow {
                        id: id.clone(),
                        session: session.to_string(),
                        kind: RunKind::Delegation,
                        name: one_line(&text, NAME_WIDTH),
                        started_ms: at,
                        last_event_ms: at,
                        status: RunStatus::Running,
                        progress: None,
                        model: text_of(record, "model"),
                        agent: text_of(record, "agent"),
                        iterations: None,
                        tokens: None,
                        elapsed_ms: None,
                        ending: None,
                        cwd: text_of(record, "cwd"),
                    },
                    open: true,
                });
                subs.insert(id, out.len() - 1);
            }
            "goal" => {
                // A goal starting while the last one is still open means the
                // session moved on without an ending being written — a crash,
                // or a kill. It is not running, whatever the clock says.
                if let Some(i) = current_goal {
                    if out[i].open {
                        out[i].open = false;
                        out[i].row.status = RunStatus::Stalled;
                    }
                }
                goal_ordinal += 1;
                let text = text_of(record, "text").unwrap_or_default();
                out.push(Building {
                    row: RunRow {
                        id: format!("{session}#{goal_ordinal}"),
                        session: session.to_string(),
                        kind: RunKind::Goal,
                        name: one_line(&text, NAME_WIDTH),
                        started_ms: at,
                        last_event_ms: at,
                        status: RunStatus::Running,
                        progress: None,
                        model: text_of(record, "model"),
                        agent: None,
                        iterations: None,
                        tokens: None,
                        elapsed_ms: None,
                        ending: None,
                        cwd: text_of(record, "cwd"),
                    },
                    open: true,
                });
                current_goal = Some(out.len() - 1);
            }
            "model_call" => {
                let Some(i) = target else { continue };
                let row = &mut out[i].row;
                // The count comes off the calls rather than off the ending, so
                // a run that is still going has one too.
                let seen = num_of(record, "iteration").unwrap_or(0);
                row.iterations = Some(row.iterations.unwrap_or(0).max(seen));
                if let Some(total) = num_of(record, "goal_total_so_far") {
                    row.tokens = Some(total);
                }
            }
            "goal_finished" => {
                let Some(i) = target else { continue };
                out[i].open = false;
                let ending = text_of(record, "ending").unwrap_or_else(|| "?".into());
                let row = &mut out[i].row;
                row.status = status_of(&ending);
                row.ending = Some(ending);
                row.tokens = num_of(record, "tokens").or(row.tokens);
                row.iterations = num_of(record, "iterations").or(row.iterations);
                row.elapsed_ms = num_of(record, "elapsed_ms");
                if !is_sub {
                    current_goal = None;
                }
            }
            // The parent's own record of a delegation, and the only place a
            // run's ceiling is written down.
            "delegation" => {
                let Some(id) = sub_id.clone() else { continue };
                let i = match subs.get(&id).copied() {
                    Some(i) => i,
                    // A delegation whose `sub.goal` is missing — a file that
                    // starts mid-run. The record carries enough to be a row on
                    // its own, so it becomes one rather than vanishing.
                    None => {
                        let elapsed = num_of(record, "elapsed_ms").unwrap_or(0);
                        let task = record
                            .get("task")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        out.push(Building {
                            row: RunRow {
                                id: id.clone(),
                                session: session.to_string(),
                                kind: RunKind::Delegation,
                                name: one_line(&task, NAME_WIDTH),
                                started_ms: at.saturating_sub(elapsed),
                                last_event_ms: at,
                                status: RunStatus::Running,
                                progress: None,
                                model: None,
                                agent: text_of(record, "agent"),
                                iterations: None,
                                tokens: None,
                                elapsed_ms: None,
                                ending: None,
                                cwd: None,
                            },
                            open: true,
                        });
                        subs.insert(id, out.len() - 1);
                        out.len() - 1
                    }
                };
                out[i].open = false;
                let ending = text_of(record, "ending").unwrap_or_else(|| "?".into());
                let row = &mut out[i].row;
                row.status = status_of(&ending);
                row.ending = Some(ending);
                row.agent = text_of(record, "agent").or(row.agent.take());
                row.tokens = num_of(record, "cost_tokens").or(row.tokens);
                row.iterations = num_of(record, "iterations").or(row.iterations);
                row.elapsed_ms = num_of(record, "elapsed_ms").or(row.elapsed_ms);
                if let (Some(done), Some(total)) =
                    (row.iterations, num_of(record, "max_iterations"))
                {
                    row.progress = Some(Progress { done, total });
                }
            }
            _ => {}
        }
    }

    out.into_iter()
        .map(|mut b| {
            if b.open {
                // The one inference in this module. See [`STALE_AFTER_MS`].
                b.row.status = if now_ms.saturating_sub(b.row.last_event_ms) <= STALE_AFTER_MS {
                    RunStatus::Running
                } else {
                    RunStatus::Stalled
                };
            }
            b.row
        })
        .collect()
}

// endregion: Runs, built

// region: Events, rendered

/// One record as one line, with the level the record's own kind implies.
///
/// **The levels are the log's three outcomes, not a severity taste.** `INFO` is
/// what happened; `WARN` is something that went wrong and the loop carried on
/// from, which is every tool failure, every refusal, every nudge and every
/// ending that is not `done` or `answered`; `ERROR` is the harness itself
/// failing — a `tool_fault` or a provider that stopped the run.
fn render(record: &Value) -> EventLine {
    let (kind, is_sub) = kind_of(record);
    let at_ms = num_of(record, "at_ms").unwrap_or(0);
    let tool = text_of(record, "tool").unwrap_or_else(|| "?".into());
    let detail_of = |key: &str| one_line(&text_of(record, key).unwrap_or_default(), ARGS_WIDTH);
    let (level, body) = match kind {
        "goal" => (
            Level::Info,
            format!(
                "goal: {}",
                one_line(&text_of(record, "text").unwrap_or_default(), NAME_WIDTH)
            ),
        ),
        "model_call" => (
            Level::Info,
            format!(
                "model call #{} — {} in, {} out ({})",
                num_of(record, "iteration").unwrap_or(0),
                num_of(record, "input_tokens").unwrap_or(0),
                num_of(record, "output_tokens").unwrap_or(0),
                text_of(record, "stop_reason").unwrap_or_else(|| "?".into()),
            ),
        ),
        // A turn whose whole content was tool calls has no text, and a blank
        // line in an event log reads as a rendering bug rather than as a turn.
        "assistant" => (
            Level::Info,
            match text_of(record, "text") {
                Some(text) => format!("assistant: {}", one_line(&text, NAME_WIDTH)),
                None => "assistant: (tool calls only, no prose)".to_string(),
            },
        ),
        "tool_call" => (
            Level::Info,
            format!("{tool}({})", args_summary(record.get("args"))),
        ),
        "tool_result" => {
            let failed = record["block"]["is_error"].as_bool().unwrap_or(false);
            let level = if failed { Level::Warn } else { Level::Info };
            let note = if failed { "error" } else { "ok" };
            let truncated = record
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let tail = if truncated { ", truncated" } else { "" };
            (level, format!("{tool} → {note}{tail}"))
        }
        // The failure *class* is not here to be shown. `agent.rs:1834` puts
        // `e.kind()` on this record under the key `kind`, and
        // `SessionLog::append` overwrites that key with the record's own kind on
        // the way to disk — so every `tool_failed` line ever written says
        // `tool_failed` where the class should be, and the class is gone. The
        // sentence in `detail` is what survived, and it is all this can honestly
        // render. Checked against the two real `tool_failed` records in
        // `~/.emma/sessions` on 2026-08-26: both say `"kind":"tool_failed"`.
        "tool_failed" => (
            Level::Warn,
            format!("{tool} failed: {}", detail_of("detail")),
        ),
        // Two kinds the fork's reader did not know, because its `agent.rs`
        // predates them. A panic in a tool and a tool the user interrupted are
        // both failures the loop carries on from, so they are `WARN` beside
        // `tool_failed` rather than `ERROR`: `ERROR` is reserved for the harness
        // saying it cannot continue.
        "tool_panicked" => (
            Level::Warn,
            format!("{tool} panicked: {}", detail_of("detail")),
        ),
        "tool_cancelled" => (Level::Warn, format!("{tool} cancelled by the user")),
        "tool_fault" => (
            Level::Error,
            format!("{tool} fault: {}", detail_of("detail")),
        ),
        "tool_unknown" => (Level::Warn, format!("no such tool: {tool}")),
        "denied" => (
            Level::Warn,
            format!(
                "{tool} denied by {}: {}",
                text_of(record, "by").unwrap_or_else(|| "?".into()),
                detail_of("reason"),
            ),
        ),
        "kick" => (
            Level::Warn,
            format!(
                "kick #{}: {}",
                num_of(record, "n").unwrap_or(0),
                text_of(record, "why").unwrap_or_default()
            ),
        ),
        "hook" => (
            Level::Info,
            format!("hook: {}", args_summary(record.get("run"))),
        ),
        // Lossy by decision, and the model is told so — a line the reader of an
        // event log wants to see, not one to slip past at INFO.
        "compacted" => (
            Level::Warn,
            format!(
                "compacted {} goals, {} messages dropped ({} → {} tokens)",
                num_of(record, "goals").unwrap_or(0),
                num_of(record, "drop_messages").unwrap_or(0),
                num_of(record, "estimated_before").unwrap_or(0),
                num_of(record, "estimated_after").unwrap_or(0),
            ),
        ),
        "cleared" => (
            Level::Info,
            format!(
                "cleared {} goals, {} messages",
                num_of(record, "goals").unwrap_or(0),
                num_of(record, "messages").unwrap_or(0)
            ),
        ),
        "model_changed" => (
            Level::Info,
            format!(
                "model {} → {}",
                text_of(record, "from").unwrap_or_default(),
                text_of(record, "to").unwrap_or_default()
            ),
        ),
        "delegation" => (
            Level::Info,
            format!(
                "delegation to {} ended {} ({} tokens, {} calls)",
                text_of(record, "agent").unwrap_or_else(|| "?".into()),
                text_of(record, "ending").unwrap_or_else(|| "?".into()),
                num_of(record, "cost_tokens").unwrap_or(0),
                num_of(record, "tool_calls").unwrap_or(0),
            ),
        ),
        "goal_finished" => {
            let ending = text_of(record, "ending").unwrap_or_else(|| "?".into());
            let level = match ending.as_str() {
                "done" | "answered" => Level::Info,
                "provider_error" => Level::Error,
                _ => Level::Warn,
            };
            let detail = text_of(record, "detail")
                .map(|d| format!(" — {}", one_line(&d, ARGS_WIDTH)))
                .unwrap_or_default();
            (
                level,
                format!(
                    "goal {ending} after {} iterations, {} tokens, {}ms{detail}",
                    num_of(record, "iterations").unwrap_or(0),
                    num_of(record, "tokens").unwrap_or(0),
                    num_of(record, "elapsed_ms").unwrap_or(0),
                ),
            )
        }
        // A kind this build does not know is still a line. A reader who can see
        // the name can grep the file; a reader shown nothing cannot.
        other => (Level::Info, other.to_string()),
    };
    let line = match (is_sub, text_of(record, "agent")) {
        (true, Some(agent)) => format!("[{agent}] {body}"),
        (true, None) => format!("[sub] {body}"),
        _ => body,
    };
    EventLine { at_ms, level, line }
}

// endregion: Events, rendered

// region: The feeds

/// Every run in every session file under `dir`, most recent first.
///
/// `now_ms` is a parameter and not a call to the clock, because "is it running"
/// is decided against it and a test that cannot say what time it is cannot pin
/// that decision.
pub fn runs(dir: &Path, now_ms: u64) -> Result<RunsFeed> {
    let mut feed = RunsFeed::default();
    for path in session_files(dir)? {
        match SessionLog::read_reporting(&path) {
            Ok((records, lost)) => {
                if !lost.is_empty() {
                    feed.damage.push(FileDamage {
                        path: path.clone(),
                        lines: lost,
                    });
                }
                feed.runs
                    .extend(rows_from_records(&session_id_of(&path), &records, now_ms));
            }
            Err(e) => feed.unreadable.push(Unreadable {
                path,
                problem: format!("{e:#}"),
            }),
        }
    }
    // Newest first, with the id breaking a tie so the order is total. Two goals
    // can genuinely share a start millisecond — `at_ms` is milliseconds and a
    // scripted run can open two inside one.
    //
    // **The tie-break is belt-and-braces and no test in this file can see it**,
    // which is worth writing down rather than leaving as an implied claim.
    // Deleting it leaves the suite green, because two other facts already make
    // the order total: `sort_by` is a stable sort, and `session_files` sorts the
    // paths, so rows already arrive in id order. It is kept because both of
    // those are somebody else's decision to change — a switch to
    // `sort_unstable_by` for speed, or a directory read that stops sorting —
    // and either would silently reintroduce a page that reorders itself between
    // two reads of an unchanged directory. What would defend it: a test that
    // could feed `runs` rows in an order the file list did not produce, which
    // means making `rows_from_records` or the file ordering injectable. That is
    // a seam this module does not have and does not otherwise need.
    feed.runs.sort_by(|a, b| {
        b.started_ms
            .cmp(&a.started_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(feed)
}

/// Which records in a file belong to one run.
///
/// A goal owns everything from its `goal` record until its `goal_finished` or
/// the next `goal`, its delegations included: they happened inside it and a
/// detail page's stream is the run as it was lived. A delegation owns the
/// records stamped with its `sub_id`.
fn records_of<'a>(records: &'a [Value], id: &str, session: &str) -> Vec<&'a Value> {
    if let Some(ordinal) = id
        .strip_prefix(session)
        .and_then(|rest| rest.strip_prefix('#'))
        .and_then(|n| n.parse::<usize>().ok())
    {
        let mut seen = 0usize;
        let mut mine = Vec::new();
        for record in records {
            let (kind, is_sub) = kind_of(record);
            if kind == "goal" && !is_sub {
                seen += 1;
                if seen > ordinal {
                    break;
                }
            }
            if seen == ordinal {
                mine.push(record);
            }
        }
        return mine;
    }
    records
        .iter()
        .filter(|r| text_of(r, "sub_id").as_deref() == Some(id))
        .collect()
}

/// The file holding `id`, its records, and what could not be read of it.
///
/// **A file that could not be opened is skipped rather than reported here.**
/// `detail` and `trace` answer "is this run there", and a tri-state return for
/// a case the caller can already see in [`runs`]'s `unreadable` list would put
/// the same fact in two places. The cost is that a run inside an unreadable
/// file looks, to these two functions alone, exactly like a run that never
/// existed — which is why a page must show [`RunsFeed::unreadable`] whatever
/// else it shows.
fn locate_run(dir: &Path, id: &str, now_ms: u64) -> Result<Option<Located>> {
    for path in session_files(dir)? {
        let session = session_id_of(&path);
        let Ok((records, lost)) = SessionLog::read_reporting(&path) else {
            continue;
        };
        let Some(row) = rows_from_records(&session, &records, now_ms)
            .into_iter()
            .find(|r| r.id == id)
        else {
            continue;
        };
        let records = records_of(&records, id, &session)
            .into_iter()
            .cloned()
            .collect();
        return Ok(Some(Located {
            row,
            records,
            damaged_lines: lost,
        }));
    }
    Ok(None)
}

/// One run found in one file: its row, its own records, and the whole file's
/// unreadable lines.
struct Located {
    row: RunRow,
    records: Vec<Value>,
    damaged_lines: Vec<usize>,
}

/// One run's totals, calls and events, or `None` when no file holds that id.
pub fn detail(dir: &Path, id: &str, now_ms: u64) -> Result<Option<RunDetail>> {
    Ok(locate_run(dir, id, now_ms)?.map(|l| build_detail(l.row, &l.records, l.damaged_lines)))
}

/// A run's records into a detail page's three panels.
///
/// **A delegation's records are in the stream and out of the totals.** They are
/// tagged with the agent that wrote them, so the event log shows the whole run;
/// they are left out of the parent's token totals and tool call list because a
/// subagent charges the parent's meter already — counting its `model_call`
/// records here would bill the same tokens twice — and because a TOOL CALLS
/// card listing calls this run did not make is a card that lies.
fn build_detail(row: RunRow, records: &[Value], damaged_lines: Vec<usize>) -> RunDetail {
    let own = row.kind == RunKind::Delegation;
    let mut tokens = TokenTotals::default();
    let mut calls: Vec<ToolCallRow> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut kicks = None;
    let mut ending_detail = None;
    let mut events = Vec::new();

    for record in records {
        let (kind, is_sub) = kind_of(record);
        events.push(render(record));
        // `own` is what keeps a delegation's own records in its own totals and
        // out of its parent's: for a sub run every record is `sub.`-prefixed.
        if is_sub != own {
            continue;
        }
        let at = num_of(record, "at_ms").unwrap_or(0);
        match kind {
            "model_call" => add_call_tokens(&mut tokens, record),
            "tool_call" => {
                let id = text_of(record, "id").unwrap_or_default();
                by_id.insert(id.clone(), calls.len());
                calls.push(ToolCallRow {
                    id,
                    tool: text_of(record, "tool").unwrap_or_else(|| "?".into()),
                    args: args_summary(record.get("args")),
                    at_ms: at,
                    // Until an answer turns up. A call the file never answers
                    // stays this way, which is what an interrupted run looks
                    // like and must not be dressed up as a success.
                    status: CallStatus::Unanswered,
                    detail: None,
                    elapsed_ms: None,
                })
            }
            kind if is_answer(kind) => {
                let Some(i) = text_of(record, "id").and_then(|id| by_id.get(&id).copied()) else {
                    continue;
                };
                answer_call(&mut calls[i], kind, record, at);
            }
            "goal_finished" => {
                kicks = num_of(record, "kicks");
                ending_detail = text_of(record, "detail");
            }
            _ => {}
        }
    }

    RunDetail {
        row,
        tokens,
        calls,
        events,
        kicks,
        ending_detail,
        damaged_lines,
    }
}

/// The record kinds that can answer a `tool_call`.
///
/// One list rather than two `match` arms in two builders, because the two
/// readers disagreeing about whether `tool_panicked` answers a call is exactly
/// the "one input shape, two answers" defect `CLAUDE.md` names.
fn is_answer(kind: &str) -> bool {
    matches!(
        kind,
        "tool_result"
            | "tool_failed"
            | "tool_fault"
            | "tool_panicked"
            | "tool_cancelled"
            | "denied"
            | "tool_unknown"
    )
}

/// Apply one answering record to the call it answers.
fn answer_call(call: &mut ToolCallRow, kind: &str, record: &Value, at: u64) {
    call.elapsed_ms = Some(at.saturating_sub(call.at_ms));
    match kind {
        "tool_result" => {
            if record["block"]["is_error"].as_bool().unwrap_or(false) {
                call.status = CallStatus::Error;
            } else if call.status == CallStatus::Unanswered {
                call.status = CallStatus::Ok;
            }
        }
        "denied" => {
            call.status = CallStatus::Denied;
            call.detail = text_of(record, "reason");
        }
        "tool_unknown" => {
            call.status = CallStatus::Error;
            call.detail = Some("no such tool".into());
        }
        "tool_cancelled" => {
            call.status = CallStatus::Error;
            // The record carries no detail — `agent.rs` writes the sentence to
            // the terminal and the model, not to the log.
            call.detail = Some("cancelled by the user".into());
        }
        // `tool_failed`, `tool_panicked` and `tool_fault`. Only `detail` is
        // readable: see the note in `render` on the failure class the record
        // loses before it reaches disk.
        _ => {
            call.status = CallStatus::Error;
            call.detail = text_of(record, "detail");
        }
    }
}

/// One `model_call` record's usage, added to a run's totals.
fn add_call_tokens(tokens: &mut TokenTotals, record: &Value) {
    tokens.calls += 1;
    tokens.input += num_of(record, "input_tokens").unwrap_or(0);
    tokens.output += num_of(record, "output_tokens").unwrap_or(0);
    tokens.cache_creation += num_of(record, "cache_creation_input_tokens").unwrap_or(0);
    tokens.cache_read += num_of(record, "cache_read_input_tokens").unwrap_or(0);
    tokens.billable += num_of(record, "billable_total_tokens").unwrap_or(0);
}

/// One run as an ordered walk of its own records, or `None` when no file
/// holds that id.
///
/// The sibling of [`detail`], reading the same records through the same
/// slicing, and differing only in what it keeps: order. A delegation's records
/// are left out of a parent's steps for the reason [`build_detail`] documents.
pub fn trace(dir: &Path, id: &str, now_ms: u64) -> Result<Option<RunTrace>> {
    Ok(locate_run(dir, id, now_ms)?.map(|l| build_trace(l.row, &l.records, l.damaged_lines)))
}

/// A run's records into an ordered walk. The call bookkeeping is
/// [`answer_call`]'s, the same function [`build_detail`] uses, so the two
/// readers can never disagree about what a call's status was.
fn build_trace(row: RunRow, records: &[Value], damaged_lines: Vec<usize>) -> RunTrace {
    let own = row.kind == RunKind::Delegation;
    let mut tokens = TokenTotals::default();
    let mut steps: Vec<Step> = Vec::new();
    let mut denials: Vec<Denial> = Vec::new();
    // Call id to its position in `steps`, so an answer finds its call.
    let mut by_id: HashMap<String, usize> = HashMap::new();

    for record in records {
        let (kind, is_sub) = kind_of(record);
        if is_sub != own {
            continue;
        }
        let at = num_of(record, "at_ms").unwrap_or(0);
        match kind {
            "model_call" => {
                add_call_tokens(&mut tokens, record);
                steps.push(Step::Turn(TurnRow {
                    iteration: num_of(record, "iteration").unwrap_or(0),
                    at_ms: at,
                    stop_reason: text_of(record, "stop_reason"),
                    input_tokens: num_of(record, "input_tokens").unwrap_or(0),
                    output_tokens: num_of(record, "output_tokens").unwrap_or(0),
                }));
            }
            "tool_call" => {
                let id = text_of(record, "id").unwrap_or_default();
                by_id.insert(id.clone(), steps.len());
                steps.push(Step::Call(ToolCallRow {
                    id,
                    tool: text_of(record, "tool").unwrap_or_else(|| "?".into()),
                    args: args_summary(record.get("args")),
                    at_ms: at,
                    status: CallStatus::Unanswered,
                    detail: None,
                    elapsed_ms: None,
                }));
            }
            kind if is_answer(kind) => {
                let id = text_of(record, "id").unwrap_or_default();
                let named = by_id.get(&id).and_then(|&i| match &steps[i] {
                    Step::Call(c) => Some(c.tool.clone()),
                    _ => None,
                });
                if kind == "denied" {
                    denials.push(Denial {
                        at_ms: at,
                        // A denial whose call is not in this slice keeps the id
                        // rather than inventing a name.
                        tool: named.unwrap_or_else(|| id.clone()),
                        by: text_of(record, "by").unwrap_or_else(|| "?".into()),
                        reason: text_of(record, "reason").unwrap_or_default(),
                    });
                }
                let Some(&i) = by_id.get(&id) else { continue };
                let Step::Call(call) = &mut steps[i] else {
                    continue;
                };
                answer_call(call, kind, record, at);
            }
            _ => {}
        }
    }

    RunTrace {
        row,
        steps,
        tokens,
        denials,
        damaged_lines,
    }
}

/// The last `n` records of one session file, rendered.
pub fn events(path: &Path, n: usize) -> Result<EventFeed> {
    let (records, damaged_lines) = SessionLog::read_reporting(path)?;
    let older = records.len().saturating_sub(n);
    Ok(EventFeed {
        lines: records[older..].iter().map(render).collect(),
        damaged_lines,
        older,
    })
}

/// The approval posture, plus what this session's log says the gate has done.
///
/// `gate` is a parameter rather than a global read. The fork asked a
/// `publish_mode` static that this tree does not have, and adding one would
/// make a read-only derivation depend on a process-wide cell written by the
/// gate — a second source of truth for something the caller is already holding.
///
/// `session` is the running session's own log file, or `None` when there is not
/// one — in which case the posture still answers and the history is empty,
/// because a run with no transcript still has a gate.
pub fn gates(gate: Gate, session: Option<&Path>) -> Result<GatesView> {
    let mut view = GatesView {
        mode_label: gate_label(gate),
        mode_about: gate_about(gate),
        // See the field's own doc: there is nothing to read.
        pending: None,
        denials: Vec::new(),
        calls: 0,
        damaged_lines: Vec::new(),
    };
    let Some(path) = session else {
        return Ok(view);
    };
    let (records, lost) = SessionLog::read_reporting(path)?;
    view.damaged_lines = lost;
    // A `denied` record names the call id and not the tool, so the name comes
    // from the `tool_call` it answers. The pairing is the same `id` the loop
    // uses to pair a result with its call, and a denial whose call is not in
    // this file — a log that starts mid-run — keeps the id rather than
    // inventing a name.
    let mut tool_of: HashMap<String, String> = HashMap::new();
    for record in &records {
        match kind_of(record).0 {
            "tool_call" => {
                view.calls += 1;
                if let (Some(id), Some(tool)) = (text_of(record, "id"), text_of(record, "tool")) {
                    tool_of.insert(id, tool);
                }
            }
            "denied" => view.denials.push(Denial {
                at_ms: num_of(record, "at_ms").unwrap_or(0),
                tool: text_of(record, "id")
                    .and_then(|id| tool_of.get(&id).cloned())
                    .or_else(|| text_of(record, "id"))
                    .unwrap_or_else(|| "?".into()),
                by: text_of(record, "by").unwrap_or_else(|| "?".into()),
                reason: text_of(record, "reason").unwrap_or_default(),
            }),
            _ => {}
        }
    }
    view.denials.reverse();
    Ok(view)
}

// endregion: The feeds

// region: Sessions

/// How many sessions a sidebar's SESSIONS section can show.
///
/// The pane is 28 to 40 columns wide and shares its height with other sections,
/// so the list is a recent-work shortlist and not a history. Eight is what fits
/// above the fold on an 80x24 terminal with two other sections intact; the
/// shell applies it, so a wider caller can ask for more.
pub const SIDEBAR_SESSIONS: usize = 8;

/// How much of a session id stands in for a name it does not have.
///
/// The front of an id is `sess-` and the leading digits of a millisecond, which
/// every session on the machine shares. The last twelve characters are the tail
/// of that millisecond and the pid, which is the half that tells two sessions
/// apart — and a sidebar truncates from the right, so a name whose front is
/// identical everywhere would render as twelve identical rows.
const ID_TAIL: usize = 12;

/// One session, summarised for a sidebar's SESSIONS list.
///
/// A session file holds many goals; this is the file, not the goal. That is the
/// difference from [`RunRow`], and it is why the two coexist: a harness page
/// asks "what ran", the sidebar asks "where was I".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// The file's name without `.jsonl` — what `--resume` takes.
    pub id: String,
    /// The latest goal's first line, cut at [`NAME_WIDTH`], falling back to
    /// the tail of the id when no goal in the file carried any text.
    pub name: String,
    /// The first goal's start.
    pub started_ms: u64,
    /// The last record belonging to any goal in the file — what the row's time
    /// column shows, because "where was I" is answered by when a session was
    /// last touched and not by when it opened.
    ///
    /// **Not quite the last record in the file**, which is what the fork's doc
    /// claimed: a record written before the first `goal` or after the last one
    /// ended — a `hook`, a `cleared`, a `model_changed` between goals — belongs
    /// to no goal and does not move this. The difference is seconds and the
    /// alternative is a second walk of the records; it is written down because
    /// a doc that says "the last record" and means "nearly" is the kind of
    /// small false claim this file is otherwise careful about.
    pub last_event_ms: u64,
    /// How many goals the file holds.
    pub goals: usize,
    /// The latest goal's status, so a session left mid-run can say so.
    pub status: RunStatus,
}

/// The sessions that ran in one directory, and what could not be read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionsFeed {
    /// Most recently touched first.
    pub sessions: Vec<SessionRow>,
    pub damage: Vec<FileDamage>,
    pub unreadable: Vec<Unreadable>,
}

impl SessionsFeed {
    /// How many lines this feed could not read, across every file it opened.
    pub fn skipped_lines(&self) -> usize {
        self.damage.iter().map(|d| d.lines.len()).sum()
    }
}

fn id_tail(id: &str) -> String {
    let count = id.chars().count();
    id.chars().skip(count.saturating_sub(ID_TAIL)).collect()
}

/// Every session under `dir` that has a goal recorded in `cwd`, most recently
/// touched first.
///
/// **Per repository, by the goal's own record.** A session is a file, and a
/// file can wander: `emma` resumed in another checkout appends to the same log.
/// One matching goal is enough to put the session in this repository's list,
/// because it *was* here — and the row still names the session's latest goal,
/// whichever directory that one ran in, since the row names the session.
///
/// **The match is [`recorded_in`]'s, not this module's.** The fork compared the
/// two paths without canonicalising, arguing that a sidebar built at startup
/// must not stat every path a year of sessions recorded. This tree already
/// decided the other way and made the answer public for exactly this caller:
/// canonicalisation *is* the whole of the answer on a machine with a symlinked
/// home or an 8.3 short name, and two comparisons that disagree about the same
/// session is worse than a few hundred `stat` calls. The fork's cost is real
/// and is the thing to measure if this ever shows up in startup time.
///
/// `now_ms` is a parameter for the reason it is one on [`runs`]: the status a
/// row carries is decided against it.
pub fn sessions_for(dir: &Path, cwd: &Path, now_ms: u64) -> Result<SessionsFeed> {
    let mut feed = SessionsFeed::default();
    for path in session_files(dir)? {
        let (records, lost) = match SessionLog::read_reporting(&path) {
            Ok(pair) => pair,
            Err(e) => {
                feed.unreadable.push(Unreadable {
                    path,
                    problem: format!("{e:#}"),
                });
                continue;
            }
        };
        if !lost.is_empty() {
            feed.damage.push(FileDamage {
                path: path.clone(),
                lines: lost,
            });
        }
        let id = session_id_of(&path);
        let goals: Vec<RunRow> = rows_from_records(&id, &records, now_ms)
            .into_iter()
            .filter(|r| r.kind == RunKind::Goal)
            .collect();
        let Some(latest) = goals.last() else { continue };
        let here = records.iter().any(|r| {
            let (kind, is_sub) = kind_of(r);
            kind == "goal" && !is_sub && recorded_in(r, cwd)
        });
        if !here {
            continue;
        }
        let name = if latest.name.is_empty() {
            id_tail(&id)
        } else {
            latest.name.clone()
        };
        feed.sessions.push(SessionRow {
            started_ms: goals.first().map_or(latest.started_ms, |g| g.started_ms),
            last_event_ms: goals.iter().map(|g| g.last_event_ms).max().unwrap_or(0),
            goals: goals.len(),
            status: latest.status,
            name,
            id,
        });
    }
    // The id breaks a tie. Undefended for the same reason [`runs`]'s is, and
    // kept for the same reason — see the note there.
    feed.sessions.sort_by(|a, b| {
        b.last_event_ms
            .cmp(&a.last_event_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(feed)
}

// endregion: Sessions

// region: The relative time column

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since the Unix epoch back into a proleptic Gregorian `(year, month,
/// day)`.
///
/// Howard Hinnant's `civil_from_days`, which is the same algorithm the fork
/// reached for in its `memory::clock`. It is duplicated here rather than shared
/// because `memory/` is a port in flight in a different worker's lane, and a
/// calendar conversion is not worth coupling this module's ability to compile
/// to another module's shape. **If both land, one of these two goes** — a
/// second answer to one input shape is the defect `CLAUDE.md` names, and thirty
/// lines of arithmetic is not a reason to keep two.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shifted so the era starts on 0000-03-01, which is what makes the leap
    // rule a division rather than a table.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March being 0
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = yoe as i64 + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// A sidebar's right-hand column: `12:42` today, `Yesterday` yesterday, and
/// `May 18` before that.
///
/// The three shapes are a resolution ladder rather than a format: within today
/// the useful fact is the hour, past that it is which day, and past that it is
/// which date. A year is appended only when the date is not in the current
/// year, because `May 18` two years running is two rows that read as the same
/// day.
///
/// `offset_secs` is east-of-UTC seconds, applied to both instants: the day
/// boundary is a local one, and a session that started at 23:42 was yesterday
/// evening to the person reading the pane, whatever UTC calls it. It is one
/// offset rather than one per instant, so a session recorded across a DST
/// change can name the wrong side of midnight by an hour — the alternative is a
/// timezone database, which this workspace does not have.
///
/// A timestamp in the future — a clock that was corrected backwards, a file
/// copied from another machine — reads as a time of day rather than a date,
/// because a sidebar dated next week is a bug report and `14:20` is not.
pub fn relative_time(at_ms: u64, now_ms: u64, offset_secs: i64) -> String {
    let local = |ms: u64| (ms / 1_000) as i64 + offset_secs;
    let at = local(at_ms);
    let now = local(now_ms);
    let (day, today) = (at.div_euclid(86_400), now.div_euclid(86_400));
    if day >= today {
        let secs = at.rem_euclid(86_400);
        return format!("{:02}:{:02}", secs / 3_600, (secs % 3_600) / 60);
    }
    if day == today - 1 {
        return "Yesterday".to_string();
    }
    let (year, month, dom) = civil_from_days(day);
    let name = MONTHS[(month as usize).clamp(1, 12) - 1];
    let (this_year, ..) = civil_from_days(today);
    if year == this_year {
        format!("{name} {dom}")
    } else {
        format!("{name} {dom} {year}")
    }
}

/// Seconds east of UTC on this machine, right now.
///
/// **Local time, deliberately.** Everything else Emma writes down is UTC
/// because it goes in a file somebody else may read; this is a pane on the
/// reader's own screen, where `12:42` must mean the clock on their wall.
///
/// Zero on non-Unix, which means this column's times are UTC there. Windows has
/// an answer and it is a different syscall; the port was done on Windows and
/// this branch was therefore never executed, so it is carried across from the
/// fork unverified — `libc::localtime_r` is a unix-only dependency of this
/// crate (`Cargo.toml`, `[target.'cfg(unix)'.dependencies]`), so nothing on
/// this machine even compiles it. What would settle it: `cargo test -p emma`
/// on a unix box with `TZ` set to something that is not UTC.
pub fn local_offset_secs() -> i64 {
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // SAFETY: `localtime_r` writes into a `tm` this call owns and reads
        // only the `time_t` it is handed — the same shape as `term.rs`'s
        // `ioctl` block. A null return means the C library could not answer,
        // and UTC is the honest fallback.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t = now as libc::time_t;
            if libc::localtime_r(&t, &mut tm).is_null() {
                return 0;
            }
            tm.tm_gmtoff as i64
        }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

// endregion: The relative time column
