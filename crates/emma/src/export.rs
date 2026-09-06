//! Training material out of session transcripts: one JSONL record per
//! assistant turn, with the thinking kept as its own field.
//!
//! **The session log is not changed and is never written to.** This module
//! only reads `~/.emma/sessions/*.jsonl` and writes somewhere else. The owner's
//! request was to organise what is already recorded, not to move it, so the
//! transcript stays the audit trail it was and this is a projection of it.
//!
//! # The schema, `emma.training.v1`
//!
//! One JSON object per line, one line per assistant turn, ordered as the
//! session recorded them:
//!
//! ```json
//! {
//!   "schema": "emma.training.v1",
//!   "session_id": "sess-1787791338091-15534",
//!   "turn_id": "turn-7",
//!   "at_ms": 1787791401234,
//!   "goal_index": 0,
//!   "goal": "find where compaction happens",
//!   "goal_ending": "done",
//!   "iteration": 3,
//!   "model": "qwen3-coder:30b",
//!   "instructions_hash": "9f0c…",
//!   "tool_schema_hash": "1a2b…",
//!   "context": "per_turn",
//!   "messages": [{"role": "user", "content": "…"}],
//!   "thinking": "The user wants me to…",
//!   "thinking_recorded": true,
//!   "thinking_signed": false,
//!   "output": "I'll investigate the repository structure.",
//!   "tool_calls": [{"id": "call-1", "name": "Bash", "arguments": {"command": "ls"}}],
//!   "tool_results": [{"tool_use_id": "call-1", "tool": "Bash", "content": "…",
//!                     "is_error": false, "truncated": false, "shed": false}],
//!   "stop_reason": "tool_use",
//!   "usage": {"input_tokens": 8123, "output_tokens": 402, "web_search_requests": 0}
//! }
//! ```
//!
//! `messages` is the ordinary chat format (`role` plus `content`, where content
//! is a string or an array of typed blocks), so a supervised fine-tuning
//! pipeline reads it with no transformation: `messages` is the prompt,
//! `thinking` plus `output` plus `tool_calls` is the completion. Tool results
//! travel inside a user message as `tool_result` blocks, which is the shape the
//! Messages API uses and the shape the run actually sent.
//!
//! **Four fields are honesty, not decoration.**
//!
//! - `context` names the fidelity of `messages`, and it has three values.
//!   `per_turn` means the list is the one that turn's request carried, rebuilt
//!   by [`session::fold_prefixes`] (the same fold `--resume` spends), so a
//!   compaction or a shed that had happened by then is applied and a later one
//!   is not. `per_turn_shed` is that same guarantee over a conversation a
//!   [`shed`](crate::session) had already emptied: the list is faithfully what
//!   was sent, and what was sent no longer holds the file contents and command
//!   outputs it names, which is a fact a trainer wants before it learns from
//!   the turn. `partial` means the fold **refused a damaged record** somewhere
//!   in this turn's prefix, so the list may not be what was sent and nothing
//!   here can tell how far it is off. The field's whole reason is that a build
//!   which cannot reconstruct a turn says so in the record rather than by
//!   silence, and `partial` is that case arriving: mainline's `Fold` reports
//!   what it refused, and an exporter that dropped the report on the floor
//!   would be claiming `per_turn` over a fold that already said it could not.
//! - `thinking_recorded` separates "the model produced no thinking" from "this
//!   log does not carry it". A turn whose `raw_content` holds a thinking block
//!   is `true` even when the block is empty; a turn with no thinking block, and
//!   a turn from a build written before `raw_content` existed, are `false` with
//!   `thinking` null. An empty string is never invented for either.
//! - `thinking_signed` says the block carried a provider signature (the
//!   Anthropic path does, a local Ollama model does not). The signature itself
//!   is not exported: it validates a replay and is worth nothing to a trainer.
//! - `shed`, on a tool result, says the exported `content` is the note the run
//!   put in place of the output rather than the output. **The content exported
//!   is the text that was actually sent**, which is not always the text the
//!   `tool_result` record holds: a `shed` that landed before the next request
//!   means the model never saw the original, so exporting the original would
//!   teach it from a message nobody sent. A shed *after* that request does not
//!   rewrite anything here, because by then the full output had gone out.
//!
//! # What this does not claim
//!
//! A record is one turn of one trajectory. `goal_ending` says how the goal it
//! belongs to finished, which is the only outcome signal here, and it is a
//! label on the whole goal rather than on the turn. Nothing in this file scores
//! a turn, and nothing filters on quality: [`Filter`] selects on the recorded
//! ending and on nothing else.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use emma_llm::{ContentBlock, Message};
use serde::Serialize;
use serde_json::{json, Value};

use crate::session::{self, SessionLog};

/// The `schema` field every record carries. Bumped when a field's meaning
/// changes, so a directory holding two generations is still separable.
pub const SCHEMA: &str = "emma.training.v1";

/// `context` for a turn the fold rebuilt exactly as the request carried it.
const CONTEXT_PER_TURN: &str = "per_turn";

/// `context` for a faithful rebuild of a conversation a `shed` had emptied.
const CONTEXT_PER_TURN_SHED: &str = "per_turn_shed";

/// `context` for a turn whose prefix the fold refused part of. See the module
/// doc: this is the value that exists so silence is never the answer.
const CONTEXT_PARTIAL: &str = "partial";

