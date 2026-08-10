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
use emma_llm::{
    AssistantTurn, Caching, Event, LlmError, Message, Mode, Provider, Request, Role, ToolCall,
};
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
    /// A ceiling, not a tripwire: tested before the call, so a run never makes
    /// more than this many. The asymmetry with `max_tokens` below is real and
    /// is why the two are worded differently to the user.
    pub max_iterations: u32,
    /// Billable tokens across the whole goal — see `Usage::billable_total_tokens`,
    /// never the bare `input_tokens`, which reports only the uncached remainder
    /// and under-counts a cached turn by up to ~10x.
    ///
    /// **A tripwire, not a ceiling.** It is tested *after* each call, so the
    /// call that crosses it is paid for in full: a 500,000 budget stopped a
    /// real run at 579,565, and the overshoot is bounded only by how large one
    /// request can be. The name reads like a limit and the behaviour is not
    /// one, so [`Ending::Tokens`] says which it is in words.
    ///
    /// Making it a true ceiling would mean gating on an estimate of the request
    /// before sending it, and the only estimator here is the packer's
    /// `chars / 4`, documented in `emma-llm` as under-counting what the
    /// provider bills. A gate built on it is wrong in both directions — it
    /// refuses calls that would have fit, and still overshoots on the ones it
    /// lets through — while *sounding* exact. An honest tripwire beats a
    /// dishonest ceiling, and the number the user is shown is the number that
    /// was actually spent either way.
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
    /// The model answered and never attempted any work.
    ///
    /// Not a failure, and the distinction is the whole point. A goal-holding
    /// loop that treats "hello" as a goal spends a model call discovering that
    /// nobody was working, then tells the user their perfectly good exchange
    /// stalled. A turn that used no tool, made no claim of completion, and
    /// followed no tool use anywhere in the goal is a *conversational* turn:
    /// there is no work in flight to nudge back into motion, and a kick would
    /// be the loop asking a question nobody asked it to hold.
    ///
    /// The line between this and [`Stalled`](Self::Stalled) is tool use, not
    /// wording. A model that ran a command, stopped, was kicked, and stopped
    /// again having done nothing did abandon work in progress, and that is
    /// still a stall.
    Answered,
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
            Self::Answered => "answered",
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
            // Deliberately not phrased as a stop. Nothing went wrong, nothing
            // was abandoned, and the answer itself is already on the screen
            // above this line — so this says what kind of turn it was and
            // claims nothing else.
            Self::Answered => {
                "answered — no tools were needed, so there was no goal to hold.".into()
            }
            Self::Iterations => format!("stopped: hit the {} model-call limit.", b.max_iterations),
            // Deliberately not "hit the N token budget", which is what it used
            // to say and which reads as "stopped at N". It does not stop at N.
            // See `Budgets::max_tokens`: the check runs after the call, so the
            // spend reported beside this sentence is larger — by 79,565 on the
            // run that prompted the rewording — and a sentence that implies
            // otherwise contradicts the number printed next to it.
            Self::Tokens => format!(
                "stopped: went past the {} token budget, which is checked after each call — so \
                 the call that crossed it is included in the total below.",
                b.max_tokens
            ),
            Self::Deadline => format!("stopped: hit the {}s time limit.", b.wall_clock.as_secs()),
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

// region: What a resumed run inherits
// ---------------------------------------------------------------------------
// What a resumed run inherits
//
// Everything `--resume` carries across a process boundary. It sits next to
// `Budgets` on purpose: four of these six fields are the *spent* half of the
// budgets above, and a resume that omitted them would not be a resume with a
// gap in it — it would be a cap that can be reset by pressing Ctrl-C.
// ---------------------------------------------------------------------------

