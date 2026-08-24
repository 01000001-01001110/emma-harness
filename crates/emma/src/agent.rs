//! The loop: send, receive, run tools, repeat, and hold the goal across turns.
//!
//! Five properties govern this file, and each of them is here because it was
//! paid for somewhere else first.
//!
//! **A session is one conversation, and a goal is a turn in it.** The loop used
//! to collapse each finished goal to the goal text and the answer, discarding
//! the tool traffic, and the second question about a file therefore arrived at a
//! model that no longer had the file — so it read it again, every time. The
//! traffic now stays. Two things pay for that: [`session::place_turn`], which is
//! the fold's own rule about never sending a `tool_use` nothing answers and is
//! shared rather than copied, and [`Agent::compact`], which applies the old
//! collapse to the oldest goals once a request passes
//! [`Budgets::max_context`]. The old behaviour is the fallback now, not the
//! policy.
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
//! **An assistant turn is echoed back exactly as it came.** The message pushed
//! into `query` is `AssistantTurn::content` — the provider's own blocks,
//! including the thinking blocks and their signatures. It is typed now rather
//! than a `serde_json::Value` blob, and the guarantee is unchanged: nothing here
//! rebuilds a turn from its text, because a re-derived signature is rejected on
//! the *next* call. See `emma_llm::content` for how a block this client cannot
//! model exactly still survives the round trip.
//!
//! **Budgets are folded and enforced, and aborting costs what it spent.** Usage
//! is added from every model call and recorded before the budget is tested, so
//! a run that dies on the token cap is recorded having spent the tokens it
//! spent. In tustle-agent the counter only reached the log on a completed turn,
//! which made aborting the cheapest way to spend money. What is *summed* is
//! [`cost_tokens`] rather than the provider's raw total, because a cached read
//! costs a tenth of what it weighs and a budget that counted it at full weight
//! would fire on the length of the conversation rather than on the bill — see
//! [`Budgets::max_tokens`], which is where that argument is written out.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use emma_harness::{Harness, HookCall, HookEvent, HookResult};
use emma_llm::{
    AssistantTurn, Caching, Content, ContentBlock, Event, LlmError, Message, Mode, Provider,
    Request, Role, ToolCall, ToolResult,
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
    /// What the goal is allowed to spend, in tokens weighted by what they cost
    /// — see [`cost_tokens`], never the bare `input_tokens`, which reports only
    /// the uncached remainder and under-counts a cached turn by up to ~10×.
    ///
    /// **Weighted, and it has to be now.** `Usage::billable_total_tokens` counts
    /// a cache read at full weight and a cache read is billed at 0.1×. That was
    /// harmless while a finished goal shrank to two lines of prose. It is not
    /// harmless now: a session carries its whole conversation, so at a 100,000
    /// token context every call charges ~100,000 against this cap and a 500,000
    /// default fires five calls into a barely-started goal — while the actual
    /// bill for those five calls is a few cents. A cap that fires on *how long
    /// the conversation is* rather than on spend is not what somebody typing
    /// `--max-tokens` is asking for, and it is the same failure this type has
    /// already had once: a limit expressed in the units of one policy, kept
    /// after the policy changed. The provider's own numbers are still logged
    /// raw; only the arithmetic that enforces this is weighted.
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
    /// How large one request's input may get before the conversation behind it
    /// is compacted — see [`Agent::compact`].
    ///
    /// **Not a per-goal budget; the only bound here that is about the session.**
    /// The other three reset when a goal does, which is right for spend and
    /// wrong for size: the conversation is what carries a follow-up question its
    /// answer, and it does not restart at the prompt. Measured against the
    /// provider's reported input for the last call, so it counts the system
    /// prompt and the tool schemas as well as the conversation — the thing it is
    /// protecting is the request, not one list inside it.
    ///
    /// It is also what keeps [`Budgets::max_tokens`] reachable in a long
    /// session: input per call is bounded by this, so the number of calls a
    /// goal gets is bounded below by roughly `max_tokens / max_context`
    /// — four at the defaults, and in practice far more, because most calls sit
    /// nowhere near the cap. Raising this without raising `max_tokens` buys
    /// context by spending calls.
    pub max_context: i64,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_iterations: 60,
            max_tokens: 500_000,
            wall_clock: Duration::from_secs(30 * 60),
            max_kicks: 3,
            max_context: 120_000,
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
                "stopped: went past the {} token budget, which counts cached input at what it \
                 costs rather than at its size, and is checked after each call — so the call \
                 that crossed it is included in the total below.",
                b.max_tokens
            ),
            Self::Deadline => format!("stopped: hit the {}s time limit.", b.wall_clock.as_secs()),
            // **Not "the partial turn is in the session log".** That sentence
            // was false in the case people actually hit. An interrupt landing
            // mid-model-call drops the provider future, so there is no turn to
            // record and no usage figure to bill: the API never answered. Every
            // turn that *completed* before the interrupt is in the log, which is
            // a different and true thing to say.
            //
            // The spend of an abandoned call is unrecoverable rather than
            // unrecorded — the tokens were spent on the provider's side and
            // nothing local ever learned the number. A resumed session
            // therefore under-counts by that one call.
            //
            // **And then the replacement asserted the abandoned call for
            // endings that never had one.** A reviewer traced the three ways a
            // run reaches this variant and only one of them abandons anything:
            //
            //   1. the iteration-boundary break, where a cancelled *tool* was
            //      recorded with its own `tool_cancelled` entry and a failure
            //      result, and no model call was in flight;
            //   2. `call_model` returning `Ok(None)`, which is the real
            //      mid-call abandon;
            //   3. `main`'s blocked-prompt path, where a hook refused the goal
            //      and nothing ran at all.
            //
            // So `DEF-024` replaced a false promise with a false claim pointing
            // the other way, and the test could not see it because it reads the
            // string rather than the path that produced it.
            //
            // The condition is stated rather than asserted. That is weaker than
            // this file usually allows, and it is the honest shape while one
            // variant covers three endings: a sentence that names the abandon
            // is wrong twice out of three, and one that omits it hides a real
            // cost. Splitting the variant is the fix that would let this assert
            // again — `Ending` is not serialised, so the cost is five match
            // arms — and it is not being done in the same change that found the
            // wording wrong.
            Self::Interrupted => "interrupted. Everything that finished before the interrupt \
                 is in the session log. If the interrupt landed during a model call, that call \
                 was abandoned: its answer and its cost are recorded nowhere, so a resumed \
                 session under-counts by it."
                .into(),
            Self::Provider(e) => format!("stopped: {e}"),
        }
    }
}

/// The provider in force, shared with everything that must not disagree about
/// which model this process is using.
///
/// **This exists because `/model` made "the model" a thing that changes.** The
/// `Agent` owns its provider outright and swaps it explicitly — see
/// [`Agent::set_provider`], and the argument on [`Setup::provider`] for why that
/// is not an interior-mutable cell. But `Delegate` resolves a subagent's client
/// when the delegation runs, long after it was constructed, and a subagent whose
/// file names no model is specified to use *the parent's*. Reading a clone
/// captured at boot would mean a subagent quietly running on the model the user
/// changed away from, with the parent's status row saying otherwise.
///
/// So there is exactly one cell, written in exactly one place — the `/model`
/// command, beside the `set_provider` call — and read by the one caller that
/// resolves late.
#[derive(Clone)]
pub struct Running(Arc<std::sync::Mutex<Arc<dyn Provider>>>);

impl Running {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self(Arc::new(std::sync::Mutex::new(provider)))
    }

    pub fn get(&self) -> Arc<dyn Provider> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, provider: Arc<dyn Provider>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = provider;
    }
}

/// What a compaction did, for the caller that asked for it to report.
///
/// The nothing-happened case carries a reason because a *user* who typed
/// `/compact` and saw nothing needs one. Automatic compaction is right to be
/// silent there and simply ignores this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compacted {
    Nothing(String),
    Done {
        goals: usize,
        messages: usize,
        before: i64,
        after: i64,
    },
}

/// What `/clear` dropped, so the receipt is counted rather than claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cleared {
    /// Finished goals. A chapter that is already a summary is not one.
    pub goals: usize,
    pub messages: usize,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub ending: Ending,
    /// The last thing the assistant said. Verbatim, marker and all — the screen
    /// is where that is taken out, by `Term`, and this is the record.
    pub text: String,
    /// What the goal spent, weighted by price — see [`cost_tokens`]. Not the
    /// provider's token count, which is in the transcript.
    pub tokens: i64,
    pub iterations: u32,
    pub kicks: u32,
}

/// The token meter one goal is charged against, shared with anything it
/// delegates to.
///
/// **Why it is not a local.** `run_goal` kept `let mut tokens` and tested it
/// after every call, which is exactly right for a loop that is the only thing
/// spending. A delegation is a second loop spending the same budget, and it
/// cannot reach a local — so the owner's ruling that a subagent spends the
/// parent's budget is either this type or an `if call.name == "Delegate"` in
/// `run_tool_call` adding the tool's self-reported cost. The second is the loop
/// branching on a tool's identity, which the module doc forbids and which this
/// project has already paid to remove twice.
///
/// **The parent link, and why `set` does not follow it.** A nested run gets a
/// meter of its own whose every `add` also lands on its parent's, so the sub's
/// spend charges the parent as it happens and the parent's `tokens > max_tokens`
/// test sees it on its very next iteration. `set` is the per-goal reset and
/// stays local: a nested `run_goal` opening its own goal would otherwise zero
/// the meter of the goal that is paying for it.
#[derive(Debug, Default)]
pub struct Spend {
    counted: std::sync::atomic::AtomicI64,
    parent: Option<Arc<Spend>>,
}

