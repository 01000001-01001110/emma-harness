//! The records a run graph can be drawn from, and nothing it would have to
//! guess.
//!
//! **Named `runfacts` rather than `telemetry`, and the rename is the point.**
//! The fork called this `telemetry`, a word that means "measurements sent
//! somewhere". This sends nothing anywhere: every record goes to the local
//! session JSONL, there is no network, no prompt text, and no hostname. A user
//! reading `telemetry.rs` in a coding agent would reasonably assume they were
//! being reported on, and being wrong about that in the reassuring direction is
//! worse than a clumsy name.
//!
//! The rule it keeps: a screen must never claim what the store cannot back, so
//! every field is something the process measured when it wrote the record, and
//! a field that would have to be estimated is absent instead.

// Port in progress. See `memory/mod.rs` for the note on why these stubs exist.