/// The state of an interrupted goal, folded back out of its session file.
///
/// **Why the counters are here at all.** `session::fold` returns the messages
/// and nothing else, and a resumed run that restarted `tokens`, `iterations`
/// and `kicks` at zero would silently grant a fresh budget: a goal that ended
/// on [`Ending::Tokens`] could then be resumed indefinitely and the cap it hit
/// would mean nothing. So the meter comes back with the conversation, or the
/// feature is a way to spend past a limit.
///
/// **Why the memo is here too.** `failed_now` is the loop's "this exact call
/// failed and nothing has changed since". Dropping it does not overspend
/// anything, but it hands the resumed model back the identical failing call as
/// its most obvious next move, which is the specific loop the memo exists to
/// break.
///
/// `Default` is the not-resuming case, and it is the one the loop takes on
/// every ordinary run: an empty conversation and every counter at zero.
#[derive(Debug, Default, Clone)]
pub struct Resumed {
    /// The message list the interrupted run last sent, in order. Placed whole
    /// into `query` — see `Agent::run_goal`.
    pub messages: Vec<Message>,
    pub tokens: i64,
    pub iterations: u32,
    pub kicks: u32,
    /// Memo keys — see [`memo_key`] — for calls that had failed with nothing
    /// having succeeded since.
    pub failed_now: Vec<String>,
    /// Human-readable labels for everything that failed during the goal, which
    /// is what a kick quotes back.
    pub failed_ever: Vec<String>,
}

// endregion: What a resumed run inherits

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
    /// Consumed by the first `run_goal` and never again: what it carries is one
    /// interrupted goal's conversation and one interrupted goal's spend, and a
    /// second goal typed at the prompt afterwards is a new goal with its own
    /// budget, exactly as it would be without a resume.
    resumed: Option<Resumed>,
}

impl<'a> Agent<'a> {
    pub fn new(s: Setup<'a>) -> Self {
        Self {
            s,
            history: Vec::new(),
            turn_seq: 0,
            resumed: None,
        }
    }