impl Spend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A meter of its own that also charges `parent`.
    pub fn child(parent: &Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            counted: std::sync::atomic::AtomicI64::new(0),
            parent: Some(parent.clone()),
        })
    }

    /// Charge, and answer what this meter now reads.
    pub fn add(&self, tokens: i64) -> i64 {
        if let Some(parent) = &self.parent {
            parent.add(tokens);
        }
        self.counted.fetch_add(tokens, Ordering::SeqCst) + tokens
    }

    pub fn get(&self) -> i64 {
        self.counted.load(Ordering::SeqCst)
    }

    /// The per-goal reset. Local by design — see the type's doc.
    pub fn set(&self, tokens: i64) {
        self.counted.store(tokens, Ordering::SeqCst);
    }
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
    /// Records the fold refused to rebuild, one sentence each.
    ///
    /// Non-empty means the conversation that came back is **known** not to match
    /// the one that was sent. The refusals themselves were always correct; they
    /// were simply invisible past the stderr they printed to, which made the
    /// test guarding them assert nothing more than "it did not panic".
    pub damage: Vec<String>,
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
    /// Whether the session's last goal was still running when the file ended.
    ///
    /// **This used to be inferred from `messages` being non-empty, and that was
    /// wrong.** `messages` is the fold of the whole file, so it is non-empty
    /// after *any* goal, finished or not — and the loop used it to seed
    /// `worked`, which decides whether a model that stops without calling a
    /// tool has answered or stalled. So resuming a session whose goal had
    /// completed and asking a plain question got the answer nudged, then
    /// nudged again, and the run ended `Stalled`: a correct answer reported as
    /// a failed run, and a non-zero exit code under `-p`.
    ///
    /// The file always knew. A `goal_finished` record carries the ending; it
    /// was read for its counters and thrown away otherwise.
    pub in_flight: bool,
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

    /// Clear the flag so the next goal starts un-interrupted.
    ///
    /// **Without this the flag was one-way**, and three help strings were
    /// wrong: `cli.rs` and the session's own opening note both say Ctrl-C
    /// interrupts *a goal*, and it ended the whole session. Worse, a Ctrl-C
    /// pressed at an idle prompt did nothing visible and then made the *next*
    /// goal abort instantly, before any model call, with the session exiting
    /// after it — a keystroke doing its damage to work the user had not typed
    /// yet.
    ///
    /// Called at the top of an interactive turn rather than at the bottom of
    /// one, so a stray press between goals is discarded rather than banked. A
    /// press landing in the microsecond between this and the goal starting is
    /// lost, which is a far smaller wrong than the one it replaces.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    /// Clear the flag if this run gets more than one goal, and say whether it
    /// did.
    ///
    /// **The decision, not just the store.** `reset` is unconditional; the rule
    /// is that an *interactive* goal starts un-interrupted and a `-p` run does
    /// not, because `-p` has exactly one goal and a Ctrl-C pressed before it
    /// starts is a person cancelling the thing they just typed. Interactively
    /// the same press is a stray keystroke at an idle prompt, and banking it
    /// would abort the next goal before a single model call.
    ///
    /// That rule lived in `main.rs` as `if !opts.print`, which put a decision
    /// in the binary where no scripted `Provider` can reach it and left the
    /// `-p` half with no test at all -- the unit test below covered `reset`,
    /// which is the store rather than the choice. `CLAUDE.md` asks for
    /// decisions in the library for exactly this reason.
    ///
    /// Returning the answer rather than performing it silently is what makes
    /// both branches observable. See the lesson filed on `Term::clipboard`: a
    /// function that decides something and returns nothing has made the
    /// decision untestable.
    ///
    /// **What this still does not cover** is that `main` calls it. The goal
    /// loop is in `main.rs`, so deleting the call leaves the workspace green;
    /// that gap is `ARCH-003` and the fix is moving the loop, not another test
    /// here.
    pub fn starting_goal(&self, one_shot: bool) -> bool {
        if one_shot {
            return false;
        }
        self.reset();
        true
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
    /// The client for the model in force.
    ///
    /// **An `Arc` rather than a `&'a dyn Provider`, and the change was the one
    /// invasive part of `/model`.** A borrow ties the agent to a provider for
    /// its whole life, which is right up until the user is allowed to change
    /// model without leaving the session. Owning a counted handle keeps the
    /// field honest — the agent has exactly one provider at a time and swapping
    /// it is an explicit event, [`Agent::set_provider`] — where an interior
    /// `RefCell` would have made "which model is running" a question with no
    /// single answer.
    pub provider: Arc<dyn Provider>,
    pub harness: &'a Harness,
    /// The system prompt, before the standing contract is appended.
    ///
    /// Taken here rather than read from `harness.instructions` because a
    /// delegated run has the same harness — the same hooks, the same working
    /// directory, the same skills — and a *different* prompt: the body of its
    /// agent file. One field is the difference between that and a second
    /// `Harness`, which would mean two objects claiming to be this project's
    /// configuration.
    pub instructions: &'a str,
    pub tools: &'a Registry,
    pub approvals: &'a Approvals,
    pub log: &'a SessionLog,
    pub term: &'a Term,
    pub interrupt: Arc<Interrupt>,
    /// The token meter this run charges. See [`Spend`].
    pub spend: Arc<Spend>,
    /// How this run decides a goal is met. One implementation today; the loop
    /// knows only the trait, so swapping in a task-list or check-command
    /// authority is a different value here rather than an edit below.
    pub done: &'a dyn DoneCheck,
    pub cwd: PathBuf,
    pub session_id: String,
    /// Background work started by this session, held here so it outlives the
    /// tool call that spawned it and can still be found, read and killed.
    ///
    /// Defaulted rather than required, so every existing construction of
    /// `Setup` — including every test — keeps compiling and simply has no
    /// background work. A required field would have made this change touch
    /// dozens of call sites to say the same nothing at each one.
    pub background: emma_tool_api::background::Registry,
    pub budgets: Budgets,
    pub caching: Caching,
    pub mode: Mode,
}

pub struct Agent<'a> {
    s: Setup<'a>,
    /// The session's one conversation, a chapter per goal, tool traffic and all.
    ///
    /// **This used to be completed goals collapsed to a goal and an answer**,
    /// with a comment defending the saving: a `tool_use` in history with no
    /// matching result is rejected by the API, and carrying the pairs puts a
    /// previous goal's file contents ahead of this one's query where every byte
    /// is paid for again. Both halves of that are true and the conclusion was
    /// still wrong, because it priced one turn and not the next one.
    ///
    /// *"read src/auth.rs and tell me what it does"* read the file and answered.
    /// *"why does it do that?"* then opened on two lines of prose — the file was
    /// gone, so it read the file again. Every follow-up re-did the work of the
    /// turn before it, which is a larger bill than the one being avoided and a
    /// worse conversation: a person asking a second question about the same file
    /// is the ordinary case, not the exception.
    ///
    /// So the traffic stays, and the two real costs are paid for properly rather
    /// than by amnesia. The unmatched-`tool_use` hazard is handled by
    /// [`session::place_turn`], which is the fold's own rule and is now shared
    /// with the loop. Unbounded growth is handled by [`Agent::compact`], which
    /// applies exactly the old collapse — the goal and the answer, without the
    /// traffic — but only to the oldest goals and only once the context cap
    /// says it must. The old behaviour is now the fallback rather than the
    /// policy.
    chapters: Vec<Chapter>,
    /// How the last goal ended, until the next one opens. See
    /// [`Agent::close_previous_goal`].
    last_ending: Option<&'static str>,
    /// Whether the user has already been told that automatic compaction cannot
    /// help. Said once: the condition holds on every subsequent call, and a
    /// warning per request would bury the run in one repeated sentence.
    said_compaction_stuck: bool,
    /// The provider's reported input size for the most recent call, which is
    /// what [`Budgets::max_context`] is tested against. `None` before the first
    /// call of the process, where an estimate stands in.
    last_input: Option<i64>,
    turn_seq: u64,
    /// Consumed by the first `run_goal` and never again: what it carries is one
    /// interrupted goal's conversation and one interrupted goal's spend, and a
    /// second goal typed at the prompt afterwards is a new goal with its own
    /// budget, exactly as it would be without a resume.
    resumed: Option<Resumed>,
    /// The model this conversation was previously being written by, set by
    /// [`Agent::set_provider`] and cleared by the first call that survives.
    ///
    /// It exists for one recovery and buys nothing else — see
    /// [`Agent::recover_from_a_model_change`], which is the only reader.
    changed_from: Option<String>,
}

