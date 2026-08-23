//! `BashOutput` — what a background shell has said since you last asked.
//!
//! The registry (`emma_tool_api::background`) already made the three hard
//! promises; this tool's job is to relay them without weakening any:
//!
//! **A finished task is still an answer.** "It finished" and "it never
//! existed" are different facts and the model can act on the difference, so a
//! task that exited comes back with its tail and its status, never with "no
//! such task". Only an id the registry has no task for — in *this* session —
//! is an error, and that error names what does exist, because the ids are
//! short and sequential and a typo should be a one-step correction.
//!
//! **Reads consume.** Each call returns what is new since the last one
//! (`read_new`), because a poll loop that re-reads the whole buffer makes the
//! model pay for the same bytes every time. The flip side is stated in the
//! description: bytes returned once are not returned again.
//!
//! **A dropped byte is announced, not absorbed.** The registry caps a task's
//! buffer and counts what it discarded; when that count is non-zero the result
//! says so via `truncated_because`, naming the cap, the loss, and the one
//! remedy that exists (read more often) — the bytes themselves are gone and
//! nothing returns them, which the message admits rather than papering over.

use emma_tool_api::background::{TaskState, MAX_TASK_OUTPUT_BYTES};
use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// One required argument, and a metadata declaration whose reasoning is longer
// than the tool — see `meta`, because "does reading consume?" is a genuine
// question here and the answer decides whether every poll prompts a human.
// ---------------------------------------------------------------------------

const NAME: &str = "BashOutput";
const KEYS: &[&str] = &["bash_id"];

#[derive(Default)]
pub struct BashOutput;

impl BashOutput {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for BashOutput {
    /// Claude Code's name, exactly, so a hook matcher or allow-list written
    /// for one works for the other.
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("bashoutput.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "bash_id": {
                    "type": "string",
                    "description": "The id a background Bash call returned, e.g. bash_1."
                }
            },
            "required": ["bash_id"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            // Reading *consumes*: the cursor advances and the same bytes are
            // not returned twice, so this is not a pure read. It is still
            // `read_only`, because the question that bit answers is "can this
            // call damage this machine" — no file is written, nothing is
            // spawned, nothing is signalled. The cursor is Emma's own session
            // bookkeeping, the same class of state as the `ReadTracker` that
            // `Read` mutates while declaring `read_only: true`. The
            // alternative — a human prompt before every poll of a build that
            // is still compiling — is the click-through trainer the gate's
            // design refuses everywhere else.
            read_only: true,
            reaches_network: false,
            // Required by `read_only: true` (the registry refuses the other
            // pair), and honest on its own terms: the *effect* of a read is
            // "cursor at the end of the buffer", which is the same after two
            // calls as after one. What differs on the second call is the
            // answer — empty — not the effect.
            idempotent: true,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        args::req_str(args_v, NAME, "bash_id")?;
        Ok(())
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(ctx, args_v))
    }
}

// endregion: The tool surface

// region: Shared vocabulary with KillShell
// ---------------------------------------------------------------------------
// Shared vocabulary with KillShell
//
// Both tools name a task's state and both refuse an unknown id, and they must
// do it in the same words — two spellings of "exited with status 1" would read
// as two different facts. `killshell.rs` imports these rather than restating
// them.
// ---------------------------------------------------------------------------

/// The one-line reading of a state that cannot be misread: running, exited
/// with a code, exited without one, killed, or never ran.
pub(crate) fn state_phrase(state: &TaskState) -> String {
    match state {
        TaskState::Running => "still running".to_string(),
        TaskState::Exited(Some(code)) => format!("exited with status {code}"),
        // `None` is the platform declining to report a code, which on unix
        // means a signal ended it. Flattening that to a number would hand the
        // model an exit status that never existed.
        TaskState::Exited(None) => {
            "exited without a status code (on unix this means a signal ended it)".to_string()
        }
        TaskState::Killed => "killed by KillShell".to_string(),
        TaskState::Failed(e) => format!("failed to run: {e}"),
    }
}

/// The single-word form for listings and terminal lines.
fn state_word(state: &TaskState) -> &'static str {
    match state {
        TaskState::Running => "running",
        TaskState::Exited(_) => "exited",
        TaskState::Killed => "killed",
        TaskState::Failed(_) => "failed",
    }
}

