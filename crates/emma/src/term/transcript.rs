//! The conversation, retained, because a full-screen Emma cannot borrow the
//! terminal's.
//!
//! # What this replaces
//!
//! Today the transcript is not Emma's at all. `insert_before` hands each line to
//! the terminal, the terminal wraps it, keeps tens of thousands of them in
//! scrollback, scrolls them with the user's own keys and reflows them on resize
//! — for no code and no memory here. `notes/design-tui-fullscreen.md` §2.1 is
//! blunt that this is the single largest thing the alternate screen takes away,
//! and this module is the whole of the replacement: append, cap, scroll, follow
//! the tail, re-wrap at a new width.
//!
//! **It draws nothing and owns no terminal.** It produces `Vec<Line>` and takes
//! a width; a [`Skin`] is passed in rather than held, so every decision in here
//! is testable without a console. That is deliberate and it is the point of
//! building it a stage early: `notes/lessons/testbackend-clears-one-cell-fewer…`
//! and the two shipped scrollback defects both say that anything with a terminal
//! in it is certified late and expensively, so the parts that can be separated
//! from one should be.
//!
//! **It is not wired into the live UI.** Nothing constructs one outside tests.
//! The inline viewport still writes through `insert_before` and still gets its
//! scrollback for free; this is here so that the flip, when it comes, is a
//! change of caller rather than a change of design.
//!
//! # Three decisions worth the argument
//!
//! **Entries keep their source, not just their pixels.** An `Assistant` entry
//! holds the markdown it arrived as, so a resize re-runs [`Markdown`] over it at
//! the new width — the same code path streaming uses, so the two cannot drift
//! into rendering the same text differently. `Activity` and `Note` entries have
//! no source: they arrive already line-shaped from [`Skin`], and re-wrapping
//! them means splitting the styled line rather than re-styling a split one (see
//! [`wrap_line`]). Search will want plain source for those too — the evaluation
//! says so — and that is a change to this type, made when search is built, not
//! guessed at now.
//!
//! **Rendering is eager, and the cost is measured rather than asserted.** The
//! plan proposed rendering lazily from the tail on resize. That needs a height
//! estimate for the entries nobody has rendered yet, and an estimate that is
//! ever wrong is a view that jumps under the reader. Rendering everything is
//! exact, keeps the row total that the cap is enforced against always true, and
//! is bounded by the cap itself — and `a_full_buffer_rewraps_faster_than_a_frame`
//! is what stops that from being an opinion.
//!
//! **The scroll position is measured from the bottom, and appending moves it.**
//! Rows scrolled *up* from the tail, rather than an index into the buffer, so
//! dropping the oldest entries at the cap cannot move what the user is looking
//! at. The plan proposed exactly that and stopped there, and it is half of the
//! answer: an offset from the tail is stable against the front, and slides
//! forward by every row that arrives at the *back*. A view scrolled up ten rows
//! that receives ten more rows of tool output is no longer looking at the same
//! text — which is the failure the whole latch exists to prevent, arriving from
//! the direction the plan did not check. So a scrolled view has its offset
//! grown by whatever was appended below it, and stays still. The tests here
//! found that; the design document did not.

use std::collections::VecDeque;

use ratatui::text::Line;

use super::markdown::Markdown;
use super::render::{owned, wrap_line, Skin};

// region: Entries
// ---------------------------------------------------------------------------

