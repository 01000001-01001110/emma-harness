//! The conversation pane: the two-column transcript, painted into a buffer.
//!
//! The mockup's shape is a narrow left gutter carrying the speaker label and a
//! wide right column carrying the message, so every message shares one left
//! edge. This module paints that shape and nothing else: it takes a
//! [`Transcript`] — which already owns the scroll offset, the follow latch and
//! the wrapped rows — and puts the visible window on screen. It holds no
//! state, opens no terminal, and decides nothing about scrolling.
//!
//! # Where everything that is not chat lives
//!
//! The mockup shows a conversation Emma never has: pure prose, two voices. A
//! real transcript is mostly tool traffic, and every kind of it needs a home
//! in the two-column layout. The decisions, each with its argument:
//!
//! - **User and assistant blocks get the gutter labels** — `You` in bold
//!   reset, `Emma` in bold accent, per the mockup. The label sits beside the
//!   block's first *visible* row, not its first row: a long answer scrolled
//!   halfway off the top still says who is speaking, where a label pinned to a
//!   row nobody can see says it to nobody.
//! - **Tool traffic — calls, results, failures, diffs — gets an empty gutter**
//!   and the full message column. A tool line is machinery inside Emma's turn,
//!   not speech, and an `Emma` label beside `✗ Bash failed` would claim the
//!   failure as prose. The `◆ ✓ ✗ ⊘ ⊗` vocabulary from [`Skin`] already says
//!   what each line is, at every fidelity, so the gutter adds nothing but a
//!   false speaker. Diff hunks carry their own `+`/`-` gutter as *content* —
//!   `super::diff` argues that a diff is a report, not the file — and the
//!   speaker gutter stays out of their way.
//! - **Notes, warnings, endings, the welcome, answered prompts**: empty
//!   gutter, same reasoning. The run summary is the harness talking about
//!   itself, and the transcript's blank-row spacing already separates it from
//!   the answer above it.
//! - **Streamed prose** is just the tail assistant block; the transcript
//!   re-renders it whole on every delta, and this pane repaints whatever it
//!   became. Nothing here knows a stream from a finished answer, which is why
//!   nothing here can get one wrong.
//! - **The approval panel is not here at all.** It is fixed chrome — the
//!   view's job, per the design's §4.6 — precisely so no amount of transcript
//!   can move it. Its record (the answered line) arrives as a `Note`.
//! - **The cap marker** is content, tagged as nobody's block, and scrolls
//!   with the oldest surviving row — where somebody looking for what is
//!   missing will actually be.
//!
//! # Wrapping is not done here
//!
//! The transcript wraps its entries once, at the width the caller keeps it
//! set to — [`message_width`] of the pane, so the caller and this painter
//! agree on where the column ends. Re-wrapping at paint time would mean two
//! renderers of the same text (the drift `markdown.rs` exists to prevent),
//! and it would re-decide the one rule that must not be re-decided: a fenced
//! code line is split at the column, never at a word, because wrapping code
//! changes what it says. This pane transmits rows verbatim; the clamp below
//! only guarantees that a row can never *paint* past the pane, even when the
//! transcript was wrapped for some other width mid-resize.
//!
//! # The scroll indicator
//!
//! When the reader has scrolled up, rows are arriving below the view and the
//! screen must say so — the mockup has no such affordance because its author
//! never scrolled. It is drawn in the mockup's own language: one dim
//! right-aligned overlay on the pane's last row, `↓ N rows below · End`,
//! naming the count ([`Transcript::behind`], the one number that does not
//! over-report at the top of a short buffer) and the key that resumes
//! following. Dim text, not colour-coded: the signal is the words, so it
//! survives every palette level.
//!
//! # What a copy contains, and degradation
//!
//! `markdown.rs` refuses to insert anything into text a reader copies. The
//! gutter is in tension with that rule, and the tension is resolved by what
//! the alternate screen already cost: native selection over a cell grid
//! interleaves *every* column — sidebar, borders, gutter — so per-line purity
//! is gone before this module draws anything, and `/export` is the honest
//! copy path. Given that, the gutter deliberately contains only spaces and,
//! on a block's first visible row, the speaker's name: no rail, no vertical
//! bar, no glyph. The worst a selected line gains is leading whitespace and
//! `You` or `Emma` — the shape of a chat log a person would paste on purpose.
//!
//! At sixteen colours and in ASCII the speakers stay distinct because the
//! labels are *words*, in bold — colour is decoration on top, never the
//! distinction, which is `render.rs`'s standing rule. When the pane is too
//! narrow to afford the gutter it collapses entirely and the message column
//! takes every cell; the speaker distinction then rides on what [`Skin`]
//! already draws — the goal glyph in front of every user line — rather than
//! on a label there is no room for.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::palette::Role;
use super::render::{cols, Skin, ASCII};
use super::transcript::{Transcript, Voice};