/// The refusal for an id this session has no task under.
///
/// Two things it must do. Name what *is* available — the ids are short and
/// sequential, so a wrong one is usually a typo or a stale memory, and the
/// listing turns either into a one-step correction. And treat an id from
/// another session exactly like one that never existed: the registry already
/// scopes the lookup, and this message deliberately has no way to say "that id
/// exists but is not yours", because saying so would confirm a guess.
pub(crate) fn unknown_id(ctx: &ToolCtx, tool: &str, id: &str) -> ToolError {
    let tasks = ctx.background.list(&ctx.session_id);
    if tasks.is_empty() {
        return ToolError::BadArguments(format!(
            "{tool}: no background task has id {id} — this session has no background tasks, \
             running or finished"
        ));
    }
    let listing: Vec<String> = tasks
        .iter()
        .map(|t| format!("{} ({}, {})", t.id, t.label, state_word(&t.state())))
        .collect();
    ToolError::BadArguments(format!(
        "{tool}: no background task in this session has id {id}; this session's tasks are: {}",
        listing.join(", ")
    ))
}

// endregion: Shared vocabulary with KillShell

// region: The read itself
// ---------------------------------------------------------------------------
// The read itself
//
// One registry call, then wording. Every branch below exists because two
// situations that would otherwise share a sentence are different facts the
// model routes on differently.
// ---------------------------------------------------------------------------

impl BashOutput {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let id = args::req_str(&args_v, NAME, "bash_id")?;
        let task = ctx
            .background
            .get(&ctx.session_id, id)
            .ok_or_else(|| unknown_id(ctx, NAME, id))?;

        // State and output arrive under one lock (see `read_new`'s doc), so
        // this pair cannot be the coherent-looking lie of a final tail
        // labelled "still running".
        let read = task.read_new();
        let phrase = state_phrase(&read.state);

        let mut content = if read.new_output.is_empty() {
            match &read.state {
                // Still going, nothing yet: an invitation to poll again, and
                // deliberately a different sentence from the finished case
                // below — one means "wait", the other means "stop waiting".
                TaskState::Running => format!(
                    "{} ({}): still running; no new output since the last read.",
                    task.id, task.label
                ),
                _ => {
                    // Finished and empty splits again: a task that never wrote
                    // anything, and one whose output earlier reads already
                    // returned. Conflating them invites re-polling for output
                    // that was already delivered, or hunting for output that
                    // never existed. `peek_all` can distinguish them because
                    // it does not move the cursor — but only when nothing was
                    // dropped, since a drained buffer that emptied out looks
                    // identical to one that was never filled.
                    if read.dropped == 0 && task.peek_all().is_empty() {
                        format!(
                            "{} ({}): {phrase}; it produced no output at all.",
                            task.id, task.label
                        )
                    } else {
                        format!(
                            "{} ({}): {phrase}; no further output — earlier reads returned \
                             the rest.",
                            task.id, task.label
                        )
                    }
                }
            }
        } else {
            format!("{} ({}): {phrase}\n{}", task.id, task.label, read.new_output)
        };

        let display = if read.new_output.is_empty() {
            format!("{}: {}, nothing new", task.id, state_word(&read.state))
        } else {
            format!(
                "{}: {}, {} new bytes",
                task.id,
                state_word(&read.state),
                read.new_output.len()
            )
        };

        // `exit_code` stays `None` even for an exited task: that field means
        // "a command this tool ran", and this tool ran none. Re-stating a
        // background task's status there on every poll would make one exit
        // look to the delegation footer like many commands.
        let outcome = ToolOutcome::new(String::new()).with_display(display);
        Ok(if read.dropped > 0 {
            // `dropped` is cumulative for the task's whole life, and so is
            // this admission: once bytes are gone, no later read of this task
            // is a complete record, however whole its own window looks. The
            // remedy named is the only one that exists — there is no argument
            // that raises the cap and nothing returns the dropped bytes.
            let reason = format!(
                "the oldest {} bytes of this task's output were dropped by the fixed {} KiB \
                 per-task buffer and nothing returns them; reading BashOutput more often, \
                 before the buffer wraps, is the only remedy",
                read.dropped,
                MAX_TASK_OUTPUT_BYTES / 1024
            );
            content.push_str(&format!("\n[truncated: {reason}]"));
            ToolOutcome { content, ..outcome }.truncated_because(reason)
        } else {
            ToolOutcome { content, ..outcome }
        })
    }
}

