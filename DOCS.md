# Writing a page in `docs/`

**`docs/` is the source of truth.** Not a rendering of the truth that lives
elsewhere — the place a stranger goes first, and the place that must be right.
If a behaviour exists in the code and this site does not say it, or says it
wrongly, **that is a defect of the same kind as a failing test**, and it is
fixed in the same change rather than filed for later.

This file is the contract every page is built against. It exists because five
agents writing five pages will otherwise produce five designs, five vocabularies
and five ideas of what counts as proof.

## The mechanics

- **Plain HTML, opened from `file://`.** No server, no build step, no
  JavaScript, no external requests. Someone must be able to double-click
  `docs/index.html` on a fresh machine and read everything.
- **One stylesheet: `assets/docs.css`.** Link it as
  `<link rel="stylesheet" href="assets/docs.css">`. **Do not write a `<style>`
  block with private colours.** If a page genuinely needs a component the
  stylesheet lacks, add it to `docs.css` with a comment saying why, so the next
  page can use it too.
- **The sidebar is `assets/NAV.html`, copied verbatim**, with `class="nav here"`
  on the current page's link. It is duplicated per page because there is nothing
  to include a partial with, and that is the price of opening from `file://`.
- **Page skeleton**: `<!doctype html>`, `<html lang="en">`, `<head>` with
  `<meta charset="utf-8">`, `<meta name="viewport" content="width=device-width,initial-scale=1">`,
  a `<title>` ending in `· Emma`, the stylesheet link; then
  `<body><div class="shell">`, the nav, `<main><div class="col">` … `</div></main></div>`.

## What a page must contain

**Every claim about behaviour cites the file it came from.** Use
`<span class="src">crates/emma/src/agent.rs</span>` under the paragraph or block
it supports. A line number is welcome but ages badly; a file and a function name
age well. **An assertion nobody can check does not belong here.**

**Every non-obvious claim carries an evidence chip**, and this is the part that
matters most:

| Chip                                       | Means                                                                                                                                                                            |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `<span class="chip cert">certified</span>` | Proven against the real thing, **and a reader can repeat it**: a file they can grep, a command they can run, a count they can redo. Say how.                                     |
| `<span class="chip obs">observed</span>`   | Seen once, live, and **not repeatable on demand**: an API call whose transcript is gone, something read off a console, a number from a run nobody kept. Say who saw it and when. |
| `<span class="chip test">tested</span>`    | Covered by a test that has been shown to fail when the guarantee is removed.                                                                                                     |
| `<span class="chip unv">unverified</span>` | Believed, not checked. **Say what would settle it.**                                                                                                                             |

The split between the first two exists because they were one chip until
2026-08-16, and two pages then reached opposite confidence about the same number
without either author noticing. One had counted it off disk. The other could not
find the file. Both wrote what they honestly knew, and a reader had no way to
adjudicate. "I saw this happen" and "you can see this happen" are different
claims, and the mark now says which.

A page with no amber chips anywhere is not a page that got everything right; it
is a page that has not been honest yet. The chips are how this site stays
trustworthy as it ages: an amber chip is a standing invitation to go and settle
something, and it is supposed to be uncomfortable.

**One exception, added 2026-09-05.** A failure the code names and does not
remove now lives on `roadmap.html#hazards` rather than in the middle of the page
that explains the mechanism, and the amber goes with it. Some pages are
therefore all green while pointing at amber a click away, which is honest. What
is not allowed is a page that is all green and points at nothing: if you moved a
hazard out, the sentence you left behind names it and links the list.

## Chapters and their pages

A page that has grown past one screenful of a subject becomes a **chapter**: it
keeps its name and its place in the sidebar, and its sections move to pages of
their own.

- **Naming.** `<chapter>-<section>.html`, lowercase, hyphenated:
  `loop-one-turn.html`, `providers-caching.html`, `consent-precedence.html`.
  The prefix means a directory listing sorts a chapter together.
