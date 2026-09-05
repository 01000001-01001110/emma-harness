# Who decides

Emma is Apache-2.0. You may use it, modify it, distribute it and build a
business on it, and you need nobody's permission to do any of that.

**What the licence does not decide is what this repository becomes.** That is
governance, and it is deliberately separate.

## The short version

**Alan Newingham has the final say on what lands here.** Not on what you may
do with the code — the licence settled that and cannot be walked back — but on
what this project ships under its own name.

## What that means in practice

- **Direction is not a vote.** Emma has a shape: an agent that holds a goal, a
  consent interface that never pretends to be a sandbox, a record that says
  what it could not verify. A change that works and is well tested can still be
  declined for being the wrong shape, and that is not a defect in the review.
- **Every change is reviewed by the person whose name is on it.** Including
  changes written by an agent, which is most of them here.
- **A fork is a legitimate answer.** If a decision here is wrong for you,
  Apache-2.0 gives you the right to take the code and go, and nothing in this
  document limits that. It is the release valve that makes a single maintainer
  safe rather than a bottleneck.

## What is expected of a change

The rules are in `CLAUDE.md` and they apply to human contributors exactly as
they apply to agents. The two that get changes turned away most often:

- **A test that cannot fail is worse than no test.** Show the mutation: break
  the guarantee, watch the test go red, put it back. "The suite is green" is
  not evidence, and this project has been wrong about that three times.
- **Say what you could not verify**, in its own section, with what would settle
  it. A change that overstates its own certainty is harder to trust than one
  that admits a gap.

Beyond that: comments carry the argument rather than restating the code, and a
behaviour change that leaves `docs/` describing the old behaviour is incomplete
rather than untidy.

## Contributions and the patent grant

Apache-2.0 §5 means a contribution is offered under Apache-2.0 unless you say
otherwise in writing, and it carries the same patent grant §3 gives everyone.
There is no separate CLA, deliberately: a contributor licence agreement is a
tax on casual contribution, and the licence already grants what this project
needs.

**One consequence, stated plainly rather than discovered later:** without a CLA
this project cannot be relicensed away from Apache-2.0 without every
contributor's agreement. That is a constraint accepted on purpose. It is also
your protection — the terms you contribute under are the terms that stay.

## Security

If you find something exploitable, say so privately first rather than opening a
public issue. This project's own notes record why: a security finding published
before its author has been told is a disclosure decision made on somebody
else's behalf.

## Provenance, because it is unusual and you should know

Roughly twenty agents wrote most of this code, and the written record exists
because no author's memory backs it. Most of it is now in the commit messages;
the lessons and audits that used to sit under `notes/` were removed from the
tree on 2026-08-30 and are reachable with `git log -p -- notes/`. When a comment here explains why an obvious approach was
rejected, it is generally because that approach was tried and reversed, and the
reversal is recorded rather than remembered.
