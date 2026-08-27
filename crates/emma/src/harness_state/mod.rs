//! What the session file says about runs, events and approval posture.
//!
//! Read-only derivation over records `session.rs` already writes — it adds no
//! new record and decides nothing. Ported from the fork reviewed on
//! 2026-08-27; its discipline is the reason it was ranked second: `Option` on
//! nearly every field with the reason it is optional, one named inference with
//! its measurement, and a per-feed count of lines it could not read, so a page
//! built on this can say how much of its own input was unreadable rather than
//! drawing a confident picture over a gap.

// Port in progress. See `memory/mod.rs` for the note on why these stubs exist.
