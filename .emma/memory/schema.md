# Memory schema — how to maintain this wiki

This file is the discipline. It ships inside Emma and is installed once, on the
first touch of a wiki. It is yours to edit: Emma never overwrites a `schema.md`
that already exists. Everything below is addressed to whoever maintains these
pages, which is usually a model.

This memory is a **wiki**, not a transcript and not a vector store. Knowledge is
integrated once, when it arrives, and kept current afterwards. It is never
re-derived per question.

## What belongs here: what was learned, never what was read

A page is **distilled** knowledge — a claim, a decision, a gotcha, written in
your own words by whoever learned it, with a `source:` saying where it came
from. It is not a copy of a web page, a file, a tool result or a conversation.

That is a rule about safety as much as about tidiness. Text pasted in verbatim
from outside is untrusted input sitting in a place a later read treats as the
project's own notes, and it survives the session that fetched it. Distilling
removes that. If a page needs its source to be checkable, cite the URL in
`source:` and say what you concluded from it — the reader can fetch it again.

## The three operations

**Ingest.** Something worth keeping appeared. Find the page that already owns
that topic and **update it**. Create a new page only when no existing page owns
it. Then update `index.md` and append to `log.md`.

**Query.** Read `index.md` first. It is the catalog, and it is small enough to
read whole long after `pages/` is not. Open only the pages the index points at.
When a question produces an answer worth keeping, file the answer back as a
page.

**Lint.** Sweep for contradictions, stale claims, orphans (pages nothing links
to), and topics mentioned everywhere but owned by no page. Report what you find.
Fix only what you are asked to fix.

## Writing a page

One topic per page, one page per topic. `pages/<slug>.md`:

```markdown
---
category: preferences
pinned: false
created: 2026-08-26
source: session 0f2a
---

# The title, as a level-one heading

The memory itself, in markdown. Link related pages with [[wiki-links]].
```

- `category` is a closed set: `preferences`, `projects`, `facts`, `workflows`,
  `people`, `references`. Nothing else parses.
- `pinned` is `true` or `false` — not `yes`, not `on`. A pinned memory is one
  that should be in front of you every session. Pin sparingly; a pinned
  everything is a pinned nothing.
- `created` is the date the page was born and does not change when you edit it.
- `source` says where the knowledge came from — a session id, a file, a person,
  a URL. Anything with a colon, a `#` or a leading dash in it is written back
  double-quoted; a value is read to the end of its line, so a `#` in a URL is
  part of the URL and not the start of a comment.
- The first `# heading` is the page's title. The index shows it. Renaming a
  page means changing that heading; the slug, which is the link target, stays
  where it is.
- No other key parses. A page carrying a fifth key is reported as unreadable
  rather than half-read, because a key nobody reads is a memory you believe you
  stored and nobody will ever see.

Write what will be useful again. Decisions and the reasoning under them,
conventions, gotchas and why they were not obvious, configuration facts about
this machine and this project. Do not write chatter.

**Never write a secret.** These files are plaintext markdown inside the
repository. They are readable by anything with the file open, they go into a
commit, and a commit is in every clone and every future state of that
repository — `git rm` does not take it back. No keys, no tokens, no passwords,
no session credentials, not even in a `source:`.

## Contradictions get flagged, never silently resolved

When new knowledge disagrees with what a page already says, you do not get to
pick. Record both, attributed and dated, and mark the disagreement with the
marker so a human can find every one of them with one grep:

```markdown
> [!CONTRADICTION] 2026-08-26
> The page says the deploy target is `dev`. This session's source says `int`.
> Unresolved — needs the owner.
```

The same applies to claims that look superseded: keep the old claim, say what
replaced it and when. Confidence in a page should match the confidence of what
it was built from. A tentative source does not become a confident page.

## Linking

`[[slug]]` points at another page. A link to a page that does not exist yet is
fine and is a useful signal for the next lint. Prefer a link over restating a
fact: **every fact has exactly one home**, and every other mention points at it.
Two pages stating the same thing is how a wiki starts to lie.

## The bookkeeping is not optional

Every mutation touches three files:

1. the page under `pages/`,
2. `index.md`, the catalog, grouped by category, one line per page,
3. `log.md`, append-only, oldest first, with a parseable prefix:
   `## [YYYY-MM-DD] <op> | <slug>` followed by one line saying what changed.

Emma's library does all three for you when the mutation goes through it. When
you edit a page by hand, do all three by hand. `index.md` is regenerated from
`pages/` and `archive/` on every mutation, so anything you drop into `pages/`
by hand appears the next time anything writes.

## Unreadable pages

A page whose frontmatter does not parse is **reported, never dropped**. It gets
a line under `## unreadable` in `index.md` saying what is wrong with it, and it
stays exactly where it is. Nothing was lost; fix the frontmatter and it returns
to its category.

## Archiving

A memory that stopped being true is **archived, not deleted**: the whole file
moves to `archive/`. History is the point. Delete nothing. An archived page is
not edited afterwards — if the topic is live again, write the current truth as
a new page and let the archive say what was believed before.
