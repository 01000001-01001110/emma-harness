# Working on Emma

Emma is a single-user CLI coding agent in Rust. `crates/{tool-api, harness, llm,
emma}` plus `tools/{fs, tasks, web, lsp}`. The loop lives in
`crates/emma/src/agent.rs`; everything with a decision in it is in the library
rather than `main.rs`, so a scripted `Provider` can drive it without a network.

## `docs/` is the source of truth. Read it first; leave it true.

**`docs/` describes how Emma works and why. It is not a rendering of a truth
that lives somewhere else — it is the place a stranger goes first, and the place
that has to be right.** Open `docs/index.html` in a browser; it needs no server
and no build step. `DOCS.md` says how a page is written.

**Read the page for the area you are about to touch before you touch it.** It
is faster than reading the crate, and it carries the arguments — what was tried,
what was reversed, and which part will bite you — that the code cannot.

**A behaviour that exists in the code and that
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

## Finish by saying what you learned, in the commit that carries it

**This rule used to name two files and both are gone.** Until 2026-08-30 every
task ended with a lesson in `notes/lessons/<slug>.md` and an entry in
`notes/STATUS.md`. `notes/` is untracked now, so the instruction has no
destination — and an instruction with no destination is worse than none,
because it gets followed into a directory nobody reads.

**What survives is the reason, which was never really about the files.** The
code lands and the reasoning evaporates: why the obvious approach was wrong,
what the passing test failed to catch, which claim from a write-up turned out to
be false. That is worth keeping, and the only person who can write it is
whoever just hit it.

**So put it in the commit message.** Not a summary of the diff — `git log` can
read the diff. The argument: what you tried that did not work, what you could
not verify, which mutation you ran and what it did. A commit message is the one
record that cannot drift from the change, because it is attached to it.

**Do not recreate `notes/` to satisfy this.** If a finding is bigger than a
commit message, say so in your reply and the owner will file it in the store
that outlives this repository.

**What this repository can no longer do is carry state between sessions** —
what is in flight, what is blocked and on what. That was `ACTIVE.md`'s job.
**Do not reconstruct it from `git log`:** a commit says what changed and never
what was abandoned halfway, and roughly twenty agents once spent two days
rebuilding that board from history and whoever happened to be around to ask.
Ask the owner instead; it is his to hold now.

## Install the binary you changed, or the change is not delivered

**The owner runs `emma` from `~/.cargo/bin`, not from `target/`.** A fix that is
committed, tested and never installed is a fix he does not have. Owner
instruction, 2026-08-23: *"make sure that file is replaced regularly."*

```bash
cargo install --path crates/emma --force
```

Run it after any round that changes what the binary does. It is a release build,
so it is slow — around ninety seconds — which is the reason to do it on a round
boundary rather than on every commit.

**This has already cost a bug report.** The owner reported that the `Alt` chords
did not open the tool pages, twice, using the word "still". The chords were
fine. His installed binary was ten hours behind `HEAD`, from before the pages
were wired. The whole exchange — his report, the investigation, the reading of
`tool_key` and `launch_tool` — was spent on a defect that did not exist, and it
ended with *"That did it."* after a reinstall.

The general form is worth keeping, because it is not really about `cargo
install`: **what the owner runs is the artefact, and the repository is not the
artefact.** A green suite over source he is not executing says nothing about
what is in front of him, and a report from him is always about the binary he
has. Ask which one that is before reading any code.

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
the recorded failures are the most valuable thing here: every expensive mistake this
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
  2026-08-12, reversing the old
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
- **Redirected output contains zero escape bytes.** A pipe or a file, on any
  path, including `-p`. There is a test now — `crates/emma/tests/no_escape_bytes.rs`,
  which runs the binary with both streams piped. Keep it.

  Two corrections, both from an independent review on 2026-08-23, and both
  about this sentence rather than about the code. It used to say *"piped and
  `-p` output"*, and `-p` with a **terminal** on stderr does colour that
  stderr — correctly, because a terminal is not somebody's pipeline. And it
  used to say "there is a test" when there was not one: what existed were
  component-level assertions inside `term/`, none of which ran the binary.
  The test's own doc records the one path it still cannot reach.

  **A third correction, 2026-08-23, and this one was about the code.** The
  sentence was false as written. It held for Emma's own styling and not for
  text passing through: `for_stream` at `Level::None` calls `plain`, which
  returns span contents verbatim, and `Skin::body` put raw tool stdout into a
  span. So `emma | tee log` with a `Bash` call that emitted colour wrote those
  escape bytes to the file, and a tool emitting a bare CR could overwrite the
  line above its own output in a record somebody reads later. Fixed at the
  source — `Skin::body` now runs the same `sanitise` `markdown.rs` has had
  since the status line hit this class — and pinned by
  `a_tools_own_escape_bytes_do_not_reach_a_redirected_stream`, which goes red
  when the call is removed.

  Worth keeping for the shape rather than the fix: `no_escape_bytes.rs` is a
  good test of the path it covers and could never have seen this, because a
  keyless run never gets as far as running a tool, so no outside bytes reach
  the renderer. And one sanitiser existed one module over with a test on it.
  **One input shape, two answers, inside one codebase** — when a tolerance is
  added to one reader, go and look at the others.
