//! Engines: the thing that turns a goal into work.
//!
//! There have only ever been two. Emma's own [`crate::agent::Agent`] loop is
//! the first and is not in here, because it is the default. The second hands
//! a whole goal to the `claude` CLI and lives in [`claude`].
//!
//! **A stub, declared ahead of its port from the macOS fork.** The port must
//! not arrive with a Windows `exited()` that answers `true` without asking:
//! that is the kill-reports-success defect this repository has already paid
//! for once.

pub mod claude;
