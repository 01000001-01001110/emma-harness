//! What a write would change, as a diff, computed before it happens.
//!
//! # Why this exists at all
//!
//! `approval.rs` argues that a prompt the user cannot evaluate manufactures
//! consent. A `Write` prompt that said `write src/auth.rs — 4,102 bytes, 118
//! lines` was exactly that: the one fact it withheld was the change. An `Edit`
//! prompt did better — it showed both sides verbatim — and still made the reader
//! do the diffing, twenty `-` lines above twenty `+` lines of which three
//! differed. This file is the missing half, used at the gate and on the
//! transcript line that opens every write.
//!
//! # The rule this file is arranged around
//!
//! **A diff is never fabricated.** Everything below is a case where an honest
//! diff cannot be produced, and every one of them says so rather than drawing
//! something plausible:
//!
//! - the file is not UTF-8, or has a NUL in it — [`Change::Undiffable`];
//! - it is larger than [`MAX_READ`], or unreadable — the same;
//! - it has more lines than [`MAX_LINES`] — the same;
//! - the changed region is too large to diff exactly — [`Patch::coarse`], which
//!   is a *coarser true statement* (these lines went, these arrived) and is
//!   labelled as one, not a guess at which lines correspond;
//! - the two sides are the same bytes — [`Change::Identical`], because "no
//!   change" is a thing the user should be told before they approve it.
//!
//! # Two sides, and where each one comes from
//!
//! `Write` carries the whole new text and the old text is on disk, so its diff
//! is the file against the proposal and its line numbers are real. `Edit`
//! carries `old_string` and `new_string`, which *are* the two sides — the diff
//! is a function of the arguments alone and so cannot disagree with what the
//! tool will do. That was the original argument for showing them verbatim and it
//! survives intact here: nothing is read, nothing is inferred, the same two texts
//! are shown with the lines they have in common written once instead of twice.
//! What it loses is line numbers, so an unanchored patch does not print any —
//! see [`Patch::anchored`].
//!
//! **Neither is the diff of what happened.** Both are computed before the write,
//! from a proposal. A tool that then fails, or is refused, changed nothing, and
//! the transcript says so on its own line. The genuinely post-hoc diff — what
//! actually landed — needs `tools/fs` to hand back the two sides it already
//! holds; see the report accompanying this change.
//!
//! # Line endings, which on this machine are the whole hazard
//!
//! Emma is developed on Windows against a CRLF working tree. A diff that
//! compared raw lines would report every line of every file as changed the first
//! time anything normalised them, which is worse than no diff: it buries the one
//! real change in two thousand false ones. So **the trailing `\r` is stripped
//! before lines are compared**, and a change *in* the endings becomes a note —
//! one sentence — rather than a wall. The same goes for a missing final newline.
//!
//! # What is drawn, and what a reader copies
//!
//! `markdown.rs` refuses to insert anything into text a reader might copy, and
//! this file deliberately does the opposite: every row carries a `+`, `-` or two
//! spaces in its gutter. The reason it is not the same rule is that a diff is not
//! the file. It is a report *about* a change, in the format every reader already
//! knows from `git diff`, and pasting it into a bug report is the use — pasting
//! it into a compiler is not. The gutter is also the only distinction that
//! survives `NO_COLOR`, a sixteen-colour terminal and the ASCII glyph set, which
//! is the rule `render.rs` already states: never colour alone.
//!
//! Colour comes from [`Role`] and nowhere else — `Ok` for an addition, `Err` for
//! a removal, dim for context — so there is no second colour vocabulary here and
//! a palette at [`Level::None`](super::palette::Level::None) emits no escape
//! byte.
//!
//! # Why the algorithm is here rather than in a crate
//!
//! `similar` is the obvious answer and was not taken, for the reason
//! `markdown.rs` gives for its own: what is actually needed is a small part of
//! it. This file diffs *lines*, never bytes, words or graphemes; it needs no
//! inline highlighting, no three-way merge and no patch parser. What is left is a
//! prefix/suffix trim and an LCS, which is the sixty lines below — and it has one
//! requirement a general crate does not offer, which is that it be **bounded**:
//! this runs on the interactive path for every `Write` a model makes, so a
//! two-hundred-megabyte file has to cost a sentence rather than a swap storm.
//! Every limit in this file is that requirement showing through.

use ratatui::text::{Line, Span};
use serde_json::Value;

use super::palette::Role;
use super::render::Skin;

// region: Limits
// ---------------------------------------------------------------------------
// Limits
//
// All four in one place because they are one decision: this code runs while a
// person is waiting, on arguments a model chose, so every input is adversarial
// by accident if not by intent.
// ---------------------------------------------------------------------------

/// The most of a file that is read to diff against. Past this the prompt says
/// how big the file is instead, which is a fact, rather than a diff of the first
/// two megabytes, which would be a lie by omission.
pub const MAX_READ: u64 = 2 * 1024 * 1024;

/// The most lines either side may have before an exact diff is refused.
pub const MAX_LINES: usize = 50_000;

/// The most LCS cells to fill for the changed region, after the common prefix
/// and suffix are trimmed off. 250k is a 500×500 middle — an edit that rewrites
/// five hundred consecutive lines is a rewrite, and gets shown as one.
const MAX_CELLS: usize = 250_000;

/// Unchanged lines kept either side of a change, as `git diff` does.
const CONTEXT: usize = 3;

/// How many diff rows reach a prompt.
///
/// **An approval prompt has a much tighter budget than a transcript entry**,
/// because it has one job — be answerable — and forty rows is already more than
/// the inline viewport can hold. It is deliberately the number the old
/// `approval::diff` used, so the cut this replaces is no larger than the one it
/// inherits, and the row that announces the cut names how many rows went.
pub const BUDGET: usize = 40;

