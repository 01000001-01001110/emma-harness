//! The one rule about blank rows in the transcript.
//!
//! # The rule
//!
//! **A blank row is a boundary between two blocks, and it is written only when
//! there is a block on both sides of it.** Nothing emits a blank row of its own:
//! an emitter that has finished a block, or is about to start one, calls
//! [`Spacing::separate`], which records that a boundary exists. The blank is
//! materialised later, by [`Spacing::apply`], and only if another block actually
//! arrives.
//!
//! Three things follow, and each of them was a defect on the screen the owner
//! photographed:
//!
//! **Two emitters asking for the same boundary produce one blank row.** The
//! answer that has just ended asks for a separator, and the approval panel that
//! follows asks for one too. Before this, each pushed its own `Line::default()`
//! and the screen got two. A blank row between blocks is a choice; two is an
//! accident, and there was no single place to notice it because the two callers
//! never met.
//!
//! **A blank row is never the first thing in the transcript**, because there is
//! nothing above it to be separated from. The transcript starts at the shell
//! prompt the user typed `emma` at, and a gap under that reads as output that
//! went missing.
//!
//! **A blank row is never written under a blank row that is a block's own
//! content.** The model writes markdown, markdown has blank lines in it, and a
//! paragraph break followed by a separator is the same two-blank defect arriving
//! from the other direction. Content blanks are *never removed* — a blank line
//! inside a fenced code block is part of what the reader copies out, and
//! [`super::markdown`] promises the rows of a code block concatenate back to the
//! source. They only suppress a separator that would have landed beside them.
//!
//! # Why this is a state machine and not a `join`
//!
//! The transcript is written a batch at a time, above a viewport, into the
//! terminal's scrollback — see [`super::frame`]. Once a row is written it is the
//! terminal's and cannot be taken back, so the decision about a blank row has to
//! be made *before* it is emitted, out of what came before it and what is about
//! to arrive. That is exactly one bit of history (was the last row blank), one
//! bit of pending intent (has a boundary been declared) and one bit for the
//! empty transcript.
//!
//! Both output paths hold one of these: the viewport, and the plain-lines
//! fallback. Neither is allowed a separator rule of its own, because the whole
//! failure being fixed is two places each doing something reasonable.

use ratatui::text::Line;

/// Where the transcript is, as far as blank rows are concerned.
#[derive(Debug, Default, Clone)]
pub struct Spacing {
    /// Whether anything at all has been written. A separator before the first
    /// block has nothing above it to separate.
    wrote: bool,
    /// Whether the last row written was blank, whoever wrote it and for
    /// whatever reason.
    last_blank: bool,
    /// A boundary has been declared and not yet materialised.
    pending: bool,
}

impl Spacing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a boundary: whatever comes next is a different block from
    /// whatever came before.
    ///
    /// Idempotent, and deliberately so — that is the whole of "two callers
    /// cannot stack their separators". It is also why it may be called by the
    /// block that is ending or by the block that is starting, without the two
    /// having to agree: a boundary is a fact about the gap, not about either
    /// side of it.
    pub fn separate(&mut self) {
        self.pending = true;
    }

    /// Take a block on its way to the transcript, and put the boundary in front
    /// of it if one is owed.
    ///
    /// The lines are returned rather than written, so the rule is decided by a
    /// function with no terminal in it and asserted by tests that need none.
    pub fn apply(&mut self, mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
        if lines.is_empty() {
            // Nothing arrived, so no boundary was crossed. The declaration
            // stands until something does arrive — an empty batch must not
            // spend the separator the next real block is owed.
            return lines;
        }
        let owed = std::mem::take(&mut self.pending);
        if owed && self.wrote && !self.last_blank && !blank(&lines[0]) {
            lines.insert(0, Line::default());
        }
        self.last_blank = lines.last().is_some_and(blank);
        self.wrote = true;
        lines
    }

    /// Rows reached the transcript without passing through [`Spacing::apply`].
    ///
    /// The fallback path streams the model's own bytes straight to stdout —
    /// untouched, which is what keeps a pipe free of anything Emma invented —
    /// so those rows are invisible to this. This is how that path says they
    /// happened, and it is the difference between a separator appearing after
    /// an answer and one not.
    pub fn wrote_text(&mut self, ends_blank: bool) {
        self.wrote = true;
        self.last_blank = ends_blank;
    }
}

