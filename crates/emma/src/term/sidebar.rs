//! The left sidebar of the full-screen frame: sessions, tools, quick help.
//!
//! Pure rendering. The shell owns the state — which sessions exist, which
//! commands are loaded, whether the sidebar is collapsed — and hands it in as
//! [`State`]; nothing here reads a file, takes a lock or asks the terminal a
//! question. That split is the same one [`super::menu`] made, and for the same
//! reason: what a pane *shows* is the part a test can hold still.
//!
//! # The numbers here were measured off the image, not the prose
//!
//! `notes/design/mockup-tui.png` was re-measured directly for this file (2026-08-13,
//! pixel sampling; character pitch 10px, sidebar box x≈17..337 of 1448 —
//! 22.2%). Where this file and `notes/design/tui-fullscreen.md` §1 disagree,
//! the image won, twice: the selected row is *accent text on a barely-raised
//! near-black band* (rgb 25,27,30 on a 13,15,19 ground), nothing like the
//! approval chip's dark-on-pink; and the `[+]` affordance is accent pink, not
//! a dim note. The measured grid is three constants below
//! ([`HEADER_INDENT`], [`LEAD`], [`RIGHT_PAD`]): headers two columns in,
//! rows led by ` > ` or three spaces, and every right-aligned column — dates,
//! keys, the affordance — ending two columns before the border, one shared
//! edge down the pane.
//!
//! The width is 22.4% of the terminal (the design's earlier sampling of the
//! same image; this pass read 22.2%, the same number at cell resolution). It
//! is clamped between a floor of 28 columns — at which `Yesterday` and a
//! usable stub of name still coexist — and a ceiling of 40, so an
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
//! Marked twice, like everything in [`super::render`]: a `>` in the lead
//! *and* a full-width band from [`Palette::band`](super::palette::Palette::band).
//! An earlier draft reused
//! [`Palette::chip`](super::palette::Palette::chip) here to avoid inventing a
//! second fg+bg pair; the image refused it — the mockup's band is accent text
//! on a subtle raised ground, and a solid pink chip row reads as a second
//! approval prompt. The pair sets both halves so it is legible on any theme,
//! and degrades to reversed video at
//! [`Level::None`](super::palette::Level::None), so the selection is never
//! carried by colour alone — the `>` survives everything.
//!
//! The band's own colours are no longer here. They were a private four-level
//! table in this file, with a note saying it lived here only because
//! `palette.rs` was not that workstream's to grow; it is now that file's, where
//! the argument for the one substitution point already lives and where a theme
//! can reach it. What survives here is the geometry.
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

/// What an unavailable tool shows where its key would be — the trailing the
/// shell's `app::tool_rows` mints for a `usertools::Entry` whose probe found
/// nothing to run (no VS Code on the box). Words rather than a style alone:
/// dimming dies on a colourless console, and a blank key would read as "no
/// shortcut" rather than "no tool". [`list_row`] treats this exact trailing
/// as a whole-row signal and dims the label with it, so the row reads as
/// switched off rather than broken. A sentinel, named and narrow on purpose:
/// [`when`] can never mint this string and no single-key binding can be
/// three characters, so neither a session's date nor a live key collides.
pub const TOOL_MISSING: &str = "n/a";

// endregion: State

// region: The session clock
// ---------------------------------------------------------------------------
// The session clock
//
// How a session's timestamp becomes the mockup's right column: `12:42` for
// today, `Yesterday`, `May 18` for this year, `May 2025` before that. Pure
// arithmetic over milliseconds plus an offset the caller supplies, because
// this module draws and std cannot ask the OS for a timezone — the shell
// owns the one platform call that can answer that question. An offset of 0
// renders UTC under a local-looking format, which is a quiet lie on any box
// west or east of Greenwich; the shell must pass the real offset, not guess.
// ---------------------------------------------------------------------------

