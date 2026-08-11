# Working on Emma

Emma is a single-user CLI coding agent in Rust. `crates/{tool-api, harness, llm,
emma}` plus `tools/{fs, tasks, web, lsp}`. The loop lives in
`crates/emma/src/agent.rs`; everything with a decision in it is in the library
rather than `main.rs`, so a scripted `Provider` can drive it without a network.

## Finish by writing down what you learned

**Every task ends with a lesson, or with a sentence saying there wasn't one.**
Write it to `notes/lessons/<slug>.md` — one file per lesson, so concurrent
authors never collide. `notes/lessons/README.md` has the format and the rules
about what belongs.

This is not bookkeeping. The code lands and the reasoning evaporates: why the
obvious approach was wrong, what the passing test failed to catch, which claim
from a write-up turned out to be false. That is the part worth keeping, and the
only person who can write it is whoever just hit it.

Report it as well as writing it — a lesson nobody relays is one nobody reads.

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

- `notes/improvements.md` — the backlog. Each item says where the idea came
  from and under what licence.
- `notes/lessons/` — what was learned, one file per lesson.
- `notes/design-*.md`, `notes/research-*.md`, `notes/survey-*.md` — the
  investigations, including the ones that concluded "do not build this".

Read the relevant one before starting. Several of them exist specifically
because a plausible idea was refuted, and repeating the refuted idea is the most
expensive thing you can do here.