// endregion: Limits

// region: What a change is
// ---------------------------------------------------------------------------
// What a change is
//
// Three variants, and the two that are not a patch are the point of the type:
// "there is no honest diff here" and "there is no change here" are both answers
// a prompt must be able to give.
// ---------------------------------------------------------------------------

/// What can honestly be said about a proposed write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A computed line diff.
    Lines(Patch),
    /// The two sides are the same bytes. Worth saying out loud: approving a
    /// no-op is a different decision from approving a rewrite.
    Identical,
    /// No diff can be drawn, and the sentence saying why. Never empty.
    Undiffable(String),
}

/// A line diff, plus everything about the change that is not a line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patch {
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
    /// Whether line numbers mean anything. True for a whole-file diff, false for
    /// `Edit`'s two argument strings — which have no position until the tool
    /// finds the anchor, so printing a `1` would be inventing one.
    pub anchored: bool,
    /// The changed region was too large to correspond line by line, so it is
    /// shown as everything that went followed by everything that arrived. True,
    /// coarser, and labelled — see [`Patch::summary`].
    pub coarse: bool,
    /// Facts that are not lines: the endings changed, the final newline went,
    /// the file is new. Never truncated, whatever the row budget does.
    pub notes: Vec<String>,
}

/// A run of changed lines with its context, and where it sits in the old and
/// new files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// One-based, as `git diff` counts.
    pub old_start: usize,
    pub new_start: usize,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub mark: Mark,
    pub text: String,
}

impl Patch {
    /// The one line that is never cut.
    ///
    /// Whatever the budget does to the body, this survives, so a diff that shows
    /// four of six hundred changed lines still states the size of what is being
    /// approved. That is the whole defence against the failure `codex-rs` shipped
    /// twice — an empty diff body, and a delete-only patch that read as
    /// add-only — where the *magnitude* of a destructive change went missing at
    /// exactly the prompt that existed to show it.
    pub fn summary(&self) -> String {
        let plural = |n: usize| if n == 1 { "line" } else { "lines" };
        let mut out = match (self.added, self.removed) {
            (0, 0) => "no lines changed".to_string(),
            (a, 0) => format!("+{a} {}", plural(a)),
            (0, r) => format!("-{r} {}", plural(r)),
            (a, r) => format!("+{a} -{r} lines"),
        };
        if self.coarse {
            out.push_str(
                ", shown as one replacement — the changed region is too large to \
                 line up exactly",
            );
        }
        out
    }
}

// endregion: What a change is

// region: Computing one
// ---------------------------------------------------------------------------
// Computing one
//
// `between` and `file` are the same function with one flag, because the only
// difference between an `Edit`'s two arguments and a file's two versions is
// whether a line number would mean anything.
// ---------------------------------------------------------------------------

impl Change {
    /// Two texts with no position: `Edit`'s `old_string` and `new_string`.
    pub fn between(old: &str, new: &str) -> Self {
        compute(old, new, false)
    }

    /// A whole file, before and after, where line numbers are real.
    pub fn file(before: &str, after: &str) -> Self {
        compute(before, after, true)
    }

    /// A file that does not exist yet: every line is an addition, and the note
    /// says so rather than leaving the reader to infer it from an absence of
    /// `-` rows.
    pub fn creation(content: &str) -> Self {
        let text = split(content);
        if text.lines.len() > MAX_LINES {
            return Self::Undiffable(format!(
                "the new file has {} lines, more than the {MAX_LINES} this prompt will render; \
                 nothing is shown rather than a fraction of it",
                text.lines.len()
            ));
        }
        let added = text.lines.len();
        let rows: Vec<Row> = text
            .lines
            .into_iter()
            .map(|text| Row {
                mark: Mark::Add,
                text,
            })
            .collect();
        Self::Lines(Patch {
            hunks: if rows.is_empty() {
                Vec::new()
            } else {
                vec![Hunk {
                    old_start: 0,
                    new_start: 1,
                    rows,
                }]
            },
            added,
            removed: 0,
            anchored: true,
            coarse: false,
            notes: vec!["a new file".to_string()],
        })
    }
}

fn compute(old: &str, new: &str, anchored: bool) -> Change {
    if old == new {
        return Change::Identical;
    }
    for (side, text) in [("the old text", old), ("the new text", new)] {
        if text.contains('\0') {
            return Change::Undiffable(format!(
                "{side} contains a NUL byte, so it is binary and there is no line diff to show"
            ));
        }
    }
    let (a, b) = (split(old), split(new));
    if a.lines.len() > MAX_LINES || b.lines.len() > MAX_LINES {
        return Change::Undiffable(format!(
            "the two sides have {} and {} lines, past the {MAX_LINES} this prompt will diff; \
             nothing is shown rather than a fraction of it",
            a.lines.len(),
            b.lines.len()
        ));
    }

    // Facts that are not lines, gathered before the comparison so that a change
    // consisting *only* of these produces a patch with no hunks and an
    // explanation, rather than an empty diff of two visibly different files.
    let mut notes = Vec::new();
    if a.crlf != b.crlf {
        notes.push(format!(
            "the line endings change from {} to {}",
            endings(a.crlf),
            endings(b.crlf)
        ));
    }
    if a.final_newline != b.final_newline {
        notes.push(if b.final_newline {
            "a final newline is added".to_string()
        } else {
            "the final newline is removed".to_string()
        });
    }

    let (rows, coarse) = ops(&a.lines, &b.lines);
    let added = rows.iter().filter(|r| r.mark == Mark::Add).count();
    let removed = rows.iter().filter(|r| r.mark == Mark::Del).count();
    let hunks = hunks(rows);
    // The two texts differ — `compute` returned early if they did not — and yet
    // nothing above found a line or an ending to point at, which leaves mixed
    // endings inside one file as the only remaining explanation. Saying nothing
    // here would render as "no lines changed" against two visibly different
    // files, which is the shape of a fabricated diff even though every part of
    // it is true.
    if hunks.is_empty() && notes.is_empty() {
        notes.push(
            "the two differ only in line endings, which this diff compares without".to_string(),
        );
    }
    Change::Lines(Patch {
        hunks,
        added,
        removed,
        anchored,
        coarse,
        notes,
    })
}