/// What a session with no goal on record is called. Sessions have ids
/// (`sess-<ms>-<pid>`), not names, and an id dressed up as a name is the
/// filename defect this constant replaces.
pub const UNTITLED_SESSION: &str = "untitled";

/// What a session row shows instead of its filename: the first goal, with
/// whitespace collapsed so a pasted multi-line goal stays one row. No length
/// cap here — the pane already truncates to fit, and two caps drift.
pub fn session_label(first_goal: Option<&str>) -> String {
    let collapsed = first_goal
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        UNTITLED_SESSION.to_string()
    } else {
        collapsed
    }
}

/// The creation time buried in a session id (`sess-{ms:013}-{pid}`, with or
/// without `.jsonl`), for when file metadata is missing or lying. `None` for
/// anything that does not parse — a made-up time is worse than no column.
pub fn session_time_ms(id: &str) -> Option<i64> {
    let id = id.strip_suffix(".jsonl").unwrap_or(id);
    let rest = id.strip_prefix("sess-")?;
    let (ms, pid) = rest.split_once('-')?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ms.parse::<i64>().ok().filter(|v| *v > 0)
}

const DAY_MS: i64 = 86_400_000;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The right column, in the mockup's buckets. `offset_min` is local minus
/// UTC in minutes (EDT is -240). Same local civil day: `12:42`. The civil
/// day before: `Yesterday`. Older, same year: `May 18`; another year:
/// `May 2025` — a date from last May shown bare would be a fresher claim
/// than the file can back. A timestamp from the future is rendered as the
/// date it says, because a skewed clock is the caller's fact to keep.
pub fn when(then_ms: i64, now_ms: i64, offset_min: i32) -> String {
    let off = i64::from(offset_min) * 60_000;
    let t = then_ms.saturating_add(off);
    let n = now_ms.saturating_add(off);
    let t_day = t.div_euclid(DAY_MS);
    let n_day = n.div_euclid(DAY_MS);
    if t_day == n_day {
        let rem = t.rem_euclid(DAY_MS);
        return format!("{:02}:{:02}", rem / 3_600_000, rem % 3_600_000 / 60_000);
    }
    if n_day - t_day == 1 {
        return "Yesterday".to_string();
    }
    let (ty, tm, td) = civil(t_day);
    let (ny, _, _) = civil(n_day);
    let month = MONTHS[(tm - 1) as usize];
    if ty == ny {
        format!("{month} {td}")
    } else {
        format!("{month} {ty}")
    }
}

/// Days since 1970-01-01 to (year, month, day). Hinnant's civil-from-days —
/// the standard closed form, with `div_euclid`/`rem_euclid` standing in for
/// the paper's floored division. Exercised against known calendar anchors in
/// the tests rather than trusted on reputation.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// endregion: The session clock

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

/// The collapse affordance on the SESSIONS header: the mockup's `[+]`, in
/// accent, on the shared right edge. The design's §3.2 argued `[-]` ("show
/// the action") and flagged the deviation as Q6, awaiting the owner; the
/// owner then ran the build against the image and reported the sidebar "not
/// formatted like the image" — which answers Q6 in the other direction, and
/// the image outranks the argument. The glyph is state-honest regardless: a
/// collapsed sidebar draws nothing at all, so this renders only on an
/// expanded pane and only ever means one thing. What it does *not* do is
/// accept a click — the shell owns input, and until it routes a mouse press
/// here this is a label for Ctrl-B, drawn where the mockup drew it.
pub const COLLAPSE_HINT: &str = "[+]";

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

/// The measured grid, in columns of the pane's inner width. Headers sit two
/// in from the border; list rows lead with ` > ` (selected) or three spaces;
/// and every right-aligned column — a date, a key, the affordance — ends two
/// columns before the border, so the whole pane shares one right edge. The
/// numbers are the mockup's, read off the image at its 10px character pitch
/// (header text at ~2.2 cells, marker at ~1.5, names at ~3.6, right column
/// ending ~1.9 cells in), rounded to the cell grid.
const HEADER_INDENT: usize = 2;
const LEAD: usize = 3;
const RIGHT_PAD: usize = 2;