impl<'a> Agent<'a> {
    /// **This constructor deliberately touches nothing outside itself.** It used
    /// to call `term.set_budgets(...)` here, on the argument that this is where
    /// the budgets already were. That was true and it was a bug waiting for a
    /// second `Agent`: the status meters belong to the *process*, and
    /// constructing a nested one re-pointed the parent's context and token
    /// meters at a sub-run's caps for the rest of the run. `main` sets them once
    /// now, from the budgets it parsed, and `Term::subordinate` is what keeps a
    /// nested run from moving them afterwards.
    pub fn new(s: Setup<'a>) -> Self {
        Self {
            s,
            chapters: Vec::new(),
            last_ending: None,
            said_compaction_stuck: false,
            last_input: None,
            turn_seq: 0,
            resumed: None,
            changed_from: None,
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

    /// The whole session as one message list — every goal, in order, with the
    /// tool traffic that has not been compacted away.
    ///
    /// This is what a request carries as `Request::history`, and it is what a
    /// fold of the session file must equal.
    pub fn conversation(&self) -> Vec<Message> {
        self.chapters
            .iter()
            .flat_map(|c| c.messages.iter().cloned())
            .collect()
    }

    /// Roughly what the conversation weighs, by the same `chars / 4` the
    /// compactor sizes chapters with — and it under-counts, which is why it is
    /// only ever reported as "roughly" and never enforces anything.
    pub fn estimated_context(&self) -> i64 {
        estimate(&self.conversation())
    }

    /// Swap the model for the rest of the session, and answer what was running.
    ///
    /// **The conversation is untouched.** Every chapter, every tool result and
    /// every summary stays exactly where it is and the whole list is re-sent to
    /// the new model on the next goal — which is the behaviour a person asking
    /// "is Sonnet enough for this?" wants, and the reason the one hazard below
    /// exists.
    pub fn set_provider(&mut self, provider: Arc<dyn Provider>) -> String {
        let was = self.s.provider.model_id().to_string();
        self.s.log.append(
            "model_changed",
            json!({ "from": was, "to": provider.model_id() }),
        );
        self.s.provider = provider;
        self.changed_from = Some(was.clone());
        was
    }

    /// Drop the conversation without ending the session.
    ///
    /// A `cleared` record goes in the log, and `session::Fold` honours it — the
    /// bytes stay in the file for a human to read and the fold skips them.
    /// Without that arm `--resume` would fold the whole file and un-clear the
    /// session, which would make this command a lie.
    ///
    /// `turn_seq` is deliberately **kept**: turn ids have to stay unique inside
    /// one session file, and the file is not cleared.
    pub fn clear(&mut self) -> Cleared {
        let cleared = Cleared {
            goals: self.chapters.iter().filter(|c| !c.summarised).count(),
            messages: self.chapters.iter().map(|c| c.messages.len()).sum(),
        };
        self.chapters.clear();
        self.last_ending = None;
        self.last_input = None;
        // A resume that has not been spent yet is still a conversation waiting
        // to be injected into the next goal's query. Leaving it would mean
        // `/clear` on a `--resume`d session cleared everything except the thing
        // the user was most obviously trying to get rid of.
        self.resumed = None;
        self.s.log.append(
            "cleared",
            json!({ "goals": cleared.goals, "messages": cleared.messages }),
        );
        cleared
    }

    /// Compact at the user's request rather than at the threshold.
    ///
    /// `everything` includes the goal that has just finished. The default keeps
    /// it because "everything but the last goal" is a rule somebody can hold in
    /// their head, where a target size is arithmetic they would have to be told.
    ///
    /// It goes through the same [`Agent::compact`] the threshold does, so there
    /// is one compaction implementation, one `compacted` record and one fold
    /// arm replaying it.
    pub fn compact_now(&mut self, everything: bool) -> Compacted {
        let n = self.chapters.len();
        let take = if everything { n } else { n.saturating_sub(1) };
        if take == 0 {
            return Compacted::Nothing(match n {
                0 => "there is no conversation yet".into(),
                _ => "the only thing in the conversation is the goal you just finished. \
                      `/compact all` includes it."
                    .into(),
            });
        }
        // The size a threshold compaction would have measured. Nothing is being
        // decided from it — the count is already fixed above — so the estimate
        // is only what the record and the report quote.
        let size = self.estimated_context();
        self.compact(take, size, "/compact")
    }

    /// The recovery for a model change that the provider would not accept.
    ///
    /// **Whether one model's signed thinking blocks are accepted by another is
    /// not established in this tree**, and it is the one thing that can turn
    /// `/model` from "works" into a 400 on the very next call. So the failure is
    /// handled rather than predicted: a `BadRequest` naming a signature or a
    /// thinking block, on the first call after a change, compacts the whole
    /// conversation to summaries — which are two plain `Message`s carrying no
    /// provider state at all — and the caller retries once.
    ///
    /// Two of the three guards are load-bearing and the third is a cheap
    /// early-out, and it is worth saying which is which rather than implying
    /// all three are needed.
    ///
    /// **Load-bearing:** only after a change — otherwise an ordinary malformed
    /// request costs somebody their tool traffic — and only on a message that
    /// names a signature or a thinking block. Both were confirmed by mutation:
    /// remove either and a test goes red.
    ///
    /// **Redundant, kept anyway:** the [`ContentBlock::is_model_bound`] check.
    /// It reads as the load-bearing one and is not, because what actually
    /// bounds this to a single attempt is [`Agent::compact`] answering
    /// `Compacted::Nothing` once the conversation is already summaries — and a
    /// conversation with no model-bound block is a conversation compaction has
    /// nothing left to take out of. Every mutation of this check is invisible
    /// for that reason. It stays because it says what the recovery is *for* at
    /// the place it happens, and because it costs one pass over a list that is
    /// about to be rewritten; it is not a second safety property.
    fn recover_from_a_model_change(&mut self, e: &LlmError) -> bool {
        let Some(was) = self.changed_from.take() else {
            return false;
        };
        let LlmError::BadRequest { message } = e else {
            return false;
        };
        let lower = message.to_ascii_lowercase();
        if !lower.contains("signature") && !lower.contains("thinking") {
            // **The fragile edge, made loud rather than left silent.** This
            // recovery keys on the provider's prose because no structured code
            // distinguishes "your history carries blocks the new model will not
            // accept" from any other bad request. A wording change upstream
            // therefore disables the recovery without disabling anything that
            // reports it — and the symptom is a session that dies on the first
            // call after `/model` with a message nobody connects to the switch.
            //
            // Reaching here means the two facts that matter are both true: the
            // model changed this turn, and the provider refused the request.
            // That is worth a sentence even when the wording did not match,
            // because it is exactly the case where the recovery was supposed to
            // help.
            self.s.term.warn(&format!(
                "the provider refused the first request after the model changed from {was}, and \
                 the refusal does not read like the stale-signature case this run knows how to \
                 recover from: {message}. If this is that case in different words, `/compact` \
                 clears the blocks the old model bound."
            ));
            return false;
        }
        if !self
            .conversation()
            .iter()
            .any(|m| m.content.blocks().iter().any(ContentBlock::is_model_bound))
        {
            return false;
        }
        let size = self.estimated_context();
        let take = self.chapters.len();
        self.s.term.warn(&format!(
            "the provider rejected this conversation after the model changed from {was}: \
             {message}"
        ));
        match self.compact(take, size, "a model change invalidated signed content") {
            Compacted::Done { messages, .. } => {
                self.s.term.note(&format!(
                    "summarised {messages} messages — a summary carries no signed thinking \
                     blocks — and retrying once. Their tool results are no longer in context."
                ));
                true
            }
            Compacted::Nothing(_) => false,
        }
    }

    pub async fn run_goal(&mut self, goal: &Goal) -> Outcome {
        let started = Instant::now();
        // The previous goal, if it stopped mid-turn, needs a word saying so
        // before this goal's opening follows it. Done here rather than at that
        // goal's end because the fold does it here too — see
        // `Fold::close_goal`, and the two lists have to agree at every point in
        // the record stream rather than only at the end of it.
        self.close_previous_goal();
        // Composed before it is logged, because the record carries the opening
        // message verbatim as well as the goal text. `opening` is built from the
        // `DoneCheck` in force, and the log names that check but does not hold
        // it — so a fold that tried to re-derive this string would have to keep
        // a name-to-`impl` table in step with `goal.rs` forever. Storing the
        // bytes costs a few hundred of them once per goal.
        // `opening_turn`, not `opening`: the whole user turn, including any
        // context a `UserPromptSubmit` hook injected in front of it. The two are
        // the same string when nothing was injected. It matters that the
        // *recorded* bytes are the composed ones — the fold rebuilds this turn
        // from this field, so recording the user's words alone would replay a
        // conversation the model never had.
        let opening = goal.opening_turn();
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
                "instructions_hash": emma_harness::hash::short(self.s.instructions),
                "tool_schema_hash": self.s.tools.schema_hash(),
                "model": self.s.provider.model_id(),
                "done_check": self.s.done.name(),
            }),
        );
        self.s.term.goal_started(&goal.text);
        // Before the first request rather than after it, so a goal never opens
        // already over the cap — which is the state a resume into a long
        // session, or a goal typed after a very long one, would otherwise start
        // in. There is no measurement to use yet at this point, so the estimate
        // stands in; see `Agent::compact`.
        self.compact_if_needed(None, "a new goal opened over the context limit");
        self.warn_if_the_budget_is_nearly_spent_on_arrival();

