//! A local-model provider, and the second citizen that proves the provider
//! boundary is a boundary.
//!
//! Ported from the Mac branch, where it is the reason that branch exists.
//! Emma's stated direction is **model-agnostic**: Anthropic is not the assumed
//! provider, and a `Provider` implementation that is not Anthropic is the only
//! thing that can demonstrate the abstraction holds.
//!
//! **The one thing this must not repeat.** The branch takes its destination
//! from `OLLAMA_HOST` with no confirmation and no boot line, so a provider the
//! user believes is local can send the whole conversation to an arbitrary
//! remote host. Its own module doc concedes that this is "the setting that
//! actually matters here" and then does not surface it. A non-loopback host is
//! named at startup, or this does not ship.

// Port in progress; see `crates/emma/src/memory/mod.rs` for why these stubs exist.