/// A section title, indented onto the measured grid, with the collapse
/// affordance right-aligned when there is one. The affordance keeps its
/// columns and the title truncates — a header that is legible but cannot be
/// closed is worse than the reverse.
fn header(name: &str, affordance: Option<&str>, w: usize, skin: &Skin) -> Line<'static> {
    let indent = HEADER_INDENT.min(w);
    let room = w.saturating_sub(indent);
    // An affordance that does not fit whole is dropped whole rather than
    // clipped: half of `[+]` is not a control, it is debris. Its right pad
    // goes first — one column off the border still reads as the control.
    let mut aff = affordance.unwrap_or("");
    let mut pad = if aff.is_empty() { 0 } else { RIGHT_PAD };
    if cols(aff) + pad > room {
        pad = 0;
        if cols(aff) > room {
            aff = "";
        }
    }
    let aff_w = cols(aff);
    let name_budget = room.saturating_sub(aff_w + pad + usize::from(aff_w > 0));
    let name = clipped(name, name_budget, skin);
    let gap = room.saturating_sub(cols(&name) + aff_w + pad);
    let mut spans = vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(name, skin.palette.bold(Role::Accent)),
        Span::raw(" ".repeat(gap)),
    ];
    if aff_w > 0 {
        // Accent, not dim — measured off the image, where the `[+]` is the
        // same pink as SESSIONS: it is a control, not a footnote.
        spans.push(Span::styled(
            aff.to_string(),
            skin.palette.style(Role::Accent),
        ));
        spans.push(Span::raw(" ".repeat(pad)));
    }
    Line::from(spans)
}

/// One list row at exactly `w` columns: ` > name        trailing  `.
///
/// The geometry is the measured grid ([`LEAD`], [`RIGHT_PAD`]); the width
/// arithmetic is the module doc's ruling made concrete: the trailing column
/// is capped at half the usable row and survives; the name takes what is
/// left and truncates with a visible mark. The gap is computed *after* both
/// cuts, from measured columns, so the trailing text ends flush on the shared
/// right edge whatever [`fit`] undershot by — a wide glyph that would not
/// split leaves the gap one wider, never the row one over.
fn list_row(row: &Row, w: usize, skin: &Skin) -> Line<'static> {
    let lead_w = LEAD.min(w);
    let pad_w = RIGHT_PAD.min(w.saturating_sub(lead_w));
    let avail = w.saturating_sub(lead_w + pad_w);
    let trailing = clipped(&row.trailing, avail / 2, skin);
    let t_w = cols(&trailing);
    let name = clipped(
        &row.name,
        avail.saturating_sub(t_w + usize::from(t_w > 0)),
        skin,
    );
    let gap = avail.saturating_sub(cols(&name) + t_w);
    // The lead is ASCII by construction, so slicing it to the pane is a
    // byte-safe way to keep a one-column pane at one column.
    let lead = &(if row.selected { " > " } else { "   " })[..lead_w];
    let pad = " ".repeat(pad_w);
    if row.selected {
        // The band: one style across every span, lead and padding included,
        // so the highlight is the full row and not a patchwork — the image
        // shows it running border to border.
        let b = skin.palette.band();
        return Line::from(vec![
            Span::styled(lead.to_string(), b),
            Span::styled(name, b),
            Span::styled(" ".repeat(gap), b),
            Span::styled(trailing, b),
            Span::styled(pad, b),
        ]);
    }
    // An unavailable tool: [`TOOL_MISSING`] in the trailing column is the
    // signal that survives everything, and the label dims with it so the row
    // reads as switched off rather than merely unbound. The words carry what
    // the dimming cannot — a colourless console still says "not found".
    let name_style = if row.trailing == TOOL_MISSING {
        skin.palette.dim()
    } else {
        skin.palette.style(Role::Text)
    };
    Line::from(vec![
        Span::raw(lead.to_string()),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(trailing, skin.palette.dim()),
        Span::raw(pad),
    ])
}