fn endings(crlf: bool) -> &'static str {
    if crlf {
        "CRLF"
    } else {
        "LF"
    }
}

/// A text, split the way a diff has to see it.
struct Text {
    /// Lines with no terminator and no trailing `\r`. See the module doc: the
    /// `\r` is stripped *before* comparison or a CRLF working tree reports every
    /// line as changed.
    lines: Vec<String>,
    /// Any line ended `\r\n`.
    crlf: bool,
    final_newline: bool,
}

fn split(s: &str) -> Text {
    let final_newline = s.ends_with('\n');
    let body = if final_newline { &s[..s.len() - 1] } else { s };
    let mut crlf = false;
    let lines: Vec<String> = if s.is_empty() {
        Vec::new()
    } else {
        body.split('\n')
            .map(|l| match l.strip_suffix('\r') {
                Some(stripped) => {
                    crlf = true;
                    stripped.to_string()
                }
                None => l.to_string(),
            })
            .collect()
    };
    Text {
        lines,
        crlf,
        final_newline,
    }
}

// endregion: Computing one

// region: The diff itself
// ---------------------------------------------------------------------------
// The diff itself
//
// Trim what matches at both ends, then an LCS over what is left — and refuse to
// fill a table larger than `MAX_CELLS`, which is the bound that makes this safe
// to run on the interactive path against a file a model named.
// ---------------------------------------------------------------------------

fn ops(a: &[String], b: &[String]) -> (Vec<Row>, bool) {
    let head = a
        .iter()
        .zip(b.iter())
        .take_while(|(x, y)| x == y)
        .count()
        .min(a.len())
        .min(b.len());
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);

    let mut rows: Vec<Row> = a[..head]
        .iter()
        .map(|t| Row {
            mark: Mark::Context,
            text: t.clone(),
        })
        .collect();

    // The bound. Past it there is no correspondence to draw, so the honest thing
    // is the coarser true statement — these went, these arrived — and the
    // summary says that is what happened.
    let coarse = mid_a.len().saturating_mul(mid_b.len()) > MAX_CELLS;
    if coarse {
        rows.extend(mid_a.iter().map(|t| Row {
            mark: Mark::Del,
            text: t.clone(),
        }));
        rows.extend(mid_b.iter().map(|t| Row {
            mark: Mark::Add,
            text: t.clone(),
        }));
    } else {
        rows.extend(middle(mid_a, mid_b));
    }

    rows.extend(a[a.len() - tail..].iter().map(|t| Row {
        mark: Mark::Context,
        text: t.clone(),
    }));
    (rows, coarse)
}

/// The classic LCS table, backtracked into rows.
///
/// `u32` cells rather than `usize`: `MAX_CELLS` bounds the table at 250k
/// entries, so a megabyte on a 32-bit count against two on a 64-bit one is worth
/// having on a path a person is waiting on.
fn middle(a: &[String], b: &[String]) -> Vec<Row> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        let mut rows: Vec<Row> = a
            .iter()
            .map(|t| Row {
                mark: Mark::Del,
                text: t.clone(),
            })
            .collect();
        rows.extend(b.iter().map(|t| Row {
            mark: Mark::Add,
            text: t.clone(),
        }));
        return rows;
    }
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut rows = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            rows.push(Row {
                mark: Mark::Context,
                text: a[i].clone(),
            });
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            rows.push(Row {
                mark: Mark::Del,
                text: a[i].clone(),
            });
            i += 1;
        } else {
            rows.push(Row {
                mark: Mark::Add,
                text: b[j].clone(),
            });
            j += 1;
        }
    }
    rows.extend(a[i..].iter().map(|t| Row {
        mark: Mark::Del,
        text: t.clone(),
    }));
    rows.extend(b[j..].iter().map(|t| Row {
        mark: Mark::Add,
        text: t.clone(),
    }));
    rows
}

/// Group changed rows into hunks, keeping [`CONTEXT`] unchanged lines either
/// side and merging runs that would otherwise share their context.
fn hunks(rows: Vec<Row>) -> Vec<Hunk> {
    let changed: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.mark != Mark::Context)
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }
    // Line numbers as the rows are walked: a context row advances both, an
    // addition only the new side, a removal only the old.
    let mut old_at = Vec::with_capacity(rows.len());
    let mut new_at = Vec::with_capacity(rows.len());
    let (mut o, mut nn) = (1usize, 1usize);
    for row in &rows {
        old_at.push(o);
        new_at.push(nn);
        match row.mark {
            Mark::Context => {
                o += 1;
                nn += 1;
            }
            Mark::Del => o += 1,
            Mark::Add => nn += 1,
        }
    }

    let mut out: Vec<Hunk> = Vec::new();
    let mut start = changed[0].saturating_sub(CONTEXT);
    let mut end = (changed[0] + CONTEXT + 1).min(rows.len());
    for &i in &changed[1..] {
        let from = i.saturating_sub(CONTEXT);
        if from <= end {
            end = (i + CONTEXT + 1).min(rows.len());
        } else {
            out.push(Hunk {
                old_start: old_at[start],
                new_start: new_at[start],
                rows: rows[start..end].to_vec(),
            });
            start = from;
            end = (i + CONTEXT + 1).min(rows.len());
        }
    }
    out.push(Hunk {
        old_start: old_at[start],
        new_start: new_at[start],
        rows: rows[start..end].to_vec(),
    });
    out
}

