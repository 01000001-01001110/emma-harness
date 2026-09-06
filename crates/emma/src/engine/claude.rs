//! The claude engine: one goal, handed whole to the `claude` CLI.
//!
//! When the resolved provider is `claude`, Emma does not run her own agent loop
//! for the goal. She spawns `claude -p <goal>` in the working directory, reads
//! its `stream-json` events, paints them through the same [`Term`] seams the
//! Emma loop uses, records them in the same session file, and returns one
//! [`Outcome`]. One agent loop, Claude's. One permission system, Claude's.
//!
//! The event shapes this parser was written against are the ones in
//! `tests/fixtures/claude-engine/stream.jsonl`. That fixture is **synthetic**,
//! written from the documented `stream-json` shape rather than captured from a
//! session: a real capture carries the machine's paths, its connected servers
//! and its session ids, none of which belong in a repository. What that costs
//! is stated where it matters — see the fixture's own note in `tests.rs` — and
//! the mitigation is that the parser never fails on an unrecognised line.
//!
//! Four properties this module is responsible for, and each has a test:
//!
//! **It never escalates.** Emma passes `--dangerously-skip-permissions` if and
//! only if the user started Emma with `--allow-all`. Otherwise she passes
//! `--permission-mode default` *positively*, so a `defaultMode` in somebody's
//! `.claude/settings.json` cannot widen a run Emma launched. See [`args`].
//!
//! **A blocked run does not look like a finished one.** In `-p` mode Claude
//! Code cannot prompt: a gated tool comes back as a `permission_denied` system
//! event and an errored tool result, and the model then writes prose about a
//! dialog nobody can see. A run that was denied anything ends
//! [`Ending::Stalled`], whatever the CLI's own `stop_reason` says.
//!
//! **An event this build does not model is shown, never dropped.** The stream
//! has no stability contract and is versioned by whatever `claude` is on PATH,
//! so [`Event::Unknown`] carries the line to the screen dim and to the log
//! whole, next to the `claude_code_version` that produced it.
//!
//! **Stopping the child reports what actually happened.** [`Signalling`] is
//! implemented on both platforms with real syscalls, and the notice names the
//! difference: Windows has no way to deliver an interrupt to a child that does
//! not share the console, so the child is ended outright and the user is told
//! the cost report went with it. The alternative — an `exited()` that answers
//! `true` without asking — is the kill-reports-success defect this repository
//! has already paid for once, and it is what the fork this module came from
//! shipped.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::agent::{Ending, Interrupt, Outcome, Spend};
use crate::session::SessionLog;
use crate::term::Term;

/// The provider name that selects this engine. Taken from the registry entry
/// rather than spelled again, so the string that selects it and the string that
/// routes to it cannot drift.
pub const NAME: &str = emma_llm::claude_cli::NAME;

/// Whether a goal running on this engine can take a mid-goal steer.
///
/// **No, and it is not a gap.** A steer is a line folded into the *next*
/// request Emma builds, and under this engine Emma builds no requests: the
/// child owns the conversation from the moment it is spawned. A steer affordance
/// left lit during a handoff would be a control that does nothing, which ruling
/// 4 forbids in as many words.
///
/// The seam this leaves is one line for the caller: the frame's steer
/// affordance is turned off for the duration of a handoff and back on after it.
/// That call does not exist in this tree yet (`Frame::set_steerable` arrives
/// with the steering package), so the constant is here for the wiring to reach
/// for rather than a magic `false` at the call site.
pub const STEERABLE: bool = false;

/// How long a child gets to finish writing after an interrupt before it is
/// killed. The runctl precedent: an interrupt lets the program write its own
/// ending, and a program that will not take the hint is not allowed to hold the
/// session.
const GRACE: Duration = Duration::from_secs(5);

/// How often the grace period is checked. Small enough that an obedient child
/// is reaped promptly, large enough that waiting costs nothing.
const POLL: Duration = Duration::from_millis(50);

// region: The stream, as values

/// One content block inside an `assistant` event.
///
/// `Other` keeps the raw value rather than discarding it, for the same reason
/// [`Event::Unknown`] exists: a block type added later must reach the log and
/// the screen, not the floor.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Prose the model wrote.
    Text(String),
    /// The signature travels with the text and is never dropped. A thinking
    /// block re-sent without its signature is rejected, so a record missing one
    /// is a resumed session that dies on its first call with an error naming a
    /// field nobody in the file mentions.
    Thinking {
        /// The thinking text, whole.
        text: String,
        /// The provider's signature over it, carried verbatim.
        signature: String,
    },
    /// A tool the child called.
    ToolUse {
        /// The `tool_use_id` a later result is attributed by.
        id: String,
        /// The tool's name, as the child's own registry spells it.
        name: String,
        /// The arguments, unmodelled.
        input: Value,
    },
    /// A block type this build does not model, kept whole.
    Other(Value),
}

/// The `system`/`init` event, reduced to the five facts Emma quotes back. The
/// rest of that event is large (every tool name, every skill, every MCP server)
/// and goes to the log rather than the screen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Init {
    /// The child's own session id, so a run can be found in claude's
    /// transcripts as well as in Emma's.
    pub session_id: String,
    /// The model the child resolved, which is not necessarily the one Emma
    /// asked for.
    pub model: String,
    /// The posture the child is running under, as the child reports it.
    pub permission_mode: String,
    /// `claude_code_version`, which is what the parser is versioned against.
    pub version: String,
    /// The working directory the child believes it is in.
    pub cwd: String,
}