// endregion: The read itself

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Each drives the real `Tool::invoke` against a hand-driven registry task —
// `push` and `set_state` play the child process, so every state is reachable
// without spawning anything.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use emma_tool_api::background::Registry;

    fn ctx_for(session: &str, reg: &Registry) -> ToolCtx {
        ToolCtx {
            // Never touched by this tool; any real directory serves.
            cwd: std::env::temp_dir(),
            session_id: session.into(),
            turn_id: "t1".into(),
            background: reg.clone(),
        }
    }

    async fn call(ctx: &ToolCtx, id: &str) -> Result<ToolOutcome, ToolError> {
        BashOutput::new()
            .invoke(ctx, json!({ "bash_id": id }))
            .await
            .expect("BashOutput must never end the turn")
    }

    #[tokio::test]
    async fn a_finished_task_returns_its_output_and_exit_state_not_an_error() {
        // The model polls after the work is done more often than during it.
        // "No such task" here would erase the difference between "it finished"
        // and "it never existed".
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "cargo build");
        task.push(b"Compiling emma\n");
        task.set_state(TaskState::Exited(Some(0)));

        let out = call(&ctx, &task.id).await.expect("a finished task errored");
        assert!(out.content.contains("Compiling emma"), "{}", out.content);
        assert!(
            out.content.contains("exited with status 0"),
            "the exit state is not stated plainly: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn reading_twice_never_returns_the_same_bytes_twice() {
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "tail -f log");
        task.push(b"one\n");

        let first = call(&ctx, &task.id).await.unwrap();
        assert!(first.content.contains("one"), "{}", first.content);

        let second = call(&ctx, &task.id).await.unwrap();
        assert!(
            !second.content.contains("one"),
            "a poll was handed bytes it had already consumed: {}",
            second.content
        );
        // Nothing new while running is a "keep waiting" sentence…
        assert!(
            second.content.contains("still running")
                && second.content.contains("no new output since the last read"),
            "{}",
            second.content
        );

        // …and nothing new after exit is a "stop waiting" sentence. The model
        // must be able to tell them apart, or it polls a corpse forever.
        task.set_state(TaskState::Exited(Some(1)));
        let third = call(&ctx, &task.id).await.unwrap();
        assert!(
            third.content.contains("exited with status 1")
                && third.content.contains("earlier reads returned the rest"),
            "{}",
            third.content
        );
        assert!(!third.content.contains("still running"), "{}", third.content);
    }

    #[tokio::test]
    async fn a_task_that_wrote_nothing_is_told_apart_from_one_already_drained() {
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "true");
        task.set_state(TaskState::Exited(Some(0)));

        let out = call(&ctx, &task.id).await.unwrap();
        assert!(
            out.content.contains("produced no output at all"),
            "silence was reported as already-read output: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn an_unknown_id_errors_and_names_what_is_available() {
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "cargo test");

        let err = call(&ctx, "bash_99").await.expect_err("a guessed id was answered");
        assert_eq!(err.kind(), "bad_arguments");
        assert!(
            err.detail().contains(&task.id) && err.detail().contains("cargo test"),
            "the refusal does not name what exists: {}",
            err.detail()
        );

        // With nothing spawned, the message says so rather than listing an
        // empty set the model would misread as a formatting accident.
        let empty = Registry::new();
        let ctx2 = ctx_for("s1", &empty);
        let err2 = call(&ctx2, "bash_1").await.expect_err("an empty registry answered");
        assert!(
            err2.detail().contains("no background tasks"),
            "{}",
            err2.detail()
        );
    }

    #[tokio::test]
    async fn an_id_from_another_session_reads_as_unknown() {
        // The ids are short and sequential, so guessing one is trivial; the
        // registry scopes the lookup and this asserts the tool does not route
        // around that scoping.
        let reg = Registry::new();
        let theirs = reg.spawn("s2", "secret work");
        theirs.push(b"not yours\n");

        let ctx = ctx_for("s1", &reg);
        let err = call(&ctx, &theirs.id).await.expect_err("cross-session read succeeded");
        assert_eq!(err.kind(), "bad_arguments");
        assert!(
            !err.detail().contains("not yours") && !err.detail().contains("secret work"),
            "the refusal leaked another session's task: {}",
            err.detail()
        );
    }

    #[tokio::test]
    async fn dropped_bytes_are_reported_with_the_cap_named() {
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "noisy");
        task.push(&vec![b'x'; MAX_TASK_OUTPUT_BYTES]);
        task.push(b"tail");

        let out = call(&ctx, &task.id).await.unwrap();
        assert!(out.truncated, "a lossy read was presented as whole");
        let reason = out.truncation.expect("the cut has no stated reason");
        assert!(reason.contains("4 bytes"), "the loss is unstated: {reason}");
        assert!(
            reason.contains(&format!("{} KiB", MAX_TASK_OUTPUT_BYTES / 1024)),
            "the cap is unnamed: {reason}"
        );
        // The model reads content, not flags, so the admission must be there
        // too.
        assert!(out.content.contains("[truncated:"), "{}", out.content);
    }

    #[test]
    fn the_declaration_is_the_coherent_pair() {
        // `read_only: true` entails `idempotent: true` at registration; this
        // pins the pair so a future edit to one is forced to reconsider the
        // other rather than tripping the registry assert at boot.
        let meta = BashOutput::new().meta();
        assert!(meta.read_only && meta.idempotent && !meta.reaches_network);
    }
}

// endregion: Tests
