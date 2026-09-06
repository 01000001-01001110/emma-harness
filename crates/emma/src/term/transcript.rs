//! The conversation, retained, because a full-screen Emma cannot borrow the
//! terminal's.
//!
//! # What this replaces
//!
//! Today the transcript is not Emma's at all. `insert_before` hands each line to
//! the terminal, the terminal wraps it, keeps tens of thousands of them in
//! scrollback, scrolls them with the user's own keys and reflows them on resize
//! — for no code and no memory here. The full-screen design is
//! blunt that this is the single largest thing the alternate screen takes away,
//! and this module is the whole of the replacement: append, cap, scroll, follow
//! the tail, re-wrap at a new width.
//!
//! **It draws nothing and owns no terminal.** It produces `Vec<Line>` and takes
//! a width; a [`Skin`] is passed in rather than held, so every decision in here
//! is testable without a console. That is deliberate and it is the point of
//! building it a stage early: a recorded lesson on TestBackend clearing one
//! cell fewer than expected, and the two shipped scrollback defects both say that anything with a terminal
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

/// Which voice a block speaks in, as the two-column pane needs to know it.
///
/// Derived here rather than in [`super::chat`], because the alternative is the
/// pane sniffing styled spans to guess who is talking — which works until the
/// day [`Skin`] restyles anything, and then fails silently. The kind is a fact
/// this module already holds; the pane should be told it, not reconstruct it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    /// The human: a goal or a command line.
    You,
    /// The assistant's prose.
    Emma,
    /// Tool traffic. Not a speaker — see [`super::chat`] for what that means
    /// for the gutter.
    Activity,
    /// The harness talking about itself: notes, warnings, endings, the
    /// welcome. Kept distinct from `Activity` so a pane may treat the two
    /// apart without this type changing again.
    Note,
}

impl EntryKind {
    /// Who this block belongs to.
    pub fn voice(&self) -> Voice {
        match self {
            Self::User(_) => Voice::You,
            Self::Assistant { .. } => Voice::Emma,
            Self::Activity(_) => Voice::Activity,
            Self::Note(_) => Voice::Note,
        }
    }
}