- **The sidebar does not grow.** It lists the fourteen chapters and nothing
  more. Sixty pages each carrying a sixty-entry sidebar would mean every nav
  change lands in sixty files, and the whole reason these pages open from
  `file://` is that there is no build step to fix that for us.
- **The chapter page becomes a contents page.** It keeps its lede, a short
  orientation, and a list of its sub-pages with a sentence each. The detail
  moves out. A reader who wants the shape reads the chapter; a reader who wants
  the mechanism opens one page.
- **Every sub-page carries a chapter strip** directly under its `<h1>`: the
  chapter name linked, then its sibling pages, with the current one marked. Use
  `<nav class="chapter">`; it is the sub-page's local map and the sidebar's job
  stops at the chapter.
- **`<h1>` is the section title, not the chapter's.** `<title>` is
  `Section · Chapter · Emma`.

### What a sub-page owes that a chapter page did not

**Show the code.** A sub-page about a mechanism quotes the function it
describes, from the file, in a `<pre><code>` block, with the file and the
function named above it. Not a paraphrase of the code and not the whole file:
the part the argument turns on, long enough to read on its own. Quote it
exactly, including the comments, because the comments carry the reasoning this
project puts there deliberately.

**Generate the quotes rather than typing them.** Pull each block out of the file
by line range with a script, then verify. Hand-typing is where misquotes come
from, and generating removes the class instead of checking for it afterwards.

Verify with three checks, not one. "Does this line exist somewhere in the
source" is the weakest of them and passes on a block that has quietly drifted:

1. **Every quoted line appears in the source**, and you can account for each one
   that does not (captured program output, a shell one-liner, your own citation
   header).
2. **Each block reproduces under a single uniform indent offset.** Dedenting
   line by line before comparing hides per-line drift inside a block that was
   legitimately dedented as a whole.
3. **Each block matches a contiguous run in one file**, not merely a set of
   lines that each exist somewhere. The terminal chapter's stricter pass caught
   two blocks that had silently welded non-adjacent regions together with no
   elision mark, and both had passed the per-line check looking perfect. Where
   you do elide, mark it.

The delegation chapter ran the first check over 1,608 lines; the session chapter
added the other two after noticing the first would not have caught a reordered
or re-indented block.

**Name your scratch files after your chapter.** The scratchpad is shared between
concurrently running agents, not isolated per session. A generic `build.py` has
already been overwritten mid-task by another agent's generator, and the failure
presented as a silent no-op build rather than as a collision.

**Write your output incrementally, and name the moment you first write.** The
rule is not "save often" — it is that work never sits only in a context window.
The moment you have the smallest section that stands on its own, put it on disk;
then append every few units of progress.

Say it that way, with a trigger and a cadence, because the terse version does not
work. On 2026-08-22 a power cut killed four agents mid-audit and every one had
been told to write its findings at the end: not a byte survived, and only saved
transcripts recovered the reading. The instruction was tightened to "as soon as
you have the method section and your first fifteen rows, write them, then append
every ten rows" — and four hours later, cancelling five agents mid-flight, the
two carrying that wording had substantial files on disk. Three others had been
told only to "append as you go" and had written nothing; all three were still
verifying citations, which is what that phrasing permits. **"As you go" has no
answer to "go from when?"** An agent waiting to feel ready is obeying it.

A crash, a cancellation, or a context limit must cost the last increment and
nothing more.

**Cover the file, not the highlights.** The bar is that somebody could rebuild
the behaviour from the page. Every public item, every branch that changes an
outcome, every constant that encodes a decision, every error path. Where a
function is uninteresting, one line saying so is coverage; silence is not.

**Say what you did not read.** A sub-page claiming to cover a file, written by
someone who read half of it, is worse than one that says which half.

## Voice

