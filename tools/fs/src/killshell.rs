//! `KillShell` — stop a background shell, and be exact about what stopped.
//!
//! Two honesty rules govern this file, both inherited from the registry's
//! module doc and both easy to lose in wording:
//!
//! **"Stopped it" and "it had already finished" are different sentences.**
//! `Task::kill` reports whether anything was actually signalled, and this tool
//! checks the state before it even asks — claiming to have stopped a task that
//! exited on its own is the plausible-success failure this project treats as
//! a defect, and it would also overwrite the task's real exit status with
//! `Killed`, destroying a fact `BashOutput` still owes the model.
//!
//! **The kill's blast radius is the shell, never a tree.** Emma does not reap
//! process trees — ruled out in `notes/plans/process-lifetime.md`, because a
//! deliberately daemonizing child is a supported use and nothing can
//! distinguish one from an orphaned mess. So a shell that spawned a server
//! keeps that server, and every success message here says so rather than
//! implying a guarantee the mechanism does not provide.

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::bashoutput::{state_phrase, unknown_id};

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// Same single argument as `BashOutput`, and deliberately the same refusal for
// an unknown id — the two tools share one vocabulary so the model learns it
// once.
// ---------------------------------------------------------------------------

const NAME: &str = "KillShell";
const KEYS: &[&str] = &["bash_id"];

#[derive(Default)]
pub struct KillShell;

impl KillShell {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for KillShell {
    /// Claude Code's name, exactly, so a hook matcher or allow-list written
    /// for one works for the other.
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/killshell.md")
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
            // Signalling a process is exactly the local damage `read_only`
            // asks about — a build stopped halfway is state this call changed.
            // No hedging about "it only sends a signal": the gate's question
            // is whether the machine is different afterwards, and it is.
            read_only: false,
            reaches_network: false,
            // A second call finds nothing left to signal and says so; the
            // machine ends in the same state as after one call. Dead stays
            // dead. What differs on a repeat is the sentence, not the effect.
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

// region: The kill, and its three honest outcomes
// ---------------------------------------------------------------------------
// The kill, and its three honest outcomes
//
// Stopped it; it had already finished; it is running and cannot be stopped.
// The third exists because `Task::kill` can only signal what its spawner
// registered, and a spawner that registered nothing is a wiring defect worth
// a loud failure rather than a shrug.
// ---------------------------------------------------------------------------

impl KillShell {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let id = args::req_str(&args_v, NAME, "bash_id")?;
        let task = ctx
            .background
            .get(&ctx.session_id, id)
            .ok_or_else(|| unknown_id(ctx, NAME, id))?;

        // Checked *before* calling `kill`, not after, and the ordering is
        // load-bearing twice over. `Task::kill` fires whatever killer is
        // still registered and then records `Killed` — on a task that already
        // exited that would signal a pid the child no longer holds and stamp
        // `Killed` over the real exit status, so a later `BashOutput` would
        // report a clean exit as a kill. A finished task is therefore
        // answered from its state alone and nothing is signalled.
        //
        // The gap between this read and the `kill` below is a real race: a
        // task can exit inside it and be reported as stopped. That window is
        // not closable from here — it would need the registry to check state
        // and take the killer under one lock — and it is admitted rather
        // than hidden.
        let state = task.state();
        if state.finished() {
            let phrase = state_phrase(&state);
            return Ok(ToolOutcome::new(format!(
                "{} ({}): nothing was signalled — it had already finished: {phrase}. Its \
                 output is still readable with BashOutput.",
                task.id, task.label
            ))
            .with_display(format!("{}: already finished, nothing signalled", task.id)));
        }

        if task.kill() {
            return Ok(ToolOutcome::new(format!(
                "{} ({}): stopped — the shell was signalled and is now recorded as killed. \
                 Not stopped: anything it spawned that detached. Emma signals the shell it \
                 started, never a process tree, so a server or daemon this task launched is \
                 still running and must be stopped by its own mechanism. Output produced \
                 before the kill is still readable with BashOutput.",
                task.id, task.label
            ))
            .with_display(format!("{}: killed (children it spawned are not)", task.id)));
        }

