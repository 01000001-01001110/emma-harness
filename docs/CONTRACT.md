# Writing a page in `docs/`

**`docs/` is the source of truth.** Not a rendering of the truth that lives
elsewhere — the place a stranger goes first, and the place that must be right.
If a behaviour exists in the code or is decided in `notes/` and this site does
not say it, or says it wrongly, **that is a defect of the same kind as a failing
test**, and it is fixed in the same change rather than filed for later.

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

| Chip                                       | Means                                                                                                             |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| `<span class="chip cert">certified</span>` | Proven against the real thing — a live API call, a real terminal, the actual file on disk. Say what was observed. |
| `<span class="chip test">tested</span>`    | Covered by a test that has been shown to fail when the guarantee is removed.                                      |
| `<span class="chip unv">unverified</span>` | Believed, not checked. **Say what would settle it.**                                                              |

A page with no amber chips anywhere is not a page that got everything right; it
is a page that has not been honest yet. The chips are how this site stays
trustworthy as it ages — an amber chip is a standing invitation to go and settle
something, and it is supposed to be uncomfortable.

## Voice

**Write for a reader who wants to know how Emma works.** Not for a contributor
being told how to behave. Rules about writing docs live in this file; rules
about the project live in `CLAUDE.md` and `records.html`. A page that tells the
reader what they must do is off-topic on every page except those two.

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

**Diagrams are hand-authored inline SVG**, drawn from the source you just read
rather than from memory or from `notes/`. Use `currentColor` and the `.box`,
`.hl`, `.edge`, `.edge-hl`, `.edge-dash` classes already in the stylesheet.
Wrap in `<figure><div class="figwrap">…</div><figcaption>` and give the `<svg>`
`role="img"` and an `aria-label` carrying the same claim as the caption. A
diagram earns its place when it shows a mechanism prose would make the reader
assemble themselves; if a sentence says it faster, write the sentence.

**A diagram drawn from recollection launders a guess into a reference.** Read
the manifests, read the function, then draw.

**Arrowheads must be defined per page.** An SVG `<marker>` does not inherit
`currentColor` from the path that references it, so the colour has to be
literal and the block has to live in each file. Use exactly this, with `PAGE`
replaced by a short page-specific prefix so two diagrams on one page cannot
collide on an id:

```html
<defs>
  <marker
    id="PAGE-arrow"
    viewBox="0 0 10 10"
    refX="9"
    refY="5"
    markerWidth="6"
    markerHeight="6"
    orient="auto-start-reverse"
  >
    <path d="M 0 0 L 10 5 L 0 10 z" fill="#6f6479" />
  </marker>
  <marker
    id="PAGE-arrow-hl"
    viewBox="0 0 10 10"
    refX="9"
    refY="5"
    markerWidth="6"
    markerHeight="6"
    orient="auto-start-reverse"
  >
    <path d="M 0 0 L 10 5 L 0 10 z" fill="#ff6ec7" />
  </marker>
</defs>
```

Those two hexes are `--dimmer` and `--accent`. They are the one place a literal
colour is allowed, and only because the format leaves no alternative.

## Keeping it true

- When you change behaviour, **update the page in the same change**. A commit
  that alters what Emma does and leaves the docs describing the old behaviour is
  incomplete, not merely untidy.
- When a chip's status changes — something unverified gets certified, something
  certified gets invalidated — **move the chip**. A stale `certified` is worse
  than an honest `unverified`.
- `notes/` keeps its jobs: `STATUS.md` says where work stands, `lessons/` says
  what was learned, `improvements.md` is the backlog, `CHANGELOG.md` addresses
  an upgrader. **`docs/` says how Emma works and why.** The overlap to avoid is
  narration of progress — this site describes the system, not the project's
  week.
- The site is deliberately dark and single-theme; the accent is the pink the
  shipped TUI actually draws with. Do not restyle it.
