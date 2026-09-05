# Contributing to Emma

Emma is a coding agent for one person on one machine, and it is written mostly
by agents. That shapes what a useful contribution looks like here, so this file
is about the parts that are unusual rather than the parts that are not.

Issues and pull requests both go to
<https://github.com/01000001-01001110/emma-harness>.

## Build and test

```
$ cargo build --workspace
$ cargo test --workspace
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The workspace pins `rust-version = "1.82"`. The suite runs on Linux, macOS and
Windows; if you are on one of those and a test fails for a reason that looks
platform-shaped, say so in the report rather than adjusting the assertion — two
of this project's platform tests were wrong in exactly that way, and the fix was
to make the test ask which platform it was on.

To run the binary you just changed:

```
$ cargo install --path crates/emma --force
```

## Reporting a bug

The most useful report names the binary. Include `emma --version` and, if you
built it yourself, the commit; your OS, terminal and provider; and the output of
`emma config check` from the directory where things went wrong.

For anything the terminal drew, the terminal matters as much as the code. A cell
buffer is not a console, and this project has shipped a display defect with a
green test behind it twice.

Report a security issue privately instead — see [SECURITY.md](SECURITY.md).

## What a change is expected to carry

### A test that can fail

Every behaviour gets a test, and a test that cannot fail is worse than no test,
because it reads as coverage. Prove yours can fail: break the guarantee, watch
the test go red, put it back, watch it go green. Say in the pull request which
mutation you ran and what it did.

This is not a formality. A source-grep test here once passed while the thing it
guarded was disabled entirely.

### A comment that carries the argument

Read the module's existing documentation before adding to it and match that
register. A comment restating the line below it is noise. A comment explaining
why the obvious approach was wrong is the reason the file is readable at all.

Public items get `///`; modules get a `//!` header naming the role and the
invariants.

### Documentation that is still true

`docs/` is the project's written record, and a behaviour it describes wrongly is
a defect of the same kind as a failing test — fixed in the change that caused
it, not filed for later. `docs/CONTRACT.md` says how a page is written.

Every non-obvious claim on a page carries an evidence chip: `certified` for
something observed against the real API, terminal or file on disk; `tested` for
something a test covers; `unverified` for something believed but unchecked,
followed by what would settle it. **When a status changes, move the chip.** A
stale `certified` costs more than an honest `unverified`, because the whole
site's credibility rests on the green ones meaning something.

Diagrams are generated, never drawn by hand:

```
$ cargo run -p emma-docsgen
```

`cargo test --workspace` regenerates them and fails if the committed SVGs differ
from what the source produces now — the same arrangement as `cargo fmt --check`.

### A changelog entry, if a user would notice

`CHANGELOG.md`, under `## Unreleased`, for a changed command, config shape, tool
argument or terminal behaviour. Not for refactors, tests or internals; `git log`
covers those. Emma is `0.x`, so a breaking change is a minor bump and gets a
`BREAKING` line saying what to do about it.

### A commit message that says what you learned

Not a summary of the diff — `git log` can read the diff. What you tried that did
not work, what you could not verify, which mutation you ran. This project has no
author's memory: roughly twenty agents have written it, each of which saw one
crate, and there is nobody left to ask why. The commit message is the one record
that cannot drift from the change, because it is attached to it.

Do not create a `notes/` directory. There used to be one, it is gone
deliberately, and an instruction pointing into a directory nobody reads is worse
than no instruction.

## Things that will come up in review

**Say what you could not verify.** Explicitly, with what would settle it. A
green suite is not evidence and has been wrong here three times: a
language-server readiness check that declared ready at 0.36s against the real
server, a frontmatter parser that dropped 86 of 90 real files because its
fixtures used LF, and a strict wire decoder that passed every test and then
decoded no real tool call because the API adds a key the model did not know.
Fixtures agree with their author. Certify against the real thing and paste what
it said.

**Do not fabricate.** A tool that cannot compute something says so rather than
showing a plausible substitute. Output that was cut names the cap, the loss and
the remedy, or says plainly that no argument raises it.

**The approval gate is a consent interface, not a sandbox.** It shows what a
tool declared and asks. It confines nothing, and `Bash` can do anything the
shell can. Do not write code or documentation implying otherwise.

**Redirected output contains zero escape bytes**, on every path including `-p`.
`crates/emma/tests/no_escape_bytes.rs` runs the binary with both streams piped.
Colour on a terminal attached to stderr is correct and is not this rule.

**The `scrolling-regions` ratatui feature stays off.** `crates/emma/Cargo.toml`
explains why, and a test in `term/frame.rs` reads the manifest and fails if the
feature is ever declared.

## Licence

Contributions are under the Apache License 2.0, the same as the rest of the
project. See [LICENSE](LICENSE).
