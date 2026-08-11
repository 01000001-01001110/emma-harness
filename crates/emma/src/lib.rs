//! Emma: the loop, the gate, and the terminal around them.
//!
//! Three crates already exist and none of them knows about the others. The
//! harness resolves a directory into a value, the provider turns a `Request`
//! into an `AssistantTurn`, and the tools do things to a filesystem. This crate
//! is the only place they meet, which makes it the only place two properties
//! can be enforced:
//!
//! **A tool failure is an observation, never an abort.** Every failure class —
//! a missing tool, bad arguments, a `ToolError`, a hook denial, a refused
//! approval — reaches the model as a `tool_result` with `is_error` set and the
//! turn continues. The one thing that ends a goal is a budget or the user.
//!
//! **Writing is gated, reading is not, and leaving the machine is gated
//! separately.** `ToolMeta::read_only` decides the first, `reaches_network` the
//! third — two axes because writing is a risk the model takes deliberately and
//! egress is how a prompt-injected page turns a read tool into an exfiltration
//! channel. A `PreToolUse` hook outranks the human on both, and nothing grants a
//! permission that outlives the process. See [`approval`], which is the most
//! consequential file here: a tool surface that can write is a different safety
//! problem from one that cannot, and Emma's can.
//!
//! **A session is one conversation.** A goal is a turn in it, not a context of
//! its own: the tool traffic of one goal is still in front of the model for the
//! next, so a follow-up question does not re-read the file its answer came
//! from. Two things keep that safe — `session::place_turn`, which never lets a
//! `tool_use` reach the wire without its result, and `Agent::compact`, which
//! summarises the oldest goals once a request passes `Budgets::max_context`.
//! Compaction is lossy by decision: the words survive, the tool results do not,
//! and the model is told so.
//!
//! **A goal may be delegated, once at a time, and the record says what came of
//! it.** [`delegate`] is a `Tool` that runs a second [`agent::Agent`] against
//! its own conversation, its own tools and its own prompt — the one in an
//! `agents/<name>.md` file — while sharing this run's approval gate, session
//! file, interrupt and token meter. The loop does not know it exists: nesting is
//! values passed into [`agent::Setup`], never a branch inside it. What comes
//! back is the sub's own text with a footer the *harness* composed from that
//! run's log, because a delegation replaces evidence with testimony and the
//! footer is what makes the testimony checkable.
//!
//! **The map.** [`agent`] is the loop and holds both properties above.
//! [`approval`] is the gate it consults, and [`goal`] is what it holds across
//! turns — the goal text, the done-check trait, and the kick. Around those:
//! [`cli`] parses the arguments, [`commands`] is the three subcommands that run
//! without a model call, [`settings`] and [`skill`] read the user's preferences
//! and the harness's skills, [`session`] writes the transcript, and [`term`] is
//! everything the person at the keyboard sees. `main.rs` only wires them
//! together.
//!
//! Anthropic is the only provider. The wire shape a second one would have to
//! displace is called out where it is built, in `agent.rs`.

pub mod agent;
pub mod approval;
pub mod cli;
pub mod commands;
pub mod delegate;
pub mod goal;
pub mod permissions;
pub mod session;
pub mod settings;
pub mod skill;
pub mod term;

pub use agent::{Agent, Budgets, Ending, Interrupt, Outcome, Resumed, Setup, Spend};
pub use approval::{Answer, Approvals, Asker, Gate, Verdict};
pub use delegate::{Delegate, Nest};
pub use goal::{Done, DoneCheck, Goal, MarkerClaim};
pub use session::{Continuity, Restored, SessionLog};