/// The single `result` event, which is always last.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Final {
    /// Why the model stopped, in the API's vocabulary.
    pub stop_reason: String,
    /// The CLI's own word for the shape of the ending.
    pub subtype: String,
    /// Whether the CLI itself considered the run an error.
    pub is_error: bool,
    /// What the child says the run cost, when it says.
    pub cost_usd: Option<f64>,
    /// The four token counts, which are exactly Emma's four.
    pub usage: emma_llm::Usage,
}

/// What one line of the stream turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The opening `system`/`init` event.
    Init(Init),
    /// Part of one assistant message. **Part**, not all of it: the CLI splits a
    /// single message across several lines, one per block group, and every line
    /// carries the same `message.id`. Emma buffers by that id (see [`Run`]) so
    /// the record holds one assistant turn where the model produced one, rather
    /// than three consecutive assistant messages, which no API will accept back.
    Assistant {
        /// The `message.id` every line of one message repeats.
        id: String,
        /// The blocks on this line.
        blocks: Vec<Block>,
    },
    /// One `user` event's tool results. `(tool_use_id, content, is_error)`.
    ToolResults(Vec<(String, String, bool)>),
    /// A gated tool in a mode that cannot prompt. The event Emma exists to make
    /// loud.
    PermissionDenied {
        /// The tool the child was refused.
        tool: String,
        /// The call it was refused for.
        tool_use_id: String,
        /// The child's own wording, shown as-is.
        message: String,
    },
    /// The one `result` event.
    Result(Final),
    /// Modelled, understood, and deliberately not shown. Carries its own name so
    /// a test can assert *which* thing was ignored rather than that something
    /// was. `rate_limit_event` is about the account, not the goal;
    /// `thinking_tokens` is a running estimate the `result` event reports
    /// exactly.
    Ignored(&'static str),
    /// Anything else, including a line that is not JSON at all.
    Unknown(String),
}

impl Event {
    /// Parse one line. Never fails: an unparseable line is [`Event::Unknown`],
    /// because a parser that returned an error here would turn a stream format
    /// change into a dead engine instead of a noisy one.
    pub fn parse(line: &str) -> Self {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Self::Unknown(line.trim().to_string());
        };
        // The `result` event has been observed *without* a `type` field on one
        // probe and with it on another, so it is recognised by either. Nothing
        // else in the stream carries `total_cost_usd`.
        let kind = v["type"].as_str().unwrap_or_default();
        if kind == "result" || (kind.is_empty() && v.get("total_cost_usd").is_some()) {
            return Self::Result(final_of(&v));
        }
        match kind {
            "system" => match v["subtype"].as_str().unwrap_or_default() {
                "init" => Self::Init(Init {
                    session_id: str_at(&v, "session_id"),
                    model: str_at(&v, "model"),
                    permission_mode: str_at(&v, "permissionMode"),
                    version: str_at(&v, "claude_code_version"),
                    cwd: str_at(&v, "cwd"),
                }),
                "permission_denied" => Self::PermissionDenied {
                    tool: str_at(&v, "tool_name"),
                    tool_use_id: str_at(&v, "tool_use_id"),
                    message: str_at(&v, "message"),
                },
                "thinking_tokens" => Self::Ignored("thinking_tokens"),
                _ => Self::Unknown(line.trim().to_string()),
            },
            "rate_limit_event" => Self::Ignored("rate_limit_event"),
            "assistant" => Self::Assistant {
                id: str_at(&v["message"], "id"),
                blocks: blocks_of(&v["message"]["content"]),
            },
            // A `user` event in this stream is never a person: it is how the CLI
            // reports the results of the tools it just ran.
            "user" => Self::ToolResults(results_of(&v["message"]["content"])),
            _ => Self::Unknown(line.trim().to_string()),
        }
    }
}

