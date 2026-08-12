//! The left sidebar of the full-screen frame: sessions, tools, quick help.
//!
//! Pure rendering. The shell owns the state — which sessions exist, which
//! commands are loaded, whether the sidebar is collapsed — and hands it in as
//! [`State`]; nothing here reads a file, takes a lock or asks the terminal a
//! question. That split is the same one [`super::menu`] made, and for the same
//! reason: what a pane *shows* is the part a test can hold still.
//!
//! # The two numbers that came from the mockup
//!
//! The width is 22.4% of the terminal — the measured proportion of the owner's
//! mockup (`notes/design-tui-fullscreen.md` §1.1, pixel-sampled; the circulated
//! prose said 25% and the image corrected it). It is clamped between a floor of
//! 28 columns — the narrowest at which a session name and a right-aligned
//! `Yesterday` coexist without truncation — and a ceiling of 40, so an
//! ultrawide window does not grow a half-screen menu. And it is never more than
//! half the terminal: a "side" bar wider than the pane it sits beside has the
//! two the wrong way round, so on a terminal too narrow for the floor the
//! sidebar shrinks rather than eating the transcript. The shell decides *when*
//! to collapse (including automatically below [`AUTO_COLLAPSE_COLS`], with the
//! user-latch rules in the design's §3.2); this file only answers "how wide".
//!
//! # What gives when a row does not fit
//!
//! A row is a name on the left and a trailing column — a time, a shortcut —
//! right-aligned. When both cannot fit, **the name truncates and the trailing
//! column survives**, because the trailing column is short and uniform and the
//! eye scans it as a column: one row whose date is missing reads as a row with
//! no date, while a name cut short still reads as that name. The trailing text
//! is itself capped at half the row so a pathological value cannot invert the
//! priority. Every cut is marked with the ellipsis glyph — this repository's
//! standing rule is that a cut says it was cut — and all arithmetic is in
//! display columns via [`cols`]/[`fit`], never `char`s, because a CJK trailing
//! column computed in characters ends two cells past the edge of the pane.
//!
//! # The selected row
//!
//! Marked twice, like everything in [`super::render`]: a `>` in the gutter
//! *and* a full-width band from [`Palette::chip`](super::palette::Palette::chip)
//! — the same treatment the `/` menu gives its highlighted row, which is why it
//! is reused rather than a new fg+bg pair invented. The chip sets both halves
//! of the pair so the band is legible on any theme, and degrades to reversed
//! video at [`Level::None`](super::palette::Level::None), so the selection is
//! never carried by colour alone.
//!
//! # Empty sections
//!
//! A header over nothing reads as a broken pane, so an empty section says in
//! words what would be there — the precedent the `/` menu set with
//! [`super::menu::NO_PROJECT_COMMANDS`]. The empty-state strings are ASCII on
//! purpose: they must render on the exact consoles the ASCII glyph set exists
//! for, and the menu's own em-dash note predates that observation rather than
//! licensing it.
//!
//! # What runs out before width does: height
//!
//! The three sections are drawn top to bottom, and when the pane is too short
//! the last visible row counts what was left out rather than the list quietly
//! stopping — the rule [`super::view`]'s menu overflow row follows, because a
//! list that is silently a third of itself cannot be used to answer "what is
//! there".

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use super::palette::Role;
use super::render::{cols, fit, Skin};

// region: Width
// ---------------------------------------------------------------------------
// Width
//
// Called before layout on every frame, so it is arithmetic and nothing else.
// ---------------------------------------------------------------------------

/// The narrowest useful sidebar: a session name beside `Yesterday`, untouched.
pub const MIN_WIDTH: u16 = 28;

/// The widest: past this an ultrawide window is spending columns on padding.
pub const MAX_WIDTH: u16 = 40;

/// Below this many total columns the shell should collapse the sidebar
/// automatically. The *decision* is the shell's — a user who re-expanded below
/// the threshold is honoured (design §3.2) — but the number lives beside the
/// rest of the width arithmetic so the two cannot drift apart.
pub const AUTO_COLLAPSE_COLS: u16 = 100;

/// The mockup's measured proportion, in thousandths: 22.4% of the width.
const PROPORTION_MILLIS: u32 = 224;