// region: The columns
// ---------------------------------------------------------------------------
// The columns
//
// One function decides the split and both sides consult it: the caller wraps
// the transcript at `message_width`, the painter draws the gutter at
// `gutter`. Two numbers derived separately is how a column ends up one cell
// wide of where the text was wrapped for.
// ---------------------------------------------------------------------------

/// The speaker column: `Emma` plus breathing room, the widest label first.
pub const GUTTER: u16 = 7;

/// The message column is never squeezed below this to pay for the gutter.
///
/// 61 puts the collapse where the design's §9 puts it — a 70-column window
/// minus its border and the gutter — but stated in the pane's own
/// coordinates, because this module sees an area, not a window.
const MIN_MESSAGE: u16 = 61;

/// The gutter this pane will draw at this width. Zero when the pane cannot
/// afford one.
pub fn gutter(width: u16) -> u16 {
    if width >= GUTTER + MIN_MESSAGE {
        GUTTER
    } else {
        0
    }
}

/// The width the transcript should be wrapped at for a pane this wide — what
/// the caller passes to [`Transcript::set_width`] before painting.
pub fn message_width(width: u16) -> u16 {
    width.saturating_sub(gutter(width)).max(1)
}

// endregion: The columns

// region: Painting
// ---------------------------------------------------------------------------

/// Paint the visible window of the transcript into `area`.
///
/// Pure: the transcript already holds the scroll offset and the follow latch,
/// and [`Transcript::visible_tagged`] already handled the window arithmetic —
/// including a block half off the top or bottom, which arrives here as
/// exactly the rows of it that show, in order. This function places rows,
/// labels first visible rows, and overlays the indicator; it decides nothing.
pub fn render(area: Rect, buf: &mut Buffer, t: &Transcript, skin: &Skin) {
    // A caller handing an area that leaks past the buffer would turn every
    // set below into a panic; painting the intersection is the whole cure.
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let gutter = gutter(area.width);

    let mut labelled: Option<usize> = None;
    for (i, row) in t.visible_tagged(skin, area.height).iter().enumerate() {
        let y = area.y + i as u16;
        if let Some((block, voice, _)) = row.block {
            // The first visible row of a speaker block carries the label —
            // which is the block's own first row when it is on screen, and
            // the pane's top row when the block is cut at the top. One rule
            // for both, so the cut case cannot be the untested one.
            if gutter > 0 && labelled != Some(block) {
                if let Some((text, style)) = label(voice, skin) {
                    buf.set_stringn(area.x, y, text, usize::from(gutter - 1), style);
                }
            }
            labelled = Some(block);
        }
        // The clamp, not the wrap: rows were wrapped by the transcript, and
        // whatever width that was, nothing may paint past this pane.
        buf.set_line(area.x + gutter, y, &row.line, area.width - gutter);
    }

    let behind = t.behind(area.height);
    if behind > 0 {
        indicator(area, buf, behind, skin);
    }
}

/// The gutter label for a voice, or none — the decision the module doc
/// argues. Only the two speakers get one.
fn label(voice: Voice, skin: &Skin) -> Option<(&'static str, Style)> {
    match voice {
        // Bold reset, not a hex: the mockup's brighter white is `Role::Text`
        // plus weight, which is brighter than the surrounding dim on every
        // theme — `palette.rs` carries that argument.
        Voice::You => Some(("You", skin.palette.bold(Role::Text))),
        Voice::Emma => Some(("Emma", skin.palette.bold(Role::Accent))),
        Voice::Activity | Voice::Note => None,
    }
}