        // Running, but `kill` had nothing to fire: the spawner never
        // registered a killer. The task is genuinely still going and this
        // call genuinely did nothing, which is a failure of the machinery —
        // not "already finished", and saying that instead would be the exact
        // plausible success the state check above exists to prevent.
        Err(ToolError::Failed(format!(
            "{} ({}) is still running, but no way to stop it was registered when it was \
             spawned; nothing was signalled and it is still running",
            task.id, task.label
        )))
    }
}

// endregion: The kill, and its three honest outcomes

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The killer is a closure over an AtomicBool, so "was anything actually
// signalled" is observable directly rather than inferred from the wording.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use emma_tool_api::background::{Registry, TaskState};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn ctx_for(session: &str, reg: &Registry) -> ToolCtx {
        ToolCtx {
            cwd: std::env::temp_dir(),
            session_id: session.into(),
            turn_id: "t1".into(),
            background: reg.clone(),
        }
    }

    async fn call(ctx: &ToolCtx, id: &str) -> Result<ToolOutcome, ToolError> {
        KillShell::new()
            .invoke(ctx, json!({ "bash_id": id }))
            .await
            .expect("KillShell must never end the turn")
    }

    /// Arms a task with an observable killer. Returns the flag that records
    /// whether the killer actually fired.
    fn armed(task: &emma_tool_api::background::Task) -> Arc<AtomicBool> {
        let fired = Arc::new(AtomicBool::new(false));
        let f = fired.clone();
        task.on_kill(move || f.store(true, Ordering::SeqCst));
        fired
    }

    #[tokio::test]
    async fn killing_a_running_task_signals_it_and_says_what_survives() {
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "python -m http.server");
        let fired = armed(&task);

        let out = call(&ctx, &task.id).await.expect("a running kill failed");
        assert!(
            fired.load(Ordering::SeqCst),
            "nothing was actually signalled"
        );
        assert_eq!(task.state(), TaskState::Killed);
        assert!(out.content.contains("stopped"), "{}", out.content);
        // The single most important sentence: the kill's blast radius is the
        // shell, and anything it spawned that detached survives.
        assert!(
            out.content.contains("never a process tree"),
            "the outcome implies a guarantee the kill does not provide: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn killing_a_finished_task_reports_that_and_signals_nothing() {
        // The killer is still registered when the task exits on its own —
        // exactly the case where a careless kill would fire it anyway and
        // stamp `Killed` over the real exit status.
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "echo done");
        let fired = armed(&task);
        task.push(b"done\n");
        task.set_state(TaskState::Exited(Some(0)));

        let out = call(&ctx, &task.id)
            .await
            .expect("an already-finished kill errored");
        assert!(!fired.load(Ordering::SeqCst), "a dead task was signalled");
        assert_eq!(
            task.state(),
            TaskState::Exited(Some(0)),
            "the real exit status was overwritten"
        );
        assert!(
            out.content.contains("already finished")
                && out.content.contains("exited with status 0"),
            "{}",
            out.content
        );
        assert!(
            !out.content.contains("stopped —"),
            "a no-op was reported as a kill: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn a_second_kill_reports_already_killed_not_stopped_again() {
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "sleep 100");
        armed(&task);

        let first = call(&ctx, &task.id).await.unwrap();
        assert!(first.content.contains("stopped"), "{}", first.content);

        let second = call(&ctx, &task.id).await.unwrap();
        assert!(
            second.content.contains("already finished")
                && second.content.contains("killed by KillShell"),
            "the second kill claimed to have stopped something: {}",
            second.content
        );
    }

    #[tokio::test]
    async fn a_running_task_with_no_killer_is_a_loud_failure_not_a_shrug() {
        // A spawner that never registered a killer is a wiring defect. The
        // honest report is "still running, could not be stopped" — either
        // success sentence would be a lie.
        let reg = Registry::new();
        let ctx = ctx_for("s1", &reg);
        let task = reg.spawn("s1", "orphan");

        let err = call(&ctx, &task.id)
            .await
            .expect_err("an impossible kill succeeded");
        assert_eq!(err.kind(), "tool_failed");
        assert!(err.detail().contains("still running"), "{}", err.detail());
        assert_eq!(task.state(), TaskState::Running);
    }

    #[tokio::test]
    async fn unknown_and_cross_session_ids_both_refuse_and_name_what_exists() {
        let reg = Registry::new();
        let mine = reg.spawn("s1", "cargo build");
        armed(&mine);
        let theirs = reg.spawn("s2", "their server");
        let theirs_fired = armed(&theirs);

        let ctx = ctx_for("s1", &reg);
        let err = call(&ctx, "bash_99")
            .await
            .expect_err("a guessed id was honoured");
        assert_eq!(err.kind(), "bad_arguments");
        assert!(err.detail().contains(&mine.id), "{}", err.detail());

        // Another session's task must be unkillable from here, and the
        // refusal must read exactly like a nonexistent id.
        let err2 = call(&ctx, &theirs.id)
            .await
            .expect_err("cross-session kill succeeded");
        assert_eq!(err2.kind(), "bad_arguments");
        assert!(
            !theirs_fired.load(Ordering::SeqCst),
            "another session's task was signalled"
        );
        assert!(
            !err2.detail().contains("their server"),
            "the refusal leaked another session's task: {}",
            err2.detail()
        );
    }
}

// endregion: Tests