/// How many columns the sidebar takes out of `total`. Zero when collapsed.
pub fn width(total: u16, collapsed: bool) -> u16 {
    if collapsed {
        return 0;
    }
    let measured = ((u32::from(total) * PROPORTION_MILLIS + 500) / 1000) as u16;
    // The half-of-total cap is applied after the floor on purpose: on a
    // terminal too narrow for the floor, "half of it" is the honest maximum,
    // not a licence to take 28 of 40 columns.
    measured.clamp(MIN_WIDTH, MAX_WIDTH).min(total / 2)
}

// endregion: Width

// region: State
// ---------------------------------------------------------------------------
// State
//
// The contract shared with the shell. Plain data, owned by the caller.
// ---------------------------------------------------------------------------

/// One list row: a name, a right-aligned trailing column, and whether the
/// highlight band is on it.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub name: String,
    /// The right-aligned column: a session's `12:42` / `Yesterday`, a
    /// command's key binding. Empty is fine and costs nothing.
    pub trailing: String,
    pub selected: bool,
}

/// Everything the sidebar draws, handed in by the shell.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub sessions: Vec<Row>,
    pub commands: Vec<Row>,
    /// `(key, description)` pairs for the QUICK HELP table.
    pub help: Vec<(String, String)>,
    pub collapsed: bool,
}

// endregion: State

// region: The empty states
// ---------------------------------------------------------------------------
// The empty states
//
// ASCII only — see the module doc. Constants rather than literals so the shell
// and the tests name the same sentence.
// ---------------------------------------------------------------------------

/// A fresh session directory. Two rows, because "empty" needs both the fact
/// and the reassurance that it is not a fault.
pub const EMPTY_SESSIONS: &str = "no sessions yet";
pub const EMPTY_SESSIONS_HINT: &str = "this one appears here when it has a goal";

/// No command vocabulary at all. Points at where commands come from, the way
/// [`super::menu::NO_PROJECT_COMMANDS`] does.
pub const EMPTY_COMMANDS: &str = "no commands yet: .emma/commands/";

/// The keymap table came up empty, which means the input layer bound nothing —
/// a fact worth a sentence rather than a blank.
pub const EMPTY_HELP: &str = "no keys bound";

/// The collapse affordance on the SESSIONS header. Shows the *action* (`-`,
/// press to remove), not the mockup's `[+]` state marker — the deliberate
/// deviation argued in the design's §3.2 and flagged for owner sign-off as Q6.
pub const COLLAPSE_HINT: &str = "[-]";

// endregion: The empty states

// region: Rendering
// ---------------------------------------------------------------------------
// Rendering
//
// One bordered block, three sections, thin rules between them. Everything is
// assembled as `Line`s first so the tests can assert on widths without a
// buffer, then painted row by row.
// ---------------------------------------------------------------------------

/// Draw the sidebar into `area`. A collapsed state or an area too small to
/// hold a border draws nothing at all — the pane is absent, not clipped.
pub fn render(area: Rect, buf: &mut Buffer, s: &State, skin: &Skin) {
    if s.collapsed || area.width < 3 || area.height < 3 {
        return;
    }
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(skin.palette.dim());
    let inner = block.inner(area);
    block.render(area, buf);
    let lines = fitted(content(s, skin, inner.width), inner.height, skin);
    for (i, line) in lines.iter().enumerate() {
        buf.set_line(inner.x, inner.y + i as u16, line, inner.width);
    }
}

/// The whole sidebar as lines, unbounded by height. [`fitted`] cuts it.
fn content(s: &State, skin: &Skin, width: u16) -> Vec<Line<'static>> {
    let w = usize::from(width);
    let mut out = Vec::new();
    out.push(header("SESSIONS", Some(COLLAPSE_HINT), w, skin));
    if s.sessions.is_empty() {
        out.extend(note_rows(EMPTY_SESSIONS, w, skin));
        out.extend(note_rows(EMPTY_SESSIONS_HINT, w, skin));
    } else {
        out.extend(s.sessions.iter().map(|r| list_row(r, w, skin)));
    }
    section_break(&mut out, w, skin);
    out.push(header("TOOLS", None, w, skin));
    if s.commands.is_empty() {
        out.extend(note_rows(EMPTY_COMMANDS, w, skin));
    } else {
        out.extend(s.commands.iter().map(|r| list_row(r, w, skin)));
    }
    section_break(&mut out, w, skin);
    out.push(header("QUICK HELP", None, w, skin));
    if s.help.is_empty() {
        out.extend(note_rows(EMPTY_HELP, w, skin));
    } else {
        // One column width for every key, so the descriptions align — capped
        // at half the pane so a long key cannot push them all off the edge.
        let key_w = s
            .help
            .iter()
            .map(|(k, _)| cols(k))
            .max()
            .unwrap_or(0)
            .min(w / 2);
        out.extend(s.help.iter().map(|(k, d)| help_row(k, d, key_w, w, skin)));
    }
    out
}

