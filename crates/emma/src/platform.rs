//! `.platform/context.yaml` — what this machine is, detected mechanically.
//!
//! Ported from the Mac branch. **No model call anywhere in here**: every field
//! is something the process looked up, and a field that would have to be
//! guessed is absent rather than inferred. A curated fence in the file
//! survives a refresh, so a human's notes are not overwritten by the next
//! detection run.

// Port in progress; see `memory/mod.rs` for why these stubs exist.