fn str_at(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

fn blocks_of(content: &Value) -> Vec<Block> {
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .map(|b| match b["type"].as_str().unwrap_or_default() {
                    "text" => Block::Text(str_at(b, "text")),
                    "thinking" => Block::Thinking {
                        text: str_at(b, "thinking"),
                        signature: str_at(b, "signature"),
                    },
                    "tool_use" => Block::ToolUse {
                        id: str_at(b, "id"),
                        name: str_at(b, "name"),
                        input: b["input"].clone(),
                    },
                    _ => Block::Other(b.clone()),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn results_of(content: &Value) -> Vec<(String, String, bool)> {
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                // A result block is recognised by carrying an id, not by its
                // `type` tag: the observed stream omits the tag on the first
                // result of a turn and includes it afterwards.
                .filter(|b| b.get("tool_use_id").is_some())
                .map(|b| {
                    (
                        str_at(b, "tool_use_id"),
                        text_of(&b["content"]),
                        b["is_error"].as_bool().unwrap_or(false),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A result's content is a string in the simple case and an array of blocks in
/// the rich one. Both flatten to the prose a transcript row shows.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|i| match i["text"].as_str() {
                Some(t) => t.to_string(),
                None => i.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn final_of(v: &Value) -> Final {
    let u = &v["usage"];
    Final {
        stop_reason: str_at(v, "stop_reason"),
        subtype: str_at(v, "subtype"),
        is_error: v["is_error"].as_bool().unwrap_or(false),
        cost_usd: v["total_cost_usd"].as_f64(),
        // The four counts are exactly Emma's four, which is the one place the
        // two systems agree on a number without a conversion. Everything else
        // on `Usage` is defaulted deliberately: `context_window` stays zero
        // because the stream reports no such thing and a plausible number here
        // would put an invented figure on a meter somebody checks, and
        // `server_tool_use` stays empty because the CLI's own tools are not
        // provider-side work Emma is billed for.
        usage: emma_llm::Usage {
            input_tokens: u["input_tokens"].as_i64().unwrap_or(0),
            output_tokens: u["output_tokens"].as_i64().unwrap_or(0),
            cache_creation_input_tokens: u["cache_creation_input_tokens"].as_i64().unwrap_or(0),
            cache_read_input_tokens: u["cache_read_input_tokens"].as_i64().unwrap_or(0),
            ..Default::default()
        },
    }
}

// endregion: The stream, as values

// region: The command line

/// Everything that decides what `claude` is invoked with. A value rather than a
/// pile of arguments so [`args`] is pure and the escalation rule is testable
/// without a process, a terminal or a filesystem.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    /// The goal's text, passed as one argument and never shell-joined.
    pub goal: String,
    /// The model Emma resolved, when one was resolved. Passed to `--model`.
    pub model: Option<String>,
    /// Whether *Emma herself* was started with `--allow-all`. The only bit that
    /// may unlock the bypass, and it is the same bit that puts Emma's own gate
    /// in `Gate::SkipAll`.
    pub allow_all: bool,
}

/// The argument list, in order.
///
/// **The escalation rule lives here and nowhere else.** Emma may hand the child
/// exactly the posture the user handed Emma, and never more. Without
/// `--allow-all` the `default` mode is passed *positively* rather than by
/// omission, so a `permissions.defaultMode` in the user's or the project's
/// `.claude/settings.json` cannot quietly widen a run Emma launched. That is the
/// difference between "Emma did not ask for more" and "Emma cannot get more",
/// and only the second is a property.
pub fn args(f: &Flags) -> Vec<String> {
    let mut out = vec![
        "-p".to_string(),
        f.goal.clone(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        // Not optional. `stream-json` in print mode is refused without it.
        "--verbose".to_string(),
    ];
    if let Some(model) = &f.model {
        out.push("--model".to_string());
        out.push(model.clone());
    }
    if f.allow_all {
        out.push("--dangerously-skip-permissions".to_string());
    } else {
        out.push("--permission-mode".to_string());
        out.push("default".to_string());
    }
    out
}

// endregion: The command line

// region: Events to transcript and record

/// What the run has learned so far. Every field is counted from events that
/// actually arrived; nothing here is estimated.
#[derive(Debug, Default)]
pub struct Run {
    /// The `init` event, once it has arrived.
    pub init: Option<Init>,
    /// The last thing the assistant said in prose. What `Outcome::text` becomes,
    /// and what the idle question and `/session` read afterwards.
    pub text: String,
    /// How many tools the child called.
    pub tool_calls: u32,
    /// Tools the child's own permission system refused. Non-zero means the goal
    /// stopped short whatever the CLI reported.
    pub denials: u32,
    /// Lines and blocks this build does not model, counted so the count can be
    /// asserted on.
    pub unknown_events: u32,
    /// The `result` event, once it has arrived.
    pub finished: Option<Final>,
    /// `tool_use_id -> tool name`, so a result can be attributed to the tool
    /// that produced it. The stream identifies a result by id only.
    names: std::collections::HashMap<String, String>,
    turn: u32,
    /// The assistant message being assembled: its `message.id` and the blocks
    /// seen for it so far. See [`Event::Assistant`] for why this exists at all.
    /// Painting is not buffered, only the record: the screen still updates per
    /// line, and the log still holds one turn per turn.
    pending: Option<(String, Vec<Block>)>,
}

impl Run {
    fn turn_id(&self) -> String {
        format!("claude-{}", self.turn)
    }

    /// Write the buffered assistant message out as one record.
    ///
    /// Called when the next message begins, when anything that is not an
    /// assistant line arrives, and once at the end of the stream. A turn that
    /// stayed buffered would be a turn missing from the transcript, and a
    /// `tool_result` recorded before the `tool_use` it answers would fold into a
    /// result with nothing to pair to.
    fn flush(&mut self, log: &SessionLog) {
        let Some((_, blocks)) = self.pending.take() else {
            return;
        };
        self.turn += 1;
        let text = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(t) if !t.trim().is_empty() => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            self.text = text.clone();
        }
        log.append(
            "assistant",
            json!({
                "turn_id": self.turn_id(),
                "text": text,
                // The CLI's own blocks, re-tagged into the shape
                // `session::Fold` reads. Never rebuilt from `text`: a thinking
                // block whose signature was reconstructed is rejected on the
                // next call, and a resumed session that dies on its first
                // request is worse than one that resumes short.
                "raw_content": raw_content(&blocks),
            }),
        );
    }
}

/// Fold one event into the run: paint it, record it, count it.
///
/// Pure apart from the two seams it is handed, both of which have test doubles
/// (`Term::recording`, `SessionLog::none`), which is why the whole of the
/// rendering and recording behaviour is testable without a child process.
pub fn apply(run: &mut Run, event: Event, term: &Term, log: &SessionLog) {
    match event {
        Event::Init(init) => {
            term.note(&header(&init));
            log.append(
                "engine",
                json!({
                    "engine": NAME,
                    "version": init.version,
                    "cli_model": init.model,
                    "permission_mode": init.permission_mode,
                    "claude_session_id": init.session_id,
                    "cwd": init.cwd,
                }),
            );
            run.init = Some(init);
        }
        Event::Assistant { id, blocks } => {
            // A new `message.id` closes the previous message. Same id means the
            // CLI split one message across lines, which it does whenever a
            // message mixes thinking, prose and tool calls.
            match &run.pending {
                Some((open, _)) if *open == id => {}
                _ => run.flush(log),
            }
            for block in &blocks {
                match block {
                    Block::Text(t) => {
                        if !t.trim().is_empty() {
                            term.text(t);
                        }
                    }
                    // One dim line, and only the first. Thinking in this stream
                    // is volume, not time and not quality: nothing in it says
                    // how long the model spent, and presenting it as insight
                    // would be reading a cost signal as a capability one. The
                    // whole of it is in the record.
                    Block::Thinking { text, .. } => {
                        if let Some(first) = text.lines().find(|l| !l.trim().is_empty()) {
                            term.note(&format!("thinking: {}", clip(first, 100)));
                        }
                    }
                    Block::ToolUse { id, name, input } => {
                        run.tool_calls += 1;
                        run.names.insert(id.clone(), name.clone());
                        term.tool_started(name, input);
                    }
                    Block::Other(v) => {
                        run.unknown_events += 1;
                        term.note(&format!(
                            "claude: unmodelled block {}",
                            clip(&v.to_string(), 160)
                        ));
                    }
                }
            }
            match &mut run.pending {
                Some((_, held)) => held.extend(blocks),
                none => *none = Some((id, blocks)),
            }
        }
        Event::ToolResults(results) => {
            run.flush(log);
            let turn_id = run.turn_id();
            for (id, content, is_error) in results {
                let tool = run.names.get(&id).cloned().unwrap_or_default();
                if is_error {
                    term.tool_failed(&tool, &content);
                } else {
                    term.tool_result(None, &content, false, None);
                }
                log.append(
                    "tool_result",
                    json!({
                        "turn_id": turn_id,
                        "id": id,
                        "tool": tool,
                        "block": {
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": content,
                            "is_error": is_error,
                        },
                    }),
                );
            }
        }
        Event::PermissionDenied {
            tool,
            tool_use_id,
            message,
        } => {
            run.flush(log);
            run.denials += 1;
            // `tool_blocked` rather than `tool_failed`: this is the treatment for
            // "no answer at the prompt could have allowed this", and in `-p`
            // mode that is exactly true. There is no prompt.
            term.tool_blocked(&tool, &message);
            log.append(
                "tool_blocked",
                json!({ "tool": tool, "tool_use_id": tool_use_id, "reason": message }),
            );
        }
        Event::Result(f) => {
            run.flush(log);
            run.finished = Some(f);
        }
        Event::Ignored(_) => {}
        Event::Unknown(line) => {
            run.flush(log);
            run.unknown_events += 1;
            // Shown dim and truncated, and logged whole. The alternative, drop
            // it, turns the next change to an undocumented event format into a
            // transcript with a silent hole in it.
            term.note(&format!("claude: unrecognised event {}", clip(&line, 160)));
            log.append("engine_unknown", json!({ "line": line }));
        }
    }
}

/// The one line printed before any work, naming whose rules are in force.
///
/// Every clause is a fact from the `init` event rather than a claim this build
/// makes about the CLI, and the second half is the honest-limits list in one
/// sentence: under this engine Emma's tools are not present and Emma's approval
/// prompt never appears.
pub fn header(init: &Init) -> String {
    format!(
        "claude engine: claude {} · model {} · permissions {} (claude's own rules, not Emma's). \
         Emma's tools and approval prompts do not apply to this goal.",
        blank_as(&init.version, "version unreported"),
        blank_as(&init.model, "unreported"),
        blank_as(&init.permission_mode, "unreported"),
    )
}

fn blank_as<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.trim().is_empty() {
        fallback
    } else {
        s
    }
}

/// The blocks as `session::Fold` wants to read them back.
///
/// One normalisation, and it earns its place: the CLI puts a `caller` field on
/// every `tool_use` block, and `ContentBlock::from_value` denies unknown fields,
/// so an untouched block would fold to `Passthrough` and stop counting as a tool
/// use. `place_turn` pairs a turn with its results by finding the tool uses, so
/// a `Passthrough` there means a resumed conversation missing the pairing the
/// API requires. Removing one field Emma does not use is cheaper than loosening
/// that guarantee for everybody.
fn raw_content(blocks: &[Block]) -> Value {
    Value::Array(
        blocks
            .iter()
            .map(|b| match b {
                Block::Text(t) => json!({"type": "text", "text": t}),
                Block::Thinking { text, signature } => {
                    json!({"type": "thinking", "thinking": text, "signature": signature})
                }
                Block::ToolUse { id, name, input } => {
                    json!({"type": "tool_use", "id": id, "name": name, "input": input})
                }
                Block::Other(v) => v.clone(),
            })
            .collect(),
    )
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

/// How the run ended, from what actually happened rather than from the CLI's
/// own word for it.
///
/// **A denial is not a completion.** The child reports `end_turn` after being
/// refused a tool, because from the model's point of view it did stop talking;
/// from the user's point of view the work did not get done and nothing on the
/// keyboard could have let it. That case is [`Ending::Stalled`], which is the
/// existing word for "stopped with work outstanding".
pub fn ending_of(run: &Run) -> Ending {
    let Some(f) = &run.finished else {
        // No result event means the stream ended early: a crash, a kill, or a
        // pipe that closed. Not a completed goal under any reading.
        return Ending::Provider("the claude CLI ended without writing a result event".to_string());
    };
    if f.is_error {
        return Ending::Provider(format!(
            "claude ended with an error ({})",
            blank_as(&f.subtype, "no subtype")
        ));
    }
    if run.denials > 0 {
        return Ending::Stalled;
    }
    match f.stop_reason.as_str() {
        "end_turn" | "stop_sequence" | "" => Ending::Done,
        "max_tokens" => Ending::Tokens,
        other => Ending::Provider(format!("claude stopped for `{other}`")),
    }
}

// endregion: Events to transcript and record

// region: Finding the CLI

/// Where the CLI is, and whether this platform can start it directly.
///
/// The distinction exists for one reason: on Windows the commonest install is
/// an npm shim, `claude.cmd`, and `CreateProcessW` — which is what
/// `std::process::Command` calls — cannot execute a script. `usertools.rs`
/// records the same finding for `code.cmd`. A shim has to go through `cmd.exe`,
/// and going through `cmd.exe` is not free: see [`shim_command_line`] for what
/// it costs and what had to be done about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The resolved absolute path. Never a bare name: `std::process::Command`
    /// resolves bare names itself and does not consult `PATHEXT`, so a bare
    /// `claude` fails with `NotFound` on a machine where `claude` works in the
    /// shell.
    pub program: PathBuf,
    /// Whether [`Launch::program`] is a `.cmd`/`.bat` script rather than a real
    /// executable. **Only ever true on Windows** — no other platform has a file
    /// the kernel refuses to exec but a shell will happily run.
    pub shim: bool,
}

/// The filenames a bare name may resolve to, in the order a real `claude.exe`
/// beats a `claude.cmd` sitting beside it.
///
/// Windows only, and it mirrors `usertools::candidates` rather than inventing a
/// second answer to the same question — one input shape with two readers in one
/// codebase is a defect class this repository has already paid for.
#[cfg(windows)]
fn candidates(name: &str) -> Vec<(String, bool)> {
    [("exe", false), ("com", false), ("cmd", true), ("bat", true)]
        .iter()
        .map(|(ext, shim)| (format!("{name}.{ext}"), *shim))
        .collect()
}

#[cfg(not(windows))]
fn candidates(name: &str) -> Vec<(String, bool)> {
    vec![(name.to_string(), false)]
}

/// Find the CLI on `PATH`.
///
/// Returns the first candidate that exists, walking `PATH` in order and, within
/// one directory, extensions in [`candidates`] order. `None` means it is not
/// installed, which the caller turns into a sentence rather than a panic.
pub fn resolve(name: &str) -> Option<Launch> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for (file, shim) in candidates(name) {
            let p = dir.join(file);
            if p.is_file() {
                return Some(Launch { program: p, shim });
            }
        }
    }
    None
}

/// Whether a directory looks like somewhere `claude` can run. Reported as a
/// note rather than a refusal: the CLI is the authority on its own preconditions
/// and this is only here to turn the commonest failure into a sentence.
pub fn missing_cli(path: &Path) -> Option<String> {
    if resolve(NAME).is_some() {
        return None;
    }
    Some(format!(
        "`claude` is not on PATH, so the claude engine cannot run a goal in {}",
        path.display()
    ))
}

/// MSVCRT argument quoting: what the child's own C runtime parses back into one
/// argument. The same algorithm `std::process::Command` uses, reimplemented here
/// because the shim path has to build the whole command line by hand and
/// therefore cannot borrow std's.
#[cfg(windows)]
fn crt_quote(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut slashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                slashes = 0;
                out.push('"');
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                slashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// The characters `cmd.exe` acts on before anything else sees the line.
#[cfg(windows)]
const CMD_META: &[char] = &['(', ')', '%', '!', '^', '"', '<', '>', '&', '|'];

/// `cmd.exe`'s layer on top of the CRT's: every metacharacter carets,
/// **including the quotes themselves**.
///
/// Quoting the double quotes is the part that is easy to get wrong and the part
/// that matters. If the quotes reach `cmd` unescaped, `cmd` enters quoted mode,
/// and inside quoted mode a caret is a literal caret while `%NAME%` is still
/// expanded — so a goal containing `%USERPROFILE%` would have the machine's home
/// directory spliced into the prompt before the model ever saw it. Escaping the
/// quotes too means `cmd` never enters quoted mode, strips every caret, and
/// hands the CRT exactly the quoted string it was given.
///
/// Measured on Windows 11, 2026-09-06, against a shim that echoes its argv:
/// without this, `pct %PATH% pct` arrived as the machine's whole `PATH`; with
/// it, it arrived verbatim. Ten hostile shapes — `& | % ! ^ " ( )`, embedded
/// backslashes, spaces — all round-trip.
#[cfg(windows)]
fn caret(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if CMD_META.contains(&c) {
            out.push('^');
        }
        out.push(c);
    }
    out
}

/// The raw command line for `cmd.exe`, everything after the program itself.
///
/// **A newline cannot be carried and is refused rather than truncated.** `cmd`
/// ends its command line at the first CR or LF and reports success on what it
/// did run: measured, a two-line goal arrived as its first line with exit code
/// 0 and nothing on stderr. That is silent loss, so the shim path says so and
/// names the two ways out instead.
#[cfg(windows)]
fn shim_command_line(program: &Path, args: &[String]) -> Result<String, String> {
    let mut line = String::from("/C ");
    line.push_str(&caret(&crt_quote(&program.display().to_string())));
    for a in args {
        if a.contains('\n') || a.contains('\r') {
            return Err(format!(
                "`claude` on this machine is {}, a script shim, and a script shim has to be run \
                 through cmd.exe — which ends its command line at the first newline and reports \
                 success on the part it ran. A goal spanning several lines would be silently cut, \
                 so it is refused instead. Install the native `claude.exe`, or put the goal on one \
                 line.",
                program.display()
            ));
        }
        line.push(' ');
        line.push_str(&caret(&crt_quote(a)));
    }
    Ok(line)
}

/// The command that starts the CLI, ready to spawn.
fn command_for(launch: &Launch, args: &[String]) -> Result<tokio::process::Command, String> {
    if !launch.shim {
        let mut c = tokio::process::Command::new(&launch.program);
        c.args(args);
        return Ok(c);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let line = shim_command_line(&launch.program, args)?;
        let mut c = tokio::process::Command::new("cmd.exe");
        c.as_std_mut().raw_arg(line);
        Ok(c)
    }
    // Not reachable — `resolve` never sets `shim` off Windows — but answered
    // rather than unwrapped, because a `Launch` is a public value somebody can
    // construct and a panic is not a message.
    #[cfg(not(windows))]
    Err(format!(
        "{} was marked a script shim, and only Windows has those",
        launch.program.display()
    ))
}

// endregion: Finding the CLI

// region: The child, and stopping it

/// The three things stopping a child needs of the operating system, behind a
/// trait so a test can assert on signals without a process. The `runctl`
/// precedent, and the same reason: a test that really signalled would be a test
/// that can kill the wrong thing on a bad day.
pub trait Signalling: Send + Sync {
    /// Ask the child to stop and write its ending. `Err` means this platform or
    /// this process cannot deliver such a request — **never** that it was
    /// delivered and ignored.
    fn interrupt(&self) -> Result<(), String>;
    /// End it outright, for a child that did not take the hint or could not be
    /// asked.
    fn kill(&self) -> Result<(), String>;
    /// Whether it has already exited. Asked of the operating system on every
    /// platform; an implementation that answered from a guess would be the
    /// kill-reports-success defect wearing a trait.
    fn exited(&self) -> bool;
}

/// What stopping the child came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// It was already gone.
    Already,
    /// The interrupt was enough, so the child wrote whatever ending it had.
    Interrupted,
    /// The interrupt was not enough within the grace period.
    Killed,
    /// The interrupt could not be delivered at all, so the child was ended
    /// outright and never got to write its `result` event.
    ///
    /// This is the ordinary Windows case, not a fault, and the notice says so:
    /// there is no way to deliver an interrupt to a child that does not share
    /// this process's console, and generating a console control event would hit
    /// Emma as well as the child.
    KilledWithoutInterrupt,
    /// The kill itself failed. Reported rather than swallowed.
    Failed,
}

/// Interrupt, wait, then kill.
///
/// The grace period is what makes the interrupt worth sending at all: a kill
/// that followed immediately would be a kill with extra steps, and the whole
/// reason to interrupt rather than kill is to let the child finish its `result`
/// event so the transcript records what the run actually spent.
///
/// **An interrupt that cannot be delivered does not end the attempt.** The fork
/// this came from returned `Failed` there, which on Windows meant every stop
/// printed "the signal to claude failed; it may still be running" while the
/// child was in fact still running — the notice was right by accident and the
/// child was never killed. Here the grace period is skipped, because waiting
/// five seconds for a request nobody sent is five seconds of a frozen prompt for
/// nothing, and the kill happens immediately.
pub async fn stop(child: &dyn Signalling, grace: Duration, poll: Duration) -> Stopped {
    if child.exited() {
        return Stopped::Already;
    }
    let asked = child.interrupt().is_ok();
    if asked {
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if child.exited() {
                return Stopped::Interrupted;
            }
            tokio::time::sleep(poll).await;
        }
        if child.exited() {
            return Stopped::Interrupted;
        }
    }
    match child.kill() {
        Ok(()) if asked => Stopped::Killed,
        Ok(()) => Stopped::KilledWithoutInterrupt,
        Err(_) => Stopped::Failed,
    }
}

/// What the user is told about a stop, in words that are true on the platform
/// they are reading them on.
pub fn notice(what: Stopped, grace: Duration) -> String {
    match what {
        Stopped::Already => "claude had already exited".to_string(),
        Stopped::Interrupted => "claude was interrupted and wrote its ending".to_string(),
        Stopped::Killed => format!(
            "claude did not exit within {}s of being interrupted and was killed; its cost report \
             is missing from this goal",
            grace.as_secs()
        ),
        Stopped::KilledWithoutInterrupt => {
            "this platform cannot interrupt a child process, so claude was ended outright rather \
             than asked to stop; its cost report is missing from this goal"
                .to_string()
        }
        Stopped::Failed => "claude could not be stopped and may still be running; look for a \
                            stray `claude` process before starting another goal"
            .to_string(),
    }
}

/// The real [`Signalling`], over the child this process just spawned.
///
/// Opened at spawn time rather than at stop time, and that is the whole design:
/// on Windows an open process handle keeps the pid reserved, so the recycled-pid
/// hazard `runctl` was written around cannot arise here at all. On Unix there is
/// no handle to hold and the pid is trusted for the same reason `runctl` does
/// not trust one — it was minted by this process, moments ago, and nothing else
/// is ever signalled.
pub struct Spawned(plat::Child);

impl Spawned {
    /// Take a hold on the child with the given pid.
    pub fn of(pid: u32) -> Result<Self, String> {
        plat::Child::open(pid).map(Self)
    }
}

impl Signalling for Spawned {
    fn interrupt(&self) -> Result<(), String> {
        self.0.interrupt()
    }

    fn kill(&self) -> Result<(), String> {
        self.0.kill()
    }

    fn exited(&self) -> bool {
        self.0.exited()
    }
}

#[cfg(unix)]
mod plat {
    //! Signals, which is what Unix has and what the whole shape of [`super::stop`]
    //! was designed around.

    /// A pid this process spawned.
    pub struct Child(u32);

    impl Child {
        pub fn open(pid: u32) -> Result<Self, String> {
            Ok(Self(pid))
        }

        pub fn interrupt(&self) -> Result<(), String> {
            send(self.0, libc::SIGINT)
        }

        pub fn kill(&self) -> Result<(), String> {
            send(self.0, libc::SIGKILL)
        }

        pub fn exited(&self) -> bool {
            // Signal 0 tests for existence without delivering anything.
            unsafe { libc::kill(self.0 as libc::pid_t, 0) != 0 }
        }
    }

    fn send(pid: u32, sig: i32) -> Result<(), String> {
        if unsafe { libc::kill(pid as libc::pid_t, sig) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }
}

#[cfg(windows)]
mod plat {
    //! A process handle, which is what Windows has instead of signals.
    //!
    //! Three differences from the Unix arm, all of them load-bearing:
    //!
    //! - **There is no interrupt.** `GenerateConsoleCtrlEvent` addresses a
    //!   process *group* attached to a console, and Emma's child does not have
    //!   its own; sending one would stop Emma too. So [`Child::interrupt`] says
    //!   no, honestly, and [`super::stop`] kills instead of pretending.
    //! - **Liveness is asked of the handle**, with a zero-millisecond wait,
    //!   rather than of a signal that does not exist. The fork answered `true`
    //!   unconditionally here, which made every Windows interrupt print "claude
    //!   had already exited" over a child that was still working.
    //! - **The handle is held for the run**, which keeps the pid from being
    //!   recycled underneath the kill.

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    /// An owned handle to the child this process spawned.
    pub struct Child(HANDLE);

    // SAFETY: a process handle is a kernel object usable from any thread, and
    // every use below is a single syscall that takes it by value. The wrapper
    // owns it exclusively and closes it once, in `Drop`.
    unsafe impl Send for Child {}
    unsafe impl Sync for Child {}

    impl Child {
        pub fn open(pid: u32) -> Result<Self, String> {
            // SAFETY: no pointers; the pid is one this process just spawned.
            let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
            if h.is_null() {
                return Err(format!(
                    "could not take a handle on the claude process (pid {pid}): {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self(h))
        }

        pub fn interrupt(&self) -> Result<(), String> {
            Err(
                "windows has no way to interrupt a child process that does not share this \
                 console; the child can only be ended outright"
                    .to_string(),
            )
        }

        pub fn kill(&self) -> Result<(), String> {
            // SAFETY: `self.0` is a live handle owned by this value.
            if unsafe { TerminateProcess(self.0, 1) } != 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error().to_string())
            }
        }

        pub fn exited(&self) -> bool {
            // SAFETY: as above. A zero timeout makes this a poll, not a wait.
            unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            // SAFETY: closed exactly once, by the owner, at the end of its life.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

// endregion: The child, and stopping it

// region: The handoff

/// Everything the handoff needs from the run around it.
pub struct Handoff {
    /// Where the child runs.
    pub cwd: PathBuf,
    /// The model Emma resolved, passed straight through.
    pub model: Option<String>,
    /// Whether Emma herself was started with `--allow-all`.
    pub allow_all: bool,
    /// How long the whole goal gets before the child is stopped.
    pub timeout: Duration,
    /// Ctrl-C, shared with the rest of the session.
    pub interrupt: Arc<Interrupt>,
    /// The session file this goal is recorded in.
    pub log: Arc<SessionLog>,
    /// The transcript seam.
    pub term: Arc<Term>,
    /// Emma's session id, for the `goal` record.
    pub session_id: String,
    /// The token meter this goal is charged against.
    pub spend: Arc<Spend>,
    /// Where the CLI is, already resolved. Resolution happens in the caller so
    /// that a missing CLI is one sentence at startup rather than a failed spawn
    /// in the middle of a goal — see [`missing_cli`].
    pub cli: Launch,
}

/// Run one goal on the claude CLI and return the same [`Outcome`] Emma's own
/// loop returns, so the caller cannot tell which engine produced it.
pub async fn run_goal(h: &Handoff, goal: &str) -> Outcome {
    let started = std::time::Instant::now();
    h.term.goal_started(goal);
    h.log.append(
        "goal",
        json!({
            "session_id": h.session_id,
            "text": goal,
            "opening": goal,
            "cwd": h.cwd.display().to_string(),
            "engine": NAME,
        }),
    );

    let flags = Flags {
        goal: goal.to_string(),
        model: h.model.clone(),
        allow_all: h.allow_all,
    };
    let mut run = Run::default();
    let ending = match drive(h, &args(&flags), &mut run).await {
        Ok(ending) => ending,
        Err(why) => {
            h.term.warn(&why);
            Ending::Provider(why)
        }
    };

    // Weighted the same way every other goal is weighted, by the same function,
    // so a claude goal and an Emma goal are comparable numbers rather than two
    // spellings of "tokens". Zero when no result event arrived, which is the
    // truth: nothing was reported.
    let usage = run
        .finished
        .as_ref()
        .map(|f| f.usage.clone())
        .unwrap_or_default();
    let tokens = h.spend.add(crate::agent::cost_tokens(&usage));

    if run.denials > 0 {
        h.term.warn(&format!(
            "{} tool call(s) were refused by claude's own permission system, which cannot prompt \
             in this mode. The goal stopped short. Grant them in claude's settings, or start emma \
             with --allow-all to pass the bypass through.",
            run.denials
        ));
    }

    let outcome = Outcome {
        ending,
        text: run.text.clone(),
        tokens,
        // One child run. Not an estimate of how many model calls happened inside
        // it, which Emma cannot see and will not guess.
        iterations: 1,
        // The kick machinery does not apply: the child decides when it is done.
        kicks: 0,
    };
    h.log.append(
        "goal_finished",
        json!({
            "ending": outcome.ending.as_str(),
            "detail": match &outcome.ending { Ending::Provider(e) => Some(e.clone()), _ => None },
            "tokens": outcome.tokens,
            "iterations": outcome.iterations,
            "kicks": outcome.kicks,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "engine": NAME,
            "tool_calls": run.tool_calls,
            "denials": run.denials,
            "cost_usd": run.finished.as_ref().and_then(|f| f.cost_usd),
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_creation_input_tokens": usage.cache_creation_input_tokens,
            "cache_read_input_tokens": usage.cache_read_input_tokens,
        }),
    );
    if let Some(cost) = run.finished.as_ref().and_then(|f| f.cost_usd) {
        h.term
            .note(&format!("claude reported this goal cost ${cost:.4}"));
    }
    h.term.goal_ended();
    outcome
}

/// Spawn, read, and stop. The only impure part of this module.
async fn drive(h: &Handoff, args: &[String], run: &mut Run) -> Result<Ending, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut child = command_for(&h.cli, args)?
        .current_dir(&h.cwd)
        // stdin closed rather than inherited. The child must never reach for the
        // keyboard Emma owns: in `-p` mode it has no prompt to draw, and an
        // inherited stdin would let it swallow a keystroke meant for Emma.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "`{}` is gone. The claude engine runs the real CLI; reinstall it, or pick another \
                 provider.",
                h.cli.program.display()
            ),
            _ => format!("could not start `{}`: {e}", h.cli.program.display()),
        })?;

    // Taken now, while the child is certainly alive, so nothing later has to
    // trust a bare pid. A handle that cannot be opened is reported rather than
    // swallowed: without it a stop could not tell a live child from a dead one.
    let pid = child.id().unwrap_or_default();
    let signals = Spawned::of(pid);
    let stdout = child.stdout.take().ok_or("claude produced no stdout")?;
    let stderr = child.stderr.take();
    let mut lines = BufReader::new(stdout).lines();

    let deadline = tokio::time::sleep(h.timeout);
    tokio::pin!(deadline);

    // Three things can end the read: the stream, the user, and the clock. The
    // last two both stop the child the same way, and the difference between them
    // is only which ending the goal gets.
    let interrupted = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if line.trim().is_empty() { continue; }
                    apply(run, Event::parse(&line), &h.term, &h.log);
                }
                Ok(None) => break None,
                Err(e) => break Some(Ending::Provider(format!("reading claude's output: {e}"))),
            },
            () = h.interrupt.wait() => break Some(Ending::Interrupted),
            () = &mut deadline => break Some(Ending::Deadline),
        }
    };

    // Whatever ended the read, an assistant message that was mid-assembly still
    // happened and still belongs in the transcript. A stream cut off by an
    // interrupt is the case that makes this matter: the last thing the model
    // said would otherwise be the one thing the record lost.
    run.flush(&h.log);

    if let Some(ending) = interrupted {
        match &signals {
            Ok(s) => {
                let stopping = stop(s, GRACE, POLL);
                tokio::pin!(stopping);
                let what = tokio::select! {
                    what = &mut stopping => what,
                    // The child exiting on its own is an answer too, and on Unix
                    // it is the only reliable one: a child that has exited but
                    // not yet been reaped still answers `kill(pid, 0)`, so a stop
                    // watching the signal alone would sit out the whole grace
                    // period and then report a kill over a child that obeyed.
                    // `Child::wait` is cancel-safe, so losing this race costs
                    // nothing.
                    _ = child.wait() => Stopped::Interrupted,
                };
                h.term.note(&notice(what, GRACE));
            }
            // No handle means no honest answer about liveness, so nothing is
            // claimed: the child is ended through the one route that needs no
            // handle, and the user is told a handle could not be taken.
            Err(why) => {
                let killed = child.start_kill().is_ok();
                h.term.warn(&format!(
                    "could not take a hold on the claude process ({why}), so its state could not \
                     be read. It was {}.",
                    if killed {
                        "ended outright"
                    } else {
                        "left running — look for a stray `claude` process"
                    }
                ));
            }
        }
        let _ = child.wait().await;
        return Ok(ending);
    }

    let status = child.wait().await.map_err(|e| e.to_string())?;
    // stderr is read only on a bad exit, and only then shown. On a good run it
    // carries progress noise nobody asked for; on a bad one it is the only place
    // the reason is written.
    if !status.success() && run.finished.is_none() {
        if let Some(mut err) = stderr {
            let mut text = String::new();
            use tokio::io::AsyncReadExt;
            let _ = err.read_to_string(&mut text).await;
            let text = text.trim();
            if !text.is_empty() {
                h.term.warn(&format!("claude said: {}", clip(text, 400)));
            }
        }
    }
    Ok(ending_of(run))
}

// endregion: The handoff

#[cfg(test)]
mod tests;