    /// Continue a session rather than start one.
    ///
    /// A builder method rather than a `Setup` field because it is the rare case
    /// — every other caller would have written `resumed: None` — and because
    /// the thing it takes is produced by a fallible read of a file, which is a
    /// step the caller has to have taken before it can construct `Setup` at
    /// all.
    pub fn resuming(mut self, resumed: Resumed) -> Self {
        self.resumed = Some(resumed);
        self
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub async fn run_goal(&mut self, goal: &Goal) -> Outcome {
        let started = Instant::now();
        // Composed before it is logged, because the record carries the opening
        // message verbatim as well as the goal text. `opening` is built from the
        // `DoneCheck` in force, and the log names that check but does not hold
        // it — so a fold that tried to re-derive this string would have to keep
        // a name-to-`impl` table in step with `goal.rs` forever. Storing the
        // bytes costs a few hundred of them once per goal.
        let opening = goal.opening();
        // Taken before the record is written, so `resumed` is the only place a
        // resume can influence this goal and it can influence it exactly once.
        let resumed = self.resumed.take().unwrap_or_default();
        self.s.log.append(
            "goal",
            json!({
                "session_id": self.s.session_id,
                "text": goal.text,
                "opening": opening,
                // The one field written for a reader rather than for the fold.
                // Bare `--resume` means "the session I was last running *here*",
                // and a file that does not say where it ran cannot answer that —
                // so the working directory is recorded once per goal, which is
                // also the only place the answer could change within a session.
                "cwd": self.s.cwd.display().to_string(),
                "instructions_hash": self.s.harness.instructions_hash(),
                "tool_schema_hash": self.s.tools.schema_hash(),
                "model": self.s.provider.model_id(),
                "done_check": self.s.done.name(),
            }),
        );
        self.s.term.goal_started(&goal.text);

        let tool_defs = self.s.tools.wire_definitions();
        // The restored conversation, with this goal's opening on the end. All
        // of it goes in `query` and none of it in `history`: the split point is
        // not recorded in the file — `history` is what the loop had collapsed,
        // `query` is the goal in flight, and the fold returns one flat list —
        // and guessing it wrong is a message list the API refuses. The cost is
        // cache: `history` carries a breakpoint that would be byte-stable for
        // the rest of the session, and a restored prefix sitting in `query`
        // does not get it. That is a bill, and a wrong split is an outage.
        let resuming_a_goal_in_flight = !resumed.messages.is_empty();
        let mut query: Vec<Message> = open_query(resumed.messages, opening);
        let mut tokens = resumed.tokens;
        let mut iterations = resumed.iterations;
        let mut kicks = resumed.kicks;
        let mut tool_calls_since_kick = 0u32;
        // Whether *this* run has nudged. Deliberately not `kicks > 0`, which is
        // what the stall rule used to read: `kicks` is a budget and comes back
        // across a resume, but the stall rule is about a model answering a
        // nudge without doing anything, and a resumed run's last nudge was
        // answered by a human typing a new goal. Reading the budget here would
        // end every resumed run on its first stop, reported as `Stalled`.
        let mut kicked = false;
        // Whether this goal has ever attempted work. It is what separates a
        // conversational turn from a stall — see [`Ending::Answered`] — and it
        // is deliberately per *goal* rather than per turn: a model that ran a
        // command and then stopped talking has abandoned something, however
        // chatty the sentence it stopped on.
        //
        // A resumed run starts as though it had, because a resume only ever
        // continues a goal that was already in flight. The error this refuses
        // to make is the asymmetric one: mistaking a stall for an answer ends a
        // real goal early and silently, while mistaking an answer for a stall
        // costs one model call and is what the loop did before today.
        let mut worked = resuming_a_goal_in_flight;
        // Cleared whenever any tool succeeds — see the module comment. This is
        // "nothing has changed since this failed", not "this failed once".
        let mut failed_now: HashSet<String> = resumed.failed_now.into_iter().collect();
        // Everything that failed at any point, for the kick to quote — capped
        // here rather than at the use site, which is where the cap used to be
        // and where the comment used to claim this list was.
        //
        // `tail(&failed_ever, 5)` still bounds what a kick shows; what it did
        // not bound was the list, and the list is now written into a session
        // record and read back by `--resume`, so an unbounded collection is a
        // growing payload rather than a transient one. The oldest go first: a
        // kick's job is to stop the model repeating what it just tried, and a
        // failure twenty calls ago that has not recurred is the least likely to
        // be the one it is about to repeat.
        let mut failed_ever: Vec<String> = resumed.failed_ever;
        trim_oldest(&mut failed_ever);
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
                // The harness prompt, then the framing that is true of every
                // goal. It goes here rather than into the opening message
                // because a preamble on the user's words is an instruction
                // they did not write — and because identical bytes on every
                // call belong in the cached prefix, not in `query`.
                instructions: format!(
                    "{}{}",
                    self.s.harness.instructions,
                    goal::standing_contract(self.s.done)
                ),
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
                Err(e) => break Ending::Provider(e.to_string()),
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
            // One record per turn, whatever the turn contained — a turn that is
            // nothing but tool calls has empty text and used to be written
            // nowhere, which left the fold with a hole exactly where the tool
            // traffic is.
            //
            // `raw_content` is the provider's own array and is the only field
            // resume can use: rebuilding a turn from `text` invalidates
            // thinking-block signatures, so a fold that had to do that would
            // produce a message list the API rejects. `text` stays beside it
            // even though every byte of it is also inside the array, for two
            // reasons — the file is an audit trail somebody reads with `grep`,
            // where one plain line beats a JSON array of escaped blocks, and
            // the fold itself reads `text` to collapse a finished goal into the
            // one-line answer `history` carries between goals.
            if !turn.text.trim().is_empty() {
                last_text = turn.text.clone();
            }
            self.s.log.append(
                "assistant",
                json!({
                    "turn_id": turn_id,
                    "text": turn.text,
                    "raw_content": turn.raw_content,
                }),
            );
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
                if tool_calls_since_kick == 0 && kicked {
                    break Ending::Stalled;
                }
                // Nothing was ever attempted. There is no work in flight to
                // nudge back into motion, so this is a reply, and the run ends
                // reporting it rather than spending a model call to discover
                // that nobody was working.
                //
                // The order of these three is the whole rule and none of it is
                // spare. After the verdict, so a first turn that claims
                // completion is still `Done`. After the stall rule, so a model
                // that was already nudged is judged by the rule written about
                // being nudged. Which leaves this one meaning exactly what it
                // says: no tool has been called in this goal, and nothing has
                // been asked of the model that it has not already answered.
                if !worked {
                    break Ending::Answered;
                }
                if kicks >= self.s.budgets.max_kicks {
                    break Ending::KicksExhausted;
                }
                kicks += 1;
                kicked = true;
                tool_calls_since_kick = 0;
                // `text` is the composed message, not a second copy of `why`:
                // `why` is the reason for a human reading the file, and the
                // message is what the model was sent — the two differ by the
                // restated goal and the list of what has already failed, and it
                // is the message the fold has to put back.
                let kick_text = goal::kick(goal, &why, &tail(&failed_ever, 5));
                self.s.log.append(
                    "kick",
                    json!({ "turn_id": turn_id, "n": kicks, "why": why, "text": kick_text }),
                );
                self.s.term.note(&format!(
                    "not done yet — nudge {kicks}/{}",
                    self.s.budgets.max_kicks
                ));
                query.push(Message::assistant(turn.raw_content.clone()));
                query.push(Message::user(kick_text));
                continue;
            }

            // `raw_content` verbatim. Never rebuilt from `text` + `tool_calls`.
            query.push(Message::assistant(turn.raw_content.clone()));

            let mut results = Vec::new();
            for call in &turn.tool_calls {
                tool_calls_since_kick += 1;
                // Set on the attempt rather than on success: a model whose only
                // tool call was denied at the gate is still mid-task, and the
                // stop that follows is a stall to be nudged, not a greeting.
                worked = true;
                let (block, succeeded, label) =
                    self.run_tool_call(call, &turn_id, &failed_now).await;
                if succeeded {
                    // Something changed, so an earlier failure is worth trying
                    // again. This is what keeps edit-then-rerun-the-tests from
                    // being blocked by the memo.
                    failed_now.clear();
                } else if let Some(label) = label {
                    failed_now.insert(memo_key(call));
                    if !failed_ever.contains(&label) {
                        failed_ever.push(label);
                        trim_oldest(&mut failed_ever);
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
        let label = label_of(&call.name, &call.input);
        let fail = |kind: &str, detail: String| {
            let block = failure_block(&call.id, &call.name, kind, &detail);
            // A failure block is a block that was sent, so it is recorded under
            // the same kind as a successful one — the fold rebuilds the user
            // turn from these and would otherwise reconstruct a turn that
            // answers only the calls that worked, which the API rejects. The
            // record immediately above this one (`tool_failed`, `denied`,
            // `tool_unknown`, …) is the prose a human reads; this is the wire.
            self.log_result_block(turn_id, call, &block, None);
            (block, false, Some(label.clone()))
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
        let pre = self
            .s
            .harness
            .run_hooks(HookEvent::PreToolUse, &hook_call)
            .await;
        self.log_hooks(turn_id, &pre.runs);
        if let Some(reason) = pre.denied {
            self.s
                .term
                .warn(&format!("{} blocked by policy: {reason}", call.name));
            self.s.log.append(
                "denied",
                json!({ "turn_id": turn_id, "id": call.id, "by": "hook", "reason": reason }),
            );
            return fail(
                "blocked_by_policy",
                format!("{reason} This is configured policy and cannot be approved away."),
            );
        }

        match self
            .s
            .approvals
            .request(tool.as_ref(), &call.input, self.s.term)
            .await
        {
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
                self.run_post_hooks(turn_id, call, e.detail(), false, Some(e.kind()))
                    .await;
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
        // Anthropic's wire shape, built here rather than by the provider — as is
        // the one in `failure_block`. That is the whole of what makes this loop
        // Anthropic-only: OpenAI expresses a result as a separate message with
        // `role: "tool"` and a `tool_call_id`, not as a block inside a user
        // message, so a second provider is a refactor of these two sites plus
        // the `raw_content` passthrough, not a new file beside `anthropic.rs`.
        // Note that `raw_content` cannot simply be deleted in that refactor: it
        // exists because thinking-block signatures do not survive reassembly, so
        // the loop has to keep handing back bytes it does not interpret.
        let block = json!({ "type": "tool_result", "tool_use_id": call.id, "content": content });
        self.log_result_block(turn_id, call, &block, Some(outcome.truncated));
        (block, true, None)
    }

    /// The one record that says what answered a `tool_use`.
    ///
    /// It carries the block rather than the rendered content string, because
    /// the string alone cannot be turned back into a message: the block also
    /// carries the `tool_use_id` that pairs it with its call, and `is_error`
    /// when it failed. The content is still in there and still greppable — it
    /// is one JSON string deeper than it used to be, which is the price of the
    /// record being sufficient rather than merely readable. It is not stored
    /// twice; a tool's output is the largest thing in this file.
    fn log_result_block(
        &self,
        turn_id: &str,
        call: &ToolCall,
        block: &Value,
        truncated: Option<bool>,
    ) {
        self.s.log.append(
            "tool_result",
            json!({ "turn_id": turn_id, "id": call.id, "tool": call.name,
                    "block": block, "truncated": truncated }),
        );
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
    memo_key_of(&call.name, &call.input)
}

/// The same key from the two fields a session record has, which is not a
/// `ToolCall`. One definition rather than two: a resume that keyed the restored
/// memo differently from the loop would restore a set that never matches, and
/// the symptom would be a memo that silently does nothing.
pub(crate) fn memo_key_of(name: &str, input: &Value) -> String {
    format!("{name}\u{1}{input}")
}

/// The label a kick quotes, from the same two fields — see [`memo_key_of`] for
/// why this is shared rather than re-derived.
pub(crate) fn label_of(name: &str, input: &Value) -> String {
    format!("{}({})", name, compact(input))
}

/// The restored conversation with this goal's opening attached.
///
/// It cannot simply be pushed. The fold ends wherever the interrupted run
/// stopped, and that is usually a **user** turn — the tool results of the last
/// complete round, or the kick that answered a turn which called nothing. Two
/// user turns in a row is a 400 rather than a conversation, so the opening
/// joins the last one instead of following it: appended as a text block after
/// the result blocks when the content is an array, and concatenated when it is
/// plain text. Nothing is dropped to make room, because dropping the trailing
/// results would leave the `tool_use` above them unanswered, which is the other
/// 400.
///
/// With nothing restored — every ordinary run — the list is empty and this is
/// the `vec![Message::user(opening)]` it replaced.
fn open_query(mut restored: Vec<Message>, opening: String) -> Vec<Message> {
    match restored.last_mut() {
        Some(last) if last.role == Role::User => match &mut last.content {
            Value::String(text) => {
                text.push_str("\n\n");
                text.push_str(&opening);
            }
            Value::Array(blocks) => blocks.push(json!({ "type": "text", "text": opening })),
            other => *other = Value::String(opening),
        },
        _ => restored.push(Message::user(opening)),
    }
    restored
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

/// How many distinct failed calls a goal remembers. Four times what a kick
/// shows, so the cap is a bound on the collection rather than a second, quieter
/// version of the kick's own limit.
const FAILED_EVER_MAX: usize = 20;

/// Drop the oldest entries past the cap. See where the list is seeded for why
/// oldest-first is the right end to lose.
fn trim_oldest(v: &mut Vec<String>) {
    if v.len() > FAILED_EVER_MAX {
        v.drain(..v.len() - FAILED_EVER_MAX);
    }
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

    #[test]
    fn the_token_budget_is_described_as_the_tripwire_it_is() {
        // Written from a real run: 500,000 budget, 579,565 spent, reported as
        // "stopped: hit the 500000 token budget. — 9 calls, 579565 tokens".
        // Both numbers were true and the sentence between them was not: it
        // reads as a ceiling the run stopped at, and the run stopped 79,565
        // tokens past it because the check runs after the call that crossed it.
        //
        // The budget stays a tripwire — see `Budgets::max_tokens` for why the
        // alternative is worse — so this asserts on the only thing left, which
        // is that the wording admits it.
        let m = Ending::Tokens.message(&Budgets::default());
        assert!(m.contains("500000"), "the budget is not named: {m}");
        assert!(
            m.contains("after"),
            "the message does not say when the check runs, so it still reads as a ceiling: {m}"
        );
        // `iterations` is genuinely a ceiling — it is tested before the call —
        // and must keep saying so, because the two now word differently on
        // purpose and a tidy-up that unified them would make one of them lie.
        let i = Ending::Iterations.message(&Budgets::default());
        assert!(i.contains("limit"), "{i}");
    }
}

// endregion: Tests