/// One rendered row and where it came from.
///
/// What [`Transcript::rows`] cannot say: which block a row renders and whether
/// it opens it, which is what a speaker gutter needs to put the label beside
/// the right row. Produced by the same assembly as `rows` — one assembly, not
/// two, because two assemblies of the same rows is how a label and the scroll
/// arithmetic drift apart.
#[derive(Debug, Clone)]
pub struct TaggedRow {
    pub line: Line<'static>,
    /// `None` for the cap marker and the blank separators, which belong to no
    /// block; otherwise the block's index, its voice, and whether this row is
    /// its first.
    pub block: Option<(usize, Voice, bool)>,
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
    /// The most recent assistant turn, as the markdown it arrived as.
    ///
    /// ⚠ THE SOURCE, NOT THE CELLS. A person copying an answer wants the text, and what is on
    /// screen is that text after wrapping, with a gutter beside it and a border around it. Native
    /// terminal selection over a cell grid returns all of that interleaved, which is why this
    /// module's own notes call `/export` the honest copy path. The source string is what was
    /// rendered FROM, so it has no wrap points that were not in the original and no furniture.
    pub fn last_assistant_source(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|e| match &e.kind {
            EntryKind::Assistant { source, .. } if !source.trim().is_empty() => {
                Some(source.clone())
            }
            _ => None,
        })
    }
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
    /// than assumed — see `total`.
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
        // Built from the tagged form so the two can never disagree about what
        // the transcript contains: the tags are these rows, annotated.
        self.tagged(skin).into_iter().map(|r| r.line).collect()
    }

    /// Every row with its provenance — the assembly `rows` and the visible
    /// windows are both cut from.
    fn tagged(&self, skin: &Skin) -> Vec<TaggedRow> {
        let mut out: Vec<TaggedRow> = Vec::new();
        if self.dropped_entries > 0 {
            out.extend(
                self.dropped_marker(skin)
                    .into_iter()
                    .map(|line| TaggedRow { line, block: None }),
            );
            out.push(TaggedRow {
                line: Line::default(),
                block: None,
            });
        }
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push(TaggedRow {
                    line: Line::default(),
                    block: None,
                });
            }
            let voice = entry.kind.voice();
            out.extend(entry.lines.iter().enumerate().map(|(j, line)| TaggedRow {
                line: owned(line),
                block: Some((i, voice, j == 0)),
            }));
        }
        out
    }

    /// The rows a pane of this height shows, at the current scroll.
    pub fn visible(&self, skin: &Skin, height: u16) -> Vec<Line<'static>> {
        self.visible_tagged(skin, height)
            .into_iter()
            .map(|r| r.line)
            .collect()
    }

    /// [`Self::visible`] for a renderer that also needs to know who is
    /// speaking on each row. Same window, same rows — the plain form above is
    /// cut from this one so a test comparing the two cannot find a difference.
    pub fn visible_tagged(&self, skin: &Skin, height: u16) -> Vec<TaggedRow> {
        let mut all = self.tagged(skin);
        let (start, end) = self.window(all.len(), height);
        all.truncate(end);
        all.drain(..start);
        all
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

    /// The first content row a pane of this height is showing.
    ///
    /// Selection anchors are content rows, not screen rows, so the pane needs
    /// the one number that converts between them. It is [`Self::window`]'s
    /// start and nothing else: two derivations of the same index would drift
    /// the first time the top clamp changed.
    pub fn window_start(&self, height: u16) -> usize {
        self.window(self.total(), height).0
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

    /// Park the offset outright, clamped to the same bound the keys obey.
    ///
    /// The scrollbar drag needs this: it computes a position from where the
    /// pointer is rather than stepping from where the view was, and stepping
    /// there through `scroll_up`/`scroll_down` would be arithmetic on the
    /// current offset — two sources for one number, which is the drift this
    /// module's offset-from-the-tail choice exists to avoid. The follow ruling
    /// is `scroll_down`'s, unchanged: at the tail the view is pinned again,
    /// because a bottomed-out view that is not following is a state nobody
    /// asked for and nobody can see.
    pub fn scroll_to(&mut self, offset: usize) {
        self.scroll = offset.min(self.max_scroll());
        self.follow = self.scroll == 0;
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

    /// Total rendered rows — the number the scroll bounds are computed from,
    /// handed out so the scrollbar can be drawn from the same one.
    ///
    /// [`Self::height`] answers the same question by materialising every row;
    /// this is the running count the buffer already keeps, which is what a
    /// widget repainted on every frame can afford to ask.
    pub fn total_rows(&self) -> usize {
        self.total()
    }
}

// region: The scrollbar
// ---------------------------------------------------------------------------
// The scrollbar
//
// Geometry only, and a pure function of the three numbers the pane already
// has: how much there is, how much shows, and how far up the view is. It
// lives beside the scroll state rather than in the painter because it is the
// *same* state read a second way — a bar computed from an offset of its own
// is the second scroll position this module exists to not have.
// ---------------------------------------------------------------------------

/// The shortest thumb the bar will draw, in rows.
///
/// Proportion alone gives a one-row thumb on anything long, and a one-row
/// thumb is two defects at once: the hardest target a pointer can be asked to
/// hit, and the most coarsely quantised, because every row of track it gives
/// back to the travel is another jump the view makes per cell of drag. Every
/// native scrollbar carries a floor for the first reason. Two rows is the
/// smallest one that is still visibly a thumb and not a tick.
///
/// It costs a row of travel, which is a row of resolution the drag no longer
/// has. On a long transcript the drag was already quantised in the thousands
/// of rows per cell, so the row buys more than it spends.
pub const THUMB_MIN: u16 = 2;

/// Where the thumb sits in a track `viewport` rows tall, as `(top, len)` rows
/// from the top of the pane, or `None` when everything fits.
///
/// `offset` is [`Transcript::scroll_offset`] — rows scrolled up *from the
/// tail* — so a following view puts the thumb at the bottom of the track and
/// Home puts it at the top. The inversion is the whole subtlety: a bar drawn
/// as if the offset counted from the top runs backwards, and runs backwards
/// convincingly enough that only a test asking where the thumb is at both
/// ends catches it.
pub fn thumb(content: usize, viewport: u16, offset: usize) -> Option<(u16, u16)> {
    let track = usize::from(viewport);
    if track == 0 || content <= track {
        return None;
    }
    // Ceiling first, so a proportional thumb on a fifty-thousand-row buffer
    // is a row rather than none: a scrollbar with no thumb on it has stopped
    // reporting the position it exists to report.
    //
    // Then [`THUMB_MIN`], which is about the pointer rather than the drawing.
    // The `.min(track)` after it is what keeps the floor inside a track
    // shorter than the floor, and it has to come last: `clamp(THUMB_MIN,
    // track)` panics outright when the track is one row tall.
    let len = (track * track)
        .div_ceil(content)
        .max(usize::from(THUMB_MIN))
        .min(track);
    // The first visible content row, by the same clamp [`Transcript::window`]
    // applies: the offset alone over-reports at the top of the buffer, and a
    // thumb that leaves the track is that over-report made visible.
    let end = content.saturating_sub(offset).max(track.min(content));
    let first = end.saturating_sub(track);
    let span = content - track;
    let travel = track - len;
    let top = if span == 0 || travel == 0 {
        0
    } else {
        // Rounded, not truncated, so the two ends are exact: the thumb
        // reaches the last row of the track at the tail rather than stopping
        // one short of it.
        (first * travel + span / 2) / span
    };
    Some((top as u16, len as u16))
}

/// What a press in the scrollbar's column landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// The thumb, this many rows below its top. Held for the whole drag so
    /// the thumb stays under the finger that picked it up.
    Thumb(u16),
    /// The track above the thumb: a page back.
    PageUp,
    /// The track below it: a page on.
    PageDown,
}

/// What a press `row` rows down the track asks for, or `None` when there is
/// no bar to press — the same question [`thumb`] answers for the painter,
/// asked from the pointer's side.
///
/// ⚠ HIT-TESTED AGAINST THE DRAWN THUMB, not against a second position
/// computed from the offset. The two would agree until the rounding in
/// [`thumb`] changed, and then a reader would be pressing a thumb the code
/// believes is a row away — the class of defect nobody reports because it
/// only shows up at one scroll position in twenty.
pub fn grab(content: usize, viewport: u16, offset: usize, row: u16) -> Option<Grab> {
    let (top, len) = thumb(content, viewport, offset)?;
    if row >= viewport {
        return None;
    }
    if row < top {
        Some(Grab::PageUp)
    } else if row < top + len {
        Some(Grab::Thumb(row - top))
    } else {
        Some(Grab::PageDown)
    }
}

/// The offset a drag to `row` asks for, anchored at the press: `press` is the
/// scroll offset the view had and the track row the button came down on.
///
/// ⚠ ANCHORED, NOT ABSOLUTE. Mapping the pointer's row straight onto the
/// track's grid looks equivalent and is not: the offset a wheel or a page
/// leaves behind sits *between* two rows of that grid, so the first drag
/// event snaps it onto the nearest one and the view lurches before the
/// pointer has moved at all. On a long transcript one row of track is
/// hundreds of rows of transcript, which is how far it lurches. Anchoring
/// makes a motionless pointer arithmetically incapable of moving the view: at
/// `row == press.1` the delta is zero and the answer is the offset it came in
/// with.
///
/// It is still the inverse of [`thumb`] to within the rounding: from an
/// on-grid press the two agree at every row of the track, which is what
/// `every_row_of_the_track_round_trips_through_the_thumb` walks. Both ends
/// are clamped, because a pointer dragged off the pane is an ordinary drag
/// and refusing it would strand the thumb mid-track.
pub fn drag_offset(content: usize, viewport: u16, press: (usize, u16), row: u16) -> usize {
    let track = usize::from(viewport);
    let Some((_, len)) = thumb(content, viewport, 0) else {
        return 0;
    };
    let travel = track - usize::from(len);
    if travel == 0 {
        // A thumb that fills its track has one position, and the tail is it.
        return 0;
    }
    let span = content - track;
    let (offset, press_row) = press;
    // The press offset can be past the span: `max_scroll` bounds the offset at
    // `total - 1` while the view stops at `total - height`, so an offset above
    // the top of the buffer is reachable and means the same view as the top.
    let offset = offset.min(span) as i128;
    let dy = i128::from(row) - i128::from(press_row);
    let span_i = span as i128;
    let travel_i = travel as i128;
    // Rounded away from zero on the half, symmetrically, so a drag up and the
    // drag back down it undoes are the same number of rows.
    let delta = (dy * span_i + dy.signum() * travel_i / 2) / travel_i;
    // Down the track is towards the tail, and the offset counts up from it.
    (offset - delta).clamp(0, span_i) as usize
}

/// The deepest offset that shows a different view in a pane `viewport` rows
/// tall. Everything above the top of the buffer looks the same.
///
/// [`Transcript::scroll_up`] stops at `total - 1` because that is the bound
/// the indicator's arithmetic wants, and [`Transcript::window`] then clamps
/// the view a whole pane-height short of it. The gap is dead travel: offset
/// that has been spent and buys no movement, so a page back down spends
/// itself undoing it and the reader clicks a bar that does nothing. Whoever
/// has a height to hand is who can refuse it, and the bar has one.
pub fn max_offset(content: usize, viewport: u16) -> usize {
    content.saturating_sub(usize::from(viewport))
}

// endregion: The scrollbar

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
    // The tagged rows
    // -----------------------------------------------------------------------

    /// The tags are the same rows annotated, and the annotation is true: first
    /// rows are marked, separators belong to nobody, and the voice is the
    /// entry's kind. The chat pane's gutter stands entirely on this.
    #[test]
    fn tagged_rows_are_the_same_rows_with_the_speaker_attached() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        t.push(EntryKind::User("fix it".into()), &skin, 40);
        t.stream("done, and here is why at some length", &skin, 40);
        t.finish();
        t.push(note("a note"), &skin, 40);

        let tagged = t.tagged(&skin);
        assert_eq!(
            tagged.iter().map(|r| plain(&r.line)).collect::<Vec<_>>(),
            t.rows(&skin).iter().map(plain).collect::<Vec<_>>(),
            "the tagged assembly and rows() disagree"
        );
        // The separators are nobody's.
        for row in tagged.iter().filter(|r| plain(&r.line).is_empty()) {
            assert_eq!(row.block, None, "a separator was given to a block");
        }
        // Each block's first row is flagged once, with its own voice.
        let firsts: Vec<(usize, Voice)> = tagged
            .iter()
            .filter_map(|r| r.block)
            .filter(|(_, _, first)| *first)
            .map(|(i, v, _)| (i, v))
            .collect();
        assert_eq!(
            firsts,
            [(0, Voice::You), (1, Voice::Emma), (2, Voice::Note)],
            "{firsts:?}"
        );
        // And the windowed form is the same window `visible` shows.
        assert_eq!(
            t.visible_tagged(&skin, 3)
                .iter()
                .map(|r| plain(&r.line))
                .collect::<Vec<_>>(),
            t.visible(&skin, 3).iter().map(plain).collect::<Vec<_>>()
        );
    }

    // -----------------------------------------------------------------------
    // The claim the plan made and did not measure
    // -----------------------------------------------------------------------
    /// **A ratio, not a stopwatch, and that is the point.** The guarantee is
    /// about the algorithm: re-wrapping a full buffer at a new width costs no
    /// more than wrapping it did in the first place, so the eager design in
    /// this module stays far below a frame. Stated as a wall-clock ceiling it
    /// was a claim about the machine instead, and it went red on a shared CI
    /// runner in a debug build while the algorithm was exactly as fast as it
    /// had ever been. A red that means "the runner was busy" teaches people to
    /// ignore red.
    ///
    /// Both halves are measured on the same machine in the same run, so the
    /// comparison survives any hardware. It fails the moment a rewrap becomes
    /// asymptotically worse than the original wrap, which is the regression
    /// worth catching.
    #[test]
    fn a_full_buffer_rewraps_for_no_more_than_it_cost_to_wrap() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        // A realistic mix: prose with a code block, a tool line, a goal.
        let build = std::time::Instant::now();
        for i in 0..500 {
            t.push(EntryKind::User(format!("goal {i}")), &skin, 120);
            t.push(
                EntryKind::Assistant {
                    source: format!(
                        "Here is what {i} does, at some length so that the wrap has real                          work to do on a narrow window.

- a bullet
- another

```rust
                         fn main() {{ println!(\"{i}\"); }}
```
"
                    ),
                    done: true,
                },
                &skin,
                120,
            );
        }
        let wrapped = build.elapsed();
        let held = t.entries.len();
        let start = std::time::Instant::now();
        t.set_width(&skin, 80);
        let elapsed = start.elapsed();
        // Three times, not once. The two measurements are the same work and
        // land within a few percent of each other on a quiet machine, so
        // `elapsed <= wrapped` would be a coin toss under load. A rewrap that
        // has gone quadratic in the entry count is slower by a factor of
        // hundreds at this size, which no amount of scheduler noise reaches.
        let budget = wrapped * 3;
        assert!(
            elapsed <= budget,
            "re-wrapping {held} entries ({} rows) took {elapsed:?}, more than three times the              {wrapped:?} it cost to wrap them once. The eager design in this module assumes a              rewrap is the same work again, so this is an asymptotic regression rather than a              slow machine",
            t.rows
        );
        // Printed so both numbers are on the record even when it passes.
        println!(
            "rewrap: {held} entries, {} rows, {elapsed:?} against {wrapped:?} to build",
            t.rows
        );
    }

    // -----------------------------------------------------------------------
    // The scrollbar's geometry
    // -----------------------------------------------------------------------

    /// A pane that holds everything has nothing to say about position, and a
    /// bar drawn over a transcript nobody can scroll is furniture claiming a
    /// state that does not exist.
    #[test]
    fn the_thumb_is_absent_until_the_content_overflows_the_pane() {
        assert_eq!(thumb(0, 10, 0), None);
        assert_eq!(thumb(9, 10, 0), None);
        assert_eq!(thumb(10, 10, 0), None);
        assert!(thumb(11, 10, 0).is_some());
        // A pane with no rows cannot hold a thumb whatever the content.
        assert_eq!(thumb(100, 0, 0), None);
    }

    /// The offset is counted from the tail, so the *following* view is the
    /// bottom of the track and Home is the top — the mapping a reader checks
    /// by looking, and the one an offset-from-the-top implementation gets
    /// backwards.
    #[test]
    fn the_thumb_is_at_the_bottom_while_following_and_at_the_top_at_home() {
        let (top, len) = thumb(100, 10, 0).expect("overflowing");
        assert_eq!(top + len, 10, "following, the thumb ends at the last row");
        // `to_top` parks the offset at `max_scroll` — one row short of the
        // total — and that view is the first `height` rows.
        let (top, _) = thumb(100, 10, 99).expect("overflowing");
        assert_eq!(top, 0, "at the top of the buffer the thumb is at the top");
    }

    /// Proportional, and never nothing: a thumb rounded to zero rows on a
    /// long transcript is a scrollbar with no position on it.
    #[test]
    fn the_thumb_is_proportional_and_never_shorter_than_the_floor() {
        let (_, len) = thumb(40, 20, 0).unwrap();
        assert_eq!(len, 10, "half the content shown is half the track");
        let (_, len) = thumb(100_000, 20, 0).unwrap();
        assert_eq!(len, THUMB_MIN, "a huge transcript keeps a grabbable thumb");
        // And the thumb never leaves the track, at any offset.
        for offset in 0..200 {
            let (top, len) = thumb(200, 12, offset).unwrap();
            assert!(
                top + len <= 12,
                "offset {offset}: {top}+{len} past the track"
            );
            assert!(len >= THUMB_MIN);
        }
        // A track shorter than the floor is still all thumb rather than a
        // length that overruns it.
        let (top, len) = thumb(50, 1, 0).unwrap();
        assert_eq!((top, len), (0, 1), "the floor overran a one-row track");
    }

    /// The floor is about the pointer, not about the drawing: a one-row thumb
    /// on a long transcript is the hardest possible target and the most
    /// coarsely quantised one, which is what the reader feels as fidget.
    #[test]
    fn a_long_transcript_still_gets_a_thumb_worth_grabbing() {
        for content in [500usize, 5_000, 100_000, 10_000_000] {
            for viewport in [8u16, 20, 40, 60] {
                let (top, len) = thumb(content, viewport, 0).unwrap();
                assert!(
                    len >= THUMB_MIN.min(viewport),
                    "content {content} track {viewport}: {len}-row thumb"
                );
                assert!(top + len <= viewport);
            }
        }
        // Proportional alone is one row here; the floor lifts it to two.
        let (_, len) = thumb(100_000, 20, 0).unwrap();
        assert_eq!(len, 2, "the floor must beat proportional rounding");
    }

    /// Dragging down never scrolls up. The sweep is the test the round trip
    /// cannot be: rounding that agrees at both ends can still reverse across
    /// one cell in the middle, and a view that goes backwards under a forward
    /// finger is the fidget by another name.
    #[test]
    fn a_drag_down_the_track_never_scrolls_the_view_backwards() {
        for (content, viewport) in [
            (12usize, 10u16),
            (25, 20),
            (31, 20),
            (200, 12),
            (1000, 10),
            (100_000, 20),
            (1_000_000, 40),
        ] {
            let (_, len) = thumb(content, viewport, 0).unwrap();
            for held in 0..len {
                let mut prev: Option<usize> = None;
                for row in 0..viewport {
                    // Pressed with the thumb at the top of the track, which
                    // is the anchor the grid inverse is written around.
                    let press = (content - usize::from(viewport), held);
                    let offset = drag_offset(content, viewport, press, row);
                    if let Some(prev) = prev {
                        assert!(
                            offset <= prev,
                            "content {content} track {viewport} held {held}: \
                             row {row} scrolled back from {prev} to {offset}"
                        );
                    }
                    prev = Some(offset);
                }
            }
        }
    }

    /// A drag event at the row the button came down on leaves the offset
    /// exactly where it was — the property the absolute mapping violates on
    /// the first event when the view sits between two grid rows.
    #[test]
    fn a_motionless_pointer_does_not_move_the_view_on_the_first_drag_event() {
        for (content, viewport) in [(1000usize, 10u16), (100_000, 20), (1_000_000, 40)] {
            for offset in [0usize, 17, 333, content - usize::from(viewport)] {
                let Some((top, len)) = thumb(content, viewport, offset) else {
                    continue;
                };
                for held in 0..len {
                    let press_row = top + held;
                    let press = (offset, press_row);
                    assert_eq!(
                        drag_offset(content, viewport, press, press_row),
                        offset.min(content - usize::from(viewport)),
                        "content {content} track {viewport} offset {offset}: \
                         motionless at row {press_row} moved the view"
                    );
                }
            }
        }
    }

    /// The bar reads one state, not its own. Three wheel notches and one
    /// nine-row page land the same offset, so they land the same thumb —
    /// which is what "tracks the existing scroll state" means when written
    /// as a test rather than as a comment.
    #[test]
    fn a_wheel_notch_and_a_page_move_the_same_scroll_state() {
        let skin = skin();
        let mut wheel = Transcript::new(Cap::default());
        for i in 0..60 {
            wheel.push(note(&format!("line {i}")), &skin, 40);
        }
        let mut page = wheel.clone();
        for _ in 0..3 {
            wheel.scroll_up(3);
        }
        page.scroll_up(9);
        assert_eq!(wheel.scroll_offset(), page.scroll_offset());
        assert_eq!(
            thumb(wheel.total_rows(), 10, wheel.scroll_offset()),
            thumb(page.total_rows(), 10, page.scroll_offset())
        );
        // And the count the bar is drawn from is the count the scroll bounds
        // are drawn from: one number, not a second measurement.
        assert_eq!(wheel.total_rows(), wheel.height(&skin));
    }

    // -----------------------------------------------------------------------
    // The scrollbar as a control
    // -----------------------------------------------------------------------

    /// The three answers a press in the bar's column can have, and the fact
    /// that decides between them is where the thumb was drawn — not where the
    /// view is, which is the same number read a different way and would put
    /// the hit-test one rounding apart from the paint.
    #[test]
    fn a_press_hits_the_thumb_it_can_see_and_the_track_either_side_of_it() {
        // Following: the thumb is at the bottom of a ten-row track.
        let (top, len) = thumb(100, 10, 0).unwrap();
        assert_eq!((top, len), (8, 2), "the floor is two rows of thumb");
        assert_eq!(grab(100, 10, 0, 8), Some(Grab::Thumb(0)));
        assert_eq!(grab(100, 10, 0, 9), Some(Grab::Thumb(1)));
        assert_eq!(grab(100, 10, 0, 0), Some(Grab::PageUp));
        assert_eq!(grab(100, 10, 0, 7), Some(Grab::PageUp));
        // At the top of the buffer the track below the thumb is a page on.
        assert_eq!(grab(100, 10, 99, 0), Some(Grab::Thumb(0)));
        assert_eq!(grab(100, 10, 99, 5), Some(Grab::PageDown));
        // A press on a bar that was never drawn is not a press on anything.
        assert_eq!(grab(10, 10, 0, 0), None);
        // Nor is one past the end of the track.
        assert_eq!(grab(100, 10, 0, 10), None);
    }

    /// A press two rows into the thumb keeps the thumb two rows under the
    /// pointer for the rest of the drag. Without the held rows the thumb
    /// jumps its own length the instant the button goes down, which is the
    /// defect every scrollbar that feels wrong has.
    #[test]
    fn the_thumb_stays_where_it_was_grabbed() {
        // Forty rows in a twenty-row pane: a ten-row thumb, top at 10.
        let (top, len) = thumb(40, 20, 0).unwrap();
        assert_eq!((top, len), (10, 10));
        assert_eq!(grab(40, 20, 0, 12), Some(Grab::Thumb(2)));
        // Dragged to row 5 still holding row 2 of the thumb: top 3.
        let offset = drag_offset(40, 20, (0, 12), 5);
        assert_eq!(thumb(40, 20, offset).unwrap().0, 3);
    }

    /// Both ends are exact, because the ends are the two positions a reader
    /// aims for: the top of the buffer and the tail.
    #[test]
    fn a_drag_to_either_end_of_the_track_reaches_that_end() {
        // Up to the first row: the topmost view there is, and the thumb rides
        // the top of the track with it.
        let (grabbed, _) = thumb(100, 10, 0).unwrap();
        let top_of_buffer = drag_offset(100, 10, (0, grabbed), 0);
        assert_eq!(
            top_of_buffer, 90,
            "the first row of the buffer is on screen"
        );
        assert_eq!(thumb(100, 10, top_of_buffer).unwrap().0, 0);
        assert_eq!(drag_offset(100, 10, (0, grabbed), 9), 0);
        assert_eq!(drag_offset(100, 10, (0, grabbed), 200), 0);
    }

    /// The offset a drag lands on draws the thumb the drag asked for. The two
    /// functions round in opposite directions and a test that only checks one
    /// end cannot see the drift; this walks every row of several tracks.
    #[test]
    fn every_row_of_the_track_round_trips_through_the_thumb() {
        // `(25, 20)` and `(31, 20)` are not decoration: at those sizes a
        // `drag_offset` that truncated where [`thumb`] rounds lands a row out,
        // and the round numbers either side of them do not notice.
        for (content, viewport) in [
            (12, 10),
            (25, 20),
            (31, 20),
            (40, 20),
            (100, 10),
            (1000, 10),
            (100_000, 20),
        ] {
            let (_, len) = thumb(content, viewport, 0).unwrap();
            let travel = viewport - len;
            for row in 0..viewport {
                let want = row.min(travel);
                let offset =
                    drag_offset(content, viewport, (content - usize::from(viewport), 0), row);
                let got = thumb(content, viewport, offset).unwrap().0;
                assert_eq!(
                    got, want,
                    "content {content} track {viewport}: row {row} asked for {want}, drew {got}"
                );
            }
        }
    }

    /// A drag sets the offset outright, so it has to make the same follow
    /// ruling the keys make: at the tail the view is pinned again, anywhere
    /// else it is not. A drag to the bottom that left the latch off would be
    /// a view that looks like it is following and silently is not.
    #[test]
    fn a_drag_to_the_tail_re_latches_the_follow() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        for i in 0..60 {
            t.push(note(&format!("line {i}")), &skin, 40);
        }
        t.scroll_to(20);
        assert_eq!(t.scroll_offset(), 20);
        assert!(!t.is_following());
        t.scroll_to(0);
        assert!(t.is_following(), "the tail is the following view");
        // And the bound is the keys' bound: nothing may scroll past it.
        t.scroll_to(usize::MAX);
        assert_eq!(t.scroll_offset(), t.total_rows() - 1);
    }
}