// endregion: The diff itself

// region: Drawing it
// ---------------------------------------------------------------------------
// Drawing it
//
// One function decides the rows and both renderers consume it, so the plain text
// in the viewport panel and the coloured lines on the transcript cannot show
// different diffs — or, worse, cut at different places and disagree about how
// much was left out.
// ---------------------------------------------------------------------------

/// One drawn row and what kind it is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Out {
    /// The summary and the notes: never cut.
    Meta(String),
    /// `@@ -12,4 +12,5 @@`, on anchored patches only.
    Hunk(String),
    Add(String),
    Del(String),
    Context(String),
    /// What the budget removed, and how much. Never silent.
    Cut(String),
}

impl Change {
    /// The rows this change draws, in order, within `budget` body rows.
    ///
    /// The summary and the notes are outside the budget on purpose: a cut diff
    /// that no longer said `-1,412 lines` would be the exact defect an approval
    /// prompt exists to prevent.
    fn out(&self, budget: usize) -> Vec<Out> {
        let patch = match self {
            Self::Identical => {
                return vec![Out::Meta(
                    "no change — the new text is byte for byte what is already there".into(),
                )]
            }
            Self::Undiffable(why) => return vec![Out::Meta(format!("no diff: {why}"))],
            Self::Lines(patch) => patch,
        };
        let mut out = vec![Out::Meta(patch.summary())];
        out.extend(patch.notes.iter().cloned().map(Out::Meta));
        if patch.coarse {
            out.extend(coarse_rows(patch, budget));
            return out;
        }

        let mut room = budget;
        let mut shown = 0usize;
        let total: usize = patch.hunks.iter().map(|h| h.rows.len()).sum();
        for hunk in &patch.hunks {
            if room == 0 {
                break;
            }
            if patch.anchored {
                out.push(Out::Hunk(format!(
                    "@@ -{} +{} @@",
                    hunk.old_start, hunk.new_start
                )));
            }
            for row in hunk.rows.iter().take(room) {
                let text = sanitise(&row.text);
                out.push(match row.mark {
                    Mark::Add => Out::Add(text),
                    Mark::Del => Out::Del(text),
                    Mark::Context => Out::Context(text),
                });
            }
            let took = hunk.rows.len().min(room);
            room -= took;
            shown += took;
        }
        if shown < total {
            out.push(Out::Cut(format!(
                "{} more diff lines not shown",
                total - shown
            )));
        }
        out
    }

    /// The diff as plain text, for the viewport panel and for any stream that is
    /// not a terminal. No escape byte can come out of here — there is nothing in
    /// it but the gutter and the text.
    pub fn to_text(&self, budget: usize) -> String {
        self.out(budget)
            .iter()
            .map(text_of)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The diff as styled lines, for the transcript.
    ///
    /// Colour *and* gutter, always, which is `render.rs`'s standing rule: an
    /// addition is `+` and green, a removal is `-` and red, and a terminal with
    /// neither colour nor the Unicode glyph set loses nothing but the shading.
    pub fn to_lines(&self, skin: &Skin, budget: usize) -> Vec<Line<'static>> {
        self.out(budget)
            .iter()
            .map(|o| {
                let style = match o {
                    Out::Add(_) => skin.palette.style(Role::Ok),
                    Out::Del(_) => skin.palette.style(Role::Err),
                    // Everything that is not a changed line is dim: the summary,
                    // the notes, the hunk headers, the cut, and the context. The
                    // eye should land on the `+` and `-` rows and nothing else.
                    Out::Meta(_) | Out::Hunk(_) | Out::Cut(_) | Out::Context(_) => {
                        skin.palette.dim()
                    }
                };
                Line::from(Span::styled(text_of(o), style))
            })
            .collect()
    }
}

/// A coarse patch, with the budget split between the two sides.
///
/// **Why this is not the ordinary head-of-the-diff cut.** A coarse patch is one
/// block of removals followed by one block of additions, so taking the first
/// forty rows takes forty removals and shows *no* additions at all — a
/// thousand-line rewrite drawn as a thousand-line deletion. That is `codex-rs`
/// #34515 with the signs the other way round, and it is worst in the case that
/// matters most: the reader is deciding whether to allow a destructive write.
/// So each side gets half the budget, whatever is left over goes to whichever
/// side can use it, and the closing row names how much of *each* went.
fn coarse_rows(patch: &Patch, budget: usize) -> Vec<Out> {
    let side = |mark: Mark| -> Vec<&Row> {
        patch
            .hunks
            .iter()
            .flat_map(|h| h.rows.iter())
            .filter(|r| r.mark == mark)
            .collect()
    };
    let (dels, adds) = (side(Mark::Del), side(Mark::Add));
    let half = budget / 2;
    // Each side asks for half; a side that wants less than half hands the rest
    // over rather than leaving the budget unspent.
    let take_del = dels
        .len()
        .min(half + half.saturating_sub(adds.len().min(half)));
    let take_add = adds.len().min(budget - take_del.min(budget));

    let mut out = Vec::new();
    if patch.anchored {
        if let Some(first) = patch.hunks.first() {
            out.push(Out::Hunk(format!(
                "@@ -{} +{} @@",
                first.old_start, first.new_start
            )));
        }
    }
    out.extend(
        dels.iter()
            .take(take_del)
            .map(|r| Out::Del(sanitise(&r.text))),
    );
    out.extend(
        adds.iter()
            .take(take_add)
            .map(|r| Out::Add(sanitise(&r.text))),
    );
    let (lost_del, lost_add) = (dels.len() - take_del, adds.len() - take_add);
    if lost_del + lost_add > 0 {
        out.push(Out::Cut(format!(
            "{lost_del} more removed and {lost_add} more added lines not shown"
        )));
    }
    out
}

