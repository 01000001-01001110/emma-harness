//! The loop: send, receive, run tools, repeat, and hold the goal across turns.
//!
//! Four properties govern this file, and each of them is here because it was
//! paid for somewhere else first.
//!
//! **A tool failure is a `tool_result`, never an abort.** Every failure class —
//! unknown tool, bad arguments, `ToolError`, hook denial, refused approval —
//! becomes a result block with `is_error` set, and the loop continues. In
//! tustle-agent this was measured: five of eight failure classes were ending
//! turns silently, the user saw "Something went wrong on my side", and the
//! model never learned anything had failed.
//!
//! **A call that failed is not repeated identically until something else has
//! succeeded.** Not "never again this turn", which is affordable only when the
//! whole tool surface is one read-only search. Emma's working rhythm is *run
//! the tests, see them fail, fix a file, run the tests again* — a memo keyed on
//! the call alone would forbid the second run, which is the one that proves the
//! fix. So the memo clears the moment any tool succeeds: an identical retry is
//! refused only when literally nothing has changed since it failed, which is
//! the case where retrying is a loop rather than a step.
//!
//! That rule also absorbed the `Bash` contract change without an edit. A
//! non-zero exit used to be `ToolError::Failed`, so `grep -q` answering "no"
//! read as a broken shell; it is now `Ok` with `exit status <n>` carried in the
//! content. Nothing here moved, because nothing here branches on a tool's
//! identity, on `ToolError::kind`, or on exit status: `Ok` is success, so those
//! calls simply stopped entering the memo and stopped being painted as errors
//! on the terminal. **Exit status is not a failure signal anywhere in this
//! file** — `ToolError` is the only one, and adding a second would re-introduce
//! precisely the bug the contract change removed.
//!
//! **`raw_content` is echoed back verbatim.** The assistant message pushed into
//! `query` is the provider's own content array. Reassembling it from `text` and
//! `tool_calls` invalidates thinking-block signatures and the next call is
//! rejected.
//!
//! **Budgets are folded and enforced, and aborting costs what it spent.** Usage
//! is added from every model call and recorded before the budget is tested, so
//! a run that dies on the token cap is recorded having spent the tokens it
//! spent. In tustle-agent the counter only reached the log on a completed turn,
//! which made aborting the cheapest way to spend money.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use emma_harness::{Harness, HookCall, HookEvent, HookResult};
use emma_llm::{AssistantTurn, Caching, Event, LlmError, Message, Mode, Provider, Request, ToolCall};
use emma_tool_api::{Registry, ToolCtx};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::approval::{Approvals, Verdict};
use crate::goal::{self, Done, DoneCheck, Goal};
use crate::session::SessionLog;
use crate::term::Term;

// region: Budgets and endings
// ---------------------------------------------------------------------------
// Budgets and endings
//
// What a run is allowed to spend, and every way it is allowed to stop. Kept
// together because they are two halves of one statement: each budget has an
// ending that names it, and every ending is reported to the user in words.
// ---------------------------------------------------------------------------

/// Everything a run is bounded by.
///
/// Three of them because they fail differently: a model calling one cheap tool
/// forever is caught by iterations and not by tokens, a model reading enormous
/// files is caught by tokens and not by iterations, and a tool waiting on a
/// network that will never answer is caught by neither.
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
    pub max_iterations: u32,
    /// Billable tokens across the whole goal — see `Usage::billable_total_tokens`,
    /// never the bare `input_tokens`, which reports only the uncached remainder
    /// and under-counts a cached turn by up to ~10x.
    pub max_tokens: i64,
    pub wall_clock: Duration,
    pub max_kicks: u32,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_iterations: 60,
            max_tokens: 500_000,
            wall_clock: Duration::from_secs(30 * 60),
            max_kicks: 3,
        }
    }
}

/// Why the loop stopped. Every variant is reported to the user in words; none
/// of them is silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// Done-detection said the goal is met.
    Done,
    /// The kick budget is spent and the goal is still not proven met.
    KicksExhausted,
    /// The model stopped twice with no tool call in between. It has answered
    /// the kick; asking again is the loop arguing with itself.
    Stalled,
    Iterations,
    Tokens,
    Deadline,
    Interrupted,
    /// The provider failed in a way retrying will not fix.
    Provider(String),
}