        let tool_defs = self.s.tools.wire_definitions();
        // The restored conversation, with this goal's opening on the end. All
        // of it goes in `query` and none of it in the chapters: the split point
        // is not recorded in the file — the chapters are the goals that have
        // finished, `query` is the goal in flight, and the fold returns one flat
        // list — and guessing it wrong is a message list the API refuses. The
        // cost is cache: the chapters carry a breakpoint that would be
        // byte-stable for the rest of the session, and a restored prefix sitting
        // in `query` does not get it. That is a bill, and a wrong split is an
        // outage.
        //
        // The second cost is compaction: a restored block is one chapter's worth
        // of `query` however many goals went into it, so it is compacted as a
        // unit at the end of this goal — a resume that comes back over
        // `max_context` therefore runs this one goal at that size, and only this
        // one. That is the same flattening resume already does, showing up once
        // more.
        // From the file's own `goal_finished` record, not from whether any
        // conversation came back — see `Resumed::in_flight` for what that cost.
        let resuming_a_goal_in_flight = resumed.in_flight;
        let mut query: Vec<Message> = open_query(resumed.messages, opening);
        // The meter, not a local: a delegation charges this same integer while
        // it runs. See [`Spend`].
        self.s.spend.set(resumed.tokens);
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
        // A resumed run starts as though it had **only when the goal it is
        // resuming was still in flight**. It used to start that way whenever
        // any conversation came back, which is every resume, so a plain
        // question asked after a completed goal could not be an answer and was
        // nudged instead. The error worth refusing is still the asymmetric one:
        // mistaking a stall for an answer ends a real goal early and silently,
        // while mistaking an answer for a stall costs a model call. That is an
        // argument for erring one way when the answer is unknown, and not for
        // declining to read the answer the file records.
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
        // The assistant turn that has been received and not yet placed. See
        // where it is set for why it is held.
        let mut pending_turn: Option<Vec<ContentBlock>> = None;

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
            // Between the budget tests and the request, so the request that
            // goes out is the compacted one. `last_input` is what the provider
            // said the previous request weighed; on the first call of a goal it
            // is whatever the previous goal ended at, which is the right number
            // — the conversation has not shrunk since.
            self.compact_if_needed(self.last_input, "the request passed the context limit");

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
                    self.s.instructions,
                    goal::standing_contract(self.s.done)
                ),
                tools: tool_defs.clone(),
                history: self.conversation(),
                query: query.clone(),
                max_tokens: 32_000,
                effort: emma_llm::Effort::XHigh,
                caching: self.s.caching,
            };

            let turn = match self.call_model(request).await {
                Ok(Some(turn)) => turn,
                Ok(None) => break Ending::Interrupted,
                // The one retry in this loop, and it is not a retry of a
                // transport failure — `emma-llm` owns those. It is the
                // conversation being made acceptable to a model the user
                // changed to mid-session. Nothing was spent: the request was
                // rejected before generation, so `iterations` does not move.
                Err(e) if self.recover_from_a_model_change(&e) => continue,
                Err(e) => break Ending::Provider(e.to_string()),
            };
            // The new model has accepted the conversation, so there is nothing
            // left to recover from.
            self.changed_from = None;
            iterations += 1;

            // Recorded before the budget is tested, so an abort costs what it
            // spent rather than nothing. The four provider fields go in raw and
            // `billable_total_tokens` beside them, because the log records what
            // the provider said; `cost_tokens` is the same call weighted by
            // price, and it is the only one the cap is tested against.
            // **`max_tokens` means the model was cut off mid-sentence.** It was
            // recorded in the line below and read by nothing: the loop treated a
            // truncated turn exactly like a finished one, so a half-written
            // answer could carry the completion marker's absence into a kick, or
            // — worse — a half-written tool call into a parse failure the model
            // was then blamed for.
            //
            // Said to the user rather than to the model, and once per
            // occurrence: the model already knows it stopped, and the person
            // paying for the turn is the one who cannot see it. Not fatal, for
            // the same reason a tool failure is not: the loop's own budgets are
            // the only thing that ends a goal.
            if turn.stop_reason == "max_tokens" {
                self.s.term.warn(
                    "the model hit its output limit and this turn is cut off mid-answer — what \
                     follows is incomplete, and a smaller step or a narrower question is the way \
                     round it",
                );
            }
            let tokens = self.s.spend.add(cost_tokens(&turn.usage));
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
                    "cost_tokens": cost_tokens(&turn.usage),
                    "goal_total_so_far": tokens,
                }),
            );
            // What the next request will carry, measured rather than guessed —
            // and it is the whole request, so it includes the system prompt and
            // the tool schemas as well as the conversation.
            self.last_input = Some(turn.usage.billable_input_tokens());
            // The live half of the status line, and the only place it is fed.
            // Both numbers are measured — the provider's own input count, and
            // this goal's weighted spend — so nothing on that line is an
            // estimate. It is updated here, after every call, which is the only
            // moment either of them can change.
            self.s
                .term
                .spent(turn.usage.billable_input_tokens(), tokens);
            // One record per turn, whatever the turn contained — a turn that is
            // nothing but tool calls has empty text and used to be written
            // nowhere, which left the fold with a hole exactly where the tool
            // traffic is.
            //
            // `raw_content` is the provider's own blocks and is the only field
            // resume can use: rebuilding a turn from `text` invalidates
            // thinking-block signatures, so a fold that had to do that would
            // produce a message list the API rejects. `text` stays beside it
            // even though every byte of it is also inside the array, because the
            // file is an audit trail somebody reads with `grep`, where one plain
            // line beats a JSON array of escaped blocks. The fold no longer
            // reads it at all — it used to, to rebuild the one-line collapse of
            // a finished goal, which is the duplicated rule that went away when
            // the conversation stopped being collapsed.
            let text = turn.text();
            if !text.trim().is_empty() {
                last_text = text.clone();
            }
            self.s.log.append(
                "assistant",
                json!({
                    "turn_id": turn_id,
                    "text": text,
                    // The record key keeps its old name on purpose: a session
                    // file written by an earlier build must still fold, and the
                    // bytes under it are the same content array they always
                    // were — it is only Rust's side of the boundary that is
                    // typed now.
                    "raw_content": turn.content,
                }),
            );
            // Held rather than pushed. Where it goes depends on what happens
            // next: beside its tool results, beside a kick, or — for every
            // ending that stops on a turn the model just produced — on the end
            // of the conversation once the loop is out. A turn whose tools were
            // never run cannot go anywhere, and `place_turn` is what decides
            // that rather than four branches each deciding it again.
            pending_turn = Some(turn.content.clone());
            // `spend.get()` rather than the value added above: a tool call
            // earlier in this turn may have been a delegation, and what bounds
            // this goal is everything charged to it rather than everything this
            // loop spent itself.
            if self.s.spend.get() > self.s.budgets.max_tokens {
                break Ending::Tokens;
            }

            if turn.tool_calls().is_empty() {
                // The model stopped. Everything from here to `continue` is the
                // goal being held rather than a conversation ending.
                let why = match self.s.done.verdict(goal, &text).await {
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
                self.s.term.kick(kicks, self.s.budgets.max_kicks);
                // The provider's own blocks, verbatim. Never rebuilt from
                // `text` plus the tool calls.
                place(&mut query, &mut pending_turn, Vec::new());
                query.push(Message::user(kick_text));
                continue;
            }

            let mut results = Vec::new();
            for call in turn.tool_calls() {
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
            place(&mut query, &mut pending_turn, results);
        };
        // The turn the loop stopped on. For `Done`, `Answered`, `Stalled` and
        // `KicksExhausted` that is the model's final message, and it is the
        // whole reason a follow-up can be asked at all — the answer has to be
        // in the conversation, not merely in the outcome. For `Tokens` it is a
        // turn whose tools were never run, and `place_turn` drops it rather
        // than leaving a `tool_use` nothing answers.
        place(&mut query, &mut pending_turn, Vec::new());

        // The clock stops here rather than freezing at whatever it last showed.
        self.s.term.goal_ended();

        let outcome = Outcome {
            ending,
            text: last_text,
            tokens: self.s.spend.get(),
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

        // The goal joins the conversation whatever the ending: a run that hit
        // its token budget still happened, and the next goal in the session
        // must not be told a story in which it did not. What joins it is the
        // goal's own message list — the opening, the turns, the tool results —
        // rather than a summary of it. The summary is what compaction makes
        // later, out of exactly these two fields, and only if it has to.
        self.last_ending = Some(outcome.ending.as_str());
        self.chapters.push(Chapter {
            goal: goal.text.clone(),
            answer: outcome.text.clone(),
            messages: query,
            summarised: false,
        });
        outcome
    }

    /// Say that the previous goal stopped short, if it did.
    ///
    /// A goal killed by a budget or by Ctrl-C leaves a **user** turn last — the
    /// tool results of a round-trip nobody answered, or a kick — and this
    /// goal's opening is a user turn too. Two user turns in a row is a 400
    /// rather than a conversation, so something has to sit between them, and
    /// the honest something is the fact: the previous goal did not finish. A
    /// model reading an abandoned goal as a completed one is the other bug this
    /// prevents, and it is the more expensive of the two.
    fn close_previous_goal(&mut self) {
        let Some(ending) = self.last_ending.take() else {
            return;
        };
        let Some(chapter) = self.chapters.last_mut() else {
            return;
        };
        if chapter.messages.last().map(|m| m.role) == Some(Role::User) {
            chapter
                .messages
                .push(Message::assistant_text(ended_note(ending)));
        }
    }

    /// A word before the first call of a goal that opens with a lot behind it.
    ///
    /// The budget is per goal and the conversation is not, so a goal late in a
    /// long session starts expensive — and the failure mode worth refusing is
    /// the silent one, where a cap somebody set is unreachable and the run
    /// simply stops with a number they cannot connect to anything. Compaction
    /// bounds the size; this says what the size means in calls.
    fn warn_if_the_budget_is_nearly_spent_on_arrival(&self) {
        if let Some(warning) =
            arrival_budget_warning(estimate(&self.conversation()), self.s.budgets.max_tokens)
        {
            self.s.term.warn(&warning);
        }
    }

    /// Shorten the conversation when a request has grown past
    /// [`Budgets::max_context`].
    ///
    /// **The trigger is measured, not estimated.** `measured` is the provider's
    /// own input count for the last call, which is the only honest number
    /// available — the estimator below under-counts, and this project has
    /// already ruled once that an honest tripwire beats a dishonest ceiling.
    /// The estimate stands in for exactly one case: the first check of a goal
    /// on a process that has not called the model yet, which is a resume into a
    /// large session.
    ///
    /// **What it replaces, and in what order.** Whole goals, oldest first,
    /// until what is left fits in half the cap — half rather than all of it
    /// because the goal now starting needs room to work in. It never touches
    /// the goal in flight: within one goal the conversation grows exactly as it
    /// did before this change, bounded by the token budget, and it is the
    /// growth *across* goals that is new and therefore the growth this bounds.
    ///
    /// **What it loses, stated rather than discovered.** Compaction replaces a
    /// goal with the goal and the answer — the two lines the loop used to keep
    /// for every goal, immediately, which is why this is a fallback rather than
    /// a new invention. What goes is the tool traffic: file contents, command
    /// output, diffs. The model is told so in the replacement text, because a
    /// model that does not know a file has left its context will answer from a
    /// memory of it, and a wrong answer delivered confidently is worse than a
    /// second read. Nothing else is dropped — every goal's words and every
    /// answer survive for the life of the session.
    ///
    /// **It is not summarised by a model, deliberately.** A model call here
    /// would spend the budget of the goal that happens to be running, can fail
    /// in the middle of one, and produces a claim about the conversation where
    /// this produces a record of it. What is written into the session file is
    /// the replacement itself, so a resume rebuilds the conversation that was
    /// sent rather than re-deriving one that might differ.
    fn compact_if_needed(&mut self, measured: Option<i64>, why: &str) {
        let cap = self.s.budgets.max_context;
        if cap <= 0 {
            return;
        }
        let size = measured.unwrap_or_else(|| estimate(&self.conversation()));
        if size <= cap {
            return;
        }
        // Half the cap rather than all of it: the goal now running needs room
        // to work in, and compacting to exactly the limit would mean compacting
        // again on the next call.
        let target = (cap / 2).max(1);
        // Measured once per chapter rather than once per step. Re-summing the
        // whole conversation on every iteration of the loop below is quadratic
        // in a list whose elements are file contents.
        let sizes: Vec<i64> = self
            .chapters
            .iter()
            .map(|c| estimate(&c.messages))
            .collect();
        let mut remaining: i64 = sizes.iter().sum();
        let mut take = 0;
        while take < sizes.len() && remaining > target {
            remaining -= sizes[take];
            take += 1;
        }
        // **The returned reason used to be dropped here.** `/compact` explains
        // itself and the threshold path did not, so a conversation over its cap
        // with nothing left to summarise — one enormous goal, or a history
        // already reduced to summaries — grew on every single call in silence
        // until the token budget or a provider 400 ended it. The user's first
        // sign was the run stopping.
        if let Compacted::Nothing(reason) = self.compact(take, size, why) {
            if !self.said_compaction_stuck {
                self.said_compaction_stuck = true;
                self.s.term.warn(&format!(
                    "this conversation is over the context limit and compaction cannot \
                     shrink it: {reason}. Every request from here is larger than the cap, \
                     and the run will end on a budget or a provider error rather than on \
                     the goal. `/clear` starts a fresh conversation; a smaller goal would \
                     also fit."
                ));
            }
        }
    }

    /// Replace the oldest `take` chapters with their summaries.
    ///
    /// **How many is the caller's decision and the only thing that differs
    /// between the two doors.** The threshold path counts until the remainder
    /// fits half the cap; `/compact` counts every finished goal, or every one
    /// but the last. Everything after that — what a summary is, the
    /// nothing-happened guard, the record the fold replays, the arithmetic in
    /// the report — is here once, because two compaction implementations would
    /// be two things a resume could disagree with.
    fn compact(&mut self, take: usize, size: i64, why: &str) -> Compacted {
        let take = take.min(self.chapters.len());
        if take == 0 {
            return Compacted::Nothing("there is nothing behind the current goal".into());
        }
        let before: i64 = self.chapters[..take]
            .iter()
            .map(|c| estimate(&c.messages))
            .sum();
        let replacement: Vec<Message> = self.chapters[..take].iter().flat_map(summarise).collect();
        let after = estimate(&replacement);
        // A conversation summarising would not shrink has nothing to give.
        // Stopping here rather than rewriting it into itself is what keeps a
        // session under a cap it cannot reach from logging a record and
        // reporting a saving on every single call.
        //
        // **Two different situations reach this line and a user needs to be
        // told which.** Automatic compaction is no longer silent either way —
        // DEF-009 gave the threshold path a one-shot warning naming `/clear`,
        // because a session pinned against a cap it cannot compact below was
        // otherwise a loop that just kept costing money. `/compact` was always
        // loud, and the first live run of it said "already summarised" about two
        // goals that plainly were not — because those goals were four words
        // long and `COMPACTED_NOTE` is three hundred characters, so the
        // replacement was *larger* than what it replaced. The sentence was true
        // of the arithmetic and false about the conversation, which is the class
        // of untruth this repository has already paid for twice.
        if after >= before {
            let all_summaries = self.chapters[..take].iter().all(|c| c.summarised);
            return Compacted::Nothing(if all_summaries {
                "the conversation is already summarised — there is nothing left to take out".into()
            } else {
                format!(
                    "summarising those {take} goal(s) would not make the conversation smaller. \
                     Compaction replaces a goal with its text, its answer and a note saying the \
                     tool results are gone, and here that note is longer than what it would \
                     replace — roughly {before} tokens becoming {after}."
                )
            });
        }
        let dropped: usize = self.chapters[..take].iter().map(|c| c.messages.len()).sum();
        self.chapters.drain(..take);
        self.chapters.insert(
            0,
            Chapter {
                goal: String::new(),
                answer: String::new(),
                messages: replacement.clone(),
                summarised: true,
            },
        );
        // Both halves, because the fold replays this rather than re-deciding
        // it: how many messages off the front went, and exactly what took their
        // place. It is also the one record a human reads to find out what the
        // model stopped being able to see.
        self.s.log.append(
            "compacted",
            json!({
                "why": why,
                "goals": take,
                "drop_messages": dropped,
                "messages": replacement,
                "request_tokens": size,
                "max_context": self.s.budgets.max_context,
                "estimated_before": before,
                "estimated_after": after,
            }),
        );
        // The threshold path narrates itself here, because nobody asked for it
        // and a conversation that silently lost its file contents is the bug
        // this sentence exists to prevent. A user-triggered compaction is
        // reported by its caller instead — `/compact` has more to say, and two
        // notes for one action reads as two actions.
        if why != "/compact" {
            self.s.term.note(&format!(
                "compacted {dropped} earlier messages from {take} finished goal(s) — roughly \
                 {before} tokens down to {after}. Their tool results are no longer in context.",
            ));
        }
        Compacted::Done {
            goals: take,
            messages: dropped,
            before,
            after,
        }
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
            self.s.term.text(&turn.text());
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
    ) -> (ToolResult, bool, Option<String>) {
        let label = label_of(&call.name, &call.input);
        let fail = |kind: &str, detail: String| {
            let block = failure_result(&call.id, &call.name, kind, &detail);
            // A failure block is a block that was sent, so it is recorded under
            // the same kind as a successful one — the fold rebuilds the user
            // turn from these and would otherwise reconstruct a turn that
            // answers only the calls that worked, which the API rejects. The
            // record immediately above this one (`tool_failed`, `denied`,
            // `tool_unknown`, …) is the prose a human reads; this is the wire.
            self.log_result_block(turn_id, call, &block, None, None);
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

        // **The unwind guard has to cover everything the tool runs, and for a
        // while it covered only `invoke`.** `DEF-007`'s own argument is that
        // *"several of its crates parse input written by the model, and one
        // `unwrap` in any of them ended everything"* -- and `validate_args` is
        // the first function on that path to see that input. There are
        // nineteen-odd implementations across `tools/fs`, `tools/lsp`,
        // `tools/tasks` and `tools/web`, every one of them parsing a JSON blob
        // the model wrote.
        //
        // A reviewer staged a panic in a test tool's `validate_args` and the
        // session died exactly the way this row says it no longer can. The
        // existing panicking fixture panics inside `invoke`, so it is a
        // positive control for the wrong half.
        let validated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tool.validate_args(&call.input)
        }));
        let validated = match validated {
            Ok(r) => r,
            // An observation, not an abort -- `INV-004`. The model is told the
            // tool broke rather than the run ending under it.
            Err(_) => {
                return fail(
                    "panicked",
                    format!(
                        "{} panicked while checking its arguments, so it was not run. That is a \
                         bug in the tool rather than in the call; a different route may work.",
                        call.name
                    ),
                );
            }
        };
        if let Err(e) = validated {
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
            // Its own treatment on screen, and not a warning: no answer at the
            // prompt could have allowed this, and a user who reads it as "I
            // could have said yes" goes looking for a prompt that never comes.
            self.s.term.tool_blocked(&call.name, &reason);
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
            Verdict::Deny(who, reason) => {
                // Said on screen as well as in the log. A call that vanishes
                // between "wants to run" and the next thing is a tool that
                // mysteriously did nothing.
                //
                // **Both of those name a decider, and both used to name the
                // wrong one.** `by` was the literal `"user"` on every path,
                // including `-p`, where there is nobody to ask by construction.
                // The session log is the only account of a run somebody did not
                // watch, and "the user said no" and "there was no user" are
                // different runs.
                self.s.term.tool_refused(&call.name, who.because());
                self.s.log.append(
                    "denied",
                    json!({
                        "turn_id": turn_id,
                        "id": call.id,
                        "by": who.logged(),
                        "reason": reason,
                    }),
                );
                return fail("not_approved", reason);
            }
        }

        let ctx = ToolCtx {
            cwd: self.s.cwd.clone(),
            session_id: self.s.session_id.clone(),
            turn_id: turn_id.to_string(),
            background: self.s.background.clone(),
        };
        // **A panicking tool is a failed tool, not a failed session.** This
        // file's own first rule is that every failure class reaches the model as
        // a `tool_result` and the turn continues — a missing tool, bad
        // arguments, a `ToolError`, a hook denial, a refused approval. A panic
        // was the one class that did not: it unwound straight through the loop,
        // past the session log, past the budget accounting, and took the run
        // with it. The tool surface here is large, several of its crates parse
        // input from the model, and an index or an `unwrap` in any of them
        // ended everything.
        //
        // `AssertUnwindSafe` is honest rather than a shrug: what crosses the
        // boundary is `&ctx` and an owned `Value`, and the loop's own state is
        // not touched by the tool. A tool that panics mid-write may of course
        // leave its own mess on disk, which is the tool's business and is
        // exactly what the message hands back to the model.
        // **Ctrl-C reaches a running tool, not just the gap between two.**
        // `invoke` used to be awaited outright, and the interrupt flag was read
        // only at iteration boundaries and around the model call — so a wrong
        // `Bash` ran to its own timeout, 120 seconds by default and up to 600,
        // with the keyboard already asking it to stop. In the framed UI raw mode
        // means the child never sees a console signal either, so nothing else
        // was going to end it.
        //
        // Dropping the `invoke` future is what cancels the work. That is only
        // safe because the one tool that owns a child process spawns it with
        // `kill_on_drop(true)` (`tools/fs/src/bash.rs`), so the process dies
        // with the future rather than outliving it. A tool that acquires
        // something needing an explicit release must say so the same way.
        //
        // The cancellation is reported as an ordinary failed call: the loop's
        // first rule is that every failure class is an observation, and a
        // cancelled tool is a failure the model should see rather than a hole
        // in the transcript.
        let invoked = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
            tool.invoke(&ctx, call.input.clone()),
        ));
        let called = tokio::select! {
            biased;
            () = self.s.interrupt.wait() => {
                self.s.log.append(
                    "tool_cancelled",
                    json!({ "turn_id": turn_id, "id": call.id, "tool": call.name }),
                );
                let detail = format!(
                    "{} was cancelled part-way through by the user. Anything it had already \
                     done stands; anything it had not is undone.",
                    call.name
                );
                self.s.term.tool_failed(&call.name, &detail);
                self.run_post_hooks(turn_id, call, &detail, false, Some("cancelled"))
                    .await;
                return fail("cancelled", detail);
            }
            r = invoked => r,
        };
        let called = match called {
            Ok(v) => v,
            Err(panic) => {
                let what = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a panic with no message".into());
                self.s.log.append(
                    "tool_panicked",
                    json!({ "turn_id": turn_id, "id": call.id, "tool": call.name,
                            "detail": what }),
                );
                self.s.term.tool_failed(&call.name, &what);
                self.run_post_hooks(turn_id, call, &what, false, Some("tool_panicked"))
                    .await;
                return fail(
                    "tool_panicked",
                    format!(
                        "{} panicked and did not finish: {what}. This is a defect in the tool, \
                         not something your arguments can fix — do not retry the same call. \
                         Anything it had already written may be half-done.",
                        call.name
                    ),
                );
            }
        };
        let outcome = match called {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => {
                // The line this whole design exists for. A typed failure is an
                // observation the model acts on; the goal continues.
                self.s.log.append(
                    "tool_failed",
                    json!({ "turn_id": turn_id, "id": call.id, "tool": call.name,
                            "kind": e.kind(), "detail": e.detail() }),
                );
                self.s.term.tool_failed(&call.name, e.detail());
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
                self.s.term.tool_failed(&call.name, &fault.to_string());
                return fail("tool_fault", fault.to_string());
            }
        };

        let mut content = outcome.content.clone();
        if outcome.truncated {
            content.push_str(&truncation_note(outcome.truncation.as_deref()));
        }
        let post = self
            .run_post_hooks(turn_id, call, &content, outcome.truncated, None)
            .await;
        for extra in post {
            content.push_str("\n\n");
            content.push_str(&extra);
        }

        self.s.term.tool_result(
            outcome.display.as_deref(),
            &content,
            outcome.truncated,
            outcome.truncation.as_deref(),
        );
        // A typed result, not Anthropic's wire shape. This site and
        // `failure_result` used to build `{"type":"tool_result", …}` objects by
        // hand, and that — with the old `raw_content` blob — was the whole of
        // what made this loop Anthropic-only: OpenAI expresses a result as a
        // separate message with `role: "tool"` and a `tool_call_id`, not as a
        // block inside a user message. Both sites now say what happened and
        // leave the spelling to the provider.
        let block = ToolResult::ok(&call.id, content);
        self.log_result_block(
            turn_id,
            call,
            &block,
            Some(outcome.truncated),
            outcome.exit_code,
        );
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
        block: &ToolResult,
        truncated: Option<bool>,
        exit_code: Option<i64>,
    ) {
        self.s.log.append(
            "tool_result",
            json!({ "turn_id": turn_id, "id": call.id, "tool": call.name,
                    // Written as the content block it becomes, `type` tag and
                    // all, because that is the shape the fold reads back and the
                    // shape `restore_records` tests `is_error` on.
                    "block": ContentBlock::ToolResult(block.clone()),
                    "truncated": truncated,
                    // Beside the block rather than inside it: the block is the
                    // wire shape and the API has no field for this. The
                    // delegation footer reads it here instead of parsing the
                    // prose, which it could never do correctly.
                    "exit_code": exit_code }),
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

/// One goal's place in the session's single conversation.
///
/// A chapter rather than a flat list because compaction works in whole goals:
/// cutting anywhere else risks separating a `tool_use` from the result that
/// answers it, and a boundary that is "wherever the arithmetic landed" is a
/// boundary somebody has to re-derive every time they read the code. `goal` and
/// `answer` are kept beside the messages because they are what a summary is
/// made of, and deriving them back out of the message list afterwards would be
/// guesswork.
struct Chapter {
    goal: String,
    answer: String,
    messages: Vec<Message>,
    /// Already a summary. Summarising it again would produce the same two
    /// messages and a second log record saying nothing happened.
    summarised: bool,
}

/// What a compacted goal leaves behind: the goal, the answer, and a sentence
/// saying what is no longer there.
///
/// Exactly the collapse `run_goal` used to apply to every goal the moment it
/// finished — see [`Agent::chapters`]. The note is the part that is new, and it
/// is the difference between a model that reads a file again and one that
/// answers from a recollection of it.
fn summarise(c: &Chapter) -> Vec<Message> {
    if c.summarised {
        return c.messages.clone();
    }
    let goal = if c.goal.trim().is_empty() {
        "[an earlier goal]"
    } else {
        c.goal.trim()
    };
    let answer = if c.answer.trim().is_empty() {
        "[this goal ended without a final message]"
    } else {
        c.answer.trim()
    };
    vec![
        Message::user(goal),
        Message::assistant_text(format!("{answer}\n\n{COMPACTED_NOTE}")),
    ]
}

/// Said to the model, in the place the traffic used to be.
const COMPACTED_NOTE: &str = "[Summarised to save context. The tool calls from this goal and \
     their results — file contents, command output, diffs — are no longer in this conversation. \
     Read anything you need again rather than recalling it.]";

/// Said to the model when the goal before this one did not finish.
///
/// Shared with `session::Fold::close_goal`, which composes the same sentence
/// from the `ending` recorded in the file: two spellings of it would be a
/// resumed conversation that differs from the one that was sent by one message,
/// which is the kind of difference nothing notices until a cache miss or a 400.
pub(crate) fn ended_note(ending: &str) -> String {
    format!("[The previous goal stopped before it was finished: {ending}.]")
}

/// Roughly how many tokens a message list is worth.
///
/// `chars / 4`, the same conservative ratio `emma-llm`'s packer uses to decide
/// whether a prefix is worth a cache breakpoint, and documented there as
/// under-counting what the provider bills. It is used here for two things that
/// tolerate it — deciding *how much* to drop once a measured number has already
/// said dropping is necessary, and standing in for that measurement on the one
/// call of a process that has none. It is never what fires a budget.
fn estimate(messages: &[Message]) -> i64 {
    let chars: usize = messages.iter().map(|m| m.content.wire_len()).sum();
    (chars / 4) as i64
}

/// What to say before the first call of a goal that opens with a lot behind it,
/// or `None` when the budget is comfortable.
///
/// **This was inside `warn_if_the_budget_is_nearly_spent_on_arrival` until
/// 2026-08-23, and had no test of any kind** — not of the threshold, not of the
/// wording, not of the silence. An independent reviewer found it by looking for
/// functions with exactly one call site, which is the third shape of this kind
/// this repository has closed this week. It is the worst of the three: the other
/// two at least had the decision tested.
///
/// The threshold is four calls' worth. Under that, a cap somebody set is
/// effectively unreachable and the run stops with a number they cannot connect
/// to anything, which is the silent failure worth refusing. Returning
/// `Option<String>` rather than printing keeps the comfortable case testable
/// too — a warning on every goal is one nobody reads.
fn arrival_budget_warning(carried: i64, max_tokens: i64) -> Option<String> {
    // `carried * 4` on an i64 that came from a character count cannot overflow
    // in practice, but `saturating_mul` costs nothing and the alternative is a
    // panic in release-mode arithmetic somebody would have to reproduce.
    if carried <= 0 || carried.saturating_mul(4) < max_tokens {
        return None;
    }
    Some(format!(
        "this goal opens with roughly {carried} tokens of conversation behind it, so the \
         {max_tokens} token budget is about {} model calls at that size. Raise it with \
         --max-tokens, lower --max-context, or start a new session.",
        (max_tokens / carried.max(1)).max(1)
    ))
}

/// Tokens weighted by what they cost, which is what a budget should count.
///
/// The multipliers are the provider's: a cache read is billed at 0.1× and a
/// cache write at 1.25×, and `Usage::billable_total_tokens` deliberately counts
/// neither — it answers "how much context did this call carry", which is a
/// different question and the right one for the field it is on. Emma asks both:
/// the size question decides compaction, and this one decides the budget. See
/// [`Budgets::max_tokens`] for what went wrong when one number answered both.
///
/// Integer arithmetic, so a cache read under ten tokens weighs nothing. At the
/// scale a budget is set in, that is not a rounding error worth a float.
fn cost_tokens(u: &emma_llm::Usage) -> i64 {
    u.input_tokens
        + (u.cache_creation_input_tokens * 5) / 4
        + u.cache_read_input_tokens / 10
        + u.output_tokens
}

/// [`session::place_turn`] against a held turn, taking it only if it was placed.
///
/// A turn that could not be placed is dropped, and dropping it here rather than
/// leaving it held is what stops it being offered again at the end of the loop.
fn place(
    query: &mut Vec<Message>,
    pending: &mut Option<Vec<ContentBlock>>,
    results: Vec<ToolResult>,
) -> bool {
    match pending.take() {
        Some(content) => crate::session::place_turn(query, content, results),
        None => false,
    }
}

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
            Content::Text(text) => {
                text.push_str("\n\n");
                text.push_str(&opening);
            }
            Content::Blocks(blocks) => blocks.push(ContentBlock::text(opening)),
        },
        _ => restored.push(Message::user(opening)),
    }
    restored
}

