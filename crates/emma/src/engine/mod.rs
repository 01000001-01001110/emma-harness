//! Engines: the thing that turns a goal into work.
//!
//! There have only ever been two. Emma's own [`crate::agent::Agent`] loop is
//! the first and is not in here, because it is the default and everything else
//! in this crate is built around it. [`claude`] is the second: it hands the
//! whole goal to the `claude` CLI and streams the result back, running no loop
//! of its own.
//!
//! The boundary is a goal. Both engines take one goal's text and return one
//! [`crate::agent::Outcome`], and `main.rs` picks between them once, on the
//! resolved provider's name. Nothing below that branch knows there is a choice,
//! and nothing above it knows how either engine works.

pub mod claude;