/// A row with nothing on it. Whitespace counts: an indented body line that
/// happens to be empty looks like a gap and should behave like one.
fn blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn text(line: &str) -> Vec<Line<'static>> {
        vec![Line::from(Span::raw(line.to_string()))]
    }

    fn rows(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// The rule, in one assertion: a boundary between two blocks is one blank
    /// row.
    #[test]
    fn a_boundary_between_two_blocks_is_one_blank_row() {
        let mut s = Spacing::new();
        assert_eq!(rows(&s.apply(text("first"))), ["first"]);
        s.separate();
        assert_eq!(rows(&s.apply(text("second"))), ["", "second"]);
    }

    /// **The defect.** Two emitters each declaring the same boundary — the
    /// answer that ended and the panel that follows it — used to push a blank
    /// row each.
    #[test]
    fn two_callers_declaring_the_same_boundary_still_produce_one_blank_row() {
        let mut s = Spacing::new();
        s.apply(text("answer"));
        s.separate();
        s.separate();
        s.separate();
        assert_eq!(rows(&s.apply(text("panel"))), ["", "panel"]);
    }

    /// Nothing above it to separate from.
    #[test]
    fn a_separator_before_the_first_block_writes_nothing() {
        let mut s = Spacing::new();
        s.separate();
        assert_eq!(rows(&s.apply(text("first"))), ["first"]);
    }

    /// The model's own paragraph break is a blank row, and a separator landing
    /// on top of it is the two-blank defect arriving from the other direction.
    #[test]
    fn a_separator_is_suppressed_beside_a_blank_the_content_wrote() {
        let mut s = Spacing::new();
        s.apply(vec![Line::from(Span::raw("answer")), Line::default()]);
        s.separate();
        assert_eq!(rows(&s.apply(text("next"))), ["next"]);

        // …and from the other side: a block that opens with its own blank row
        // does not get a second one in front of it.
        let mut s = Spacing::new();
        s.apply(text("answer"));
        s.separate();
        let out = s.apply(vec![Line::default(), Line::from(Span::raw("next"))]);
        assert_eq!(rows(&out), ["", "next"]);
    }

    /// **Content blanks are never removed.** A fenced code block with a blank
    /// line in it is copied out of the terminal by somebody, and
    /// [`super::super::markdown`] promises those rows concatenate back to the
    /// source. This is the assertion that stops "collapse blank runs" from
    /// looking like a reasonable simplification.
    #[test]
    fn blank_rows_a_block_wrote_itself_are_left_alone() {
        let mut s = Spacing::new();
        let code = vec![
            Line::from(Span::raw("fn main() {")),
            Line::default(),
            Line::default(),
            Line::from(Span::raw("}")),
        ];
        assert_eq!(rows(&s.apply(code)), ["fn main() {", "", "", "}"]);
    }

    /// An empty batch is not a block, so it neither spends the separator nor
    /// counts as content. The streaming path calls through here with nothing in
    /// hand all the time — a fragment with no newline in it yet.
    #[test]
    fn an_empty_batch_does_not_spend_the_separator() {
        let mut s = Spacing::new();
        s.apply(text("first"));
        s.separate();
        assert!(s.apply(Vec::new()).is_empty());
        assert_eq!(rows(&s.apply(text("second"))), ["", "second"]);
    }

    /// Rows that bypassed `apply` still count as the transcript, or the
    /// fallback path would put a separator where the previous block already
    /// ended in a blank — and would put one before its very first line.
    #[test]
    fn rows_written_outside_this_still_count_as_the_transcript() {
        let mut s = Spacing::new();
        s.wrote_text(false);
        s.separate();
        assert_eq!(rows(&s.apply(text("block"))), ["", "block"]);

        let mut s = Spacing::new();
        s.wrote_text(true);
        s.separate();
        assert_eq!(rows(&s.apply(text("block"))), ["block"]);
    }
}