/// A blank row, a thin rule inset one column, a blank row. These blank rows
/// are the sidebar's *internal* layout — the exception `welcome.rs` already
/// holds under the spacing rule, which governs the gaps between transcript
/// blocks, not the inside of one widget.
fn section_break(out: &mut Vec<Line<'static>>, w: usize, skin: &Skin) {
    out.push(Line::raw(""));
    // A pane too narrow for an inset rule gets a blank row instead: one
    // stranded glyph does not read as a rule, and the inset would overrun.
    if w >= 3 {
        out.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                skin.glyphs.rule.repeat(w.saturating_sub(2)),
                skin.palette.dim(),
            ),
        ]));
    } else {
        out.push(Line::raw(""));
    }
    out.push(Line::raw(""));
}

/// A section title, with the collapse affordance right-aligned when there is
/// one. The affordance keeps its columns and the title truncates — a header
/// that is legible but cannot be closed is worse than the reverse.
fn header(name: &str, affordance: Option<&str>, w: usize, skin: &Skin) -> Line<'static> {
    // An affordance wider than the pane is dropped whole rather than clipped:
    // half of `[-]` is not a control, it is debris.
    let aff = affordance.filter(|a| cols(a) <= w).unwrap_or("");
    let aff_w = cols(aff);
    let name_budget = w.saturating_sub(aff_w + usize::from(aff_w > 0));
    let name = clipped(name, name_budget, skin);
    let gap = w.saturating_sub(cols(&name) + aff_w);
    let mut spans = vec![Span::styled(name, skin.palette.bold(Role::Accent))];
    spans.push(Span::raw(" ".repeat(gap)));
    if aff_w > 0 {
        spans.push(Span::styled(aff.to_string(), skin.palette.dim()));
    }
    Line::from(spans)
}

/// One list row: `> name        trailing`, at exactly `w` columns.
///
/// The width arithmetic is the module doc's ruling made concrete: the trailing
/// column is capped at half the row and survives; the name takes what is left
/// and truncates with a visible mark. The gap is computed *after* both cuts,
/// from measured columns, so the trailing text ends flush against the pane's
/// right edge whatever [`fit`] undershot by — a wide glyph that would not
/// split leaves the gap one wider, never the row one over.
fn list_row(row: &Row, w: usize, skin: &Skin) -> Line<'static> {
    let avail = w.saturating_sub(2);
    let marker_w = w.min(2);
    let trailing = clipped(&row.trailing, avail / 2, skin);
    let t_w = cols(&trailing);
    let name = clipped(
        &row.name,
        avail.saturating_sub(t_w + usize::from(t_w > 0)),
        skin,
    );
    let gap = avail.saturating_sub(cols(&name) + t_w);
    // The marker is ASCII by construction, so slicing it to the pane is a
    // byte-safe way to keep a one-column pane at one column.
    let marker = &(if row.selected { "> " } else { "  " })[..marker_w];
    if row.selected {
        // The band: one style across every span, padding included, so the
        // highlight is the full row and not a patchwork. `chip` is the one
        // sanctioned fg+bg pair and already degrades to reversed video when
        // there is no colour — see the module doc.
        let band = skin.palette.chip(Role::Accent);
        return Line::from(vec![
            Span::styled(marker.to_string(), band),
            Span::styled(name, band),
            Span::styled(" ".repeat(gap), band),
            Span::styled(trailing, band),
        ]);
    }
    Line::from(vec![
        Span::raw(marker.to_string()),
        Span::styled(name, skin.palette.style(Role::Text)),
        Span::raw(" ".repeat(gap)),
        Span::styled(trailing, skin.palette.dim()),
    ])
}