impl Ending {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::KicksExhausted => "kicks_exhausted",
            Self::Stalled => "stalled",
            Self::Iterations => "iterations",
            Self::Tokens => "tokens",
            Self::Deadline => "deadline",
            Self::Interrupted => "interrupted",
            Self::Provider(_) => "provider_error",
        }
    }

    /// What the user is told. Named the limit that fired, because "I stopped"
    /// without a reason is indistinguishable from a crash.
    pub fn message(&self, b: &Budgets) -> String {
        match self {
            Self::Done => "goal complete".into(),
            Self::KicksExhausted => format!(
                "stopped: the goal was not confirmed done after {} nudges. What was changed is \
                 on disk; nothing was rolled back.",
                b.max_kicks
            ),
            Self::Stalled => "stopped: the model stopped twice without using a tool, so it has \
                 nothing further to do here."
                .into(),
            Self::Iterations => format!("stopped: hit the {} model-call limit.", b.max_iterations),
            Self::Tokens => format!("stopped: hit the {} token budget.", b.max_tokens),
            Self::Deadline => format!(
                "stopped: hit the {}s time limit.",
                b.wall_clock.as_secs()
            ),
            Self::Interrupted => "interrupted. The partial turn is in the session log.".into(),
            Self::Provider(e) => format!("stopped: {e}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub ending: Ending,
    /// The last thing the assistant said.
    pub text: String,
    pub tokens: i64,
    pub iterations: u32,
    pub kicks: u32,
}

// endregion: Budgets and endings

// region: Ctrl-C
// ---------------------------------------------------------------------------
// Ctrl-C
//
// The one thing besides a budget that ends a goal, and the only piece of this
// file that is shared with a signal handler.
// ---------------------------------------------------------------------------

/// Ctrl-C, shared between the signal handler and everything that can be waiting.
///
/// A flag as well as a notification: the flag is what an iteration boundary
/// tests, and the notification is what cuts a model call that is thirty seconds
/// into a sixty-second answer. `notify_one` rather than `notify_waiters` so a
/// signal that arrives in the gap between two waits still lands — the permit is
/// held until somebody collects it.
#[derive(Default)]
pub struct Interrupt {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Interrupt {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn trip(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    pub fn tripped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        loop {
            if self.tripped() {
                return;
            }
            self.notify.notified().await;
        }
    }

    /// Wire the real signal. Not done in `new` so a test never installs a
    /// process-wide handler.
    pub fn install(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                me.trip();
            }
        });
    }
}

// endregion: Ctrl-C

// region: The loop
// ---------------------------------------------------------------------------
// The loop
//
// Everything the module doc is about: send, receive, run tools, repeat. The
// four properties are enforced across `run_goal` and `run_tool_call` — read
// those two together, because the memo one is split between them.
// ---------------------------------------------------------------------------

pub struct Setup<'a> {
    pub provider: &'a dyn Provider,
    pub harness: &'a Harness,
    pub tools: &'a Registry,
    pub approvals: &'a Approvals,
    pub log: &'a SessionLog,
    pub term: &'a Term,
    pub interrupt: Arc<Interrupt>,
    /// How this run decides a goal is met. One implementation today; the loop
    /// knows only the trait, so swapping in a task-list or check-command
    /// authority is a different value here rather than an edit below.
    pub done: &'a dyn DoneCheck,
    pub cwd: PathBuf,
    pub session_id: String,
    pub budgets: Budgets,
    pub caching: Caching,
    pub mode: Mode,
}

pub struct Agent<'a> {
    s: Setup<'a>,
    /// Completed goals, collapsed to the goal and the answer.
    ///
    /// Not the tool traffic. A `tool_use` block in history with no matching
    /// result is rejected by the API, and carrying the pairs would put a whole
    /// previous goal's file contents ahead of this one's query — after the
    /// cached prefix, where every byte is paid for at full price on every call.
    history: Vec<Message>,
    turn_seq: u64,
}