/// One block in the conversation.
#[derive(Debug, Clone)]
pub enum EntryKind {
    /// What the user typed, verbatim — a goal or a command line.
    User(String),
    /// Assistant prose, as the markdown it arrived as. The last entry may still
    /// be growing: `done` is false until the turn ends.
    Assistant { source: String, done: bool },
    /// Tool traffic — the `◆ ✓ ✗ ⊘ ⊗` vocabulary and diff hunks — already
    /// line-shaped by [`Skin`] and [`super::diff`].
    Activity(Vec<Line<'static>>),
    /// Notes, warnings, endings, the welcome, an answered prompt. Also [`Skin`]
    /// output; separate from `Activity` because the two get different gutters
    /// when the two-column transcript is built.
    Note(Vec<Line<'static>>),
}

/// An entry and its rendering at the current width.
#[derive(Debug, Clone)]
struct Entry {
    kind: EntryKind,
    lines: Vec<Line<'static>>,
}

impl Entry {
    /// Render this entry at `width`, replacing whatever was cached.
    ///
    /// The blank row *between* entries is not produced here: it is a fact about
    /// the gap, not about either side of it, and [`super::spacing`] argues that
    /// at length. An entry that rendered its own trailing blank would stack it
    /// with the separator and give the screen two.
    fn render(&mut self, skin: &Skin, width: u16) {
        self.lines = match &self.kind {
            EntryKind::User(text) => skin
                .goal(text)
                .iter()
                .flat_map(|l| wrap_line(l, width))
                .collect(),
            EntryKind::Assistant { source, .. } => {
                // A fresh state machine per render. `Markdown` carries an
                // open-fence bit across lines, so re-running an entry means
                // re-running all of it — starting mid-way would render the
                // inside of a code block as prose.
                let mut md = Markdown::new();
                source
                    .split('\n')
                    .flat_map(|line| md.line(line, width, skin))
                    .collect()
            }
            EntryKind::Activity(lines) | EntryKind::Note(lines) => {
                lines.iter().flat_map(|l| wrap_line(l, width)).collect()
            }
        };
        // A block that ends in blank rows of its own would double the separator
        // the next block gets — the defect `super::spacing` was written for,
        // arriving here from a different direction.
        while self.lines.last().is_some_and(is_blank) {
            self.lines.pop();
        }
        while self.lines.first().is_some_and(is_blank) {
            self.lines.remove(0);
        }
    }
}

fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

// endregion: Entries

// region: The buffer
// ---------------------------------------------------------------------------

/// How much is kept. Whichever binds first.
///
/// **Both numbers are guesses and are written down as such.** The plan proposes
/// them, the evaluation calls them a guess, and neither is wrong: terminal
/// scrollback was effectively unbounded and this is a ceiling that did not exist
/// before. What makes it honest rather than a silent loss is [`Transcript::dropped`]
/// and the marker row it produces — the cap is allowed to bind, it is not
/// allowed to bind quietly. The measurement that should set them is a real
/// session's memory footprint, and it has not been taken.
#[derive(Debug, Clone, Copy)]
pub struct Cap {
    pub entries: usize,
    pub rows: usize,
}

impl Default for Cap {
    fn default() -> Self {
        Self {
            entries: 2_000,
            rows: 50_000,
        }
    }
}

/// The conversation as Emma will have to keep it.
#[derive(Debug, Clone)]
pub struct Transcript {
    entries: VecDeque<Entry>,
    /// The width every cached rendering was made at. A change invalidates all
    /// of them.
    width: u16,
    /// Rows scrolled up from the tail. Zero is the bottom.
    scroll: usize,
    /// Whether new output pins the view to the bottom.
    follow: bool,
    cap: Cap,
    /// Entries dropped off the front, and the rows they rendered as. Kept
    /// forever: the marker they produce is permanent, because the fact that
    /// output is missing does not expire.
    dropped_entries: usize,
    dropped_rows: usize,
    /// Where the whole conversation is, for the marker to point at. The session
    /// JSONL is complete whatever this buffer drops.
    record: Option<String>,
    /// The rendered height of everything held, kept in step with the entries so
    /// the cap and the scroll bounds never have to walk the buffer.
    rows: usize,
    /// The cap marker's rows plus its blank, measured whenever it could have
    /// changed. Zero until the cap first binds.
    marker_rows: usize,
}

impl Transcript {
    pub fn new(cap: Cap) -> Self {
        Self {
            entries: VecDeque::new(),
            width: 0,
            scroll: 0,
            follow: true,
            cap,
            dropped_entries: 0,
            dropped_rows: 0,
            record: None,
            rows: 0,
            marker_rows: 0,
        }
    }

    /// Where the complete conversation lives, named in the cap marker.
    ///
    /// Takes a [`Skin`] because it changes the marker's text and therefore its
    /// height, and a height the scroll bounds believe has to be measured rather
    /// than assumed — see [`Self::total`].
    pub fn record_at(&mut self, path: impl Into<String>, skin: &Skin) {
        self.record = Some(path.into());
        self.remeasure_marker(skin);
    }

    /// The marker is a wrapped sentence, so its height depends on the width and
    /// on what it says. Measured here, once, whenever either could have moved.
    fn remeasure_marker(&mut self, skin: &Skin) {
        self.marker_rows = if self.dropped_entries > 0 {
            // Its own rows, plus the blank that separates it from the oldest
            // block that survived.
            self.dropped_marker(skin).len() + 1
        } else {
            0
        };
    }

    /// Add a block.
    ///
    /// Rendered immediately at the current width — see the module doc on why
    /// eagerly — which is also what keeps [`Self::rows`] true for the cap.
    pub fn push(&mut self, kind: EntryKind, skin: &Skin, width: u16) {
        self.set_width(skin, width);
        let mut entry = Entry {
            kind,
            lines: Vec::new(),
        };
        entry.render(skin, self.width);
        // The separator this block will be given, counted with it: it is a row
        // that appears below a scrolled view exactly as the block's own rows
        // are.
        let separator = usize::from(!self.entries.is_empty());
        let added = entry.lines.len() + separator;
        self.rows += entry.lines.len();
        self.entries.push_back(entry);
        self.enforce_cap(skin);
        self.appended(added);
    }

    /// Extend the assistant entry being streamed, or start one.
    ///
    /// **The tail entry is re-rendered whole on every delta.** The inline design
    /// could not do this — a fragment written above the viewport can never be
    /// extended, which is why `View::partial` exists — and it is the one thing
    /// an owned buffer straightforwardly wins: a code fence that opens three
    /// deltas in restyles everything it covers, instead of the first rows
    /// staying prose forever.
    pub fn stream(&mut self, delta: &str, skin: &Skin, width: u16) {
        self.set_width(skin, width);
        let growable = matches!(
            self.entries.back().map(|e| &e.kind),
            Some(EntryKind::Assistant { done: false, .. })
        );
        if !growable {
            self.push(
                EntryKind::Assistant {
                    source: String::new(),
                    done: false,
                },
                skin,
                width,
            );
        }
        let Some(entry) = self.entries.back_mut() else {
            return;
        };
        if let EntryKind::Assistant { source, .. } = &mut entry.kind {
            source.push_str(delta);
        }
        let was = entry.lines.len();
        self.rows -= was;
        entry.render(skin, self.width);
        let now = entry.lines.len();
        self.rows += now;
        self.enforce_cap(skin);
        self.appended(now.saturating_sub(was));
    }

    /// Rows arrived at the tail.
    ///
    /// Following means the view stays at the bottom; not following means the
    /// view stays where it is, which — with the offset counted from the bottom —
    /// costs one addition. Getting this wrong is invisible until somebody
    /// scrolls up during a long tool call and watches their page walk away from
    /// them.
    fn appended(&mut self, rows: usize) {
        if self.follow {
            self.scroll = 0;
        } else {
            self.scroll = (self.scroll + rows).min(self.max_scroll());
        }
    }

    /// The turn ended: the tail entry stops growing.
    pub fn finish(&mut self) {
        if let Some(entry) = self.entries.back_mut() {
            if let EntryKind::Assistant { done, .. } = &mut entry.kind {
                *done = true;
            }
        }
    }

    /// Re-wrap everything for a new width.
    ///
    /// A no-op at the same width, which matters more than it looks: every append
    /// calls through here, and a resize path that re-rendered the buffer on
    /// every line of tool output would be the slowest thing in Emma.
    pub fn set_width(&mut self, skin: &Skin, width: u16) {
        let width = width.max(1);
        if width == self.width {
            return;
        }
        self.width = width;
        self.rows = 0;
        for entry in &mut self.entries {
            entry.render(skin, width);
            self.rows += entry.lines.len();
        }
        self.remeasure_marker(skin);
        // The rows a given entry occupies changed, so a scroll offset measured
        // in rows now points somewhere else. Clamping is the honest minimum;
        // holding the *entry* the user was reading is a nicer behaviour and a
        // different feature, and guessing at it here would be the estimate this
        // module chose not to have.
        self.scroll = self.scroll.min(self.max_scroll());
        if self.follow {
            self.scroll = 0;
        }
    }

    /// Drop from the front until both halves of the cap are satisfied.
    fn enforce_cap(&mut self, skin: &Skin) {
        let before = self.dropped_entries;
        while self.entries.len() > self.cap.entries
            || (self.rows > self.cap.rows && self.entries.len() > 1)
        {
            let Some(gone) = self.entries.pop_front() else {
                break;
            };
            self.rows -= gone.lines.len();
            self.dropped_rows += gone.lines.len();
            self.dropped_entries += 1;
        }
        if self.dropped_entries != before {
            self.remeasure_marker(skin);
        }
    }

    // -----------------------------------------------------------------------
    // Reading it
    // -----------------------------------------------------------------------

    /// Every row, in order, with one blank between blocks and the cap marker on
    /// top when the cap has bound.
    ///
    /// The marker is part of the content rather than chrome drawn over it, so it
    /// scrolls with the oldest thing that survived — which is where somebody
    /// looking for what is missing will actually be.
    pub fn rows(&self, skin: &Skin) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        if self.dropped_entries > 0 {
            out.extend(self.dropped_marker(skin));
            out.push(Line::default());
        }
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push(Line::default());
            }
            out.extend(entry.lines.iter().map(owned));
        }
        out
    }

    /// The rows a pane of this height shows, at the current scroll.
    pub fn visible(&self, skin: &Skin, height: u16) -> Vec<Line<'static>> {
        let all = self.rows(skin);
        let (start, end) = self.window(all.len(), height);
        all[start..end].to_vec()
    }

    /// The half-open row range a pane of this height is looking at.
    ///
    /// **The scroll cannot lift the window off the top of the buffer.** The
    /// offset is counted from the bottom and knows nothing about the pane's
    /// height, so the clamp has to happen where the height is known: scrolled
    /// all the way up, the window is the *first* `height` rows rather than a
    /// sliver of them. Home showing one row of a twenty-row pane is the kind of
    /// arithmetic that looks right in the type and wrong on the screen.
    fn window(&self, total: usize, height: u16) -> (usize, usize) {
        let height = usize::from(height);
        if height == 0 || total == 0 {
            return (0, 0);
        }
        let end = total.saturating_sub(self.scroll).max(height.min(total));
        (end.saturating_sub(height), end)
    }

    /// What the cap ate, said out loud.
    ///
    /// **The number and the remedy, never just an ellipsis.** A transcript that
    /// is quietly a third of itself is the fabrication rule's exact case: name
    /// the cap, name the loss, name where the whole thing is. The session log is
    /// complete regardless of anything this buffer does, so there is always a
    /// remedy to name.
    fn dropped_marker(&self, skin: &Skin) -> Vec<Line<'static>> {
        let text = match &self.record {
            Some(path) => format!(
                "{} older output dropped ({} blocks, {} rows) — all of it is in {path}",
                skin.glyphs.ellipsis, self.dropped_entries, self.dropped_rows
            ),
            None => format!(
                "{} older output dropped ({} blocks, {} rows) — the session log has all of it",
                skin.glyphs.ellipsis, self.dropped_entries, self.dropped_rows
            ),
        };
        let line = Line::from(ratatui::text::Span::styled(text, skin.palette.dim()));
        // **Wrapped, never cut.** The first version truncated this to one row
        // and a test caught it at eighty columns: what fell off the end was the
        // path to the complete record — the entire remedy, from the one line on
        // screen whose job is to carry it. A marker that says output is missing
        // and then loses the answer is worse than no marker.
        wrap_line(&line, self.width)
    }

    /// How many blocks the cap has dropped. Zero means nothing was lost.
    pub fn dropped(&self) -> usize {
        self.dropped_entries
    }

    /// Total rows, including separators and the marker.
    pub fn height(&self, skin: &Skin) -> usize {
        self.rows(skin).len()
    }

    // -----------------------------------------------------------------------
    // Scrolling
    //
    // The latch, in the shape `frame.rs`'s `pinned` uses: distinguish where the
    // system put the view from where the user put it. New output moves a
    // following view and never moves a scrolled one — a transcript that yanks
    // itself to the bottom while somebody is reading it is unusable during
    // exactly the long tool call they scrolled up to read about.
    // -----------------------------------------------------------------------

    /// Total rendered rows, separators and marker included, without
    /// materialising them. `rows()` is the exact list and walking it on every
    /// keypress would rebuild the buffer to answer "how far can I scroll".
    fn total(&self) -> usize {
        let separators = self.entries.len().saturating_sub(1);
        // `marker_rows` is measured when the marker changes rather than
        // estimated from the width here. An estimate that is ever wrong is a
        // scroll bound that is wrong, and a scroll bound that is wrong is a row
        // of the transcript nobody can reach.
        self.rows + separators + self.marker_rows
    }

    fn max_scroll(&self) -> usize {
        // One row short of everything: the window clamp in [`Self::window`]
        // keeps the top honest, and leaving the offset able to reach the total
        // would let `behind` claim rows nobody can scroll past.
        self.total().saturating_sub(1)
    }

    /// Scroll up, away from the tail. Breaks the follow latch.
    pub fn scroll_up(&mut self, rows: usize) {
        let limit = self.max_scroll();
        if limit == 0 {
            return;
        }
        self.scroll = (self.scroll + rows).min(limit);
        if self.scroll > 0 {
            self.follow = false;
        }
    }

    /// Scroll down, towards the tail. Reaching it resumes following, because
    /// arriving at the bottom and *not* being pinned there is a state nobody
    /// asked for and nobody can see.
    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
        if self.scroll == 0 {
            self.follow = true;
        }
    }

    /// The End key, and every submit: back to the tail, following again.
    pub fn follow_tail(&mut self) {
        self.scroll = 0;
        self.follow = true;
    }

    /// Home: as far back as there is.
    pub fn to_top(&mut self) {
        self.scroll = self.max_scroll();
        if self.scroll > 0 {
            self.follow = false;
        }
    }

    pub fn is_following(&self) -> bool {
        self.follow
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll
    }

    /// How many rows sit below the bottom of a pane of this height — the
    /// `↓ N new rows` indicator's number. Zero while following.
    ///
    /// Takes the height because the offset alone over-reports at the top of a
    /// short buffer, where the window clamp has already stopped the view from
    /// going as far as the offset says. An indicator that promises rows the End
    /// key does not produce is a readout that lies, and the status line's rule
    /// applies here too.
    pub fn behind(&self, height: u16) -> usize {
        let total = self.total();
        let (_, end) = self.window(total, height);
        total.saturating_sub(end)
    }
}