/// One QUICK HELP row: the key bold in a shared column, the description dim.
/// Bold-reset rather than a hex for the key, for the reason `palette.rs` gives
/// against the mockup's brighter white: bold is brighter on every theme.
fn help_row(key: &str, desc: &str, key_w: usize, w: usize, skin: &Skin) -> Line<'static> {
    // Every fixed piece is budgeted against what is actually left, so a pane
    // narrower than the indent-plus-key-column shrinks pieces instead of
    // writing past its edge.
    let indent = 2usize.min(w);
    let kb = key_w.min(w.saturating_sub(indent));
    let key = clipped(key, kb, skin);
    let after_key = w.saturating_sub(indent + cols(&key));
    let gap = (kb.saturating_sub(cols(&key)) + 2).min(after_key);
    let desc = clipped(desc, after_key.saturating_sub(gap), skin);
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(key, skin.palette.bold(Role::Text)),
        Span::raw(" ".repeat(gap)),
        Span::styled(desc, skin.palette.dim()),
    ])
}

/// An empty-state sentence, dim, indented under its header like a row.
///
/// Wrapped, not truncated: these sentences are the whole content of an empty
/// section, and at the design's own 28-column floor the sessions hint is
/// wider than the pane — a truncated explanation is half an explanation,
/// which is the defect the sentence exists to prevent. A single word wider
/// than the pane is still clipped with a mark; there is no honest wrap for it.
fn note_rows(text: &str, w: usize, skin: &Skin) -> Vec<Line<'static>> {
    let indent = 2usize.min(w);
    let budget = w.saturating_sub(indent);
    if budget == 0 {
        return vec![Line::raw("")];
    }
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if cols(word) > budget {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            rows.push(clipped(word, budget, skin));
            continue;
        }
        let need = if current.is_empty() {
            cols(word)
        } else {
            cols(&current) + 1 + cols(word)
        };
        if need > budget {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows.into_iter()
        .map(|t| {
            Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(t, skin.palette.dim()),
            ])
        })
        .collect()
}

/// [`fit`], made total. `fit` itself can overrun a budget narrower than its
/// own ellipsis — the ASCII set's `...` is three columns — and a row that
/// overruns writes into the border. Below that, a run of dots is all the cut
/// mark there is room for; zero is zero.
fn clipped(text: &str, budget: usize, skin: &Skin) -> String {
    if cols(text) <= budget {
        return text.to_string();
    }
    let ellipsis = skin.glyphs.ellipsis;
    if budget < cols(ellipsis) {
        return ".".repeat(budget);
    }
    fit(text, budget, ellipsis)
}