/// The text of one row, gutter and all. **The single place the gutter is
/// written**, so the panel and the transcript cannot disagree about which side a
/// line is on — which is the `#34515` failure, a delete-only patch that read as
/// add-only, made structurally impossible rather than merely tested for.
fn text_of(out: &Out) -> String {
    match out {
        Out::Meta(t) => format!("  {t}"),
        Out::Hunk(t) => format!("  {t}"),
        Out::Cut(t) => format!("  … {t}"),
        Out::Add(t) => format!("  + {t}"),
        Out::Del(t) => format!("  - {t}"),
        Out::Context(t) => format!("    {t}"),
    }
}

/// What a file's own bytes may not become on screen.
///
/// The same rule `markdown.rs` applies to the model's prose, applied to the
/// other text that comes from outside: a file can contain the escape byte, and
/// ratatui counts a stored escape as a column the terminal does not — which
/// makes the frame wrong about every row below it, and those rows are the
/// transcript this whole design keeps in scrollback.
fn sanitise(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

// endregion: Drawing it

// region: What a call would change
// ---------------------------------------------------------------------------
// What a call would change
//
// The one place that knows `Write` and `Edit` by name, so the gate and the
// transcript line ask the same question and get the same answer. Anything else
// gets `None` and keeps whatever preview it had — a tool that changes files and
// is not listed here shows no diff rather than a wrong one.
// ---------------------------------------------------------------------------

/// Whether this tool's transcript line already carries its diff.
///
/// Asked by [`Term::prompt_header`](crate::term::Term::prompt_header) so the
/// approval question does not print the same diff a second time under it. One
/// predicate, consulted by both, so "the diff is drawn" and "the diff is not
/// repeated" cannot come apart — which is how a prompt ends up eighty rows long
/// or, worse, showing a diff nothing else showed.
pub fn changes_a_file(tool: &str) -> bool {
    matches!(tool, "Write" | "Edit")
}

/// The diff a tool call would produce, or `None` when this is not a call whose
/// change can be shown.
pub fn for_call(tool: &str, args: &Value) -> Option<Change> {
    if !changes_a_file(tool) {
        return None;
    }
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    match tool {
        "Write" => {
            let (path, content) = (s("file_path")?, s("content")?);
            Some(match before(path) {
                Before::Missing => Change::creation(content),
                Before::Text(old) => Change::file(&old, content),
                Before::Unreadable(why) => Change::Undiffable(why),
            })
        }
        // The two sides are the arguments themselves, so nothing is read and
        // nothing is inferred. See the module doc.
        "Edit" => Some(Change::between(s("old_string")?, s("new_string")?)),
        _ => None,
    }
}

enum Before {
    Missing,
    Text(String),
    Unreadable(String),
}

/// The file as it stands, for `Write` to be diffed against.
///
/// **Every failure is named rather than swallowed.** A file that cannot be read
/// must not fall through to "missing", because "missing" renders as a brand new
/// file made entirely of additions — which is precisely the mislabelled
/// destructive change this file exists to make impossible. Only `NotFound` is
/// `Missing`.
fn before(path: &str) -> Before {
    let p = std::path::Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        // The same working directory `ToolCtx::cwd` is built from, so a relative
        // path resolves where the tool will resolve it.
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p),
            Err(e) => {
                return Before::Unreadable(format!("the working directory is unreadable: {e}"))
            }
        }
    };
    let meta = match std::fs::metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Before::Missing,
        Err(e) => return Before::Unreadable(format!("{path} could not be read: {e}")),
    };
    if meta.is_dir() {
        return Before::Unreadable(format!("{path} is a directory"));
    }
    if meta.len() > MAX_READ {
        return Before::Unreadable(format!(
            "{path} is {} bytes, past the {MAX_READ} this prompt will read; \
             the change is not shown rather than shown in part",
            meta.len()
        ));
    }
    match std::fs::read(&full) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Before::Text(text),
            Err(_) => Before::Unreadable(format!(
                "{path} is not UTF-8, so there is no line diff to show"
            )),
        },
        Err(e) => Before::Unreadable(format!("{path} could not be read: {e}")),
    }
}