// endregion: The buffer

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::{plain, UNICODE};

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    fn text(t: &Transcript, skin: &Skin) -> Vec<String> {
        t.rows(skin).iter().map(plain).collect()
    }

    fn note(s: &str) -> EntryKind {
        EntryKind::Note(vec![Line::from(ratatui::text::Span::raw(s.to_string()))])
    }

    // -----------------------------------------------------------------------
    // Appending and the blank-row rule
    // -----------------------------------------------------------------------

    #[test]
    fn blocks_are_separated_by_exactly_one_blank_row() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.push(note("first"), &skin, 40);
        t.push(note("second"), &skin, 40);
        assert_eq!(text(&t, &skin), ["first", "", "second"]);
    }

    /// The rule `super::spacing` argues: a blank row is never the first thing,
    /// and an emitter never writes one of its own.
    #[test]
    fn no_entry_carries_a_blank_row_of_its_own() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.push(
            EntryKind::Note(vec![
                Line::default(),
                Line::from(ratatui::text::Span::raw("body".to_string())),
                Line::default(),
            ]),
            &skin,
            40,
        );
        t.push(note("next"), &skin, 40);
        assert_eq!(
            text(&t, &skin),
            ["body", "", "next"],
            "an entry's own blank rows stacked with the separator"
        );
    }

    // -----------------------------------------------------------------------
    // Wrapping and resize
    // -----------------------------------------------------------------------

    #[test]
    fn a_line_wider_than_the_pane_takes_the_rows_it_needs() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.push(note(&"x".repeat(30)), &skin, 10);
        let rows = text(&t, &skin);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows.concat(), "x".repeat(30), "the wrap lost characters");
    }

    /// The whole reason entries keep their source: a resize is a re-render, not
    /// a re-cut of rows that were already cut.
    #[test]
    fn widening_the_window_puts_a_wrapped_block_back_on_one_row() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.push(note(&"y".repeat(30)), &skin, 10);
        assert_eq!(text(&t, &skin).len(), 3);
        t.set_width(&skin, 40);
        assert_eq!(text(&t, &skin), ["y".repeat(30)]);
        // …and back again, from the re-rendered form rather than from a
        // one-way cut.
        t.set_width(&skin, 10);
        assert_eq!(text(&t, &skin).len(), 3);
    }

    /// Wide characters are the case the character-counting version got wrong:
    /// ten CJK glyphs are twenty columns, not ten.
    #[test]
    fn a_wide_character_block_wraps_by_columns_not_by_characters() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.push(note(&"一".repeat(10)), &skin, 10);
        let rows = text(&t, &skin);
        assert_eq!(rows.len(), 2, "{rows:?}");
        for row in &rows {
            assert_eq!(row.chars().count(), 5, "{rows:?}");
        }
        assert_eq!(rows.concat(), "一".repeat(10));
    }

    // -----------------------------------------------------------------------
    // Streaming
    // -----------------------------------------------------------------------

    #[test]
    fn deltas_join_into_one_entry_rather_than_one_entry_each() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.stream("hello ", &skin, 40);
        t.stream("world", &skin, 40);
        assert_eq!(text(&t, &skin), ["hello world"]);
    }

    /// What an owned buffer buys and the inline viewport could not: text
    /// already on screen restyles when a later delta changes what it is.
    #[test]
    fn a_fence_arriving_later_restyles_the_rows_it_covers() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.stream("```\nlet x = 1;", &skin, 40);
        let opened = t.rows(&skin);
        t.stream("\n```\nafter", &skin, 40);
        let closed = t.rows(&skin);
        assert_ne!(
            opened.first().map(|l| l.spans.len()),
            None,
            "nothing rendered"
        );
        assert!(
            closed.iter().any(|l| plain(l).contains("after")),
            "{:?}",
            closed.iter().map(plain).collect::<Vec<_>>()
        );
        // The code row is styled as code in both, which is the property: the
        // renderer re-runs over the whole source, so the first row cannot be
        // left as prose by a fence that opened before it.
        let code_row = |lines: &[Line<'static>]| {
            lines
                .iter()
                .find(|l| plain(l).contains("let x = 1;"))
                .map(|l| l.spans.iter().map(|s| s.style).collect::<Vec<_>>())
        };
        assert_eq!(code_row(&opened), code_row(&closed));
    }

    #[test]
    fn a_finished_turn_does_not_absorb_the_next_one() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.stream("first answer", &skin, 40);
        t.finish();
        t.stream("second answer", &skin, 40);
        assert_eq!(text(&t, &skin), ["first answer", "", "second answer"]);
    }

    // -----------------------------------------------------------------------
    // The cap
    // -----------------------------------------------------------------------

    /// **The guarantee.** The buffer is allowed to drop; it is not allowed to
    /// drop quietly.
    #[test]
    fn dropping_the_oldest_blocks_leaves_a_marker_saying_so() {
        let skin = skin();
        let mut t = Transcript::new(Cap {
            entries: 3,
            rows: 50_000,
        });
        for i in 0..10 {
            t.push(note(&format!("block {i}")), &skin, 60);
        }
        let rows = text(&t, &skin);
        assert!(!rows.iter().any(|r| r.contains("block 0")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("block 9")), "{rows:?}");
        let marker = &rows[0];
        assert!(marker.contains("dropped"), "{marker:?}");
        // The count, so the loss has a size…
        assert!(
            marker.contains('7'),
            "the marker did not say how much: {marker:?}"
        );
        // …and the remedy, so it has an answer.
        assert!(marker.contains("session log"), "{marker:?}");
        assert_eq!(t.dropped(), 7);
    }

    #[test]
    fn the_marker_names_the_record_when_there_is_one() {
        let skin = skin();
        let mut t = Transcript::new(Cap {
            entries: 1,
            rows: 50_000,
        });
        t.record_at("~/.emma/sessions/sess-abc.jsonl", &skin);
        t.push(note("gone"), &skin, 80);
        t.push(note("kept"), &skin, 80);
        let rows = text(&t, &skin);
        // The remedy survives the wrap. The first version of this cut the
        // marker to one row and lost the path off the right-hand edge; a
        // narrow window is exactly where somebody needs it most.
        assert!(
            rows.join("").contains("sess-abc.jsonl"),
            "the marker lost the path to the record: {rows:?}"
        );
        assert!(rows.iter().any(|r| r.contains("kept")), "{rows:?}");
    }

    /// The marker wraps rather than truncating at every width Emma will draw.
    #[test]
    fn the_marker_keeps_its_remedy_at_every_width() {
        let skin = skin();
        for width in [24u16, 40, 60, 80, 120] {
            let mut t = Transcript::new(Cap {
                entries: 1,
                rows: 50_000,
            });
            t.record_at("~/.emma/sessions/sess-abc.jsonl", &skin);
            t.push(note("gone"), &skin, width);
            t.push(note("kept"), &skin, width);
            let rows = text(&t, &skin);
            assert!(
                rows.join("").contains("sess-abc.jsonl"),
                "at {width} columns the marker lost its remedy: {rows:?}"
            );
            // …and the rows it took are the rows the scroll arithmetic counts.
            assert_eq!(
                t.height(&skin),
                t.total(),
                "the marker's measured height and its rendered height disagree at {width}"
            );
        }
    }

    /// The row half of the cap, and the reason it never empties the buffer: a
    /// single block taller than the whole budget is still the block the user is
    /// looking at.
    #[test]
    fn the_row_budget_binds_without_emptying_the_buffer() {
        let skin = skin();
        let mut t = Transcript::new(Cap {
            entries: 10_000,
            rows: 5,
        });
        for i in 0..20 {
            t.push(note(&format!("row {i}")), &skin, 40);
        }
        assert!(t.rows <= 5, "the row budget did not bind: {}", t.rows);
        assert!(t.dropped() > 0);

        let mut t = Transcript::new(Cap {
            entries: 10_000,
            rows: 2,
        });
        t.push(note(&"z".repeat(400)), &skin, 40);
        assert_eq!(t.entries.len(), 1, "the last block was dropped");
        assert_eq!(t.dropped(), 0, "and nothing was reported as lost");
    }

    // -----------------------------------------------------------------------
    // Scrolling and the follow latch
    // -----------------------------------------------------------------------

    #[test]
    fn a_following_view_shows_the_tail_and_new_output_keeps_it_there() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        for i in 0..20 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        let shown = t.visible(&skin, 3);
        assert_eq!(
            shown.iter().map(plain).collect::<Vec<_>>(),
            ["line 18", "", "line 19"]
        );
        assert!(t.is_following());
    }

    /// **The latch.** Output arriving while the user is reading earlier output
    /// must not move the page.
    #[test]
    fn scrolling_up_survives_everything_that_arrives_afterwards() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        for i in 0..20 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        t.scroll_up(10);
        assert!(!t.is_following());
        let before = t.visible(&skin, 3).iter().map(plain).collect::<Vec<_>>();
        for i in 20..30 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        assert_eq!(
            t.visible(&skin, 3).iter().map(plain).collect::<Vec<_>>(),
            before,
            "new output dragged the reader's page to the bottom"
        );
        // …and the indicator counts the rows that arrived below the view, so
        // "N new rows" is ten plus the ten that came in, not ten.
        assert_eq!(t.behind(3), 30);
    }

    #[test]
    fn scrolling_back_to_the_bottom_resumes_following() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        for i in 0..20 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        t.scroll_up(5);
        assert!(!t.is_following());
        t.scroll_down(5);
        assert!(t.is_following(), "arriving at the tail did not re-latch");
        t.scroll_up(5);
        t.follow_tail();
        assert!(t.is_following());
        assert_eq!(t.scroll_offset(), 0);
    }

    #[test]
    fn scrolling_cannot_pass_the_top_or_the_bottom() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        for i in 0..5 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        t.scroll_up(10_000);
        let top = t.visible(&skin, 3).iter().map(plain).collect::<Vec<_>>();
        assert_eq!(top.first().map(String::as_str), Some("line 0"), "{top:?}");
        // **A full pane, not a sliver.** The offset is counted from the bottom
        // and can exceed what a pane of this height can show; without the clamp
        // in `window` the top of the buffer is drawn as one row in a three-row
        // pane, and `first() == "line 0"` is true either way. This is the
        // assertion that tells them apart.
        assert_eq!(top.len(), 3, "the pane was not filled at the top: {top:?}");
        t.to_top();
        assert_eq!(
            t.visible(&skin, 3).iter().map(plain).collect::<Vec<_>>(),
            top
        );
        t.scroll_down(10_000);
        assert_eq!(t.scroll_offset(), 0);
        assert!(t.is_following());
    }

    /// Dropping the oldest content must not move what the reader is looking at
    /// — the reason the offset is measured from the bottom.
    #[test]
    fn the_cap_binding_does_not_move_the_readers_page() {
        let skin = skin();
        let mut t = Transcript::new(Cap {
            entries: 12,
            rows: 50_000,
        });
        for i in 0..12 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        t.scroll_up(4);
        let before = t.visible(&skin, 3).iter().map(plain).collect::<Vec<_>>();
        for i in 12..16 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        assert!(t.dropped() > 0, "the fixture did not make the cap bind");
        assert_eq!(
            t.visible(&skin, 3).iter().map(plain).collect::<Vec<_>>(),
            before,
            "dropping the oldest output moved the page"
        );
    }

    /// A pane shorter than the content shows the last `height` rows and no more
    /// — the assertion a row dump makes and a height check does not.
    #[test]
    fn a_pane_shows_exactly_the_rows_it_has_room_for() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        for i in 0..20 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        for height in [0u16, 1, 5, 40] {
            let shown = t.visible(&skin, height);
            assert!(shown.len() <= usize::from(height), "{height}");
        }
        // Taller than the content shows all of it, not padding.
        assert_eq!(t.visible(&skin, 200).len(), t.height(&skin));
    }

    // -----------------------------------------------------------------------
    // The claim the plan made and did not measure
    // -----------------------------------------------------------------------

    /// **"Cheap at transcript scale" was an assertion; this is the number.**
    ///
    /// The evaluation flagged re-running markdown over every entry on resize as
    /// unmeasured optimism, and `notes/lessons/measure-an-adopted-idea-before-…`
    /// says what to do about that. A full buffer re-wrapped has to beat a frame
    /// at 60Hz by a wide margin or the eager design in this module is wrong and
    /// the laziness the plan wanted has to be built after all.
    ///
    /// **The measurement, 2026-08-11, Windows 11 / Ryzen:** 1 000 entries and
    /// 5 000 rendered rows re-wrapped from 120 columns to 80 in **3.5 ms in
    /// release** and **21 ms in a debug build**. So the eager design is right at
    /// this size and the plan's "cheap at transcript scale" is true — with a
    /// caveat it did not have: the cost is linear, so at [`Cap::default`]'s full
    /// 50 000 rows it is roughly 35 ms in release, which is *two frames*, not
    /// none. A resize is a rare, user-initiated event and a two-frame hitch on
    /// one is acceptable where a two-frame hitch per keystroke would not be —
    /// but if the cap is ever raised, this is the number that has to be
    /// re-measured before it is.
    ///
    /// The assertion's bound is deliberately loose — a hundred milliseconds, not
    /// sixteen — because this runs on whatever machine CI has and a flaky timing
    /// test gets deleted, which would lose the measurement entirely. The real
    /// number is printed on every run.
    #[test]
    fn a_full_buffer_rewraps_faster_than_a_frame() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        // A realistic mix: prose with a code block, a tool line, a goal.
        for i in 0..500 {
            t.push(EntryKind::User(format!("goal {i}")), &skin, 120);
            t.push(
                EntryKind::Assistant {
                    source: format!(
                        "Here is what {i} does, at some length so that the wrap has real \
                         work to do on a narrow window.\n\n- a bullet\n- another\n\n```rust\n\
                         fn main() {{ println!(\"{i}\"); }}\n```\n"
                    ),
                    done: true,
                },
                &skin,
                120,
            );
        }
        let held = t.entries.len();
        let start = std::time::Instant::now();
        t.set_width(&skin, 80);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "re-wrapping {held} entries ({} rows) at a new width took {elapsed:?}; the eager \
             design in this module assumes it is far below a frame",
            t.rows
        );
        // Printed so the number is on the record even when it passes.
        println!("rewrap: {held} entries, {} rows, {elapsed:?}", t.rows);
    }
}
