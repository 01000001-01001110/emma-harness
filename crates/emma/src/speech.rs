//! Reading an answer out loud.
//!
//! Output only: no capture, no microphone, nothing leaving the machine, and
//! off unless asked for. The audit that cleared it is
//! `notes/audits/2026-08-27-divergent-emma-fork.md` §3.2.
//!
//! **The branch is macOS-only, through `say(1)`, and the owner runs Windows.**
//! So this lands with a real second implementation or it lands as a setting
//! that honestly says the feature is unavailable here — never as a row that
//! does nothing, which is the defect class `notes/design/coverage-contract.md`
//! exists to prevent.
//!
//! Two decisions from the branch worth keeping rather than re-deriving:
//! a voice is stored **by name**, so a machine without it falls back to the
//! system default and a synced settings file still starts; and an enhanced
//! voice is a **recommendation in the listing, never a silent substitution**,
//! because preferring one would talk a user out of the voice they chose in
//! their own accessibility settings.

// Port in progress; see `memory/mod.rs` for why these stubs exist.
