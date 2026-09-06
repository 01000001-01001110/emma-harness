//! Training material out of session transcripts: one JSONL record per
//! assistant turn, with the thinking kept as its own field.
//!
//! **The session log is not changed and is never written to.** The per-turn
//! context comes from `session::fold_prefixes`, the same fold the loop uses,
//! stopped before each assistant record.
//!
//! **A stub, declared ahead of its port from the macOS fork.**