/// One QUICK HELP row: the key in a shared column, then the description —
/// both dim. Measured off the image: the help table is uniformly the
/// secondary grey (the "shortcut keys are brighter" note in the design's
/// §1.2 was sampled from the status bar, not from this section), and the two
/// columns are told apart by alignment, which no colour level takes away.
fn help_row(key: &str, desc: &str, key_w: usize, w: usize, skin: &Skin) -> Line<'static> {
    // Every fixed piece is budgeted against what is actually left, so a pane
    // narrower than the indent-plus-key-column shrinks pieces instead of
    // writing past its edge.
    let indent = HEADER_INDENT.min(w);
    let kb = key_w.min(w.saturating_sub(indent));
    let key = clipped(key, kb, skin);
    let after_key = w.saturating_sub(indent + cols(&key));
    let gap = (kb.saturating_sub(cols(&key)) + 2).min(after_key);
    // The description stops on the same right edge as every other column —
    // prose running into the border reads as an overflow, not a margin.
    let pad = RIGHT_PAD.min(after_key.saturating_sub(gap));
    let desc = clipped(desc, after_key.saturating_sub(gap + pad), skin);
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(key, skin.palette.dim()),
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
    use ratatui::style::{Color, Modifier};

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
            Row {
                name: "Code".into(),
                trailing: TOOL_MISSING.into(),
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
    /// on the shared right edge — two columns before the border, like every
    /// date, key and the affordance — rather than two cells past the pane.
    #[test]
    fn a_cjk_trailing_column_ends_flush_with_the_shared_right_edge() {
        let s = skin(Level::Truecolor);
        let line = list_row(
            &Row {
                name: "s".into(),
                trailing: "\u{4e00}\u{4e8c}\u{4e09}".into(),
                selected: false,
            },
            26,
            &s,
        );
        assert_eq!(line.width(), 26, "{:?}", plain(&line));
        assert!(
            plain(&line).ends_with("\u{4e00}\u{4e8c}\u{4e09}  "),
            "{:?}",
            plain(&line)
        );
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

    /// With colour, the band spans the whole row — lead, padding and trailing
    /// included, border to border as the image shows — and it is the measured
    /// pair: accent text on a barely-raised near-black. Not the approval
    /// chip's dark-on-pink, which is the exact regression the owner reported
    /// as "not formatted like the image".
    #[test]
    fn the_band_is_accent_on_a_subtle_ground_and_covers_the_full_row() {
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
        let b = s.palette.band();
        assert_eq!(line.width(), 26, "{:?}", plain(&line));
        for span in &line.spans {
            assert_eq!(span.style, b, "unbanded span {:?}", span.content);
        }
        assert_eq!(b.fg, Some(s.palette.color(Role::Accent)));
        assert_eq!(b.bg, Some(Color::Rgb(25, 27, 30)), "the sampled band bg");
        assert_ne!(
            b,
            s.palette.chip(Role::Accent),
            "the chip pair came back: dark-on-pink is not the mockup's band"
        );
    }

    /// **The receipt for the move: the drawn row is byte-for-byte the row that
    /// shipped, at every fidelity.**
    ///
    /// `band` moved out of this file and into the palette, where a theme can
    /// reach it. The risk that carries is not that the pair stops existing —
    /// the test above would catch that — but that one of its four fidelities
    /// quietly changes value on the way across. So this asserts the painted
    /// cells rather than the style object: the whole selected row, from the
    /// left border to the right, against the exact colours the private table
    /// used to produce. The `Ansi256` and `Ansi16` rows are the ones worth
    /// having, because those are the two the palette now computes differently
    /// — derived from the pair's hex, and inherited by name — where this file
    /// used to write 234 and `DarkGray` as literals.
    #[test]
    fn the_selected_row_is_painted_exactly_as_it_was_before_the_band_moved() {
        let expected = [
            (
                Level::Truecolor,
                Some(Color::Rgb(245, 84, 143)),
                Some(Color::Rgb(25, 27, 30)),
            ),
            (
                Level::Ansi256,
                Some(Color::Indexed(204)),
                Some(Color::Indexed(234)),
            ),
            (
                Level::Ansi16,
                Some(Color::LightMagenta),
                Some(Color::DarkGray),
            ),
        ];
        for (level, fg, bg) in expected {
            let sk = skin(level);
            let rows = draw(&state(), &sk, 30, 22);
            let y = rows
                .iter()
                .position(|r| r.contains("product-strategy"))
                .unwrap() as u16;
            let area = Rect::new(0, 0, 30, 22);
            let mut buf = Buffer::empty(area);
            render(area, &mut buf, &state(), &sk);
            // 1..29: everything inside the border, which is where the image
            // shows the band running.
            for x in 1..29u16 {
                let style = buf[(x, y)].style();
                assert_eq!(style.fg, fg, "cell {x} at {level:?}");
                assert_eq!(style.bg, bg, "cell {x} at {level:?}");
            }
            // The border keeps its own colour — the band is the row, not the
            // pane. (A painted cell's background is `Reset` rather than `None`;
            // the assertion is that it is not the band's.)
            assert_ne!(buf[(0u16, y)].style().bg, bg, "the band ate the border");
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

    /// The affordance, measured: the mockup's `[+]`, accent like the header,
    /// on the shared right edge two columns before the border — and the
    /// header itself two columns in from the left. Q6 proposed `[-]`; the
    /// owner's run against the image overruled it.
    #[test]
    fn the_affordance_is_the_mockups_plus_in_accent_on_the_shared_right_edge() {
        assert_eq!(COLLAPSE_HINT, "[+]", "Q6 was answered by the image");
        let sk = skin(Level::Truecolor);
        let rows = draw(&state(), &sk, 30, 22);
        let (y, header) = rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains("SESSIONS"))
            .unwrap();
        assert!(
            header.starts_with(&format!("{}  SESSIONS", UNICODE.border.vertical_left)),
            "{header:?}"
        );
        assert!(
            header.ends_with(&format!(
                "{COLLAPSE_HINT}  {}",
                UNICODE.border.vertical_right
            )),
            "{header:?}"
        );
        // Accent, not a dim footnote. Char position is cell position here:
        // every glyph on this row is one column wide.
        let area = Rect::new(0, 0, 30, 22);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &state(), &sk);
        let x = header.chars().position(|c| c == '[').unwrap() as u16;
        assert_eq!(
            buf[(x, y as u16)].style().fg,
            Some(sk.palette.color(Role::Accent)),
            "the affordance lost its accent"
        );
    }

    /// The TOOLS contract: an available tool advertises its key; an
    /// unavailable one — the shell hands its trailing in as [`TOOL_MISSING`]
    /// — keeps its label, says so in words, and dims whole. The words are
    /// the half of the signal that survives the consoles the dimming dies
    /// on. Rows are built here exactly as `app::tool_rows` builds them from
    /// a `usertools::Entry`; the constant is the interlock between the two.
    #[test]
    fn an_unavailable_tool_reads_as_switched_off_not_broken() {
        let mut st = state();
        st.commands = vec![
            Row {
                name: "Shell".into(),
                trailing: "s".into(),
                selected: false,
            },
            Row {
                name: "Code".into(),
                trailing: TOOL_MISSING.into(),
                selected: false,
            },
        ];
        let sk = skin(Level::Truecolor);
        let grid = draw(&st, &sk, 30, 22);
        let area = Rect::new(0, 0, 30, 22);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &st, &sk);
        let y = grid.iter().position(|r| r.contains("Code")).unwrap();
        assert!(grid[y].contains(TOOL_MISSING), "{:?}", grid[y]);
        let x = grid[y].chars().position(|c| c == 'C').unwrap() as u16;
        assert_eq!(
            buf[(x, y as u16)].style().fg,
            Some(sk.palette.color(Role::Dim)),
            "the unavailable label did not dim"
        );
        let ys = grid.iter().position(|r| r.contains("Shell")).unwrap();
        let xs = grid[ys].chars().position(|c| c == 'S').unwrap() as u16;
        assert_eq!(
            buf[(xs, ys as u16)].style().fg,
            Some(Color::Reset),
            "an available label must stay at full strength"
        );
        // The colourless, glyphless console: the words are the signal.
        let grid = draw(&st, &ascii_skin(), 30, 22);
        let row = grid.iter().find(|r| r.contains("Code")).unwrap();
        assert!(row.contains(TOOL_MISSING), "{row:?}");
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
    // The session clock
    // -----------------------------------------------------------------------

    /// The mockup's buckets, against two calendar anchors computed by hand
    /// (1970-04-11 is day 100; 2026-08-12 is day 20,677: 2020-01-01 is
    /// 18,262, six years add 366+365+365+365+366+365 — 2020 and 2024 leap —
    /// Jan..Jul of 2026 add 212, the 12th adds 11). The first draft of this
    /// anchor forgot 2020's leap day and the algorithm caught the fixture,
    /// which is the right direction for that to fail.
    #[test]
    fn the_session_clock_matches_the_mockups_buckets() {
        let d = 20_677 * DAY_MS; // 2026-08-12 00:00 local
        let t = d + 12 * 3_600_000 + 42 * 60_000;
        assert_eq!(when(t, d + 13 * 3_600_000, 0), "12:42");
        assert_eq!(when(d + 9 * 3_600_000 + 5 * 60_000, t, 0), "09:05");
        assert_eq!(when(t, t + DAY_MS, 0), "Yesterday");
        assert_eq!(when(t, t + 100 * DAY_MS, 0), "Aug 12");
        assert_eq!(when(t, t + 200 * DAY_MS, 0), "Aug 2026");
        assert_eq!(when(100 * DAY_MS, 300 * DAY_MS, 0), "Apr 11");
    }

    /// The offset moves the midnight boundary, not just the clock face —
    /// which is the whole reason the shell must pass a real offset rather
    /// than letting 0 quietly mean Greenwich.
    #[test]
    fn the_offset_moves_the_midnight_boundary_not_just_the_clock() {
        // 23:30 UTC against 00:01 UTC next day: UTC says Yesterday...
        let then = DAY_MS - 30 * 60_000;
        let now = DAY_MS + 60_000;
        assert_eq!(when(then, now, 0), "Yesterday");
        // ...but two hours east, both instants share a civil day.
        assert_eq!(when(then, now, 120), "01:30");
        // West of Greenwich, the small hours of 1970-01-01 fall into a
        // different civil *year*; the bucket crosses it without flinching.
        assert_eq!(when(30 * 60_000, 2 * 3_600_000, -60), "Yesterday");
    }

    #[test]
    fn a_session_id_yields_its_creation_time_and_garbage_yields_none() {
        assert_eq!(
            session_time_ms("sess-1786499687418-67640"),
            Some(1_786_499_687_418)
        );
        assert_eq!(
            session_time_ms("sess-1786499687418-67640.jsonl"),
            Some(1_786_499_687_418)
        );
        assert_eq!(session_time_ms("sess-none"), None);
        assert_eq!(session_time_ms("sess-abc-123"), None);
        assert_eq!(session_time_ms("sess-1786499687418-"), None);
        assert_eq!(session_time_ms("anything.jsonl"), None);
    }

    #[test]
    fn a_session_label_is_the_goal_collapsed_never_the_filename() {
        assert_eq!(
            session_label(Some(" fix\tthe\nfrontmatter parser ")),
            "fix the frontmatter parser"
        );
        assert_eq!(session_label(None), UNTITLED_SESSION);
        assert_eq!(session_label(Some("   ")), UNTITLED_SESSION);
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