/// Cut the assembled lines to the pane's height, counting the loss out loud.
fn fitted(mut lines: Vec<Line<'static>>, height: u16, skin: &Skin) -> Vec<Line<'static>> {
    let h = usize::from(height);
    if h == 0 || lines.len() <= h {
        return lines;
    }
    let kept = h - 1;
    let dropped = lines.len() - kept;
    lines.truncate(kept);
    lines.push(Line::from(Span::styled(
        format!("{} {dropped} more rows", skin.glyphs.ellipsis),
        skin.palette.dim(),
    )));
    lines
}

// endregion: Rendering

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::super::palette::{Level, Palette};
    use super::super::render::{plain, ASCII, UNICODE};
    use super::*;

    fn skin(level: Level) -> Skin {
        Skin::new(Palette::new(level), UNICODE)
    }

    fn ascii_skin() -> Skin {
        Skin::new(Palette::new(Level::None), ASCII)
    }

    fn state() -> State {
        State {
            sessions: vec![
                Row {
                    name: "product-strategy".into(),
                    trailing: "12:42".into(),
                    selected: true,
                },
                Row {
                    name: "fix the frontmatter parser".into(),
                    trailing: "Yesterday".into(),
                    selected: false,
                },
            ],
            commands: vec![
                Row {
                    name: "/help".into(),
                    trailing: String::new(),
                    selected: false,
                },
                Row {
                    name: "/resume".into(),
                    trailing: String::new(),
                    selected: false,
                },
            ],
            help: vec![
                ("Ctrl+b".into(), "toggle sidebar".into()),
                ("PgUp/PgDn".into(), "scroll".into()),
            ],
            collapsed: false,
        }
    }

    fn draw(s: &State, skin: &Skin, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, s, skin);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Width
    // -----------------------------------------------------------------------

    /// The collapse contract: zero, whatever the terminal is doing. The shell
    /// lays the main pane out from this number, so a stray column here is a
    /// stray column on every frame.
    #[test]
    fn a_collapsed_sidebar_takes_no_columns_at_any_size() {
        for total in [0, 10, MIN_WIDTH, 80, AUTO_COLLAPSE_COLS, 200, u16::MAX] {
            assert_eq!(width(total, true), 0, "at {total} columns");
        }
    }

    /// The measured 22.4%, floored and ceilinged. The literals are pinned
    /// because "clamp(round(0.224·cols), 28, 40)" is an argument in a comment
    /// until a number is written down.
    #[test]
    fn the_width_is_the_mockups_proportion_between_a_floor_and_a_ceiling() {
        // 120 × 0.224 = 26.9 — under the floor, so the floor.
        assert_eq!(width(120, false), 28);
        // In range, the proportion itself.
        assert_eq!(width(160, false), 36);
        // 200 × 0.224 = 44.8 — over the ceiling, so the ceiling.
        assert_eq!(width(200, false), 40);
        assert_eq!(width(400, false), 40);
    }

    /// The floor must not eat a narrow terminal: below 2×MIN_WIDTH the sidebar
    /// shrinks to at most half rather than leaving the transcript a sliver.
    /// The shell will usually have collapsed it by then (AUTO_COLLAPSE_COLS),
    /// but a user who re-expanded is honoured, and honouring them must not
    /// mean a 28-column sidebar beside a 12-column transcript.
    #[test]
    fn an_expanded_sidebar_never_takes_more_than_half_the_terminal() {
        for total in 0..=200u16 {
            assert!(
                width(total, false) <= total / 2,
                "{} of {total} columns",
                width(total, false)
            );
        }
        assert_eq!(width(50, false), 25);
    }

    // -----------------------------------------------------------------------
    // Rows under a hostile width
    // -----------------------------------------------------------------------

    /// No assembled row is ever wider than the pane, whatever the content —
    /// long names, CJK in either column, both at once. Checked on the lines
    /// rather than the buffer because `set_line` clips, and a clipped overrun
    /// is exactly the invisible defect this test exists to catch.
    #[test]
    fn no_row_is_ever_wider_than_the_pane() {
        let s = skin(Level::Truecolor);
        let hostile = [
            Row {
                name: "a-name-much-longer-than-any-sidebar".into(),
                trailing: "Yesterday".into(),
                selected: false,
            },
            Row {
                name: "一二三四五六七八九十一二三四五六".into(),
                trailing: "昨日昨日昨日".into(),
                selected: true,
            },
            Row {
                name: "x".into(),
                trailing: "a-trailing-column-of-absurd-length".into(),
                selected: false,
            },
        ];
        for w in 0..=44usize {
            for row in &hostile {
                let line = list_row(row, w, &s);
                assert!(
                    line.width() <= w,
                    "{} columns in a {w}-column row: {:?}",
                    line.width(),
                    plain(&line)
                );
            }
            let mut chrome = vec![
                header("SESSIONS", Some(COLLAPSE_HINT), w, &s),
                help_row("Ctrl+b", "toggle the sidebar on and off", 6, w, &s),
                help_row("一二三", "描述の説明", 9, w, &s),
            ];
            chrome.extend(note_rows(EMPTY_SESSIONS_HINT, w, &s));
            chrome.extend(note_rows("supercalifragilisticexpialidocious", w, &s));
            for line in chrome {
                assert!(line.width() <= w, "{:?} at {w}", plain(&line));
            }
        }
    }

    /// When both cannot fit, the name gives and the trailing column survives —
    /// and the cut is marked, never silent.
    #[test]
    fn the_name_truncates_visibly_and_the_trailing_column_survives() {
        let s = skin(Level::Truecolor);
        let line = list_row(
            &Row {
                name: "a-session-name-that-cannot-possibly-fit".into(),
                trailing: "Yesterday".into(),
                selected: false,
            },
            26,
            &s,
        );
        let text = plain(&line);
        assert!(text.contains("Yesterday"), "{text:?}");
        assert!(
            text.contains(s.glyphs.ellipsis),
            "the cut is silent: {text:?}"
        );
    }

    /// A pathological trailing column is capped at half the row so it cannot
    /// invert the priority and evict the name entirely.
    #[test]
    fn a_pathological_trailing_column_cannot_evict_the_name() {
        let s = skin(Level::Truecolor);
        let line = list_row(
            &Row {
                name: "real-name".into(),
                trailing: "t".repeat(60),
                selected: false,
            },
            28,
            &s,
        );
        let text = plain(&line);
        assert!(text.contains("real-name"), "{text:?}");
        assert!(text.contains(s.glyphs.ellipsis), "{text:?}");
    }

    /// Right alignment is column arithmetic: a CJK trailing column ends flush
    /// at the pane edge rather than two cells past it.
    #[test]
    fn a_cjk_trailing_column_ends_flush_with_the_pane() {
        let s = skin(Level::Truecolor);
        let line = list_row(
            &Row {
                name: "s".into(),
                trailing: "一二三".into(),
                selected: false,
            },
            26,
            &s,
        );
        assert_eq!(line.width(), 26, "{:?}", plain(&line));
        assert!(plain(&line).ends_with("一二三"));
    }

    // -----------------------------------------------------------------------
    // The selected row
    // -----------------------------------------------------------------------

    /// Colour is never the only signal: with no colour at all the selected row
    /// still carries its `>` marker and reversed video, and no other row does.
    #[test]
    fn the_selected_row_is_distinguishable_with_no_colour_at_all() {
        let rows = draw(&state(), &skin(Level::None), 30, 20);
        let selected: Vec<&String> = rows.iter().filter(|r| r.contains("> ")).collect();
        assert_eq!(selected.len(), 1, "{rows:#?}");
        assert!(selected[0].contains("product-strategy"));

        // …and the band survives as reversed video, which is what `chip`
        // degrades to. Asserted on the cells because the marker alone could
        // pass with the band gone.
        let area = Rect::new(0, 0, 30, 20);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &state(), &skin(Level::None));
        let y = rows
            .iter()
            .position(|r| r.contains("product-strategy"))
            .unwrap() as u16;
        assert!(
            buf[(2u16, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED),
            "the selected row lost its band"
        );
        let other = rows.iter().position(|r| r.contains("fix the")).unwrap() as u16;
        assert!(!buf[(2u16, other)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    /// With colour, the band spans the whole row — padding and trailing
    /// included — because a band that stops at the name is a patchwork.
    #[test]
    fn the_band_covers_the_full_row_not_just_the_name() {
        let s = skin(Level::Truecolor);
        let line = list_row(
            &Row {
                name: "short".into(),
                trailing: "12:42".into(),
                selected: true,
            },
            26,
            &s,
        );
        let band = s.palette.chip(Role::Accent);
        for span in &line.spans {
            assert_eq!(span.style, band, "unbanded span {:?}", span.content);
        }
    }

    // -----------------------------------------------------------------------
    // Sections, rules, and the ASCII fallback
    // -----------------------------------------------------------------------

    #[test]
    fn the_three_sections_and_their_rules_are_drawn() {
        let rows = draw(&state(), &skin(Level::Truecolor), 32, 22);
        let all = rows.join("\n");
        for headed in ["SESSIONS", "TOOLS", "QUICK HELP"] {
            assert!(all.contains(headed), "{headed} missing:\n{all}");
        }
        // Two rules between three sections, inset from the border. The border
        // rows are runs of the same glyph, so they are told apart by their
        // corner pieces rather than by the run.
        let rules = rows
            .iter()
            .filter(|r| {
                r.starts_with(UNICODE.border.vertical_left) && r.contains(&UNICODE.rule.repeat(4))
            })
            .count();
        assert_eq!(rules, 2, "{all}");
    }

    #[test]
    fn the_collapse_affordance_sits_at_the_right_edge_of_the_sessions_header() {
        let rows = draw(&state(), &skin(Level::Truecolor), 30, 22);
        let header = rows.iter().find(|r| r.contains("SESSIONS")).unwrap();
        // Right edge of the *inner* pane: the border column follows it.
        assert!(
            header
                .trim_end_matches(['│', '|', ' '])
                .ends_with(COLLAPSE_HINT),
            "{header:?}"
        );
    }

    /// The whole pane at the ASCII glyph set with no colour: every cell must
    /// be ASCII, and the structure — headers, rules, marker, empty states —
    /// must still read. This is the console the glyph mechanism exists for.
    #[test]
    fn the_ascii_fallback_is_pure_ascii_and_still_reads_as_structure() {
        let mut s = state();
        s.sessions[0].name = "a-name-far-too-long-for-any-pane-at-all".into();
        let rows = draw(&s, &ascii_skin(), 30, 14);
        let all = rows.join("\n");
        assert!(
            all.is_ascii(),
            "a non-ASCII cell reached an ASCII console:\n{all}"
        );
        assert!(all.contains("SESSIONS"));
        assert!(all.contains("---"), "no rule survived:\n{all}");
        assert!(all.contains("> "), "the selection marker is gone:\n{all}");
    }

    /// The empty-state sentences must survive the same console, which is why
    /// they are constants and why this pins them to ASCII.
    #[test]
    fn the_empty_state_sentences_are_ascii() {
        for text in [
            EMPTY_SESSIONS,
            EMPTY_SESSIONS_HINT,
            EMPTY_COMMANDS,
            EMPTY_HELP,
        ] {
            assert!(text.is_ascii(), "{text:?}");
        }
    }

    // -----------------------------------------------------------------------
    // Empty sections
    // -----------------------------------------------------------------------

    /// A section header over nothing reads as a broken pane. Every empty
    /// section says in words what would be there — the `/` menu's precedent.
    #[test]
    fn an_empty_section_explains_itself_rather_than_showing_a_bare_header() {
        // 30 columns: narrow enough that the sessions hint has to wrap, which
        // is the point — the whole sentence must reach the screen at the
        // design's floor widths, across rows, not truncated to fit one.
        let rows = draw(&State::default(), &skin(Level::Truecolor), 30, 24);
        let flat: String = rows
            .iter()
            .map(|r| r.trim_matches(['│', '|', ' ']))
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for sentence in [
            EMPTY_SESSIONS,
            EMPTY_SESSIONS_HINT,
            EMPTY_COMMANDS,
            EMPTY_HELP,
        ] {
            assert!(flat.contains(sentence), "{sentence:?} missing from: {flat}");
        }
        // Wrapped, not cut: no ellipsis anywhere in an empty pane.
        assert!(
            !flat.contains(UNICODE.ellipsis),
            "an explanation was cut: {flat}"
        );
    }

    // -----------------------------------------------------------------------
    // Height, collapse, and degenerate areas
    // -----------------------------------------------------------------------

    /// Too short a pane counts its loss out loud rather than stopping quietly.
    #[test]
    fn a_short_pane_names_how_many_rows_it_dropped() {
        let rows = draw(&state(), &skin(Level::Truecolor), 30, 8);
        let all = rows.join("\n");
        assert!(all.contains("more rows"), "the loss is silent:\n{all}");
        // And QUICK HELP is what fell off — the sections above survive.
        assert!(all.contains("SESSIONS"), "{all}");
    }

    #[test]
    fn a_collapsed_state_draws_nothing_even_if_asked_to_render() {
        let mut s = state();
        s.collapsed = true;
        let rows = draw(&s, &skin(Level::Truecolor), 30, 20);
        assert!(
            rows.iter().all(|r| r.trim().is_empty()),
            "a collapsed sidebar painted cells: {rows:#?}"
        );
    }

    /// Degenerate areas are a no-op, not a panic — a resize can report
    /// anything for a frame.
    #[test]
    fn a_zero_or_tiny_area_is_survived() {
        let s = state();
        let sk = skin(Level::Truecolor);
        for (w, h) in [(0, 0), (1, 1), (2, 30), (30, 2), (3, 3)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render(area, &mut buf, &s, &sk);
        }
    }

    /// `clipped` is total where `fit` is not: an ASCII ellipsis is three
    /// columns and `fit` will happily return it against a budget of one.
    #[test]
    fn a_budget_narrower_than_the_ellipsis_still_never_overruns() {
        for sk in [skin(Level::None), ascii_skin()] {
            for budget in 0..=10usize {
                let out = clipped(&"x".repeat(40), budget, &sk);
                assert!(
                    cols(&out) <= budget,
                    "{out:?} is {} columns in {budget} ({:?} glyphs)",
                    cols(&out),
                    sk.glyphs.ellipsis
                );
            }
        }
    }
}