**Write for a reader who wants to know how Emma works.** Not for a contributor
being told how to behave. Rules about writing docs live in this file; rules
about the project live in `CLAUDE.md`, `CONTRIBUTING.md` and `records.html`. A
page that tells the reader what they must do is off-topic everywhere else.

**No self-approving narration.** Do not call the system's own choices
_deliberate_, _by design_, _on purpose_, _load-bearing_, _honest_, _rigorous_,
_principled_, _the right call_, or _precisely/exactly the_. Do not tell the
reader that something "is not decoration" or "is not an accident". Strike the
approving word and check that a fact survives — if nothing does, the sentence
was praise. Where the reasoning matters, give it its own sentence and let the
reader judge it.

State what the code does and why the alternative was worse. That is the whole
job. A page can be admiring of nothing and still be worth reading; a page that
compliments its own subject twelve times reads as a system praising itself.

**Almost no em-dashes.** Roughly one per thousand words, not one per sentence.
The em-dash is the punctuation of the same reflex that produces the glazing: it
appends a reveal or an aside to a sentence that had already finished its work,
and at volume it makes every paragraph move at the same pace. Most of them
convert cleanly:

| Instead of                         | Write                          |
| ---------------------------------- | ------------------------------ |
| a clause added after a dash        | a full stop and a new sentence |
| a parenthetical between two dashes | commas, or brackets            |
| a dash before a list               | a colon                        |
| a dash joining cause to effect     | "because", "so", "which"       |

Where a dash genuinely carries a sharp turn the sentence needs, keep it. The
test is whether the sentence still reads if you replace the dash with a full
stop. If it does, use the full stop.

The same goes for other uniform rhythms: sentences that are all the same length,
paragraphs that are all three sentences, and lists whose items all open with a
bolded phrase. Vary them, or the page reads as generated no matter how accurate
it is.

**Write the argument, not the inventory.** A list of every public function is
reference material a reader can get from the source. What they cannot get from
the source is _why_ the thing is shaped this way, what was tried and rejected,
and which part will bite them. Where a design was reversed, say so and say by
what — a reversal that is not legible as a reversal invites the reversed idea
back, which this project treats as its most expensive available mistake.

**Diagrams are generated, never drawn by hand.** This reverses what this file
said until 2026-09-04, and following the old instruction now produces an SVG the
suite rejects.

`crates/docsgen` reads the source with `syn`, extracts the enum variants, struct
fields, constants and guard clauses a diagram claims to show, and renders them.
`cargo run -p emma-docsgen` rewrites every diagram in place, between the
`<!-- diagram:NAME -->` and `<!-- /diagram -->` markers on each page. Nothing
outside those markers is touched, so a page's prose and its diagram are edited
by different hands and cannot collide.

The generated SVGs are committed, and `cargo test --workspace` regenerates them
and fails if the committed files differ from what the source produces now. Same
arrangement as `cargo fmt --check`. A stale diagram is therefore a broken build
rather than something a reader has to notice.

**Do not hand-edit inside the markers**, and do not add a diagram to a page
without adding it to `render_all` in `crates/docsgen/src/lib.rs`. The theme,
the arrow markers and the layout shapes live in `theme.rs`, `svg.rs` and
`shapes.rs`; a new diagram picks from `ladder`, `set` and `fan` rather than
positioning boxes itself.

**The reason the generator exists is worth keeping.** A diagram is trusted in
proportion to how confident it looks, so one drawn from recollection launders a
guess into a reference. Reading the manifest and the function before drawing was
the old instruction, and it depended on every author choosing to obey it. Now
the drawing reads the function itself, and the test notices when the function
changes and the drawing does not.

A diagram earns its place when it shows a mechanism prose would make the reader
assemble themselves. If a sentence says it faster, write the sentence.

## Before you report the page done

Run these. They are the whole of what a later cleanup pass would have caught, so
running them here means no cleanup pass is needed. **A page that needs a
copy-edit afterwards is a page whose author skipped this list.**