/// Which goals a record may come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Everything recorded, including goals that stalled, ran out of budget or
    /// were interrupted. **The default, and deliberately so:** a trajectory
    /// that failed is training material too, and a corpus of nothing but
    /// successes cannot teach recovery from anything.
    #[default]
    Any,
    /// Only goals whose `goal_finished` record says `done` or `answered`: the
    /// pair `main.rs` and `emma agents` already treat together as "there is an
    /// answer".
    Finished,
}

impl Filter {
    /// The `--min-ending` word, or an error naming both values it could have
    /// been. It never guesses: a typo that silently meant `any` would export a
    /// corpus the flag says it did not.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "any" => Ok(Self::Any),
            "finished" => Ok(Self::Finished),
            other => bail!("unknown --min-ending `{other}`: it is `any` or `finished`."),
        }
    }

    fn admits(&self, ending: Option<&str>) -> bool {
        match self {
            Self::Any => true,
            Self::Finished => matches!(ending, Some("done") | Some("answered")),
        }
    }
}

/// One assistant turn, as the schema above spells it.
///
/// The fields are the schema and the module doc is their reference; the ones
/// carrying an argument rather than a value are commented where they sit.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Turn {
    pub schema: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub at_ms: u64,
    pub goal_index: usize,
    pub goal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_ending: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u64>,
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub instructions_hash: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tool_schema_hash: String,
    /// How far `messages` can be trusted: `per_turn`, `per_turn_shed` or
    /// `partial`. Never omitted, because an absent fidelity claim reads as the
    /// strongest one.
    pub context: &'static str,
    pub messages: Vec<Message>,
    /// The reasoning, on its own. `None` when the log carries none — see
    /// `thinking_recorded`, which is the field that tells the two apart.
    pub thinking: Option<String>,
    pub thinking_recorded: bool,
    pub thinking_signed: bool,
    /// The visible half. `None`, never `""`, for a turn that was pure tool
    /// calls.
    pub output: Option<String>,
    pub tool_calls: Vec<Value>,
    pub tool_results: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

// region: The projection

