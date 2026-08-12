# Working on Emma

Emma is a single-user CLI coding agent in Rust. `crates/{tool-api, harness, llm,
emma}` plus `tools/{fs, tasks, web, lsp}`. The loop lives in
`crates/emma/src/agent.rs`; everything with a decision in it is in the library
rather than `main.rs`, so a scripted `Provider` can drive it without a network.

## Finish by writing down what you learned, and where things stand

**Every task ends with two things, or a sentence saying either one didn't
apply: a lesson in `notes/lessons/<slug>.md`, and an entry in
`notes/STATUS.md` saying where the work now stands.** They fail differently if
skipped. Skip the lesson and the same mistake gets made twice. Skip the status
entry and the next agent — maybe you, an hour and a context-limit later, maybe
someone else entirely — rebuilds the board from `git log` and whoever happens
to be around to ask, which is what roughly twenty agents did in two days on
this repository before there was anywhere to write it down. `notes/lessons/`
already has one file per author for exactly this reason; `notes/STATUS.md` is
the second half, for the question a lesson doesn't answer: not what did we
learn, but where are we right now.

**Update it when the board moved** — something landed, something is now
blocked on a named thing, or a plan you were following turned out to be wrong.
Not on every commit inside a task: a rule that fires that often gets skimmed
and then skipped. Not unconditionally at every task's end either — a dead end
that changed nothing durable gets a lesson, not a status line pretending the
board is different.

**Whoever did the work writes it, same reasoning as a lesson.** A status
relayed through an orchestrator loses whatever the orchestrator didn't think
worth repeating — the exact failure the one-file-per-lesson rule exists to
avoid. An orchestrator may open the file to read it before dispatching; it
does not write a worker's entry for it.

**Append; never edit or delete another agent's entry.** A single mutable
status paragraph is the shape that loses work when two agents finish
together — this repository already had two agents write one file at once and
got away with it on luck alone, once, which is not a plan. Put your entry on
top and stop; if it lands next to one you didn't expect, leave it, even if the
two disagree — a disagreement between two entries is a real signal about the
state of the repo, and silently overwriting one to make the file tidy destroys
that signal along with the entry.

**Date every entry, and don't trust one older than the newest commit.**
`git log -1` against the top entry's date is the check: commits after it mean
the entry is stale by construction, not by suspicion, and the move is the same
one this file already asks for on a green test suite — go and look at the real
thing before repeating what the document claims.

**The four records don't overlap.** A commit message says what changed and
why. `notes/lessons/` says what was learned. `notes/improvements.md` says what
could be done next. `notes/STATUS.md` says where things stand right now — what
is in flight, what is blocked and on what, what to pick up next. A status entry
that restates a commit message is doing `git log`'s job; one that points at an
`notes/improvements.md` item number instead of re-describing the idea is doing
its own.

This is not bookkeeping. The code lands and the reasoning evaporates: why the
obvious approach was wrong, what the passing test failed to catch, which claim
from a write-up turned out to be false, and — just as perishably — what state
the work was in when the context ended. That is all worth keeping, and the
only person who can write any of it is whoever just hit it.

Report both as well as writing them — a lesson or a status nobody relays is
one nobody reads.

## The house rules

**Comments carry the why, not the what.** Read a module's existing doc before
adding to it; match that register. A comment restating the code is noise here. A
comment carrying the argument for a decision is the point.

**Every behaviour gets a test, and the tests that matter must fail if the
guarantee is removed.** Prove that by breaking the code and watching the test go
red — then put it back. Report which mutations you ran and what happened. A test
that cannot fail is worse than no test: it is a false receipt. This has been paid
for here already, by a source-grep test that passed while the thing it guarded
was disabled entirely.

**A green suite is not evidence.** It has been wrong three times in this
project: a language-server readiness check that declared ready at 0.36s against
the real server, a frontmatter parser that silently dropped 86 of 90 real files
because its fixtures used LF, and a strict wire decoder that passed everything
and then decoded no real tool call because the API adds a key the model did not
know. Fixtures agree with their author. **Certify against the real thing** — the
live API, the real terminal, the actual file on disk — and paste what it
actually said.

**Say what you could not verify.** Explicitly, in a section of its own, with
what would settle it. This matters most for anything drawn on a terminal: a
fully-tested scrollback defect has shipped from `crates/emma/src/term/` twice,
because a cell buffer is not a console.

**Do not fabricate.** If a tool cannot compute something, it says so rather than
showing a plausible substitute. If output was cut, it names the cap, the loss and
the remedy — or says plainly that no argument raises it.

## Constraints that are load-bearing

- **`Viewport::Inline` only, never the alternate screen.** It costs scrollback
  and mouse selection, and Emma's output is a transcript people read afterwards.
- **The `scrolling-regions` ratatui feature stays off.** `crates/emma/Cargo.toml`
  explains why; `term/frame.rs` has a test that reads the manifest and fails if
  it is ever named.
- **Piped and `-p` output contains zero escape bytes.** There is a test. Keep it.
- **A tool failure is an observation, not an abort.** It comes back as a
  `tool_result` with `is_error` and the loop continues.
- **The approval gate is a consent interface, not a sandbox.** It enforces
  nothing a tool does not honestly declare, and `Bash` can do anything. Do not
  write documentation implying otherwise.
- **Licences before code.** A README is not a licence, and `NOASSERTION` on
  GitHub means open the actual file. One project in this space carries a rider
  granting no rights to Anthropic — see `notes/improvements.md`.

## Where things are written down

- `notes/STATUS.md` — where the work stands right now, dated entries,
  append-only. Read it first; distrust an entry older than the newest commit.
- `notes/improvements.md` — the backlog. Each item says where the idea came
  from and under what licence.
- `notes/lessons/` — what was learned, one file per lesson.
- `notes/design-*.md`, `notes/research-*.md`, `notes/survey-*.md` — the
  investigations, including the ones that concluded "do not build this".

Read the relevant one before starting. Several of them exist specifically
because a plausible idea was refuted, and repeating the refuted idea is the most
expensive thing you can do here.
