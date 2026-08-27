//! A per-repository wiki of **distilled durable knowledge**.
//!
//! Ported from a divergent fork reviewed on 2026-08-27
//! (`notes/audits/2026-08-27-divergent-emma-fork.md`), with one deliberate
//! subtraction: the fork also wrote the **verbatim body of every successful
//! `WebFetch`** into `.emma/memory/raw/web/`. That is not ported. Owner
//! ruling, 2026-08-27: the wiki keeps *durable knowledge distilled from* a
//! page, never the page.
//!
//! **Why the subtraction is a security fix and not a preference.** Verbatim
//! remote text inside the working tree is untrusted input sitting where a
//! later read treats it as the project's own notes — a prompt-injection
//! surface that also lands in a diff. The fork guarded it with a secret strip
//! its own comment declares is not a boundary. A distilled page is written
//! deliberately by whoever learned the thing, which removes the surface rather
//! than filtering it.

// Port in progress. The module's contents are owned by one worker; this file
// exists so the module is declared and testable from the first commit.
