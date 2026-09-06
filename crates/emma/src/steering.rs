//! Steering: what a person types while a goal is already running.
//!
//! **The rule this feature has to survive, not weaken.** `approval.rs` drains
//! the line channel before every prompt, because a `y` typed before a question
//! must never answer it. A steering line is queued, never delivered into an
//! open prompt, and taken up at the next model turn.
//!
//! **A stub, declared ahead of its port from the macOS fork.** The fold arm
//! and `session::append_user_text` it relies on are already in `session.rs`.