// endregion: What a call would change

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Written against the four guarantees rather than against the output format:
// a diff is never fabricated, a cut always names its size, a destructive change
// is never drawn as an additive one, and a redirected stream gets no escape
// byte. Each of these fails if the behaviour is removed — the mutations are in
// the report.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::{for_stream, plain, UNICODE};

    fn skin(level: Level) -> Skin {
        Skin::new(Palette::new(level), UNICODE)
    }

    fn text(change: &Change) -> String {
        change.to_text(BUDGET)
    }

    // -----------------------------------------------------------------------
    // The diff itself
    // -----------------------------------------------------------------------

    #[test]
    fn an_edit_shows_what_the_two_sides_have_in_common_once() {
        // The complaint the whole feature answers: twenty `-` lines above
        // twenty `+` lines of which one differs. Written as "the shared lines
        // appear once" rather than as a row count, because that is the property.
        let old = "fn a() {\n    let x = 1;\n    body();\n}\n";
        let new = "fn a() {\n    let x = 2;\n    body();\n}\n";
        let out = text(&Change::between(old, new));
        assert!(out.contains("  - "), "{out}");
        assert!(out.contains("let x = 1;"), "{out}");
        assert!(out.contains("let x = 2;"), "{out}");
        // `fn a() {` is unchanged and is shown once, with no gutter mark.
        assert_eq!(
            out.matches("fn a() {").count(),
            1,
            "an unchanged line was printed on both sides: {out}"
        );
        assert!(out.contains("    fn a() {"), "{out}");
        assert!(out.contains("+1 -1 lines"), "{out}");
    }

    #[test]
    fn an_unanchored_diff_prints_no_line_numbers_it_does_not_have() {
        // `Edit`'s arguments have no position until the tool finds the anchor,
        // so a `@@ -1` here would be an invented fact.
        let out = text(&Change::between("a\n", "b\n"));
        assert!(!out.contains("@@"), "{out}");
        // …and a whole-file diff does carry them, which is what makes the
        // absence above a distinction rather than a missing feature.
        let file = text(&Change::file(
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n",
            "1\n2\n3\n4\nX\n6\n7\n8\n9\n",
        ));
        assert!(file.contains("@@ -2 +2 @@"), "{file}");
    }

    #[test]
    fn context_is_kept_around_a_change_and_the_rest_of_a_long_file_is_not() {
        let before: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        let after = before.replace("line 100\n", "CHANGED\n");
        let out = text(&Change::file(&before, &after));
        assert!(out.contains("CHANGED"), "{out}");
        assert!(out.contains("line 97"), "{out}");
        // Three lines of context, so line 96 is not in the hunk — and the 190
        // untouched lines are not printed at all.
        assert!(!out.contains("line 96"), "{out}");
        assert!(!out.contains("line 5"), "{out}");
    }

    #[test]
    fn two_distant_changes_are_two_hunks_and_two_adjacent_ones_are_one() {
        let before: String = (1..=60).map(|i| format!("line {i}\n")).collect();
        let far = before
            .replace("line 5\n", "A\n")
            .replace("line 50\n", "B\n");
        let out = text(&Change::file(&before, &far));
        assert_eq!(out.matches("@@ -").count(), 2, "{out}");
        let near = before.replace("line 5\n", "A\n").replace("line 6\n", "B\n");
        let out = text(&Change::file(&before, &near));
        assert_eq!(out.matches("@@ -").count(), 1, "{out}");
    }

    // -----------------------------------------------------------------------
    // A diff is never fabricated
    //
    // Every case where an honest diff cannot be produced, asserted as "it says
    // so" rather than "it produces nothing" — silence would read as no change.
    // -----------------------------------------------------------------------

    #[test]
    fn binary_content_is_refused_with_a_reason_rather_than_diffed() {
        let out = text(&Change::between("ok\n", "ok\n\0\x01\x02"));
        assert!(out.contains("no diff:"), "{out}");
        assert!(out.contains("binary"), "{out}");
        assert!(
            !out.contains("  + "),
            "a binary file was drawn as lines: {out}"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_drawn_as_a_new_one() {
        // The mislabelling this file exists to prevent, in its most dangerous
        // shape: an unreadable *existing* file rendered as a creation would
        // show a screen of `+` lines for a call that destroys the old contents.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked");
        std::fs::write(&path, "old\n").unwrap();
        let big = dir.path().join("big");
        std::fs::write(&big, "x".repeat(MAX_READ as usize + 1)).unwrap();

        let change = for_call(
            "Write",
            &serde_json::json!({ "file_path": big.to_str().unwrap(), "content": "new\n" }),
        )
        .unwrap();
        let out = text(&change);
        assert!(out.contains("no diff:"), "{out}");
        assert!(out.contains("bytes"), "{out}");
        assert!(
            !out.contains("a new file"),
            "an existing file was called new: {out}"
        );

        // A directory is the same class of mistake.
        let change = for_call(
            "Write",
            &serde_json::json!({ "file_path": dir.path().to_str().unwrap(), "content": "x" }),
        )
        .unwrap();
        assert!(text(&change).contains("is a directory"), "{change:?}");
    }

    #[test]
    fn a_no_op_write_says_so_rather_than_showing_an_empty_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("same.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let change = for_call(
            "Write",
            &serde_json::json!({ "file_path": path.to_str().unwrap(), "content": "one\ntwo\n" }),
        )
        .unwrap();
        assert_eq!(change, Change::Identical);
        assert!(text(&change).contains("no change"), "{change:?}");
    }

    #[test]
    fn a_new_file_is_all_additions_and_is_labelled_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        let change = for_call(
            "Write",
            &serde_json::json!({ "file_path": path.to_str().unwrap(), "content": "a\nb\n" }),
        )
        .unwrap();
        let out = text(&change);
        assert!(out.contains("a new file"), "{out}");
        assert!(out.contains("+2 lines"), "{out}");
        assert!(!out.contains("  - "), "{out}");
    }

    /// **Only `NotFound` is a new file, and this is the test that keeps it that
    /// way.** Every other way of failing to read the old side has to end up in
    /// [`Change::Undiffable`], because "missing" renders as a screen of `+` rows
    /// — a destructive overwrite drawn as a creation, at the one prompt where
    /// that matters. The three arms below are the three that can actually be
    /// reached: bytes that are not UTF-8, a path the platform refuses outright,
    /// and (with the two above) a file too large or a directory.
    #[test]
    fn every_way_of_failing_to_read_the_old_side_is_a_refusal_and_not_a_creation() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("blob.bin");
        std::fs::write(&binary, [0xff, 0xfe, 0x00, 0x41]).unwrap();
        let out = text(
            &for_call(
                "Write",
                &serde_json::json!({ "file_path": binary.to_str().unwrap(), "content": "text\n" }),
            )
            .unwrap(),
        );
        assert!(out.contains("not UTF-8"), "{out}");
        assert!(!out.contains("a new file"), "{out}");
        assert!(!out.contains("  + "), "{out}");

        // A path the platform will not even stat. `metadata` returns something
        // other than `NotFound`, which must not be read as "there is nothing
        // there yet".
        let illegal = format!("{}\0nul", dir.path().join("x").to_str().unwrap());
        let out = text(
            &for_call(
                "Write",
                &serde_json::json!({ "file_path": illegal, "content": "t" }),
            )
            .unwrap(),
        );
        assert!(out.contains("no diff:"), "{out}");
        assert!(!out.contains("a new file"), "{out}");
    }

    /// The destructive case, which is the one the prompt exists for: replacing a
    /// file with a shorter one must read as a removal.
    #[test]
    fn deleting_most_of_a_file_reads_as_a_removal_and_not_as_an_addition() {
        let before: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        let change = Change::file(&before, "line 1\n");
        let Change::Lines(patch) = &change else {
            panic!("{change:?}")
        };
        assert_eq!(patch.removed, 39);
        assert_eq!(patch.added, 0);
        let out = text(&change);
        assert!(out.contains("-39 lines"), "{out}");
        assert!(
            !out.contains("  + "),
            "a delete-only patch drew a `+`: {out}"
        );
    }

    // -----------------------------------------------------------------------
    // Line endings, which on this machine are the hazard
    // -----------------------------------------------------------------------

    #[test]
    fn changing_only_the_line_endings_is_one_sentence_and_not_a_wall() {
        // The failure being prevented: a CRLF working tree diffed against LF
        // content, where every line of a two-thousand-line file reads as
        // changed and the real change is invisible.
        let crlf: String = (1..=2000).map(|i| format!("line {i}\r\n")).collect();
        let lf = crlf.replace("\r\n", "\n");
        let change = Change::file(&crlf, &lf);
        let Change::Lines(patch) = &change else {
            panic!("{change:?}")
        };
        assert_eq!(patch.added, 0, "line endings were diffed as content");
        assert_eq!(patch.removed, 0, "line endings were diffed as content");
        let out = text(&change);
        assert!(out.contains("CRLF to LF"), "{out}");
        assert!(out.lines().count() < 5, "{out}");
    }

    #[test]
    fn a_real_change_inside_a_crlf_file_is_the_only_thing_reported() {
        let crlf: String = (1..=50).map(|i| format!("line {i}\r\n")).collect();
        let edited = crlf.replace("line 25\r\n", "CHANGED\r\n");
        let change = Change::file(&crlf, &edited);
        let Change::Lines(patch) = &change else {
            panic!("{change:?}")
        };
        assert_eq!((patch.added, patch.removed), (1, 1));
        // …and no stray carriage return reached the drawn rows.
        assert!(!text(&change).contains('\r'), "a CR reached the screen");
    }

    #[test]
    fn a_missing_final_newline_is_a_note_rather_than_a_changed_line() {
        let change = Change::file("a\nb\n", "a\nb");
        let Change::Lines(patch) = &change else {
            panic!("{change:?}")
        };
        assert_eq!((patch.added, patch.removed), (0, 0));
        assert!(text(&change).contains("final newline"), "{change:?}");
    }

    // -----------------------------------------------------------------------
    // Size
    // -----------------------------------------------------------------------

    /// **A cut always says what it cut and how much.** This is the defect this
    /// repository fixed in the web tools hours before this file was written, and
    /// it matters more here: the reader is being asked to approve the part they
    /// cannot see.
    #[test]
    fn a_huge_diff_is_cut_and_names_the_size_of_what_it_cut() {
        // Well inside the cell bound, so this is the ordinary hunk-by-hunk cut
        // rather than the coarse path below.
        let before: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        let after: String = (1..=1000)
            .map(|i| {
                if i <= 100 {
                    format!("CHANGED {i}\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        let change = Change::file(&before, &after);
        let Change::Lines(patch) = &change else {
            panic!("{change:?}")
        };
        assert!(!patch.coarse);
        let out = text(&change);
        assert!(out.lines().count() <= BUDGET + 8, "{}", out.lines().count());
        assert!(out.contains("more diff lines not shown"), "{out}");
        // The magnitude survives the cut. Without this the prompt shows forty
        // rows of a two-hundred-row change and reads like a small one.
        assert!(out.contains("+100 -100 lines"), "{out}");
    }

    #[test]
    fn a_region_too_large_to_line_up_is_shown_coarsely_and_says_so() {
        // No common prefix or suffix, so the middle is the whole thing and the
        // cell bound decides. What must not happen is a swap storm or a guess.
        let before: String = (1..=800).map(|i| format!("a {i}\n")).collect();
        let after: String = (1..=800).map(|i| format!("b {i}\n")).collect();
        let change = Change::file(&before, &after);
        let Change::Lines(patch) = &change else {
            panic!("{change:?}")
        };
        assert!(patch.coarse, "the cell bound did not fire");
        assert_eq!((patch.added, patch.removed), (800, 800));
        let out = text(&change);
        assert!(out.contains("too large to line up"), "{out}");
        // **Both sides are on screen.** A coarse patch is all removals then all
        // additions, so a head-of-the-diff cut would draw an 800-line rewrite as
        // an 800-line deletion — the mislabelling the prompt exists to prevent,
        // in the case where it costs the most.
        assert!(out.contains("  - a 1"), "{out}");
        assert!(out.contains("  + b 1"), "{out}");
        assert!(
            out.contains("more removed and") && out.contains("more added lines not shown"),
            "{out}"
        );
        assert!(out.lines().count() <= BUDGET + 8, "{}", out.lines().count());
    }

    #[test]
    fn a_coarse_patch_spends_a_side_of_the_budget_it_does_not_need() {
        // One side far shorter than half: the slack goes to the other rather
        // than being left unspent, and the count still adds up. A very long old
        // side is what makes this coarse at all — the cell bound is a product,
        // so a short new side has to be met by a very long old one.
        let before: String = (1..=50_000).map(|i| format!("a {i}\n")).collect();
        let change = Change::file(&before, "b 1\nb 2\nb 3\nb 4\nb 5\nb 6\n");
        let Change::Lines(patch) = &change else {
            panic!("{change:?}")
        };
        assert!(patch.coarse);
        let out = change.to_text(BUDGET);
        let dels = out.lines().filter(|l| l.starts_with("  - ")).count();
        let adds = out.lines().filter(|l| l.starts_with("  + ")).count();
        assert_eq!(adds, 6, "{out}");
        assert_eq!(dels, BUDGET - 6, "{out}");
    }

    #[test]
    fn a_file_with_more_lines_than_the_limit_is_refused_by_line_count() {
        let huge: String = "x\n".repeat(MAX_LINES + 1);
        let out = text(&Change::file("y\n", &huge));
        assert!(out.contains("no diff:"), "{out}");
        assert!(out.contains("lines"), "{out}");
        // And a creation of one, which takes the other path.
        let out = text(&Change::creation(&huge));
        assert!(out.contains("no diff:"), "{out}");
    }

    // -----------------------------------------------------------------------
    // Getting out
    // -----------------------------------------------------------------------

    /// The guarantee `emma … | tee` depends on, restated for this file.
    #[test]
    fn with_no_colour_not_one_escape_byte_is_emitted() {
        let s = skin(Level::None);
        let changes = [
            Change::file("a\nb\n", "a\nc\n"),
            Change::between("x", "y"),
            Change::creation("new\n"),
            Change::Identical,
            Change::Undiffable("because".into()),
        ];
        for change in &changes {
            for line in change.to_lines(&s, BUDGET) {
                let out = for_stream(&s, &line);
                assert!(
                    !out.contains('\x1b'),
                    "an escape sequence reached a redirected stream: {out:?}"
                );
                assert_eq!(out, plain(&line));
            }
            assert!(!change.to_text(BUDGET).contains('\x1b'));
        }
    }

    /// **A file's own bytes cannot become a control sequence.** The same rule
    /// `markdown.rs` applies to the model's prose — and here the text comes from
    /// a file on disk, which nobody screened either.
    #[test]
    fn an_escape_byte_inside_a_file_never_reaches_a_cell() {
        let s = skin(Level::Truecolor);
        let change = Change::file("plain\n", "hello \x1b[31mred\x1b[0m\ttabbed\n");
        for line in change.to_lines(&s, BUDGET) {
            let text = plain(&line);
            assert!(
                !text.chars().any(char::is_control),
                "a control byte reached a cell: {text:?}"
            );
        }
        assert!(text(&change).contains("red"), "the text was swallowed");
        assert!(text(&change).contains("tabbed"));
    }

    /// Colour and gutter, both, always — `render.rs`'s standing rule applied
    /// here. A reader who cannot distinguish the red from the green still reads
    /// the diff, and a reader with no colour at all loses only the shading.
    #[test]
    fn an_addition_and_a_removal_differ_in_gutter_as_well_as_in_colour() {
        let s = skin(Level::Truecolor);
        let lines = Change::file("a\n", "b\n").to_lines(&s, BUDGET);
        let add = lines.iter().find(|l| plain(l).contains("+ b")).unwrap();
        let del = lines.iter().find(|l| plain(l).contains("- a")).unwrap();
        assert_ne!(add.spans[0].style.fg, del.spans[0].style.fg);
        assert_eq!(add.spans[0].style.fg, Some(s.palette.color(Role::Ok)));
        assert_eq!(del.spans[0].style.fg, Some(s.palette.color(Role::Err)));
        // The gutter alone carries it at every fidelity below truecolor.
        for level in [Level::None, Level::Ansi16, Level::Ansi256] {
            let out = Change::file("a\n", "b\n").to_text(BUDGET);
            assert!(out.contains("  + b") && out.contains("  - a"), "{level:?}");
        }
    }

    #[test]
    fn the_two_renderers_cut_at_the_same_place() {
        // The panel and the transcript must not disagree about how much of a
        // diff there was — a viewport saying "12 more" over a transcript that
        // showed 20 is the class of drift this shape exists to prevent.
        let s = skin(Level::Truecolor);
        let before: String = (1..=300).map(|i| format!("line {i}\n")).collect();
        let change = Change::file(&before, &before.replace("line 1\n", "X\n"));
        for budget in [1usize, 4, 40] {
            let as_text = change.to_text(budget);
            let as_lines: Vec<String> = change
                .to_lines(&s, budget)
                .iter()
                .map(plain)
                .collect::<Vec<_>>();
            assert_eq!(as_text.lines().collect::<Vec<_>>(), as_lines, "{budget}");
        }
    }

    #[test]
    fn a_tool_with_no_diff_to_show_gets_none_rather_than_an_invented_one() {
        assert!(for_call("Bash", &serde_json::json!({ "command": "ls" })).is_none());
        // …and a Write missing its arguments is `None` too, rather than a diff
        // against the empty string.
        assert!(for_call("Write", &serde_json::json!({ "file_path": "a" })).is_none());
        assert!(for_call("Edit", &serde_json::json!({ "old_string": "a" })).is_none());
    }
}

// endregion: Tests