impl<'a> Agent<'a> {
    pub fn new(s: Setup<'a>) -> Self {
        Self {
            s,
            history: Vec::new(),
            turn_seq: 0,
        }
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub async fn run_goal(&mut self, goal: &Goal) -> Outcome {
        let started = Instant::now();
        self.s.log.append(
            "goal",
            json!({
                "session_id": self.s.session_id,
                "text": goal.text,
                "instructions_hash": self.s.harness.instructions_hash(),
                "tool_schema_hash": self.s.tools.schema_hash(),
                "model": self.s.provider.model_id(),
                "done_check": self.s.done.name(),
            }),
        );
        self.s.term.goal_started(&goal.text);

        let tool_defs = self.s.tools.wire_definitions();
        let mut query: Vec<Message> = vec![Message::user(goal.opening(self.s.done))];
        let mut tokens = 0i64;
        let mut iterations = 0u32;
        let mut kicks = 0u32;
        let mut tool_calls_since_kick = 0u32;
        // Cleared whenever any tool succeeds — see the module comment. This is
        // "nothing has changed since this failed", not "this failed once".
        let mut failed_now: HashSet<String> = HashSet::new();
        // Everything that failed at any point, for the kick to quote. Capped so
        // a long goal cannot turn the kick into a transcript.
        let mut failed_ever: Vec<String> = Vec::new();
        let mut last_text = String::new();

        let ending = loop {
            if self.s.interrupt.tripped() {
                break Ending::Interrupted;
            }
            if started.elapsed() > self.s.budgets.wall_clock {
                break Ending::Deadline;
            }
            if iterations >= self.s.budgets.max_iterations {
                break Ending::Iterations;
            }

            self.turn_seq += 1;
            let turn_id = format!("turn-{}", self.turn_seq);

            let request = Request {
                instructions: self.s.harness.instructions.clone(),
                tools: tool_defs.clone(),
                history: self.history.clone(),
                query: query.clone(),
                max_tokens: 32_000,
                effort: emma_llm::Effort::XHigh,
                caching: self.s.caching,
            };

            let turn = match self.call_model(request).await {
                Ok(Some(turn)) => turn,
                Ok(None) => break Ending::Interrupted,
                Err(e) => break Ending::Provider(crate::commands::rename_auth(&e.to_string())),
            };
            iterations += 1;

            // Recorded before the budget is tested, so an abort costs what it
            // spent rather than nothing.
            tokens += turn.usage.billable_total_tokens();
            self.s.log.append(
                "model_call",
                json!({
                    "turn_id": turn_id,
                    "iteration": iterations,
                    "stop_reason": turn.stop_reason,
                    "input_tokens": turn.usage.input_tokens,
                    "output_tokens": turn.usage.output_tokens,
                    "cache_creation_input_tokens": turn.usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": turn.usage.cache_read_input_tokens,
                    "billable_total_tokens": turn.usage.billable_total_tokens(),
                    "goal_total_so_far": tokens,
                }),
            );
            if !turn.text.trim().is_empty() {
                last_text = turn.text.clone();
                self.s
                    .log
                    .append("assistant", json!({ "turn_id": turn_id, "text": turn.text }));
            }
            if tokens > self.s.budgets.max_tokens {
                break Ending::Tokens;
            }

            if turn.tool_calls.is_empty() {
                // The model stopped. Everything from here to `continue` is the
                // goal being held rather than a conversation ending.
                let why = match self.s.done.verdict(goal, &turn.text).await {
                    Done::Yes => break Ending::Done,
                    Done::No(why) => why,
                };
                // The model has answered the kick without doing anything. A
                // third attempt is the loop arguing with itself.
                if tool_calls_since_kick == 0 && kicks > 0 {
                    break Ending::Stalled;
                }
                if kicks >= self.s.budgets.max_kicks {
                    break Ending::KicksExhausted;
                }
                kicks += 1;
                tool_calls_since_kick = 0;
                self.s.log.append(
                    "kick",
                    json!({ "turn_id": turn_id, "n": kicks, "why": why }),
                );
                self.s.term.note(&format!(
                    "not done yet — nudge {kicks}/{}",
                    self.s.budgets.max_kicks
                ));
                query.push(Message::assistant(turn.raw_content.clone()));
                query.push(Message::user(goal::kick(goal, &why, &tail(&failed_ever, 5))));
                continue;
            }

            // `raw_content` verbatim. Never rebuilt from `text` + `tool_calls`.
            query.push(Message::assistant(turn.raw_content.clone()));

            let mut results = Vec::new();
            for call in &turn.tool_calls {
                tool_calls_since_kick += 1;
                let (block, succeeded, label) = self
                    .run_tool_call(call, &turn_id, &failed_now)
                    .await;
                if succeeded {
                    // Something changed, so an earlier failure is worth trying
                    // again. This is what keeps edit-then-rerun-the-tests from
                    // being blocked by the memo.
                    failed_now.clear();
                } else if let Some(label) = label {
                    failed_now.insert(memo_key(call));
                    if !failed_ever.contains(&label) {
                        failed_ever.push(label);
                    }
                }
                results.push(block);
            }
            query.push(Message::tool_results(results));
        };

        let outcome = Outcome {
            ending,
            text: last_text,
            tokens,
            iterations,
            kicks,
        };
        self.s.log.append(
            "goal_finished",
            json!({
                "ending": outcome.ending.as_str(),
                "detail": match &outcome.ending { Ending::Provider(e) => Some(e.clone()), _ => None },
                "tokens": outcome.tokens,
                "iterations": outcome.iterations,
                "kicks": outcome.kicks,
                "elapsed_ms": started.elapsed().as_millis() as u64,
            }),
        );

        // The goal joins the history whatever the ending: a run that hit its
        // token budget still happened, and the next goal in the session must
        // not be told a story in which it did not.
        self.history.push(Message::user(goal.text.clone()));
        if !outcome.text.trim().is_empty() {
            self.history
                .push(Message::assistant(Value::String(outcome.text.clone())));
        }
        outcome
    }

    /// One model call, with streaming and Ctrl-C. `Ok(None)` is an interrupt.
    async fn call_model(&self, request: Request) -> Result<Option<AssistantTurn>, LlmError> {
        let (tx, mut rx) = mpsc::channel::<Event>(64);
        let mut send = Box::pin(self.s.provider.send(request, self.s.mode, Some(tx)));
        let mut streamed = false;
        let mut closed = false;

        let turn = loop {
            tokio::select! {
                biased;
                _ = self.s.interrupt.wait() => return Ok(None),
                // Disabled once the channel closes, which happens when `send`
                // drops its sender. Without the guard this branch is
                // permanently ready and starves the one below it.
                ev = rx.recv(), if !closed => match ev {
                    Some(ev) => { streamed |= self.on_event(ev); }
                    None => closed = true,
                },
                turn = &mut send => break turn?,
            }
        };
        while let Ok(ev) = rx.try_recv() {
            streamed |= self.on_event(ev);
        }
        if streamed {
            self.s.term.end_of_text();
        } else {
            self.s.term.text(&turn.text);
        }
        Ok(Some(turn))
    }

    /// Returns whether anything was written to the assistant's line.
    fn on_event(&self, ev: Event) -> bool {
        match ev {
            Event::TextDelta(text) => {
                self.s.term.delta(&text);
                true
            }
            Event::ToolUseStarted { .. } => false,
            Event::Retrying {
                attempt,
                max_attempts,
                delay,
                reason,
            } => {
                // A retry the user cannot see is indistinguishable from a hang.
                self.s.term.warn(&format!(
                    "retrying ({attempt}/{max_attempts}) in {}ms: {reason}",
                    delay.as_millis()
                ));
                false
            }
        }
    }

    /// Everything that happens to one `tool_use` block.
    ///
    /// Returns the result block, whether the tool actually ran and succeeded,
    /// and a label for the kick when it did not.
    async fn run_tool_call(
        &self,
        call: &ToolCall,
        turn_id: &str,
        failed_now: &HashSet<String>,
    ) -> (Value, bool, Option<String>) {
        let label = format!("{}({})", call.name, compact(&call.input));
        let fail = |kind: &str, detail: String| {
            (
                failure_block(&call.id, &call.name, kind, &detail),
                false,
                Some(label.clone()),
            )
        };

        let Some(tool) = self.s.tools.get(&call.name) else {
            self.s.log.append(
                "tool_unknown",
                json!({ "turn_id": turn_id, "tool": call.name, "id": call.id }),
            );
            return fail(
                "no_such_tool",
                format!(
                    "`{}` is not available. Available: {}",
                    call.name,
                    self.s.tools.names().join(", ")
                ),
            );
        };

        if let Err(e) = tool.validate_args(&call.input) {
            return fail(e.kind(), e.detail().to_string());
        }

        if failed_now.contains(&memo_key(call)) {
            return fail(
                "already_failed",
                format!(
                    "this exact {} call already failed and nothing has changed since, so it \
                     was not run again. Change the arguments or take a different route.",
                    call.name
                ),
            );
        }

        self.s.term.tool_started(&call.name, &call.input);
        self.s.log.append(
            "tool_call",
            json!({ "turn_id": turn_id, "id": call.id, "tool": call.name, "args": call.input }),
        );

        // Policy before convenience. A `PreToolUse` denial is not something the
        // person at the keyboard gets a vote on, so it is resolved before they
        // are asked — a gate a human can wave through is advice.
        let hook_call = HookCall {
            tool_name: &call.name,
            tool_call_id: &call.id,
            args: &call.input,
            session_id: &self.s.session_id,
            turn_id,
            result: None,
        };
        let pre = self.s.harness.run_hooks(HookEvent::PreToolUse, &hook_call).await;
        self.log_hooks(turn_id, &pre.runs);
        if let Some(reason) = pre.denied {
            self.s.term.warn(&format!("{} blocked by policy: {reason}", call.name));
            self.s.log.append(
                "denied",
                json!({ "turn_id": turn_id, "id": call.id, "by": "hook", "reason": reason }),
            );
            return fail(
                "blocked_by_policy",
                format!("{reason} This is configured policy and cannot be approved away."),
            );
        }

        match self.s.approvals.request(tool.as_ref(), &call.input, self.s.term).await {
            Verdict::Allow => {}
            Verdict::Deny(reason) => {
                self.s.log.append(
                    "denied",
                    json!({ "turn_id": turn_id, "id": call.id, "by": "user", "reason": reason }),
                );
                return fail("not_approved", reason);
            }
        }

        let ctx = ToolCtx {
            cwd: self.s.cwd.clone(),
            session_id: self.s.session_id.clone(),
            turn_id: turn_id.to_string(),
        };
        let outcome = match tool.invoke(&ctx, call.input.clone()).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => {
                // The line this whole design exists for. A typed failure is an
                // observation the model acts on; the goal continues.
                self.s.log.append(
                    "tool_failed",
                    json!({ "turn_id": turn_id, "id": call.id, "tool": call.name,
                            "kind": e.kind(), "detail": e.detail() }),
                );
                self.s.term.tool_result(None, e.detail(), true);
                self.run_post_hooks(turn_id, call, e.detail(), false, Some(e.kind())).await;
                return fail(e.kind(), e.detail().to_string());
            }
            // The outer `Result` is the tool claiming the session cannot
            // continue. Even that is not allowed to abort silently: it becomes
            // a result the model can read, and the loop's own budgets remain
            // the only thing that ends a goal.
            Err(fault) => {
                self.s.log.append(
                    "tool_fault",
                    json!({ "turn_id": turn_id, "id": call.id, "tool": call.name,
                            "detail": fault.to_string() }),
                );
                self.s.term.tool_result(None, &fault.to_string(), true);
                return fail("tool_fault", fault.to_string());
            }
        };

        let mut content = outcome.content.clone();
        if outcome.truncated {
            content.push_str(
                "\n[truncated: this is not the whole output. Narrow the request if you need \
                 the rest.]",
            );
        }
        let post = self
            .run_post_hooks(turn_id, call, &content, outcome.truncated, None)
            .await;
        for extra in post {
            content.push_str("\n\n");
            content.push_str(&extra);
        }

        self.s
            .term
            .tool_result(outcome.display.as_deref(), &content, false);
        self.s.log.append(
            "tool_result",
            json!({ "turn_id": turn_id, "id": call.id, "tool": call.name,
                    "content": content, "truncated": outcome.truncated }),
        );
        // Anthropic's wire shape, built here rather than by the provider — as is
        // the one in `failure_block`. That is the whole of what makes this loop
        // Anthropic-only: OpenAI expresses a result as a separate message with
        // `role: "tool"` and a `tool_call_id`, not as a block inside a user
        // message, so a second provider is a refactor of these two sites plus
        // the `raw_content` passthrough, not a new file beside `anthropic.rs`.
        // Note that `raw_content` cannot simply be deleted in that refactor: it
        // exists because thinking-block signatures do not survive reassembly, so
        // the loop has to keep handing back bytes it does not interpret.
        (
            json!({ "type": "tool_result", "tool_use_id": call.id, "content": content }),
            true,
            None,
        )
    }

