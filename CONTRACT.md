# Emma Information Authority

There is no single file that answers every question.

## Current product behavior
AUTHORITY: docs/
VALIDATED BY: source + runtime evidence

docs/ describes the behavior Emma promises today.

If source/runtime contradicts docs, the discrepancy is a DEFECT.
Do not silently assume either side is intended.
Investigate and reconcile them.

## Implementation reality
AUTHORITY: crates/ + tools/
CERTIFICATION: actual execution

Source defines what is implemented.
Runtime observation establishes what actually occurs in an environment.

Tests are evidence, not authority.

## Architecture
AUTHORITY: source
PUBLIC REPRESENTATION: docs/architecture.*

Architecture diagrams are derived artifacts.
They never override source.

## Current project state
AUTHORITY: notes/STATUS.md

STATUS is append-only and temporal.
An entry older than the newest relevant commit is potentially stale.

## Active work
AUTHORITY: notes/ACTIVE.md

Exactly one file identifies currently approved work.

It links to plans; it does not duplicate them.

## Plans
AUTHORITY: only when referenced by notes/ACTIVE.md

A plan that is not active is historical/proposed material.

## Design decisions
AUTHORITY: notes/design-*.md for rationale only

Design notes explain why a decision was made.
They do not prove the implementation still follows it.

## Research
AUTHORITY: notes/research-* and notes/survey-* for research findings only

Research does not establish current behavior.

## Lessons
AUTHORITY: notes/lessons/* for learned constraints and failures.

## Backlog
AUTHORITY: notes/improvements.md

An improvement is not approved work merely because it exists.

## History
CHANGELOG.md = user-visible changes
git = implementation history