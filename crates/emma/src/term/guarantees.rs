//! The guarantees of `frame.rs` and `app.rs`, asserted from outside them.
//!
//! **This module exists because those two files are being replaced.** The Mac
//! branch's versions are the base for the merged tree
//! (`notes/design/tui-fork-integration.md`), and
//! `notes/design/term-hardening-backport.md` counted what that costs: 65
//! hardening items, **57 of which had no test outside the file being
//! replaced**. A test living inside a file that is about to be overwritten is
//! deleted by the overwrite, and the guarantee it defended disappears with
//! nothing to announce it — the suite stays green because the assertion left
//! along with the code.
//!
//! The four that did survive were a thinner net than they looked. Two are in
//! `tests/pty.rs` and environment-gated: they print a skip and return, so an
//! absence of failure is not evidence. The two in `tests/no_escape_bytes.rs`
//! cannot see a tool's own bytes, because a keyless run never reaches a tool.
//!
//! So this file is the net. Everything here is written to go **red on
//! arrival** if the incoming version of `frame.rs` or `app.rs` lacks the fix,
//! rather than to pass quietly because the assertion was deleted too.
//!
//! # How these are written, and why not simply moved
//!
//! A test moved verbatim breaks on a rename and reads as a regression when it
//! is really a refactor. Two shapes are preferred here, in this order:
//!
//! 1. **Source-reading assertions**, where the guarantee is about *ordering* or
//!    *presence* — the panic hook installing before raw mode, a feature staying
//!    out of a manifest. These survive any rename, and they are the only way to
//!    assert an ordering that has no observable effect until the process dies.
//!    `frame.rs` already used this shape for the `scrolling-regions` check, and
//!    that test is one of `CLAUDE.md`'s named enforcers.
//! 2. **Behaviour through the narrowest surface that exists**, for everything
//!    else. Narrow because every item named here couples this file to the two
//!    being replaced, and a wide surface turns a legitimate refactor into a
//!    wall of red.
//!
//! **A compile error here is a success, not a mishap.** If the incoming
//! `frame.rs` has no `erase_frame`, this file stops compiling, and that is the
//! loudest possible signal that an item on the checklist needs a decision. Read
//! `notes/design/term-hardening-backport.md` and either re-apply the fix or
//! record why the item no longer applies.
//!
//! # What this file is not
//!
//! It is not a second copy of the `term/` test suite, and it must not grow into
//! one. Only guarantees that (a) somebody argued for in a commit or a lesson,
//! and (b) have no defender outside the replaced files, belong here. Everything
//! else stays where it is written.

// The items are added by the enforcer lift, one region per source file.