```bash
# em-dashes in prose the reader actually sees: a handful per page.
# The naive `grep -o '—' | wc -l` is the wrong instrument. It counts diagram
# labels, quoted program output, citation separators and the invisible nav
# comment, so a finished page reports as half-done. Strip those first.
python -c "
import re, html
s = open('docs/PAGE.html', encoding='utf-8').read()
for pat in [r'<svg.*?</svg>', r'<pre.*?</pre>', r'<!--.*?-->', r'<span class=\"src\">.*?</span>']:
    s = re.sub(pat, '', s, flags=re.S)
s = html.unescape(s)   # &mdash; is an em-dash too, and no character grep sees it
print(s.count('—') + s.count('–'))
"

# self-approving narration: expect zero hits in YOUR prose.
# Strip <pre> first. Quoted source legitimately contains these words, because
# this codebase writes "load-bearing" and "deliberately" in its own comments,
# and a raw grep reports a finished page as dirty. Never edit a word out of a
# code quote to satisfy this check: the quote must match the file.
python -c "
import re
s = open('docs/PAGE.html', encoding='utf-8').read()
for pat in [r'<pre.*?</pre>', r'<!--.*?-->']:
    s = re.sub(pat, '', s, flags=re.S)
s = re.sub(r'<em>&ldquo;.*?&rdquo;</em>', '', s, flags=re.S)   # quoted source comments
hits = re.findall(r'deliberately|by design|on purpose|load-bearing|is not decoration|not an accident|the right call|precisely the|principled|rigorous', s, re.I)
print(hits if hits else 'clean')
"

# every page links the shared stylesheet and ships no private one
grep -c 'href="assets/docs.css"' docs/PAGE.html   # 1
grep -c '<style\|<script\|href="http\|src="http' docs/PAGE.html   # 0

# the sidebar differs from the canonical one by exactly the `here` class.
# Copy NAV.html byte for byte, column zero included: this diff is sensitive to
# indentation, and a sidebar indented to match its surroundings fails it.
diff <(sed -n '/<nav class="side">/,/<\/nav>/p' docs/PAGE.html) \
     <(sed -n '/<nav class="side">/,/<\/nav>/p' docs/assets/NAV.html)

# amber exists somewhere: a page with no unverified claims has not been honest
grep -c 'chip unv' docs/PAGE.html

# every quoted code line still matches the file it came from.
# A page that misquotes the source is worse than one that omits it, because the
# reader has no reason to doubt a block that looks copied. Check each <pre>
# against its cited file; the lines that legitimately differ are formulas,
# shell one-liners and captured console output, and you should be able to name
# every one of them. Written after a chapter checked 1,110 quoted lines this
# way and could account for all 11 mismatches.
```

Then read the page aloud, or as close as you can get. Uniform rhythm survives
every grep: sentences all one length, paragraphs all three sentences, list items
all opening with a bolded phrase. That is what makes prose read as generated,
and no command will find it.

**Check that each command produced non-empty output before believing it.** A
`grep` that silently matched nothing and a `grep` that failed look identical
from the exit code, and this project has already shipped a comparison of two
empty files reported as a match.

## Keeping it true

- When you change behaviour, **update the page in the same change**. A commit
  that alters what Emma does and leaves the docs describing the old behaviour is
  incomplete, not merely untidy.
- When a chip's status changes — something unverified gets certified, something
  certified gets invalidated — **move the chip**. A stale `certified` is worse
  than an honest `unverified`.
- **`docs/` says how Emma works and why, and that is now nearly all of it.**
  `notes/` left the repository on 2026-08-30, taking the status board, the
  lessons and the backlog with it; the backlog is `docs/roadmap.html` now and
  the rest is in commit messages. `CHANGELOG.md` still addresses an upgrader and
  nobody else. The overlap to avoid is unchanged: this site describes the
  system, not the project's week.
- The site is deliberately dark and single-theme; the accent is the pink the
  shipped TUI actually draws with. Do not restyle it.