    async fn run_post_hooks(
        &self,
        turn_id: &str,
        call: &ToolCall,
        content: &str,
        truncated: bool,
        error: Option<&str>,
    ) -> Vec<String> {
        let hook_call = HookCall {
            tool_name: &call.name,
            tool_call_id: &call.id,
            args: &call.input,
            session_id: &self.s.session_id,
            turn_id,
            result: Some(HookResult {
                content,
                truncated,
                error,
            }),
        };
        let verdict = self
            .s
            .harness
            .run_hooks(HookEvent::PostToolUse, &hook_call)
            .await;
        self.log_hooks(turn_id, &verdict.runs);
        verdict.context
    }

    fn log_hooks(&self, turn_id: &str, runs: &[emma_harness::HookRun]) {
        for run in runs {
            self.s
                .log
                .append("hook", json!({ "turn_id": turn_id, "run": run }));
        }
    }

}

// endregion: The loop

// region: The memo key, and what the model is shown
// ---------------------------------------------------------------------------
// The memo key, and what the model is shown
//
// Four small functions the loop leans on: what makes two calls identical, the
// shape a failure takes on the wire, and two ways of shortening an argument
// so it fits somewhere it has to fit.
// ---------------------------------------------------------------------------

/// What makes two calls "the same call". Name plus arguments, canonically
/// rendered — so `Bash(ls)` and `Bash(ls)` collide and `Bash(ls a)` does not.
fn memo_key(call: &ToolCall) -> String {
    format!("{}\u{1}{}", call.name, call.input)
}