/// Every assistant turn in one session's records, in order.
///
/// The session id is taken from the `goal` records rather than from the file
/// name, so a transcript that has been copied or renamed still says which
/// session it is. A file with no goal record falls back to the name it was
/// read under.
pub fn turns(records: &[Value], fallback_id: &str) -> Vec<Turn> {
    // Which goal each record belongs to, and how that goal ended. The ending
    // arrives after every turn it labels, so it takes a first pass.
    let mut goal_of: Vec<usize> = Vec::with_capacity(records.len());
    let mut goals: Vec<(String, String, String, String, String)> = Vec::new();
    let mut endings: Vec<Option<String>> = Vec::new();
    let mut current: usize = usize::MAX;
    for r in records {
        match r["kind"].as_str().unwrap_or_default() {
            "goal" => {
                goals.push((
                    string(r, "text"),
                    string(r, "model"),
                    string(r, "instructions_hash"),
                    string(r, "tool_schema_hash"),
                    string(r, "session_id"),
                ));
                endings.push(None);
                current = goals.len() - 1;
            }
            "goal_finished" => {
                if let Some(slot) = endings.last_mut() {
                    *slot = Some(string(r, "ending"));
                }
            }
            _ => {}
        }
        goal_of.push(current);
    }

    // The per-turn side tables, keyed by the `turn_id` every one of these
    // records already carries. A result keeps the index it was written at,
    // because whether a later `shed` rewrote it before it was sent is a
    // question about position in the file and cannot be asked afterwards.
    let mut results: BTreeMap<&str, Vec<(usize, Value)>> = BTreeMap::new();
    let mut meta: BTreeMap<&str, &Value> = BTreeMap::new();
    let mut sheds: Vec<(usize, BTreeMap<String, String>)> = Vec::new();
    for (i, r) in records.iter().enumerate() {
        if r["kind"] == "shed" {
            sheds.push((i, shed_replacements(r)));
        }
        let Some(turn) = r["turn_id"].as_str() else {
            continue;
        };
        match r["kind"].as_str().unwrap_or_default() {
            "model_call" => {
                meta.insert(turn, r);
            }
            "tool_result" => results.entry(turn).or_default().push((
                i,
                json!({
                    "tool_use_id": r["id"],
                    "tool": r["tool"],
                    "content": r["block"]["content"],
                    "is_error": r["block"]["is_error"].as_bool().unwrap_or(false),
                    "truncated": r["truncated"].as_bool().unwrap_or(false),
                    "shed": false,
                }),
            )),
            _ => {}
        }
    }

    let session_id = goals
        .iter()
        .find_map(|g| (!g.4.is_empty()).then(|| g.4.clone()))
        .unwrap_or_default();
    let session_id = if session_id.is_empty() {
        fallback_id.to_string()
    } else {
        session_id
    };

    // The fold's own verdict on this file, asked once. `None` is the ordinary
    // answer and costs one fold; the search below only runs for a transcript
    // that really is damaged.
    let damaged_from = first_damaged_prefix(records);

    let prefixes = session::fold_prefixes(records);
    let mut out = Vec::new();
    for (n, (index, messages)) in prefixes.iter().cloned().enumerate() {
        let r = &records[index];
        let goal_index = goal_of[index];
        let (goal, model, instructions_hash, tool_schema_hash, _) =
            goals.get(goal_index).cloned().unwrap_or_default();
        let turn_id = string(r, "turn_id");
        let blocks: Vec<ContentBlock> = r["raw_content"]
            .as_array()
            .map(|a| a.iter().cloned().map(ContentBlock::from_value).collect())
            .unwrap_or_default();

        // Null rather than an empty string when nothing was recorded, and an
        // empty string only when an empty block really was. See the module doc.
        let thinking_block = blocks.iter().find_map(|b| match b {
            ContentBlock::Thinking(t) => Some(t),
            _ => None,
        });
        let text: String = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        // The request that carried this turn's results is the next turn's. A
        // shed between the two rewrote them before they were ever sent; a shed
        // after it came too late to change what went out.
        let next_turn = prefixes
            .get(n + 1)
            .map(|(i, _)| *i)
            .unwrap_or(records.len());
        let tool_results = results
            .get(turn_id.as_str())
            .map(|rows| {
                rows.iter()
                    .map(|(at, row)| apply_sheds(row.clone(), &sheds, *at, next_turn))
                    .collect()
            })
            .unwrap_or_default();

        let call = meta.get(turn_id.as_str());
        out.push(Turn {
            schema: SCHEMA,
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            at_ms: r["at_ms"].as_u64().unwrap_or(0),
            goal_index,
            goal,
            goal_ending: endings.get(goal_index).cloned().flatten(),
            iteration: call.and_then(|c| c["iteration"].as_u64()),
            model,
            instructions_hash,
            tool_schema_hash,
            context: fidelity(index, damaged_from, &sheds),
            messages,
            thinking: thinking_block.map(|t| t.thinking.clone()),
            thinking_recorded: thinking_block.is_some(),
            thinking_signed: thinking_block.is_some_and(|t| t.signature.is_some()),
            // `None` rather than `""` for a turn that was pure tool calls, on
            // the same rule the thinking field follows: nothing said and
            // nothing recorded are different facts.
            output: (!text.is_empty()).then_some(text),
            // From the turn's own `tool_use` blocks rather than from the
            // `tool_call` records, because those two disagree in exactly the
            // interesting case: a call the loop refused to dispatch was still
            // a call the model made, and a corpus that hides it teaches
            // nothing about what got refused. The block carries the same id,
            // name and arguments the record would have.
            tool_calls: blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse(c) => Some(json!({
                        "id": c.id, "name": c.name, "arguments": c.input,
                    })),
                    _ => None,
                })
                .collect(),
            tool_results,
            stop_reason: call.and_then(|c| c["stop_reason"].as_str().map(str::to_string)),
            // Read out of the `model_call` record rather than off a `Usage`,
            // because the record is what the run wrote and the struct is what
            // this build happens to have. `web_search_requests` is billed apart
            // from the tokens and is zero on every provider that searches
            // nothing; leaving it out under-reports what a turn cost.
            //
            // A count the record does not carry exports as `null` rather than
            // as `0`, and that is the same rule `thinking` follows: a session
            // written before the field existed did not measure zero searches,
            // it measured nothing. Certified on this box — sessions from
            // 2026-08-23 export `"web_search_requests": null`, ones from
            // 2026-09-05 export `0`.
            usage: call.map(|c| {
                json!({
                    "input_tokens": c["input_tokens"],
                    "output_tokens": c["output_tokens"],
                    "cache_read_input_tokens": c["cache_read_input_tokens"],
                    "cache_creation_input_tokens": c["cache_creation_input_tokens"],
                    "billable_total_tokens": c["billable_total_tokens"],
                    "web_search_requests": c["web_search_requests"],
                })
            }),
        });
    }
    out
}

/// The `results_shed` rows of one `shed` record, as id → replacement text.
fn shed_replacements(r: &Value) -> BTreeMap<String, String> {
    r["results_shed"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| (string(row, "tool_use_id"), string(row, "content")))
                .collect()
        })
        .unwrap_or_default()
}

/// Replace a result's content with the shed note if one landed on it in the
/// window `(written, sent)` — see the module doc's fourth honesty field.
fn apply_sheds(
    mut row: Value,
    sheds: &[(usize, BTreeMap<String, String>)],
    written: usize,
    sent: usize,
) -> Value {
    let id = row["tool_use_id"].as_str().unwrap_or_default().to_string();
    for (at, replacements) in sheds {
        if *at <= written || *at >= sent {
            continue;
        }
        if let Some(content) = replacements.get(&id) {
            row["content"] = json!(content);
            row["shed"] = json!(true);
        }
    }
    row
}

/// The `context` value for the turn at `index`. `partial` wins over
/// `per_turn_shed`: a fold that refused a record cannot promise the shed it
/// replayed was the whole of what happened either.
fn fidelity(
    index: usize,
    damaged_from: Option<usize>,
    sheds: &[(usize, BTreeMap<String, String>)],
) -> &'static str {
    if damaged_from.is_some_and(|k| index >= k) {
        return CONTEXT_PARTIAL;
    }
    if sheds.iter().any(|(at, _)| *at < index) {
        return CONTEXT_PER_TURN_SHED;
    }
    CONTEXT_PER_TURN
}

