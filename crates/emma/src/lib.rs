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
//! **Writing is gated and reading is not.** `ToolMeta::read_only` decides, a
//! `PreToolUse` hook outranks the human, and nothing grants a permission that
//! outlives the process. See [`approval`], which is the most consequential file
//! here: tustle-agent's tool surface could not write, and Emma's can.

pub mod agent;
pub mod approval;
pub mod cli;
pub mod commands;
pub mod goal;
pub mod session;
pub mod settings;
pub mod skill;
pub mod term;

pub use agent::{Agent, Budgets, Ending, Interrupt, Outcome, Setup};
pub use approval::{Answer, Approvals, Asker, Gate, Verdict};
pub use goal::{Done, DoneCheck, Goal, MarkerClaim};
pub use session::SessionLog;