/// A tool that failed, as the observation the model reads and the loop keeps
/// going from.
///
/// `is_error` as well as the kind in the body: the flag is what the API and the
/// model's own training key on, and the body is what says which failure it was
/// and what to do instead. Both halves are load-bearing and neither substitutes
/// for the other.
fn failure_result(id: &str, tool: &str, kind: &str, detail: &str) -> ToolResult {
    ToolResult::failed(
        id,
        json!({ "kind": kind, "tool": tool, "detail": detail }).to_string(),
    )
}

/// What is appended to a result the tool says it cut.
///
/// A tool that knows which cap bound says so itself — the amount, and the
/// argument that raises it — and that sentence is repeated verbatim rather
/// than summarised. A summary of a truncation notice is how the numbers get
/// lost, and the numbers are the entire content: a page whose *link list* was
/// cut and a page whose *prose* was cut both read as "truncated", and only one
/// of them is fixed by asking for fewer characters.
///
/// The fallback is for tools that set the flag alone. It says so, and its
/// advice is marked as advice — "narrow the request" is simply wrong when what
/// was dropped is an inventory the model has no way to narrow, so the model is
/// told that nobody named a limit rather than being sent to guess at one.
fn truncation_note(reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("\n[truncated: {reason}]"),
        None => "\n[truncated: this is not the whole output, and this tool did not say which \
                 limit cut it. Narrow the request if you need the rest.]"
            .to_string(),
    }
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
        let b = failure_result("tu_1", "Bash", "tool_failed", "exited with 1");
        assert!(b.is_error);
        assert_eq!(b.tool_use_id, "tu_1");
        let body: Value = serde_json::from_str(&b.content).unwrap();
        assert_eq!(body["kind"], "tool_failed");
        assert_eq!(body["tool"], "Bash");

        // …and on the wire it is still the block the API takes, carrying the
        // flag the model keys on. A failure that renders without `is_error`
        // reads to the model as a successful call that returned odd text.
        let wire = ContentBlock::ToolResult(b).to_value();
        assert_eq!(wire["type"], "tool_result");
        assert_eq!(wire["is_error"], true);

        // A success carries no flag at all — the same two keys it always did,
        // and no third one to make an ordinary result look conditional.
        let ok = ContentBlock::ToolResult(ToolResult::ok("tu_2", "fine")).to_value();
        assert_eq!(ok["type"], "tool_result");
        assert!(ok.get("is_error").is_none(), "{ok}");
    }

    /// The model's half of the field report. A `WebFetch` on a news hub came
    /// back marked truncated with no subject: the prose was well inside its
    /// cap and the *link list* was what got cut, but nothing said so, and the
    /// only argument the model had been told about was the one that would not
    /// have helped. Whatever the tool worked out, the model reads verbatim.
    #[test]
    fn a_stated_reason_reaches_the_model_verbatim() {
        let reason = "50 of 70 links shown, 20 dropped by max_links=50; \
                      re-read with max_links=70 for the rest";
        let note = truncation_note(Some(reason));
        assert!(note.contains(reason), "the reason was reworded: {note}");
        assert!(note.contains("truncated"), "{note}");

        // And a tool that named nothing must not be dressed up as one that
        // did. The generic line has to admit that no limit was named.
        let vague = truncation_note(None);
        assert!(vague.contains("did not say which limit"), "{vague}");
    }

    /// The interrupt flag clears, so a goal starts un-interrupted.
    ///
    /// **It used to be one-way**, and two independent audits found the same
    /// consequence: `cli.rs` and the session's opening note both promise Ctrl-C
    /// interrupts *a goal*, and it ended the whole session instead. A press at
    /// an idle prompt was worse — nothing visible happened, then the next goal
    /// typed aborted before any model call and the session exited after it.
    #[test]
    fn an_interrupt_can_be_cleared_so_the_next_goal_starts_clean() {
        let i = Interrupt::new();
        assert!(!i.tripped());
        i.trip();
        assert!(i.tripped(), "trip must set it");
        i.reset();
        assert!(
            !i.tripped(),
            "a goal would start already interrupted, before any model call"
        );
        // And it still trips again afterwards: reset is not a disable.
        i.trip();
        assert!(i.tripped());
    }

    /// The choice above the store: which runs start clean, and which do not.
    ///
    /// **`reset` had a test and the rule using it did not.** The rule lived in
    /// `main.rs` as `if !opts.print`, so the `-p` half -- a press before a
    /// one-shot run is a cancellation and must survive -- was asserted
    /// nowhere. Inverting either branch turns this red.
    #[test]
    fn a_one_shot_run_keeps_an_interrupt_and_an_interactive_one_discards_it() {
        // Interactive: a stray press at an idle prompt is discarded, or it
        // aborts a goal the user has not typed yet.
        let i = Interrupt::new();
        i.trip();
        assert!(
            i.starting_goal(false),
            "an interactive goal must start clean"
        );
        assert!(
            !i.tripped(),
            "the stray press was banked against the next goal"
        );

        // `-p`: exactly one goal, and a press before it starts is the person
        // cancelling what they just typed. Discarding it would run the goal
        // they had just asked not to run.
        let i = Interrupt::new();
        i.trip();
        assert!(!i.starting_goal(true), "a one-shot run must not clear it");
        assert!(i.tripped(), "the cancellation was thrown away");

        // Nothing tripped: neither shape invents an interrupt.
        let i = Interrupt::new();
        i.starting_goal(false);
        assert!(!i.tripped());
        i.starting_goal(true);
        assert!(!i.tripped());
    }

    #[test]
    fn the_memo_key_separates_different_arguments() {
        let mk = |args: Value| ToolCall {
            id: "x".into(),
            name: "Bash".into(),
            input: args,
            ..Default::default()
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

    /// **No message a user reads carries a run of spaces.**
    ///
    /// Two did. A multi-line Rust string literal keeps every byte between the
    /// quotes, including the indentation of the continuation lines, so a
    /// literal wrapped across three lines *without* a trailing backslash renders
    /// with eighteen spaces in the middle of a sentence. Both were strings this
    /// project had rewritten carefully for tone — the interrupted ending and the
    /// model-change warning — and the care went into wording that then reached
    /// the terminal mangled.
    ///
    /// Their own tests could not see it: each asserts on a *substring* that sits
    /// entirely on one side of the gap. That is the trap. A test written against
    /// the part of a string you are thinking about will not notice the part you
    /// are not.
    ///
    /// This asserts on the rendered message rather than on the source text. A
    /// source grep for a backslash would be the false-receipt shape this
    /// repository already has a name for, and it would also pass on a string
    /// that was never wrapped at all.
    #[test]
    fn no_ending_message_carries_a_run_of_spaces() {
        let endings = [
            Ending::Done,
            Ending::KicksExhausted,
            Ending::Stalled,
            Ending::Answered,
            Ending::Iterations,
            Ending::Tokens,
            Ending::Deadline,
            Ending::Interrupted,
            Ending::Provider("the provider said no".into()),
        ];
        for ending in endings {
            let message = ending.message(&Budgets::default());
            assert!(
                !message.contains("  "),
                "{ending:?} renders with a run of spaces, which is a wrapped string \
                 literal missing its continuation backslash: {message:?}"
            );
            assert!(!message.is_empty(), "{ending:?} renders as nothing at all");
        }
    }
    /// The interrupted ending does not promise a record that does not exist.
    ///
    /// It used to read "The partial turn is in the session log", which is false
    /// in the case people actually hit: an interrupt landing mid-model-call
    /// drops the provider future, so there is no turn to record and no usage
    /// figure to bill. A sentence that sends someone to look for something
    /// that was never written is worse than saying nothing.
    #[test]
    fn the_interrupted_ending_does_not_claim_a_partial_turn_was_saved() {
        let m = Ending::Interrupted.message(&Budgets::default());
        assert!(
            !m.contains("The partial turn is in the session log"),
            "it still promises a record that is not written: {m}"
        );
        assert!(m.contains("abandoned"), "{m}");
        // And it says the cost is unrecoverable rather than merely missing,
        // because a resumed session under-counts by that call and nothing local
        // can ever learn the number.
        assert!(m.contains("cost"), "{m}");

        // **And it must not assert the abandon, because two of the three ways
        // a run reaches this variant abandon nothing.** A reviewer traced them:
        // the iteration-boundary break (a cancelled tool, recorded with its own
        // entry), `call_model` returning `Ok(None)` (the real mid-call case),
        // and `main`'s blocked-prompt path (nothing ran). This test could not
        // see that, because it reads the string rather than the path -- so the
        // condition is now asserted here rather than left to the wording.
        assert!(
            m.contains("If the interrupt landed"),
            "the sentence asserts an abandoned call for endings that never had one; \
             it must state the condition: {m}"
        );
    }

    /// The arrival warning fires when the budget is under four calls' worth,
    /// stays quiet above it, and says what to do about it.
    ///
    /// **Written because the function had no test at all** — not the threshold,
    /// not the wording, not the silence. A reviewer found it by listing
    /// functions with exactly one call site. Everything below would have passed
    /// against a body that did nothing except the silent case, which is why the
    /// silent case is asserted first and separately.
    #[test]
    fn the_arrival_warning_fires_only_when_the_budget_is_nearly_gone() {
        // Comfortable: 10k carried against a 500k budget is fifty calls.
        assert!(
            arrival_budget_warning(10_000, 500_000).is_none(),
            "an ordinary goal would open with a budget warning, and a warning on \
             every goal is one nobody reads"
        );
        // Nothing carried at all — the first goal of a session.
        assert!(
            arrival_budget_warning(0, 500_000).is_none(),
            "a fresh session warned about a conversation it does not have"
        );
        // A negative estimate cannot happen, and must not divide.
        assert!(arrival_budget_warning(-1, 500_000).is_none());

        // Exactly four calls' worth is the boundary, and it is inclusive: at
        // this point the cap is close enough to be worth saying.
        assert!(
            arrival_budget_warning(125_000, 500_000).is_some(),
            "the boundary case was silent, so the warning fires later than its \
             own doc says"
        );
        assert!(
            arrival_budget_warning(124_999, 500_000).is_none(),
            "one token below the boundary warned, so the threshold is not where \
             it is documented"
        );

        // And the text: the numbers a reader needs, and the way out.
        let w = arrival_budget_warning(250_000, 500_000).expect("two calls' worth was silent");
        assert!(
            w.contains("250000") && w.contains("500000"),
            "the warning names neither the conversation nor the budget, so a \
             reader cannot tell which to change: {w}"
        );
        assert!(
            w.contains("2 model calls"),
            "the count of remaining calls is the whole point of the warning: {w}"
        );
        assert!(
            w.contains("--max-tokens"),
            "the warning says there is a problem and not what to do: {w}"
        );
    }

    #[test]
    fn every_ending_says_which_limit_fired() {
        // **The old assertion was `m.chars().any(is_ascii_digit)` and it could
        // not fail.** A reviewer replaced the deadline message with
        // `"stopped: it ran out of time (see rule 7)."` — a sentence naming no
        // limit at all — and it stayed green, because the cross-reference
        // carried a digit. `HARD-003` listed this as still open in its own
        // notes; that mutation is the receipt.
        //
        // What the guarantee actually is: **the number in the message is the
        // budget that fired.** So the budgets are given values nothing else on
        // the line could produce, and each message must contain its own. Any
        // digit anywhere is not evidence; this digit, here, is.
        let b = Budgets {
            max_kicks: 4_242,
            max_iterations: 5_353,
            max_tokens: 6_464,
            wall_clock: std::time::Duration::from_secs(7_575),
            ..Budgets::default()
        };
        for (ending, expected) in [
            (Ending::KicksExhausted, "4242"),
            (Ending::Iterations, "5353"),
            (Ending::Tokens, "6464"),
            (Ending::Deadline, "7575"),
        ] {
            let m = ending.message(&b);
            assert!(
                m.contains(expected),
                "{ending:?} must name its own limit {expected}, and says: {m}"
            );
            // And it must not name somebody else's, which is the failure a
            // copy-pasted arm produces and which the digit test could not see
            // either.
            for (other, wrong) in [
                (Ending::KicksExhausted, "4242"),
                (Ending::Iterations, "5353"),
                (Ending::Tokens, "6464"),
                (Ending::Deadline, "7575"),
            ] {
                if format!("{other:?}") != format!("{ending:?}") {
                    assert!(
                        !m.contains(wrong),
                        "{ending:?} names {other:?}'s limit {wrong}: {m}"
                    );
                }
            }
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

    /// The token budget counts what a call **cost**, not how much context it
    /// carried.
    ///
    /// **Written because the weighting could be deleted with the workspace
    /// green.** Replacing the three multipliers with a plain sum — the exact
    /// arithmetic `billable_total_tokens` already does — turned nothing red.
    /// The scripted provider sets `input_tokens` and leaves both cache fields
    /// at zero, so every end-to-end test in this repository exercises the one
    /// input for which the weighted and unweighted sums agree.
    ///
    /// `Budgets::max_tokens` is where the argument is: a cache read is billed
    /// at 0.1× and a cache write at 1.25×, and a session that re-sends its whole
    /// conversation charges ~100,000 tokens of *size* per call against a bill of
    /// a few cents. Counting size fired the default budget five calls into a
    /// barely-started goal. That is the bug this function is the fix for, and
    /// until now the fix was undefended.
    #[test]
    fn the_budget_counts_what_a_call_cost_rather_than_what_it_carried() {
        // A long conversation being re-read from cache: almost all of the
        // context is a cache read, which is the case the budget used to get
        // wrong.
        let cached = emma_llm::Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            // Every field named, zeros included, so the cache split a reader
            // has to see is on the page — and so that a `Usage` which gains a
            // field fails to compile *here*, where the weighting that has to
            // account for it is written.
            cache_read_input_tokens: 100_000,
        };
        // 1,000 + 100,000/10 + 500.
        assert_eq!(cost_tokens(&cached), 11_500);
        // And the number the *log* carries is the unweighted one, so the two
        // must not be the same function. A cache read counted at its size is
        // roughly nine times the bill here.
        assert!(
            cached.billable_total_tokens() > cost_tokens(&cached) * 8,
            "the weighting is not being applied: size {} against cost {}",
            cached.billable_total_tokens(),
            cost_tokens(&cached)
        );

        // A cache *write* is the other direction — billed above face value, so
        // counting it at size under-charges.
        let written = emma_llm::Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 1_000,
            cache_read_input_tokens: 0,
        };
        assert_eq!(cost_tokens(&written), 1_250);

        // The control, and the reason the two above are not just arithmetic:
        // with no caching at all the weighted count is the plain sum, so
        // nothing here is a general multiplier applied to every call.
        let plain = emma_llm::Usage {
            input_tokens: 700,
            output_tokens: 300,
            ..Default::default()
        };
        assert_eq!(cost_tokens(&plain), 1_000);
        assert_eq!(cost_tokens(&plain), plain.billable_total_tokens());
    }

    /// The list of failed calls a goal remembers is bounded.
    ///
    /// **The mutation that survived:** making `trim_oldest` a no-op. Nothing
    /// went red, because no test has ever produced more than a handful of
    /// distinct failures. The list is written into a session record and read
    /// back by `--resume`, so an unbounded one is a payload that grows for the
    /// life of the session rather than a transient.
    ///
    /// Oldest-first is asserted as well as the size, because a cap that kept
    /// the wrong end would still pass a length check while throwing away the
    /// failure most likely to be repeated next.
    #[test]
    fn the_failed_call_list_is_bounded_and_loses_its_oldest_first() {
        let mut v: Vec<String> = (0..FAILED_EVER_MAX + 5)
            .map(|i| format!("call-{i}"))
            .collect();
        trim_oldest(&mut v);
        assert_eq!(v.len(), FAILED_EVER_MAX, "the list grew without bound");
        assert_eq!(
            v.first().unwrap(),
            "call-5",
            "the newest entries were dropped"
        );
        assert_eq!(v.last().unwrap(), &format!("call-{}", FAILED_EVER_MAX + 4));

        // The control: a list under the cap is not touched at all, so the
        // assertions above are about the cap rather than about a function that
        // always trims.
        let mut short: Vec<String> = vec!["only".into()];
        trim_oldest(&mut short);
        assert_eq!(short, vec!["only".to_string()]);
    }
}

// endregion: Tests