/// The shortest prefix of `records` the fold refused something in, or `None`.
///
/// A turn at or after that index cannot be claimed as `per_turn`. The fold is
/// asked rather than second-guessed: which records count as damage is
/// `session.rs`'s decision, and a copy of that rule here would be a second
/// implementation of it — the thing `fold_prefixes` exists to avoid.
///
/// **Why a search rather than a fold per turn.** `Fold::damage` is only ever
/// pushed to, so "this prefix contains damage" is monotone in the prefix
/// length and a bisection finds the first one in a handful of folds. The
/// ordinary transcript is undamaged and pays for exactly one.
fn first_damaged_prefix(records: &[Value]) -> Option<usize> {
    if session::fold_records_reporting(records).1.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = (0usize, records.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if session::fold_records_reporting(&records[..mid])
            .1
            .is_empty()
        {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// The JSONL body for one session, or `None` when the filter kept nothing.
///
/// A pure function on purpose: the whole file is built in memory and written in
/// one go, which is what makes a re-export byte-identical rather than nearly so.
pub fn body(records: &[Value], fallback_id: &str, filter: Filter) -> Option<String> {
    let mut text = String::new();
    for turn in turns(records, fallback_id) {
        if !filter.admits(turn.goal_ending.as_deref()) {
            continue;
        }
        let Ok(line) = serde_json::to_string(&turn) else {
            continue;
        };
        text.push_str(&line);
        text.push('\n');
    }
    (!text.is_empty()).then_some(text)
}

// endregion: The projection

// region: Writing

/// Where exports land when nothing else was said: `~/.emma/training`, beside
/// the sessions rather than inside them, because that directory is read-only
/// evidence.
pub fn default_out_dir(home: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("EMMA_TRAINING_DIR") {
        return Some(PathBuf::from(dir));
    }
    home.map(|h| h.join(".emma").join("training"))
}

/// Export one session file into `out_dir`, returning how many records were
/// written. `Ok(0)` means the filter kept nothing, and in that case any earlier
/// export of the same session is removed rather than left to disagree.
///
/// The output path is `<out_dir>/<session file stem>.training.jsonl`, and the
/// file is truncated and rewritten whole. Same input, same bytes, no appends:
/// running this twice cannot double anything.
pub fn export_session(path: &Path, out_dir: &Path, filter: Filter) -> Result<usize> {
    let records = SessionLog::read(path)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let target = out_dir.join(format!("{stem}.training.jsonl"));
    match body(&records, &stem, filter) {
        Some(text) => {
            std::fs::create_dir_all(out_dir)
                .with_context(|| format!("creating {}", out_dir.display()))?;
            std::fs::write(&target, &text)
                .with_context(|| format!("writing {}", target.display()))?;
            Ok(text.lines().count())
        }
        None => {
            let _ = std::fs::remove_file(&target);
            Ok(0)
        }
    }
}

/// Every session in `session_dir`, or the one named, into `out_dir`.
///
/// Nothing leaves this machine: the only thing this touches besides the
/// transcripts it reads is the output directory.
pub fn export(
    session_dir: Option<&Path>,
    out_dir: Option<&Path>,
    session: Option<&str>,
    filter: Filter,
    out: &mut dyn Write,
) -> Result<()> {
    let Some(dir) = session_dir else {
        bail!(
            "no session directory: the home directory could not be determined, so there is \
             nothing recorded to read. Name one with --session-dir."
        );
    };
    let Some(out_dir) = out_dir else {
        bail!(
            "no output directory: the home directory could not be determined. Name one with \
             --out."
        );
    };
    let mut files = session_files(dir)?;
    if let Some(id) = session {
        files.retain(|p| p.file_stem().is_some_and(|s| s == id));
        if files.is_empty() {
            bail!("no session `{id}` in {}", dir.display());
        }
    }
    if files.is_empty() {
        writeln!(out, "no sessions in {}: nothing to export.", dir.display())?;
        return Ok(());
    }

    let mut total = 0usize;
    let mut written = 0usize;
    let mut partial = 0usize;
    for path in &files {
        let n = export_session(path, out_dir, filter)?;
        total += n;
        if n > 0 {
            written += 1;
        }
        // Counted from what was written rather than guessed at: a corpus with
        // records in it that the fold could not vouch for is a thing the person
        // building on it has to be told, and the field alone is no use to
        // somebody who never opens the file.
        partial += partial_records(path, out_dir);
    }
    writeln!(out, "sessions read   {}", files.len())?;
    writeln!(out, "files written   {written}")?;
    writeln!(out, "records         {total}")?;
    writeln!(out, "out             {}", out_dir.display())?;
    if partial > 0 {
        writeln!(
            out,
            "\n{partial} record(s) say `\"context\": \"partial\"`: the fold refused a damaged \
             record in that turn's prefix, so the messages on those lines may not be what the \
             request carried. Everything else is the conversation as it was sent."
        )?;
    }
    if total == 0 {
        writeln!(
            out,
            "\nNothing matched. With --min-ending finished only goals that ended done or \
             answered are kept; the default keeps everything."
        )?;
    }
    Ok(())
}

/// How many lines of one session's export claim less than full fidelity.
///
/// Reads the file just written rather than re-deriving it, so the number and
/// the corpus cannot disagree. Zero when there is nothing to read: this is a
/// report, and a report that fails a run is worse than one that is silent.
fn partial_records(session_path: &Path, out_dir: &Path) -> usize {
    let stem = session_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let target = out_dir.join(format!("{stem}.training.jsonl"));
    let Ok(text) = std::fs::read_to_string(&target) else {
        return 0;
    };
    text.lines()
        .filter(|line| {
            serde_json::from_str::<Value>(line)
                .map(|v| v["context"] == CONTEXT_PARTIAL)
                .unwrap_or(false)
        })
        .count()
}

/// Every `.jsonl` in a session directory, in id order: the same ordering
/// `emma agents` and `session::locate` use, and the reason a sweep is
/// deterministic.
fn session_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// The standing-toggle path: refresh one session's export where it stands.
///
/// Called at the end of a goal when `training_capture` is on, so the material
/// accumulates without anybody running a command. It rewrites the whole file
/// for that session rather than appending, which is the same writer the CLI
/// spends and the reason re-processing a session cannot duplicate a record.
///
/// Silent on failure and returning nothing: a full disk or an unwritable
/// directory must not be the thing that kills a run whose goal already
/// finished. The command reports properly when a person asks it to.
pub fn capture(session_path: &Path, home: Option<&Path>) -> Option<usize> {
    let out_dir = capture_out_dir(session_path, home)?;
    export_session(session_path, &out_dir, Filter::Any).ok()
}

/// Where the live capture writes: a sibling of the store the session was read
/// from, rather than a directory resolved from the process's home.
///
/// For a real run the two are the same answer. The session file is
/// `~/.emma/sessions/<id>.jsonl`, so the sibling is `~/.emma/training`, which
/// is exactly what [`default_out_dir`] returns.
///
/// They differ for a session log that is not in the real store, which is every
/// integration test: an `Agent` driven over a fixture log in a `TempDir` used
/// to write its training file into the developer's own `~/.emma/training`,
/// because the toggle was read from the real home and so was the destination.
/// That left 21 files named after test fixtures in a real corpus, found
/// 2026-08-27. Deriving from the session file removes the whole class instead
/// of teaching each test to clean up after itself: a fixture session in a
/// temporary directory writes inside that directory and goes when it goes.
///
/// `EMMA_TRAINING_DIR` still wins, because an explicit destination is an
/// answer and this is only the default. The home is the last resort, for a
/// session path with no grandparent to be a sibling of.
pub fn capture_out_dir(session_path: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("EMMA_TRAINING_DIR") {
        return Some(PathBuf::from(dir));
    }
    session_path
        .parent()
        .and_then(Path::parent)
        .map(|store| store.join("training"))
        .or_else(|| default_out_dir(home))
}

// endregion: Writing

fn string(r: &Value, key: &str) -> String {
    r[key].as_str().unwrap_or_default().to_string()
}

// region: Tests

#[cfg(test)]
mod tests {
    use super::*;

    /// A session's records, written the way `Agent::run_goal` writes them: a
    /// `goal`, then per turn a `model_call`, an `assistant` carrying
    /// `raw_content`, and a `tool_call`/`tool_result` pair for anything it
    /// asked for.
    fn goal(text: &str) -> Value {
        json!({
            "kind": "goal", "at_ms": 1, "session_id": "sess-test-1",
            "opening": text, "text": text, "model": "qwen3:8b",
            "instructions_hash": "abc123", "tool_schema_hash": "def456",
            "cwd": "C:\\src\\emma",
        })
    }

    fn model_call(turn: &str, iteration: u64, stop: &str) -> Value {
        json!({
            "kind": "model_call", "turn_id": turn, "iteration": iteration,
            "stop_reason": stop, "input_tokens": 10, "output_tokens": 5,
            "billable_total_tokens": 15, "web_search_requests": 0, "at_ms": 2,
        })
    }

    fn assistant(turn: &str, blocks: Value) -> Value {
        json!({ "kind": "assistant", "turn_id": turn, "raw_content": blocks, "at_ms": 3 })
    }

    fn tool_pair(turn: &str, id: &str, tool: &str, result: &str) -> [Value; 2] {
        [
            json!({ "kind": "tool_call", "turn_id": turn, "id": id, "tool": tool,
                    "args": {"command": "ls"}, "at_ms": 4 }),
            json!({ "kind": "tool_result", "turn_id": turn, "id": id, "tool": tool,
                    "truncated": false, "at_ms": 5,
                    "block": {"type": "tool_result", "tool_use_id": id, "content": result} }),
        ]
    }

    fn finished(ending: &str) -> Value {
        json!({ "kind": "goal_finished", "ending": ending, "at_ms": 9 })
    }

    /// One goal: a thinking turn that calls a tool, then a thinking turn that
    /// answers.
    fn session() -> Vec<Value> {
        let [call, result] = tool_pair("t1", "c1", "Bash", "src\ntests");
        vec![
            goal("list the tree"),
            model_call("t1", 1, "tool_use"),
            assistant(
                "t1",
                json!([
                    {"type": "thinking", "thinking": "I should look first."},
                    {"type": "text", "text": "Listing."},
                    {"type": "tool_use", "id": "c1", "name": "Bash", "input": {"command": "ls"}},
                ]),
            ),
            call,
            result,
            model_call("t2", 2, "end_turn"),
            assistant(
                "t2",
                json!([
                    {"type": "thinking", "thinking": "Two directories."},
                    {"type": "text", "text": "src and tests."},
                ]),
            ),
            finished("done"),
        ]
    }

    #[test]
    fn thinking_is_its_own_field_and_never_merged_into_the_output() {
        let turns = turns(&session(), "sess-test-1");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].thinking.as_deref(), Some("I should look first."));
        assert!(turns[0].thinking_recorded);
        assert!(!turns[0].thinking_signed, "a local model signs nothing");
        assert_eq!(turns[0].output.as_deref(), Some("Listing."));
        // The whole point of the field: the visible answer does not carry the
        // reasoning, and the reasoning does not carry the answer.
        assert!(!turns[0].output.as_deref().unwrap().contains("look first"));
        assert!(!turns[0].thinking.as_deref().unwrap().contains("Listing"));
        assert_eq!(turns[0].session_id, "sess-test-1");
        assert_eq!(turns[0].model, "qwen3:8b");
        assert_eq!(turns[0].instructions_hash, "abc123");
        assert_eq!(turns[0].iteration, Some(1));
        assert_eq!(turns[0].goal_ending.as_deref(), Some("done"));
    }

    /// Three different facts, three different records, and the module doc's rule
    /// that an empty string is never invented.
    #[test]
    fn a_turn_with_no_thinking_is_null_and_a_recorded_empty_one_is_not() {
        let records = vec![
            goal("answer"),
            model_call("t1", 1, "end_turn"),
            assistant("t1", json!([{"type": "text", "text": "no reasoning here"}])),
            model_call("t2", 2, "end_turn"),
            assistant(
                "t2",
                json!([{"type": "thinking", "thinking": ""}, {"type": "text", "text": "hm"}]),
            ),
            finished("answered"),
        ];
        let turns = turns(&records, "sess-test-1");
        assert_eq!(turns[0].thinking, None);
        assert!(
            !turns[0].thinking_recorded,
            "nothing recorded is not the same as an empty thought"
        );
        assert_eq!(turns[1].thinking.as_deref(), Some(""));
        assert!(turns[1].thinking_recorded);

        // And the same distinction on the visible half: a turn that was pure
        // tool calls says null rather than "".
        let [call, result] = tool_pair("t3", "c3", "Bash", "ok");
        let records = vec![
            goal("run it"),
            model_call("t3", 1, "tool_use"),
            assistant(
                "t3",
                json!([{"type": "tool_use", "id": "c3", "name": "Bash", "input": {}}]),
            ),
            call,
            result,
        ];
        assert_eq!(super::turns(&records, "s").remove(0).output, None);
    }

    #[test]
    fn tool_calls_and_the_results_that_answered_them_travel_with_the_turn() {
        let turns = turns(&session(), "sess-test-1");
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0]["name"], "Bash");
        assert_eq!(turns[0].tool_calls[0]["arguments"]["command"], "ls");
        assert_eq!(turns[0].tool_results.len(), 1);
        assert_eq!(turns[0].tool_results[0]["tool_use_id"], "c1");
        assert_eq!(turns[0].tool_results[0]["content"], "src\ntests");
        assert_eq!(turns[0].tool_results[0]["is_error"], false);
        assert_eq!(turns[0].tool_results[0]["shed"], false);
        // The answering turn asked for nothing, and says so with empty lists
        // rather than by carrying the previous turn's traffic.
        assert!(turns[1].tool_calls.is_empty());
        assert!(turns[1].tool_results.is_empty());
    }

    /// A call the loop never dispatched is still a call the model made. The
    /// turn is dropped from the *context* of later turns, because an
    /// unanswered `tool_use` cannot be sent, and it is still exported as its
    /// own record with the call on it and no result.
    #[test]
    fn a_call_that_was_never_answered_is_still_exported_as_a_call() {
        let records = vec![
            goal("do the forbidden thing"),
            model_call("t1", 1, "tool_use"),
            assistant(
                "t1",
                json!([
                    {"type": "thinking", "thinking": "I will try."},
                    {"type": "tool_use", "id": "c1", "name": "Shell",
                     "input": {"command": "rm -rf /"}},
                ]),
            ),
            finished("stalled"),
        ];
        let turns = turns(&records, "s");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0]["name"], "Shell");
        assert!(turns[0].tool_results.is_empty());
        assert_eq!(turns[0].goal_ending.as_deref(), Some("stalled"));
    }

    /// The fidelity claim, and the reason `context` is a field: the messages
    /// are what that turn's request carried, not the session's final state.
    #[test]
    fn the_context_is_the_conversation_as_it_stood_before_that_turn() {
        let turns = turns(&session(), "sess-test-1");
        assert_eq!(turns[0].context, "per_turn");
        // Turn one saw only the goal.
        assert_eq!(turns[0].messages.len(), 1);
        assert_eq!(turns[0].messages[0].role, emma_llm::Role::User);
        assert_eq!(
            turns[0].messages[0].content.to_string(),
            "\"list the tree\""
        );
        // Turn two saw the goal, turn one, and the result that answered it.
        assert_eq!(turns[1].messages.len(), 3);
        assert_eq!(turns[1].messages[1].role, emma_llm::Role::Assistant);
        assert_eq!(turns[1].messages[2].role, emma_llm::Role::User);
        assert!(turns[1].messages[2].content.to_string().contains("tests"));
        // And the thinking is in the context of the *next* turn, as the
        // provider received it, while still being its own field on its own
        // record. Both, on purpose.
        assert!(turns[1].messages[1]
            .content
            .to_string()
            .contains("look first"));
    }

    /// A compacted session exports what the run sent, because the fold replays
    /// the `compacted` record rather than re-deriving a summary.
    #[test]
    fn a_compacted_session_exports_the_summary_the_run_actually_sent() {
        // The order a real run writes: the next goal opens, the goal before it
        // moves into history, and compaction rewrites the front of history
        // before the next model call.
        let mut records = session();
        records.push(goal("now count them"));
        records.push(json!({
            "kind": "compacted", "at_ms": 10, "drop_messages": 4,
            "messages": [{"role": "user", "content": "earlier: listed the tree"}],
            "why": "context",
        }));
        records.push(model_call("t3", 1, "end_turn"));
        records.push(assistant(
            "t3",
            json!([{"type": "thinking", "thinking": "Two."}, {"type": "text", "text": "Two."}]),
        ));
        records.push(finished("answered"));

        let turns = turns(&records, "sess-test-1");
        assert_eq!(turns.len(), 3);
        let context = turns[2]
            .messages
            .iter()
            .map(|m| m.content.to_string())
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            context.contains("earlier: listed the tree"),
            "the replacement the run sent is missing: {context}"
        );
        assert!(
            !context.contains("src\\ntests"),
            "the compacted-away tool traffic came back: {context}"
        );
        // The turns from before the compaction still carry their own,
        // uncompacted context, because that is what those requests carried.
        assert!(turns[1]
            .messages
            .iter()
            .any(|m| m.content.to_string().contains("tests")));
        assert_eq!(turns[2].goal_index, 1);
        assert_eq!(turns[2].goal_ending.as_deref(), Some("answered"));
    }

    /// A `compacted` record the fold refuses is the case `context` exists for.
    /// The turns before the damage keep their claim; the ones after it say
    /// `partial` rather than promising a conversation nobody can rebuild.
    #[test]
    fn a_turn_after_a_record_the_fold_refused_says_partial_and_the_ones_before_do_not() {
        let mut records = session();
        records.push(goal("now count them"));
        // `drop_messages` missing: the arm in `session.rs` refuses this and
        // records why, because defaulting it to zero left the fold rebuilding
        // a conversation different from the one that was sent.
        records.push(json!({
            "kind": "compacted", "at_ms": 10,
            "messages": [{"role": "user", "content": "earlier: listed the tree"}],
            "why": "context",
        }));
        records.push(model_call("t3", 1, "end_turn"));
        records.push(assistant("t3", json!([{"type": "text", "text": "Two."}])));
        records.push(finished("answered"));

        let turns = turns(&records, "sess-test-1");
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].context, "per_turn");
        assert_eq!(turns[1].context, "per_turn");
        assert_eq!(
            turns[2].context, "partial",
            "a turn folded over a refused record must not claim per-turn fidelity"
        );

        // And the summary line says it, because a field nobody opens the file
        // to read is not a report.
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        let out = home.path().join("training");
        write_session(&sessions, "sess-test-1", &records);
        let mut sink = Vec::new();
        export(Some(&sessions), Some(&out), None, Filter::Any, &mut sink).unwrap();
        let said = String::from_utf8(sink).unwrap();
        assert!(said.contains("1 record(s) say"), "not reported: {said}");
    }

    /// The shed rule, in both directions. A result the run replaced before the
    /// next request exports as the replacement, because that is what the model
    /// read; a result already sent whole before the shed exports whole.
    #[test]
    fn a_shed_result_exports_as_the_text_that_was_actually_sent() {
        let [call, result] = tool_pair("t1", "c1", "Read", "the whole file, all of it");
        let shed = json!({
            "kind": "shed", "at_ms": 6, "why": "context", "results": 1,
            "results_shed": [{"tool_use_id": "c1",
                              "content": "[result from Read shed to make room]"}],
        });
        let records = vec![
            goal("read it"),
            model_call("t1", 1, "tool_use"),
            assistant(
                "t1",
                json!([{"type": "tool_use", "id": "c1", "name": "Read", "input": {}}]),
            ),
            call,
            result,
            shed,
            model_call("t2", 2, "end_turn"),
            assistant("t2", json!([{"type": "text", "text": "done"}])),
            finished("answered"),
        ];
        let turns = turns(&records, "s");
        assert_eq!(
            turns[0].tool_results[0]["content"], "[result from Read shed to make room]",
            "the corpus carries a message the model was never sent"
        );
        assert_eq!(turns[0].tool_results[0]["shed"], true);
        // The context the *next* turn saw carries the same note, from the fold
        // rather than from here — the two must not disagree.
        assert!(turns[1]
            .messages
            .iter()
            .any(|m| m.content.to_string().contains("shed to make room")));
        assert_eq!(turns[1].context, "per_turn_shed");
        // The turn before the shed record still claims plain per-turn: nothing
        // had been shed out of what it was sent.
        assert_eq!(turns[0].context, "per_turn");

        // The other direction: the same shed, moved after the request that
        // carried the result whole, changes nothing.
        let mut late = records.clone();
        late.remove(5);
        late.insert(
            8,
            json!({
                "kind": "shed", "at_ms": 8, "why": "context", "results": 1,
                "results_shed": [{"tool_use_id": "c1", "content": "[shed]"}],
            }),
        );
        let turns = super::turns(&late, "s");
        assert_eq!(
            turns[0].tool_results[0]["content"],
            "the whole file, all of it"
        );
        assert_eq!(turns[0].tool_results[0]["shed"], false);
    }

    #[test]
    fn the_ending_filter_selects_and_the_default_keeps_everything() {
        let mut records = session();
        records.push(goal("a goal that ran out"));
        records.push(model_call("t9", 1, "end_turn"));
        records.push(assistant("t9", json!([{"type": "text", "text": "half"}])));
        records.push(finished("tokens"));

        let all = body(&records, "s", Filter::Any).unwrap();
        assert_eq!(
            all.lines().count(),
            3,
            "a failed trajectory is material too"
        );
        let kept = body(&records, "s", Filter::Finished).unwrap();
        assert_eq!(kept.lines().count(), 2);
        assert!(!kept.contains("ran out"));
    }

    fn write_session(dir: &Path, id: &str, records: &[Value]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        let mut text = String::new();
        for r in records {
            text.push_str(&r.to_string());
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn exporting_twice_writes_the_same_bytes_and_touches_no_transcript() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        let out = home.path().join("training");
        let path = write_session(&sessions, "sess-test-1", &session());
        let before = std::fs::read(&path).unwrap();

        let mut sink = Vec::new();
        export(Some(&sessions), Some(&out), None, Filter::Any, &mut sink).unwrap();
        let first = std::fs::read(out.join("sess-test-1.training.jsonl")).unwrap();
        let listing = std::fs::read_dir(&out).unwrap().count();
        let mut sink = Vec::new();
        export(Some(&sessions), Some(&out), None, Filter::Any, &mut sink).unwrap();
        let second = std::fs::read(out.join("sess-test-1.training.jsonl")).unwrap();

        assert_eq!(first, second, "a re-export is not byte-identical");
        assert_eq!(
            std::fs::read_dir(&out).unwrap().count(),
            listing,
            "a re-export grew the directory"
        );
        assert_eq!(first.iter().filter(|b| **b == b'\n').count(), 2);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the transcript is read-only evidence and was written to"
        );
    }

    /// The filter emptying a session removes its earlier export rather than
    /// leaving a stale file that disagrees with the flag it was asked for.
    #[test]
    fn a_filter_that_keeps_nothing_leaves_no_stale_export_behind() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        let out = home.path().join("training");
        let mut records = session();
        records.pop();
        records.push(finished("tokens"));
        let path = write_session(&sessions, "sess-test-1", &records);

        assert_eq!(export_session(&path, &out, Filter::Any).unwrap(), 2);
        assert!(out.join("sess-test-1.training.jsonl").exists());
        assert_eq!(export_session(&path, &out, Filter::Finished).unwrap(), 0);
        assert!(!out.join("sess-test-1.training.jsonl").exists());
    }

    #[test]
    fn a_named_session_is_the_only_one_exported_and_a_typo_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        let out = home.path().join("training");
        write_session(&sessions, "sess-a", &session());
        write_session(&sessions, "sess-b", &session());

        let mut sink = Vec::new();
        export(
            Some(&sessions),
            Some(&out),
            Some("sess-a"),
            Filter::Any,
            &mut sink,
        )
        .unwrap();
        assert!(out.join("sess-a.training.jsonl").exists());
        assert!(!out.join("sess-b.training.jsonl").exists());

        let mut sink = Vec::new();
        assert!(export(
            Some(&sessions),
            Some(&out),
            Some("sess-z"),
            Filter::Any,
            &mut sink
        )
        .is_err());
    }

    /// The capture path writes beside the session it read, not beside the
    /// developer's own. See [`capture_out_dir`] for the corpus this polluted.
    #[test]
    fn the_capture_writes_beside_the_session_it_read() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let path = write_session(&sessions, "sess-test-1", &session());
        // A home that is not this session's grandparent, so a capture that
        // resolved from the home would land in the wrong tree and be seen.
        let elsewhere = tempfile::tempdir().unwrap();

        assert_eq!(capture(&path, Some(elsewhere.path())), Some(2));
        assert!(tmp
            .path()
            .join("training")
            .join("sess-test-1.training.jsonl")
            .exists());
        assert!(!elsewhere.path().join(".emma").join("training").exists());
    }

    #[test]
    fn the_ending_flag_names_its_own_values_rather_than_guessing() {
        assert_eq!(Filter::parse("any").unwrap(), Filter::Any);
        assert_eq!(Filter::parse("finished").unwrap(), Filter::Finished);
        assert!(Filter::parse("done").is_err());
        assert_eq!(Filter::default(), Filter::Any);
    }

    /// A record is one JSON object with the fields the doc comment promises,
    /// so a pipeline reading the schema off the doc is reading the truth.
    #[test]
    fn a_record_carries_every_field_the_schema_documents() {
        let text = body(&session(), "sess-test-1", Filter::Any).unwrap();
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        for key in [
            "schema",
            "session_id",
            "turn_id",
            "at_ms",
            "goal_index",
            "goal",
            "goal_ending",
            "iteration",
            "model",
            "instructions_hash",
            "tool_schema_hash",
            "context",
            "messages",
            "thinking",
            "thinking_recorded",
            "thinking_signed",
            "output",
            "tool_calls",
            "tool_results",
            "stop_reason",
            "usage",
        ] {
            assert!(first.get(key).is_some(), "the schema promises {key}");
        }
        assert_eq!(first["schema"], SCHEMA);
        // The four counts the record carries plus the searches, which are
        // billed apart from the tokens.
        for key in [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "billable_total_tokens",
            "web_search_requests",
        ] {
            assert!(
                first["usage"].get(key).is_some(),
                "the usage object promises {key}"
            );
        }
        assert!(first["tool_results"][0].get("shed").is_some());
    }
}

// endregion: Tests