- **A tool failure is an observation, not an abort.** It comes back as a
  `tool_result` with `is_error` and the loop continues.
- **The approval gate is a consent interface, not a sandbox.** It enforces
  nothing a tool does not honestly declare, and `Bash` can do anything. Do not
  write documentation implying otherwise.
- **Licences before code.** A README is not a licence, and `NOASSERTION` on
  GitHub means open the actual file. One project in this space carries a rider
  granting no rights to Anthropic, with `use` defined to include
  benchmarking and analysis — so a model-driven agent merely reading it is
  plausibly inside the restriction.

## Where things are written down

**`notes/` is no longer part of this repository.** As of 2026-08-30 it is
untracked and gitignored: 154 files still sit on the owner's disk, and their
durable content was distilled into his knowledge vault first — the lessons, the
design arguments and the surveys of other harnesses, plus the four mockup PNGs
copied out with their checksums verified, because a markdown store cannot carry
an image.

**What follows from that, and it is the part that matters to you:** the four
records below still have four different jobs, and three of them now have
nowhere in this repository to live. Do not recreate `notes/` to satisfy a rule.
If you learned something durable, say it in the commit message that carries the
change, where it stays attached to the diff that caused it. If you are working
for the owner, he will file it.

The one thing this repository cannot do any more is hold *state between
sessions* — what is in flight, what is blocked. That was `ACTIVE.md`'s job and
it is now the owner's to carry. **Do not infer it from `git log`**: a commit
says what changed, never what was abandoned halfway.

- `notes/ACTIVE.md` — **gone from the repo.** Was: what is being worked on right
  now, and what to pick up next.
- `notes/STATUS.md` — **gone from the repo.** Was: where the work stands, dated
  entries, append-only, and distrusted once older than the newest commit.
- `CHANGELOG.md` — what is different for somebody upgrading, and nothing else.
  Add to `## Unreleased` when you change something a user would notice: a
  command, a config shape, a tool's arguments, what the terminal does. Not for
  refactors, tests or internals — those are `git log`'s job. Emma is `0.x`, so a
  breaking change is a **minor** bump and gets a `BREAKING` line saying what to
  do about it. Four records, four jobs: this one is the only one addressed to
  somebody who has not read the code.
- `docs/` — **the one written record this repository still keeps**, and the
  rules at the top of this file about it are unchanged. It describes how Emma
  works and why, it carries an evidence chip per non-obvious claim, and a
  behaviour it describes wrongly is a defect of the same kind as a failing
  test.
- `notes/improvements.md`, `notes/lessons/`, `notes/design/`, `notes/research/`,
  `notes/audits/`, `notes/plans/`, `notes/archive/` — **all gone from the repo.**
  The backlog, the lessons, the investigations and the superseded arguments.
  Their content was mined into the owner's vault before removal; the files
  remain on his machine, ignored.

**None of it is in this repository's history either.** The history was
rewritten on 2026-09-01 to drop `notes/`, `verification/`, `blog/` and every
image from every commit, so `git log -p -- notes/` returns nothing in a clone.
This paragraph said the opposite until 2026-09-05, and four other files
repeated it; an independent review caught that `git log --all -- notes/` was
empty. The files exist on the owner's disk, ignored, and their durable content
is in his vault. A citation to `notes/` anywhere in this repository names
something a reader cannot open.

Read the relevant one before starting. Several of them exist specifically
because a plausible idea was refuted, and repeating the refuted idea is the most
expensive thing you can do here.