/// The `↓ N rows below · End` overlay on the pane's last row.
fn indicator(area: Rect, buf: &mut Buffer, behind: usize, skin: &Skin) {
    // `Glyphs` has no arrow yet, and growing it belongs to the file that owns
    // the glyph sets — a seam to replace when it does. Until then the set
    // itself says which console this is; the two consts are `PartialEq` for
    // exactly this kind of question.
    let arrow = if skin.glyphs == ASCII { "v" } else { "↓" };
    let text = format!(" {arrow} {behind} rows below {} End ", skin.glyphs.sep);
    let width = cols(&text).min(usize::from(area.width));
    let x = area.x + area.width - width as u16;
    let y = area.y + area.height - 1;
    buf.set_stringn(x, y, &text, width, skin.palette.dim());
}

// endregion: Painting

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::{Skin, ASCII, UNICODE};
    use crate::term::transcript::{Cap, EntryKind, Transcript};
    use ratatui::text::Line;

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    /// A transcript wrapped the way a caller keeps one: at the message width
    /// of the pane it will be painted into.
    fn painted(t: &Transcript, skin: &Skin, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, t, skin);
        buf
    }

    /// One buffer row as text — the row-dump form every vertical assertion
    /// here uses, per the standing lesson about asserting on side properties.
    fn row(buf: &Buffer, y: u16) -> String {
        (buf.area.x..buf.area.right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    fn note(s: &str) -> EntryKind {
        EntryKind::Note(vec![Line::from(ratatui::text::Span::raw(s.to_string()))])
    }

    /// The mockup's fixture: a user line, an answer.
    fn conversation(skin: &Skin, width: u16) -> Transcript {
        let mut t = Transcript::new(Cap::default());
        let w = message_width(width);
        t.push(EntryKind::User("fix the tests".into()), skin, w);
        t.stream("Working on it now.", skin, w);
        t.finish();
        t
    }

    // -----------------------------------------------------------------------
    // The two columns
    // -----------------------------------------------------------------------

    #[test]
    fn the_speaker_sits_in_the_gutter_and_every_message_shares_one_left_edge() {
        let skin = skin();
        let t = conversation(&skin, 80);
        let buf = painted(&t, &skin, 80, 10);
        // Row 0: the user. Label at the left, message at the column.
        assert_eq!(row(&buf, 0).trim_end(), "You    ▶ fix the tests");
        // Row 1 is the separator; row 2 the answer, on the same left edge.
        assert_eq!(row(&buf, 1).trim(), "");
        let answer = row(&buf, 2);
        assert!(answer.starts_with("Emma   "), "{answer:?}");
        assert_eq!(&answer[7..7 + "Working".len()], "Working", "{answer:?}");
    }

    #[test]
    fn the_labels_wear_the_mockups_colours() {
        let skin = skin();
        let t = conversation(&skin, 80);
        let buf = painted(&t, &skin, 80, 10);
        // `Emma` accent, `You` the user's own foreground — both bold.
        let you = buf[(0u16, 0u16)].style();
        let emma = buf[(0u16, 2u16)].style();
        assert_eq!(you.fg, Some(ratatui::style::Color::Reset));
        assert_eq!(emma.fg, Some(skin.palette.color(Role::Accent)));
        for s in [you, emma] {
            assert!(s.add_modifier.contains(ratatui::style::Modifier::BOLD));
        }
    }

    /// **The degradation guarantee.** With no colour at all, on an ASCII
    /// console, the speakers are still told apart — by words, not shading.
    #[test]
    fn speakers_are_distinguishable_without_colour() {
        let skin = Skin::new(Palette::new(Level::None), ASCII);
        let t = conversation(&skin, 80);
        let buf = painted(&t, &skin, 80, 10);
        assert!(row(&buf, 0).starts_with("You"), "{:?}", row(&buf, 0));
        assert!(row(&buf, 2).starts_with("Emma"), "{:?}", row(&buf, 2));
        // …in bold, the one emphasis a colourless terminal has.
        assert!(buf[(0u16, 0u16)]
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD));
        // And nothing painted a byte an ASCII console cannot draw.
        for y in 0..10u16 {
            assert!(row(&buf, y).is_ascii(), "{:?}", row(&buf, y));
        }
    }

    /// Tool traffic and notes are not speech: empty gutter, full column, the
    /// Skin vocabulary carries what they are.
    #[test]
    fn activity_and_notes_keep_an_empty_gutter() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        let w = message_width(80);
        t.push(EntryKind::User("go".into()), &skin, w);
        t.push(
            EntryKind::Activity(skin.tool_started("Bash", "cargo test")),
            &skin,
            w,
        );
        t.push(EntryKind::Note(skin.note("done")), &skin, w);
        let buf = painted(&t, &skin, 80, 12);
        let all: Vec<String> = (0..12).map(|y| row(&buf, y)).collect();
        let tool = all.iter().find(|r| r.contains("Bash")).unwrap();
        let note = all.iter().find(|r| r.contains("done")).unwrap();
        for r in [tool, note] {
            assert_eq!(&r[..7], "       ", "a non-speaker grew a label: {r:?}");
            assert!(!r[7..].trim().is_empty(), "{r:?}");
        }
    }

    // -----------------------------------------------------------------------
    // The window
    // -----------------------------------------------------------------------

    /// Every visible row is painted verbatim, in order, at the column — the
    /// pane transmits the transcript, it does not re-render it. This is also
    /// the partial-visibility guarantee: `visible_tagged` hands over exactly
    /// the rows of a half-shown block, and this proves they land unshifted.
    #[test]
    fn every_visible_row_is_painted_verbatim_in_order() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        let w = message_width(80);
        t.push(EntryKind::User("a goal".into()), &skin, w);
        t.stream(
            "Prose that wraps across several rows at this width, followed by \
             code:\n```rust\nfn main() { body(); }\n```\nand a tail.",
            &skin,
            w,
        );
        t.finish();
        for (height, scroll) in [(4u16, 0usize), (4, 3), (20, 0), (5, 100)] {
            let mut t = t.clone();
            t.scroll_up(scroll);
            let buf = painted(&t, &skin, 80, height);
            let expected: Vec<String> = t
                .visible(&skin, height)
                .iter()
                .map(|l| crate::term::render::plain(l))
                .collect();
            // Scrolled, the last row's tail is the indicator's — by design —
            // so verbatim is asserted on everything above it; the indicator
            // tests own that row.
            let check = if t.behind(height) > 0 {
                expected.len().saturating_sub(1)
            } else {
                expected.len()
            };
            for (i, want) in expected.iter().take(check).enumerate() {
                // The first seven cells are the gutter; everything after is
                // the message, verbatim. All-ASCII gutter, so the byte slice
                // is the column slice.
                let got = row(&buf, i as u16);
                assert_eq!(
                    got[usize::from(GUTTER)..].trim_end(),
                    want.trim_end(),
                    "row {i} at height {height}, scrolled {scroll}"
                );
            }
        }
    }

    /// A block cut off at the top of the pane still says who is speaking:
    /// the label rides the first *visible* row.
    #[test]
    fn a_block_cut_at_the_top_keeps_its_speaker() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        let w = message_width(80);
        let long: String = (0..30).map(|i| format!("paragraph {i}\n")).collect();
        t.stream(&long, &skin, w);
        t.finish();
        t.scroll_up(10);
        let buf = painted(&t, &skin, 80, 6);
        // The pane's top row is some middle paragraph…
        assert!(row(&buf, 0).contains("paragraph"), "{:?}", row(&buf, 0));
        // …and the gutter beside it names the speaker anyway.
        assert!(
            row(&buf, 0).starts_with("Emma"),
            "a cut block lost its speaker: {:?}",
            row(&buf, 0)
        );
        // Once labelled, the rest of the block's rows stay unlabelled.
        assert!(row(&buf, 1).starts_with("       "), "{:?}", row(&buf, 1));
    }

    /// Code crossed the pane unreflowed while prose beside it wrapped at
    /// words — the contrast that proves the pane transmitted the rule rather
    /// than re-deciding it.
    #[test]
    fn a_fenced_code_block_crosses_the_pane_unreflowed() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        let width = 40u16;
        let w = message_width(width); // no gutter this narrow: w == width
        let code = "x".repeat(80);
        t.stream(
            &format!(
                "some prose words that will wrap here because they run long\n```\n{code}\n```\n"
            ),
            &skin,
            w,
        );
        t.finish();
        let buf = painted(&t, &skin, width, 12);
        let rows: Vec<String> = (0..12).map(|y| row(&buf, y)).collect();
        let joined: String = rows
            .iter()
            .map(|r| r.trim_end())
            .collect::<Vec<_>>()
            .concat();
        assert!(
            joined.contains(&code),
            "the code was reflowed or lost: {rows:?}"
        );
        // The prose wrapped at a word — a row ends short of the column, on a
        // whole word — so the full-width code rows are a decision, not a
        // coincidence of length.
        let prose = rows
            .iter()
            .map(|r| r.trim_end())
            .find(|r| r.ends_with("wrap here"))
            .unwrap_or_else(|| panic!("the prose did not wrap at a word: {rows:?}"));
        assert!(cols(prose) < usize::from(width), "{prose:?}");
    }

    // -----------------------------------------------------------------------
    // The pane's edges
    // -----------------------------------------------------------------------

    /// **No row ever paints outside the pane**, whatever width the transcript
    /// was wrapped at — the mid-resize case, where the rows are wider than
    /// the column for one frame.
    #[test]
    fn nothing_paints_outside_the_pane() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        // Deliberately wrapped far wider than the pane it is painted into.
        t.push(note(&"wide ".repeat(40)), &skin, 200);
        t.push(
            EntryKind::User("also wide ".repeat(20).trim().into()),
            &skin,
            200,
        );

        let outer = Rect::new(0, 0, 60, 12);
        let pane = Rect::new(5, 2, 30, 6);
        let mut buf = Buffer::empty(outer);
        render(pane, &mut buf, &t, &skin);
        for y in outer.y..outer.bottom() {
            for x in outer.x..outer.right() {
                if !pane.contains(ratatui::layout::Position::new(x, y)) {
                    assert_eq!(
                        buf[(x, y)].symbol(),
                        " ",
                        "({x},{y}) outside the pane was painted"
                    );
                }
            }
        }
        // And an area that leaks past the buffer is clipped, not a panic.
        render(Rect::new(50, 8, 30, 30), &mut buf, &t, &skin);
    }

    #[test]
    fn a_narrow_pane_gives_every_column_to_the_message() {
        let skin = skin();
        assert_eq!(gutter(80), GUTTER);
        assert_eq!(gutter(40), 0);
        assert_eq!(message_width(40), 40);
        let t = conversation(&skin, 40);
        let buf = painted(&t, &skin, 40, 10);
        // No label column: the goal glyph — Skin's own vocabulary — is what
        // still separates the speakers, at column zero.
        assert!(row(&buf, 0).starts_with("▶ fix"), "{:?}", row(&buf, 0));
        assert!(
            !(0..10).any(|y| row(&buf, y).contains("Emma")),
            "a label was drawn with no gutter to hold it"
        );
    }

    // -----------------------------------------------------------------------
    // The indicator
    // -----------------------------------------------------------------------

    /// Scrolled up, the pane names how much is below and the key back; at the
    /// tail it says nothing — an affordance for a state, not a decoration.
    #[test]
    fn the_indicator_appears_only_behind_the_tail_and_names_the_count() {
        let skin = skin();
        let mut t = Transcript::new(Cap::default());
        for i in 0..30 {
            t.push(note(&format!("line {i}")), &skin, message_width(80));
        }
        let following = painted(&t, &skin, 80, 5);
        assert!(
            !row(&following, 4).contains("below"),
            "{:?}",
            row(&following, 4)
        );

        t.scroll_up(10);
        let behind = t.behind(5);
        assert!(behind > 0);
        let buf = painted(&t, &skin, 80, 5);
        let last = row(&buf, 4);
        assert!(last.contains(&format!("↓ {behind} rows below")), "{last:?}");
        assert!(last.contains("End"), "the way back is not named: {last:?}");
        assert!(last.trim_end().ends_with("End"), "{last:?}");
    }

    #[test]
    fn the_indicator_degrades_to_ascii_with_the_console() {
        let skin = Skin::new(Palette::new(Level::None), ASCII);
        let mut t = Transcript::new(Cap::default());
        for i in 0..30 {
            t.push(note(&format!("line {i}")), &skin, message_width(80));
        }
        t.scroll_up(10);
        let buf = painted(&t, &skin, 80, 5);
        let last = row(&buf, 4);
        assert!(
            last.contains("v ") && last.contains("rows below"),
            "{last:?}"
        );
        assert!(last.is_ascii(), "{last:?}");
    }
}