fn failure_block(id: &str, tool: &str, kind: &str, detail: &str) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": id,
        // `is_error` as well as the kind in the body: the flag is what the API
        // and the model's own training key on, and the body is what says which
        // failure it was and what to do instead.
        "is_error": true,
        "content": json!({ "kind": kind, "tool": tool, "detail": detail }).to_string(),
    })
}

/// Arguments, short enough to sit inside a kick.
fn compact(args: &Value) -> String {
    let s = args
        .as_object()
        .and_then(|o| {
            o.get("command")
                .or_else(|| o.get("file_path"))
                .or_else(|| o.get("pattern"))
        })
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| args.to_string());
    let s = s.replace('\n', " ");
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s
    }
}

fn tail(v: &[String], n: usize) -> Vec<String> {
    v.iter().rev().take(n).rev().cloned().collect()
}

// endregion: The memo key, and what the model is shown

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The pure pieces only. Everything that needs a provider, a harness and a
// registry is driven end to end from `tests/loop.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_block_is_flagged_and_typed() {
        let b = failure_block("tu_1", "Bash", "tool_failed", "exited with 1");
        assert_eq!(b["is_error"], true);
        let body: Value = serde_json::from_str(b["content"].as_str().unwrap()).unwrap();
        assert_eq!(body["kind"], "tool_failed");
        assert_eq!(body["tool"], "Bash");
    }

    #[test]
    fn the_memo_key_separates_different_arguments() {
        let mk = |args: Value| ToolCall {
            id: "x".into(),
            name: "Bash".into(),
            input: args,
        };
        assert_eq!(
            memo_key(&mk(json!({ "command": "ls" }))),
            memo_key(&mk(json!({ "command": "ls" })))
        );
        assert_ne!(
            memo_key(&mk(json!({ "command": "ls" }))),
            memo_key(&mk(json!({ "command": "ls -a" })))
        );
    }

    #[test]
    fn every_ending_says_which_limit_fired() {
        let b = Budgets::default();
        for ending in [
            Ending::KicksExhausted,
            Ending::Iterations,
            Ending::Tokens,
            Ending::Deadline,
        ] {
            let m = ending.message(&b);
            assert!(
                m.chars().any(|c| c.is_ascii_digit()),
                "{ending:?} does not name its limit: {m}"
            );
        }
    }
}

// endregion: Tests
