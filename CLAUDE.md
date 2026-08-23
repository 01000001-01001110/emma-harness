# Working on Emma

Emma is a single-user CLI coding agent in Rust. `crates/{tool-api, harness, llm,
emma}` plus `tools/{fs, tasks, web, lsp}`. The loop lives in
`crates/emma/src/agent.rs`; everything with a decision in it is in the library
rather than `main.rs`, so a scripted `Provider` can drive it without a network.

## `docs/` is the source of truth. Read it first; leave it true.

**`docs/` describes how Emma works and why. It is not a rendering of a truth
that lives somewhere else — it is the place a stranger goes first, and the place
that has to be right.** Open `docs/index.html` in a browser; it needs no server
and no build step. `docs/CONTRACT.md` says how a page is written.

**Read the page for the area you are about to touch before you touch it.** It
is faster than reading the crate, and it carries the arguments — what was tried,
what was reversed, and which part will bite you — that the code cannot.

**A behaviour that exists in the code, or is decided in `notes/`, and that
`docs/` does not describe or describes wrongly, is a defect of the same kind as
a failing test.** Not untidiness to file for later. It is fixed in the change
that caused it, by whoever caused it — the same reasoning as a lesson: relayed
through somebody else it loses whatever they did not think worth repeating.
**A commit that changes what Emma does and leaves the docs describing the old
behaviour is incomplete.**

**Every non-obvious claim on a page carries an evidence chip** — `certified`
(proven against the real API, terminal, or file on disk), `tested` (a test shown
to fail when the guarantee is removed), or `unverified` (believed, not checked,
and then say what would settle it). **When a status changes, move the chip.** A
stale `certified` is worse than an honest `unverified`, because the whole site's
credibility rests on the green ones meaning something. A page with no amber on
it has not been honest yet.

**Diagrams are drawn from the source, at the time of drawing.** A diagram is
trusted in proportion to how confident it looks, so one drawn from recollection
launders a guess into a reference. Read the manifest, read the function, then
draw.

The other records keep their jobs — this one describes the _system_, never the
project's week. Where `docs/` and a note disagree, the code decides, and the
disagreement is itself worth a sentence.

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

## Report the round to the project channel when the turn is done

**Post to the project's Discord channel every round.** Owner ruling,
2026-08-23: *"At each of these I want an update in discord"* — each round, not
only at the end of a long turn, and not each commit inside a round. A round is
one reply to the owner: whatever was finished between one message and the next.
The webhook lives in `EMMA_PROGRESS_WEBHOOK` in `.env`, which `.gitignore`
covers.

```bash
python verification/scripts/post_progress.py <<'EOF'
**Emma** — what actually happened this round, in Discord markdown.
EOF
```

The message goes on stdin so a round's text is never baked into a file, and the
script reads the credential itself. **A User-Agent is not optional**: without one
Discord's edge answers `403 error code: 1010`, a browser-signature ban that reads
as "this webhook is dead" rather than "add a header". That cost a diagnosis
once; the script carries the header and the reason.

**The URL never goes in a file git tracks, and that includes this one.** A
webhook is a credential: anyone holding it can post as the project. This
repository is private today, which is not the same as safe — a credential in a
commit is in every clone and every future state of that repository, and
`git rm` does not remove it from history. `.env` is where the repo already keeps
this class of thing.

**Say what happened, not what was attempted.** The same rule as everywhere else
here: a round that fixed two things and left a third broken says so. A progress
report that reads as uniformly successful is the one nobody believes twice, and
this one is addressed to somebody who was not watching — which makes an
overstatement harder to catch and therefore worse.

Post the outcome, the evidence, and what is still open. If a round produced
nothing worth reading, post nothing.

## The premise the rules follow from

**This codebase has no author's memory, and every reader is a stranger —
including the owner.** Roughly twenty agents have written it; each saw one
crate, and the owner has never had the whole of it in his head. That is not a
gap somebody will eventually close by reading everything. It is the permanent
condition of the project.

In an ordinary codebase the author's memory is the backstop — ask them why,
and they know. Here there is nobody to ask, so the written record has to carry
its own scepticism: what was measured, what was assumed, and what nobody has
actually looked at. That is why a comment carries the argument rather than
restating the code, why a test that cannot fail is called a false receipt, and
why "say what you could not verify" is a rule rather than a courtesy — each is
a substitute for a memory that does not exist. And it is why the failures in
`notes/lessons/` are the most valuable thing here: every expensive mistake this
project has made was a confident claim with nobody present who knew better — a
token figure that did not survive measurement, an anchoring technique that was
reversed, a strict decoder that passed every test and broke on the first live
call, a test-count command that reported green over a red test. In each case
the record was the only possible correction, and where the record was silent,
the mistake shipped.

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

- **The interactive frame is full-screen, on the alternate screen** — since
  2026-08-12, stage 2 of `notes/design/tui-fullscreen.md`, reversing the old
  "`Viewport::Inline` only, never the alternate screen" rule on the owner's
  decision. What the reversal cost, per that design's §2: terminal scrollback
  (replaced by the retained `term/transcript.rs` buffer, keys and wheel),
  native mouse selection (mouse capture owns the wheel; Shift-drag selects,
  cleanly only with the sidebar collapsed, until `/export` lands), and
  re-reading the styled run after exit (gone — the session JSONL named in the
  exit line is the record). Three things still hold absolutely: the plain
  fallback (`-p`, pipes, `EMMA_NO_FRAME`, no console) never touches the
  alternate screen; every exit path — return, `Drop`, panic, Ctrl-C,
  `process::exit` — leaves it (a stranded alt screen is the failure people
  uninstall over); and `EMMA_UI=inline` keeps the old inline viewport for one
  release as the escape hatch.
- **The `scrolling-regions` ratatui feature stays off.** The reversal above
  does not touch this. `crates/emma/Cargo.toml` explains why; `term/frame.rs`
  has a test that reads the manifest, comments stripped, and fails if the
  feature is ever declared. (Stripped because the manifest names the feature
  in a comment to explain why it is off — naming it is fine, enabling it is
  not.)
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

- `notes/ACTIVE.md` — what is being worked on **right now**, and what to pick up
  next. Small, current, rewritten in place rather than appended to. Read it
  first; `notes/README.md` maps the rest of the directory.
- `notes/STATUS.md` — where the work stands, dated entries, append-only.
  Distrust an entry older than the newest commit.
- `CHANGELOG.md` — what is different for somebody upgrading, and nothing else.
  Add to `## Unreleased` when you change something a user would notice: a
  command, a config shape, a tool's arguments, what the terminal does. Not for
  refactors, tests or internals — those are `git log`'s job. Emma is `0.x`, so a
  breaking change is a **minor** bump and gets a `BREAKING` line saying what to
  do about it. Four records, four jobs: this one is the only one addressed to
  somebody who has not read the code.
- `notes/improvements.md` — the backlog. Each item says where the idea came
  from and under what licence.
- `notes/lessons/` — what was learned, one file per lesson.
- `notes/design/`, `notes/research/`, `notes/audits/`, `notes/plans/` — the
  investigations, including the ones that concluded "do not build this".
  `design/` is how a subsystem works and why; `research/` is external fact and
  surveys of other harnesses; `audits/` is a dated finding, true of the day it
  was written; `plans/` is forward work.
- `notes/archive/` — superseded, kept for the argument it carries. Never cite it
  as a description of the system now.

Read the relevant one before starting. Several of them exist specifically
because a plausible idea was refuted, and repeating the refuted idea is the most
expensive thing you can do here.
