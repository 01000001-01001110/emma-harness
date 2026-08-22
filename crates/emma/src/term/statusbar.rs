//! The full-screen status bar: one row of measured facts, divided into cells.
//!
//! This is the fullscreen successor to `Skin::status` in [`super::render`], and
//! it inherits that row's one law: **everything on it is true.** A field whose
//! value was never measured is absent rather than zero — zero tokens and
//! unknown tokens are different facts — and a row that runs out of room drops
//! whole cells in a stated order rather than cutting a number in half. A
//! truncated measurement is a lie; an absent one is honest.
//!
//! # The two kinds of number
//!
//! `ctx_*` is a *snapshot* — how full the context window is right now; it rises
//! and falls. `total_*` is an *accumulation* — what the goal has been billed
//! for; it only rises. The old status line put both on screen as `used/cap`
//! pairs and the owner read them as inverted, because a total is usually larger
//! than a level and two identically-shaped numbers invite comparison
//! (`render.rs` carries the whole argument, and the `spend`→`total` rename that
//! came out of it). This row goes one step further: the snapshot is drawn as a
//! percentage and a meter — the shape of a level — and the accumulation as raw
//! token counts — the shape of a bill. Two different pictures cannot be read
//! as each other's inverse.
//!
//! # The drop order, and why context is last
//!
//! From `notes/design/tui-fullscreen.md` §7, which extends the drop order
//! `Skin::status` already argued once: names shorten before anything is lost,
//! then decoration goes before measurement, and among measurements the one
//! that survives longest is the one that changes what the user does next.
//! Running out of context forces an action — compact, or start over — so the
//! context percentage is the last thing standing. The spent total is a fact to
//! know; the context level is a decision to make.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::palette::Role;
use super::render::{cols, fit, Skin, ASCII};

// region: The contract
// ---------------------------------------------------------------------------
// The contract
//
// Filled by the shell, rendered here. This module owns no state and computes
// no measurement: `mode` and `env` are whatever the shell says they are today
// (Emma has no modes; the honest occupants are the run posture and the
// working state), and rendering them without judgement is the point — a label
// is only drawn when there is a value under it.
// ---------------------------------------------------------------------------

/// Everything the status bar can say. One struct, filled by the shell.
///
/// The counts are bare `i64` rather than `Option` because that is the shared
/// contract; absence is encoded in the values. The rule this module applies:
/// **a cap that is not positive is no cap**, so no percentage or meter is drawn
/// against it — a proportion of nothing is an invention — and a cell with
/// neither a positive count nor a positive cap has nothing measured behind it
/// and does not appear. `up`, `down` and `elapsed` keep the honest type:
/// `None` is unknown, `Some(0)` is a measured zero, and the two render
/// differently because they are different facts.
pub struct Bar {
    pub mode: String,
    pub model: String,
    pub env: String,
    pub ctx_used: i64,
    pub ctx_max: i64,
    pub total_used: i64,
    pub total_max: i64,
    pub up: Option<i64>,
    pub down: Option<i64>,
    pub elapsed: Option<std::time::Duration>,
}

/// Draw the bar into the top row of `area`.
///
/// Pure: composes a line that fits the width by arithmetic, then writes it.
/// `set_line` clips as a backstop, but the composition never relies on it —
/// clipping is exactly the mid-number cut this module exists to never make.
pub fn render(area: Rect, buf: &mut Buffer, bar: &Bar, skin: &Skin) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let line = compose(area.width, bar, skin);
    buf.set_line(area.x, area.y, &line, area.width);
}

// endregion: The contract

// region: Glyphs
// ---------------------------------------------------------------------------
// Glyphs
//
// The bar's own vocabulary, chosen off the skin's glyph set rather than added
// to it: `render::Glyphs` is shared chrome another team is working in today,
// and the two sets are told apart by comparing against `render::ASCII` — the
// same detection, one step removed.
// ---------------------------------------------------------------------------

struct BarGlyphs {
    vbar: &'static str,
    up: &'static str,
    down: &'static str,
    full: &'static str,
    empty: &'static str,
    /// ASCII wraps the meter in `[ ]` so a run of `#` and `-` still reads as a
    /// gauge rather than as punctuation; the block glyphs need no frame.
    open: &'static str,
    close: &'static str,
}

fn bar_glyphs(skin: &Skin) -> BarGlyphs {
    if skin.glyphs == ASCII {
        BarGlyphs {
            vbar: "|",
            up: "^",
            down: "v",
            full: "#",
            empty: "-",
            open: "[",
            close: "]",
        }
    } else {
        BarGlyphs {
            vbar: "\u{2502}", // │
            up: "\u{2191}",   // ↑
            down: "\u{2193}", // ↓
            // ▊ (left three-quarters block), not █: ten adjacent full blocks
            // fuse into one solid bar and the proportion stops being countable
            // — the owner's "separators between the segments" report. Measured
            // off the image, each meter block inks ~7px of a ~9.3px cell:
            // three-quarters fill, quarter-cell gap — which is exactly this
            // glyph, and it buys the gap without spending an extra column.
            // Empty is ░ where the image uses a grey block of the same shape:
            // a deliberate deviation, because filled and empty must differ by
            // *shape*, not colour alone, or the meter dies at `Level::None` —
            // the mock was never run on a colourless terminal. The ASCII `#`
            // needs no such treatment; discrete glyphs never fuse.
            full: "\u{258A}",  // ▊
            empty: "\u{2591}", // ░
            open: "",
            close: "",
        }
    }
}

// endregion: Glyphs

// region: Fitting the width
// ---------------------------------------------------------------------------
// Fitting the width
//
// A ladder of degradation steps, walked until one fits. Each step is strictly
// poorer than the one before, so what survives at any width is a statement of
// priority: decoration before measurement, and among measurements the total
// before the context level, because the level is the number that changes what
// the user does next.
// ---------------------------------------------------------------------------

/// Ten, though the image shows eleven blocks (seven accent, four grey —
/// counted from the pixel runs). Ten is the design doc's stated count and the
/// round number the rounding arithmetic is pinned against; an eleventh
/// segment buys no information and reads as the mock's artist losing count,
/// not as a specification.
const SEGMENTS_FULL: usize = 10;
const SEGMENTS_SHORT: usize = 5;
/// The fewest columns an env value is worth fitting into. Below this the cell
/// is dropped whole: `clau…` still identifies a model, `cl…` identifies
/// nothing, and a cell that identifies nothing is worse than an absent one.
const ENV_MIN: usize = 6;

/// The bar, fitted to its width.
///
/// The ladder, in the order things are surrendered — `Skin::status`'s
/// drop-whole-fields rule become a cell-shedding order. A cell's separator is
/// part of the cell for this purpose: when the cell goes its rule goes with
/// it, so the rules can never be what pushes a row over its width.
///
/// 1. the env value is shortened (a name may be fitted; a number never is)
/// 2. the help hints go (they are the only cell that is not a measurement)
/// 3. the ↑/↓ split goes (the TOKENS figure it details survives)
/// 4. the meter shrinks to five segments (still a proportion)
/// 5. the env cell goes whole
/// 6. the tokens cell goes whole (a fact to know; the level is a decision to
///    make)
/// 7. the meter goes, keeping the percentage
/// 8. the mode cell goes, clock and all
/// 9. the context label goes, keeping the bare percentage — last thing
///    standing, because running out of context forces the next action
/// 10. nothing: an empty row rather than a cut number
fn compose(width: u16, bar: &Bar, skin: &Skin) -> Line<'static> {
    for step in 0..=10 {
        if let Some(line) = attempt(step, width, bar, skin) {
            return line;
        }
    }
    unreachable!("the last step always fits: it is empty")
}

fn attempt(step: u8, width: u16, bar: &Bar, skin: &Skin) -> Option<Line<'static>> {
    if step >= 10 {
        return Some(Line::default());
    }
    let g = bar_glyphs(skin);
    let mut cells: Vec<Vec<Span<'static>>> = Vec::new();
    if step < 8 {
        if let Some(c) = mode_cell(bar, skin) {
            cells.push(c);
        }
    }
    if step < 6 {
        if let Some(c) = tokens_cell(bar, skin, &g, step < 3) {
            cells.push(c);
        }
    }
    let segments = match step {
        0..=3 => SEGMENTS_FULL,
        4..=6 => SEGMENTS_SHORT,
        _ => 0,
    };
    if let Some(c) = context_cell(bar, skin, &g, segments, step < 9) {
        cells.push(c);
    }
    let help = (step < 2).then(|| help_cell(skin));
    if step < 5 {
        if let Some(value) = env_value(bar, skin) {
            let label = label("ENV", skin);
            // Everything else is already fixed, so the room the value has is
            // what the row leaves over — measured, not guessed.
            let fixed = row_width(&cells, help.as_deref(), &g)
                + sep_width(&g, !cells.is_empty())
                + cols(&label.content);
            let room = usize::from(width).saturating_sub(fixed);
            let value = if cols(&value) <= room {
                value
            } else if step == 0 {
                // The first attempt is everything whole; shortening is a
                // decision the ladder makes explicitly at the next step.
                return None;
            } else if room >= ENV_MIN {
                fit(&value, room, skin.glyphs.ellipsis)
            } else {
                return None;
            };
            cells.push(vec![
                label,
                Span::styled(value, skin.palette.style(Role::Text)),
            ]);
        }
    }
    layout(cells, help, width, skin, &g)
}

/// Join the cells with vertical rules and refuse — rather than clip — a row
/// that does not fit.
///
/// The rules come from the image: every cell boundary carries one, **including
/// the help cell's** — right-aligned hints with only air before them read as
/// loose text, not as a divided bar — and the row's slack is spread evenly
/// across the gaps rather than piled up in front of the hints. The mockup's
/// rules sit three to five cells clear of their neighbours on a wide frame
/// because the cells are distributed, not left-packed; a bar that crams four
/// cells against the left edge and leaves one sixty-column hole is not the
/// picture. Under pressure the gaps compress to their one-space minimum
/// before any cell is dropped, so the distribution never costs a measurement.
fn layout(
    cells: Vec<Vec<Span<'static>>>,
    help: Option<Vec<Span<'static>>>,
    width: u16,
    skin: &Skin,
    g: &BarGlyphs,
) -> Option<Line<'static>> {
    let width = usize::from(width);
    let content: usize = cells
        .iter()
        .map(|c| c.iter().map(Span::width).sum::<usize>())
        .sum();
    let hw: usize = help.as_ref().map_or(0, |h| h.iter().map(Span::width).sum());
    if cells.is_empty() {
        // Nothing measured: no rules, because a divider needs two sides. The
        // hints alone keep to the right edge, where the mockup puts them.
        return match help {
            Some(h) if hw <= width => {
                let mut spans = vec![Span::raw(" ".repeat(width - hw))];
                spans.extend(h);
                Some(Line::from(spans))
            }
            Some(_) => None,
            None => Some(Line::default()),
        };
    }
    let rules = cells.len() - 1 + usize::from(help.is_some());
    let min = content + rules * sep_width(g, true) + hw;
    if min > width {
        return None;
    }
    // With help present the row is anchored at both edges and the slack is
    // shared out, remainder to the leftmost gaps. Without help nothing anchors
    // the right edge, so the row stays left-packed at minimum gaps — trailing
    // background is invisible, widened gaps between survivors are not.
    let (base, rem) = if help.is_some() && rules > 0 {
        ((width - min) / rules, (width - min) % rules)
    } else {
        (0, 0)
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut gap = 0usize;
    let mut rule = |spans: &mut Vec<Span<'static>>| {
        let extra = base + usize::from(gap < rem);
        gap += 1;
        // The extra columns split around the glyph so the rule stays near the
        // middle of its gap, as the image draws it.
        spans.push(Span::styled(
            format!(
                "{}{}{}",
                " ".repeat(1 + extra / 2),
                g.vbar,
                " ".repeat(1 + extra - extra / 2)
            ),
            skin.palette.dim(),
        ));
    };
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            rule(&mut spans);
        }
        spans.extend(cell);
    }
    if let Some(h) = help {
        rule(&mut spans);
        spans.extend(h);
    }
    Some(Line::from(spans))
}

fn sep_width(g: &BarGlyphs, needed: bool) -> usize {
    if needed {
        cols(g.vbar) + 2
    } else {
        0
    }
}

fn row_width(cells: &[Vec<Span<'static>>], help: Option<&[Span<'static>]>, g: &BarGlyphs) -> usize {
    let cell_w: usize = cells
        .iter()
        .map(|c| c.iter().map(Span::width).sum::<usize>())
        .sum();
    let seps = cells.len().saturating_sub(1) * sep_width(g, true);
    // The help cell's fixed cost mirrors `layout` exactly: one column of
    // padding, the rule and its trailing space, then the hints. If these two
    // ever disagree, the env-room arithmetic hands out columns the layout
    // will refuse, and some width overflows — the property test walks every
    // width precisely to catch that drift.
    let help_w = help.map_or(0, |h| {
        2 + cols(g.vbar) + h.iter().map(Span::width).sum::<usize>()
    });
    cell_w + seps + help_w
}

// endregion: Fitting the width

// region: The cells
// ---------------------------------------------------------------------------
// The cells
//
// Each returns `None` when there is nothing measured behind it. A label is
// never drawn over an empty value: the mockup's MODE and ENV are placeholder
// content (design §1.5), and rendering their labels regardless would be the
// bar pretending Emma has features it does not.
// ---------------------------------------------------------------------------

/// A cell label: accent and bold, per the mockup's measured colours (the prose
/// description said grey; the picture says accent, and the picture wins —
/// design §1.4). Bold so the label still stands apart at `Level::None`.
/// Two spaces to the value, measured off the image's character grid: every
/// label sits two cells clear of its value (`TOKENS  12,842`), consistently
/// across all four cells.
fn label(text: &str, skin: &Skin) -> Span<'static> {
    Span::styled(format!("{text}  "), skin.palette.bold(Role::Accent))
}

/// The run posture, and the clock beside it. The clock lives here because a
/// duration belongs to the working state it measures; when the mode cell is
/// shed, the clock goes with it — whole, like every field.
fn mode_cell(bar: &Bar, skin: &Skin) -> Option<Vec<Span<'static>>> {
    let mut spans = Vec::new();
    if !bar.mode.is_empty() {
        spans.push(label("MODE", skin));
        spans.push(Span::styled(
            bar.mode.clone(),
            skin.palette.style(Role::Text),
        ));
    }
    if let Some(e) = bar.elapsed {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(clock(e), skin.palette.style(Role::Text)));
    }
    (!spans.is_empty()).then_some(spans)
}

/// The cumulative bill, labelled `TOKENS` per the mockup. The label used to be
/// `TOTAL`, chosen when the word was the only guard against the old
/// `spend`-beside-`ctx` inversion (`render.rs` carries that history) — but on
/// this bar the guard is *shape*: the bill is raw counts, the level is a
/// percentage and a meter, and a test pins that the two can never converge.
/// With shape carrying the load the label is free to say what the mockup says,
/// and the owner measured the built bar against the image and asked for it.
/// What must not regress is the shape rule itself: raw token counts here,
/// never a percentage — an accumulation drawn as a level is how the confusion
/// started.
fn tokens_cell(bar: &Bar, skin: &Skin, g: &BarGlyphs, arrows: bool) -> Option<Vec<Span<'static>>> {
    if bar.total_used <= 0 && bar.total_max <= 0 {
        return None;
    }
    let mut spans = vec![label("TOKENS", skin)];
    let value = if bar.total_max > 0 {
        // The mockup's cell shows no cap because the mockup has no budget
        // configured; a measured budget still draws, because hiding a real
        // measurement to match placeholder content would be the bar lying by
        // omission. With no budget the cell is the mockup's exactly.
        format!("{}/{}", group(bar.total_used.max(0)), group(bar.total_max))
    } else {
        group(bar.total_used.max(0))
    };
    spans.push(Span::styled(value, skin.palette.style(Role::Text)));
    // A count below zero is not a measurement; it is treated as absent rather
    // than clamped, because a clamped zero would claim a measured zero.
    let up = bar.up.filter(|n| *n >= 0);
    let down = bar.down.filter(|n| *n >= 0);
    if arrows && (up.is_some() || down.is_some()) {
        spans.push(Span::styled(" (".to_string(), skin.palette.dim()));
        if let Some(u) = up {
            // Everything here is measured off the image, not the design doc's
            // prose — the doc has been wrong about this cell twice. The ↑ and
            // its figure are both green (`Role::Ok`), one space between them;
            // two spaces separate the up figure from the ↓.
            spans.push(Span::styled(
                format!("{} {}", g.up, group(u)),
                skin.palette.style(Role::Ok),
            ));
        }
        if let Some(d) = down {
            if up.is_some() {
                spans.push(Span::raw("  "));
            }
            // The ↓ arrow is accent but its figure is plain foreground — the
            // image is unambiguous (the digits sample neutral grey-white, the
            // arrow pink), and the doc's "`↓` in accent" describes only the
            // arrow. Painting the figure accent was this cell's third
            // prose-derived error.
            spans.push(Span::styled(
                g.down.to_string(),
                skin.palette.style(Role::Accent),
            ));
            spans.push(Span::styled(
                format!(" {}", group(d)),
                skin.palette.style(Role::Text),
            ));
        }
        spans.push(Span::styled(")".to_string(), skin.palette.dim()));
    }
    Some(spans)
}

/// The context level: a percentage and a meter — the shape of a snapshot.
/// With a count but no cap, the count alone: the proportion would be invented.
fn context_cell(
    bar: &Bar,
    skin: &Skin,
    g: &BarGlyphs,
    segments: usize,
    with_label: bool,
) -> Option<Vec<Span<'static>>> {
    if bar.ctx_used <= 0 && bar.ctx_max <= 0 {
        return None;
    }
    let mut spans = Vec::new();
    if with_label {
        spans.push(label("CONTEXT", skin));
    }
    if bar.ctx_max > 0 {
        let pct = percent(bar.ctx_used, bar.ctx_max);
        spans.push(Span::styled(
            format!("{pct}%"),
            skin.palette.style(Role::Text),
        ));
        if segments > 0 {
            // Percentage before the meter, two cells apart — measured off the
            // image, same gap as label-to-value.
            spans.push(Span::raw("  "));
            spans.extend(meter(bar.ctx_used, bar.ctx_max, segments, skin, g));
        }
    } else {
        spans.push(Span::styled(
            group(bar.ctx_used),
            skin.palette.style(Role::Text),
        ));
    }
    Some(spans)
}

/// Provider/model and whatever else the shell put in `env`, joined by the
/// skin's separator. The one cell whose values may be shortened under
/// pressure: they are names, and a fitted name still identifies — a fitted
/// number misleads.
fn env_value(bar: &Bar, skin: &Skin) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if !bar.model.is_empty() {
        parts.push(&bar.model);
    }
    if !bar.env.is_empty() {
        parts.push(&bar.env);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(&format!(" {} ", skin.glyphs.sep)))
}

/// `/help · /exit`, dim. The mockup's `? help  q quit` shows keys that are not
/// bound — a bare letter cannot act while an input box exists — and a hint
/// that does nothing when pressed is a small lie. These two are commands that
/// really run.
fn help_cell(skin: &Skin) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("/help {} /exit", skin.glyphs.sep),
        skin.palette.dim(),
    )]
}

// endregion: The cells

// region: The meter's arithmetic
// ---------------------------------------------------------------------------
// The meter's arithmetic
//
// Both roundings err in the same direction: they may overstate what is used,
// never what is left. A meter that reads full at 99% panics nobody into
// anything worse than an early compaction; a meter that shows a free segment
// at the cap — or a row that says 99% while the window is full — invites one
// more call that the budget machinery then eats. Overstating room is the lie
// that costs money, so it is the one this arithmetic cannot produce.
// ---------------------------------------------------------------------------

/// The percentage: rounded *up*, and clamped to 99 until the cap is truly
/// reached — `100%` is a claim of exhaustion and is never made early. Past the
/// cap the real figure shows, because a level over its cap is a fact the user
/// should see stated plainly.
fn percent(used: i64, cap: i64) -> i64 {
    let used = used.max(0);
    // Manual ceiling: `i64::div_ceil` is unstable on this toolchain, and both
    // operands are non-negative with a positive divisor, where this form is
    // exact.
    let p = (used * 100 + cap - 1) / cap;
    if used < cap {
        p.min(99)
    } else {
        p
    }
}

/// Filled segments: a partial segment rounds up — so anything used shows as at
/// least one — but the last segment fills only at the cap. Floor would do the
/// opposite: at 99% it shows nine of ten and offers a segment of room that
/// does not exist.
fn filled(used: i64, cap: i64, segments: usize) -> usize {
    let used = used.max(0);
    if used >= cap {
        return segments;
    }
    let f = ((used * segments as i64 + cap - 1) / cap) as usize;
    f.min(segments - 1)
}

/// Filled segments accent, empty dim — but the glyphs differ too (`█`/`░`,
/// `#`/`-`), so the proportion survives `Level::None` on shape alone. The
/// palette's rule that no distinction is colour-only, applied here without
/// needing reversed video: these glyphs are self-distinguishing.
fn meter(used: i64, cap: i64, segments: usize, skin: &Skin, g: &BarGlyphs) -> Vec<Span<'static>> {
    let f = filled(used, cap, segments);
    let mut spans = Vec::new();
    if !g.open.is_empty() {
        spans.push(Span::styled(g.open.to_string(), skin.palette.dim()));
    }
    if f > 0 {
        spans.push(Span::styled(
            g.full.repeat(f),
            skin.palette.style(Role::Accent),
        ));
    }
    if f < segments {
        spans.push(Span::styled(
            g.empty.repeat(segments - f),
            skin.palette.dim(),
        ));
    }
    if !g.close.is_empty() {
        spans.push(Span::styled(g.close.to_string(), skin.palette.dim()));
    }
    spans
}

// endregion: The meter's arithmetic

// region: Small formats
// ---------------------------------------------------------------------------
// Small formats
//
// `clock` duplicates `render.rs`'s private one, kept local rather than made
// public there: that file is shared chrome under concurrent edit today, and a
// four-line function is cheaper than a cross-file seam. Unify when the shell
// settles; the tests pin it to the same outputs `render.rs`'s tests pin.
//
// `group` deliberately does NOT match `render.rs`'s `human`. The old inline
// row abbreviated (`12k`) because it shared one line with everything else;
// the mockup's bar writes the full grouped figure (`12,842`), and this bar
// can afford to — under pressure its ladder drops whole cells rather than
// squeezing them, so abbreviation here would be precision surrendered with
// nothing bought.
// ---------------------------------------------------------------------------

/// `12842` → `12,842`. Grouped, never abbreviated, never truncated — the
/// number is whole or (by the ladder) gone.
fn group(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

// endregion: Small formats

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::{plain, UNICODE};

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    fn ascii_skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), ASCII)
    }

    /// A bar with every field measured — the 200-column happy case.
    fn bar() -> Bar {
        Bar {
            mode: "ASSIST".into(),
            model: "claude-opus-4".into(),
            env: "C:\\src\\emma".into(),
            ctx_used: 68_000,
            ctx_max: 100_000,
            total_used: 12_842,
            total_max: 500_000,
            up: Some(8_128),
            down: Some(4_714),
            elapsed: Some(std::time::Duration::from_secs(72)),
        }
    }

    fn text(width: u16, b: &Bar, s: &Skin) -> String {
        plain(&compose(width, b, s))
    }

    #[test]
    fn at_a_wide_row_every_measured_cell_appears() {
        let out = text(200, &bar(), &skin());
        // ceil(6.8) = 7 of 10 segments; ▊ then ░, contiguous in the plain text.
        let meter = format!("{}{}", "\u{258A}".repeat(7), "\u{2591}".repeat(3));
        for expected in [
            "MODE  ASSIST",
            "1m12s",
            "TOKENS  12,842/500,000",
            // The parenthetical, exactly as the image spaces it: arrow, one
            // space, figure; two spaces before the ↓.
            "(\u{2191} 8,128  \u{2193} 4,714)",
            "CONTEXT  68%",
            meter.as_str(),
            "ENV  claude-opus-4",
            "C:\\src\\emma",
            "/help",
            "/exit",
        ] {
            assert!(out.contains(expected), "{expected:?} missing from: {out}");
        }
    }

    /// The mockup divides the bar with vertical rules: one between each pair
    /// of adjacent cells, and one before the help — four cells plus help,
    /// exactly four rules. And the slack is *shared*: the image's cells are
    /// distributed across the row, so no gap may hoard the leftover width
    /// while the others sit at minimum. A row that packs left and dumps
    /// sixty columns in front of the hints fails the evenness check.
    #[test]
    fn every_cell_boundary_carries_a_rule_and_the_slack_is_shared() {
        let (b, s) = (bar(), skin());
        let line = compose(200, &b, &s);
        assert_eq!(line.width(), 200, "an anchored row fills its width");
        let out = plain(&line);
        let chars: Vec<char> = out.chars().collect();
        let mut gaps = Vec::new();
        for (i, c) in chars.iter().enumerate() {
            if *c != '\u{2502}' {
                continue;
            }
            let before = chars[..i].iter().rev().take_while(|c| **c == ' ').count();
            let after = chars[i + 1..].iter().take_while(|c| **c == ' ').count();
            gaps.push(before + after);
        }
        assert_eq!(
            gaps.len(),
            4,
            "4 rules (MODE|TOKENS|CONTEXT|ENV|help): {out}"
        );
        let (lo, hi) = (gaps.iter().min().unwrap(), gaps.iter().max().unwrap());
        assert!(*lo >= 2, "a rule without air on both sides: {gaps:?}");
        assert!(hi - lo <= 1, "slack hoarded by one gap: {gaps:?} in {out}");
        // The last rule is the help cell's.
        let tail = out.rsplit('\u{2502}').next().unwrap();
        assert!(tail.contains("/help"), "{out}");
    }

    /// A dropped cell takes its rule with it — and a bar with nothing
    /// measured draws no rule at all, because a divider needs two sides.
    #[test]
    fn a_dropped_cell_takes_its_rule_with_it() {
        // Width 4 is the bare percentage: no cells left to divide.
        let out = text(4, &bar(), &skin());
        assert!(!out.contains('\u{2502}'), "{out}");
        let mut b = bar();
        b.mode = String::new();
        b.model = String::new();
        b.env = String::new();
        b.ctx_used = 0;
        b.ctx_max = 0;
        b.total_used = 0;
        b.total_max = 0;
        b.up = None;
        b.down = None;
        b.elapsed = None;
        let out = text(200, &b, &skin());
        assert!(out.contains("/help"), "{out}");
        assert!(
            !out.contains('\u{2502}'),
            "a rule with nothing on its left: {out}"
        );
    }

    /// The bar renders into a buffer through the public entry, and stays
    /// inside its row.
    #[test]
    fn render_writes_the_row_into_the_buffer() {
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &bar(), &skin());
        let row: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row.contains("CONTEXT"), "{row}");
        // And a zero-sized area is a no-op, not a panic.
        render(
            Rect::new(0, 0, 0, 0),
            &mut Buffer::empty(area),
            &bar(),
            &skin(),
        );
    }

    // -----------------------------------------------------------------------
    // Absent measurements
    // -----------------------------------------------------------------------

    /// The standing rule: unknown and zero are different facts. `None` renders
    /// nothing; `Some(0)` renders a zero, because a measured zero was measured.
    #[test]
    fn an_unknown_count_is_absent_and_a_measured_zero_is_zero() {
        let mut b = bar();
        b.up = None;
        b.down = None;
        let out = text(200, &b, &skin());
        assert!(!out.contains('↑'), "{out}");
        assert!(!out.contains('↓'), "{out}");
        assert!(!out.contains('('), "{out}");

        b.up = Some(0);
        b.down = Some(4_714);
        let out = text(200, &b, &skin());
        assert!(out.contains("\u{2191} 0"), "{out}");
        assert!(out.contains("\u{2193} 4,714"), "{out}");

        // A negative count is not a measurement either — and it must not be
        // clamped into a claim of zero.
        b.up = Some(-5);
        let out = text(200, &b, &skin());
        assert!(!out.contains('↑'), "{out}");
    }

    #[test]
    fn no_goal_running_means_no_clock() {
        let mut b = bar();
        b.elapsed = None;
        let out = text(200, &b, &skin());
        assert!(!out.contains("1m12s"), "{out}");
        assert!(out.contains("MODE  ASSIST"), "{out}");
    }

    /// An empty value drops its whole cell, label included: a label over
    /// nothing is the bar pretending a feature exists.
    #[test]
    fn an_empty_value_takes_its_label_with_it() {
        let mut b = bar();
        b.mode = String::new();
        b.model = String::new();
        b.env = String::new();
        b.ctx_used = 0;
        b.ctx_max = 0;
        b.total_used = 0;
        b.total_max = 0;
        b.up = None;
        b.down = None;
        b.elapsed = None;
        let out = text(200, &b, &skin());
        for gone in ["MODE", "ENV", "TOTAL", "CONTEXT", "%"] {
            assert!(!out.contains(gone), "{gone:?} drawn over nothing: {out}");
        }
        // Nothing measured, nothing said — but the hints still work.
        assert!(out.contains("/help"), "{out}");
    }

    /// Mode empty but a goal running: the clock is real and shows, the MODE
    /// label does not.
    #[test]
    fn a_clock_without_a_mode_shows_without_the_label() {
        let mut b = bar();
        b.mode = String::new();
        let out = text(200, &b, &skin());
        assert!(!out.contains("MODE"), "{out}");
        assert!(out.contains("1m12s"), "{out}");
    }

    /// A count with no cap shows the count and invents no proportion.
    #[test]
    fn a_count_with_no_cap_gets_no_percentage_and_no_meter() {
        let mut b = bar();
        b.ctx_max = 0;
        b.ctx_used = 5_000;
        let out = text(200, &b, &skin());
        assert!(out.contains("CONTEXT  5,000"), "{out}");
        assert!(!out.contains('%'), "{out}");
        assert!(!out.contains('\u{258A}'), "{out}");
    }

    // -----------------------------------------------------------------------
    // The two kinds of number
    // -----------------------------------------------------------------------

    /// The confusion that renamed `spend`: two `used/cap` pairs side by side
    /// read as comparable, and the larger "level" looked inverted. Here the
    /// snapshot is a percentage and the accumulation is a raw pair — different
    /// shapes, unmistakable for each other. Pinned as shape: the context cell
    /// carries no slash, the tokens cell no percent sign. The label became
    /// `TOKENS` for the mockup once shape carried the guard; the shape rule
    /// itself is what must never regress.
    #[test]
    fn the_snapshot_and_the_accumulation_have_different_shapes() {
        let out = text(200, &bar(), &skin());
        assert!(out.contains("TOKENS  12,842/500,000"), "{out}");
        assert!(out.contains("CONTEXT  68%"), "{out}");
        assert!(
            !out.contains("68,000/100,000"),
            "the snapshot drawn as a pair: {out}"
        );
        assert!(!out.contains("spend"), "{out}");
        assert!(
            !out.contains("TOTAL"),
            "the pre-mockup label is back: {out}"
        );
        let ctx = out.split("CONTEXT").nth(1).unwrap();
        let ctx = ctx.split('\u{2502}').next().unwrap();
        assert!(!ctx.contains('/'), "a slash in the context cell: {ctx}");
        let tokens = out.split("TOKENS").nth(1).unwrap();
        let tokens = tokens.split('\u{2502}').next().unwrap();
        assert!(
            !tokens.contains('%'),
            "a percent in the tokens cell: {tokens}"
        );
    }

    /// The parenthetical's colours, measured off the image: the ↑ and its
    /// figure are one green span; the ↓ arrow is accent but its figure is
    /// plain foreground. The design doc's prose has been wrong about this
    /// cell twice, so the styles are pinned span by span.
    #[test]
    fn the_down_arrow_is_accent_but_its_figure_is_foreground() {
        let s = skin();
        let g = bar_glyphs(&s);
        let spans = tokens_cell(&bar(), &s, &g, true).expect("a measured cell");
        let up = spans
            .iter()
            .find(|sp| sp.content.contains('\u{2191}'))
            .expect("an up span");
        assert_eq!(up.content.as_ref(), "\u{2191} 8,128");
        assert_eq!(up.style, s.palette.style(Role::Ok));
        let down = spans
            .iter()
            .find(|sp| sp.content.contains('\u{2193}'))
            .expect("a down span");
        assert_eq!(
            down.content.as_ref(),
            "\u{2193}",
            "the accent stops at the arrow"
        );
        assert_eq!(down.style, s.palette.style(Role::Accent));
        let figure = spans
            .iter()
            .find(|sp| sp.content.contains("4,714"))
            .expect("a down figure");
        assert_eq!(figure.style, s.palette.style(Role::Text));
    }

    // -----------------------------------------------------------------------
    // The meter
    // -----------------------------------------------------------------------

    /// The direction of every rounding error: used may be overstated, room
    /// never. Full — the meter's and the percentage's — is claimed only at the
    /// cap.
    #[test]
    fn the_meter_never_reads_full_below_the_cap() {
        for used in [1, 50_000, 99_999] {
            let mut b = bar();
            b.ctx_used = used;
            let out = text(200, &b, &skin());
            assert!(
                out.contains('\u{2591}'),
                "no empty segment at {used}: {out}"
            );
            assert!(!out.contains("100%"), "full claimed at {used}: {out}");
        }
        let mut b = bar();
        b.ctx_used = 100_000;
        let out = text(200, &b, &skin());
        assert!(out.contains("100%"), "{out}");
        assert!(out.contains(&"\u{258A}".repeat(10)), "{out}");
        assert!(!out.contains('\u{2591}'), "{out}");
    }

    /// …and the mirror: anything used shows. A meter that reads empty at 3%
    /// overstates the room left, which is the same lie from the other end.
    #[test]
    fn anything_used_fills_at_least_one_segment() {
        let mut b = bar();
        b.ctx_used = 1;
        let out = text(200, &b, &skin());
        assert!(out.contains('\u{258A}'), "{out}");
        assert!(out.contains("1%"), "{out}");
    }

    /// The filled glyph is the self-separating ▊, never the full block: ten
    /// adjacent full blocks fuse into one solid bar and the segmentation the
    /// owner asked for disappears. If █ ever returns, this goes red.
    #[test]
    fn the_filled_segments_stay_individually_countable() {
        let out = text(200, &bar(), &skin());
        assert!(
            !out.contains('\u{2588}'),
            "the fusing full block is back: {out}"
        );
        assert!(out.contains('\u{258A}'), "{out}");
    }

    #[test]
    fn the_roundings_are_pinned_at_their_edges() {
        assert_eq!(percent(0, 100_000), 0);
        assert_eq!(percent(1, 100_000), 1);
        assert_eq!(percent(99_999, 100_000), 99);
        assert_eq!(percent(100_000, 100_000), 100);
        // Over the cap the true figure shows — it is a fact, stated plainly.
        assert_eq!(percent(105_000, 100_000), 105);
        assert_eq!(filled(0, 100_000, 10), 0);
        assert_eq!(filled(1, 100_000, 10), 1);
        assert_eq!(filled(99_999, 100_000, 10), 9);
        assert_eq!(filled(100_000, 100_000, 10), 10);
        assert_eq!(filled(200_000, 100_000, 10), 10);
    }

    #[test]
    fn the_ascii_meter_is_a_bracketed_gauge() {
        let out = text(200, &bar(), &ascii_skin());
        assert!(out.contains("[#######---]"), "{out}");
        assert!(out.contains("^ 8,128"), "{out}");
        assert!(out.contains("v 4,714"), "{out}");
        assert!(out.contains(" | "), "{out}");
        // The rules degrade to `|` with the rest: four of them, plus the two
        // places the ASCII `sep` glyph is also `|` — the env value's join
        // and `/help | /exit`.
        assert_eq!(out.matches('|').count(), 6, "{out}");
        for unicode_only in ['\u{258A}', '\u{2591}', '\u{2191}', '\u{2193}', '\u{2502}'] {
            assert!(
                !out.contains(unicode_only),
                "{unicode_only} in ASCII: {out}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The drop order
    // -----------------------------------------------------------------------

    /// The ladder holds at every width, written as implications rather than at
    /// hand-picked columns: if a richer feature survived, everything that
    /// outranks it survived too. A reordering of the ladder breaks one of
    /// these at some width, whatever the exact thresholds are.
    #[test]
    fn what_survives_at_any_width_respects_the_order() {
        let (b, s) = (bar(), skin());
        for w in 0..=220u16 {
            let out = text(w, &b, &s);
            let implies = [
                // The hints are the first cell gone, so their presence
                // implies everything that outlives them.
                ("/help", "\u{2191} 8,128"),
                // The ↑/↓ split outlives nothing below it…
                ("\u{2191} 8,128", "ENV"),
                // …and each cell implies the cells that outrank it.
                ("ENV", "TOKENS"),
                ("TOKENS", "MODE"),
                ("MODE", "%"),
                ("\u{258A}", "%"),
            ];
            for (poorer, richer) in implies {
                assert!(
                    !out.contains(poorer) || out.contains(richer),
                    "at {w}: {poorer:?} shown but {richer:?} already dropped: {out}"
                );
            }
        }
    }

    /// The reason the ladder ends where it does: the context level is the one
    /// number that changes what the user does next.
    #[test]
    fn the_percentage_is_the_last_thing_standing() {
        let out = text(4, &bar(), &skin());
        assert_eq!(out.trim(), "68%");
    }

    /// The hard guarantee behind the whole ladder: at no width does the row
    /// overflow, and at no width is a number partially shown. A number is
    /// whole or it is gone. The composed width includes every separator, so
    /// this is also the proof that the rules never push a row over its width
    /// — a separator miscounted anywhere (`sep_width`, the help rule's fixed
    /// cost) surfaces here as an overflow at some width.
    #[test]
    fn no_width_overflows_the_row_or_cuts_a_number() {
        let (b, s) = (bar(), skin());
        for w in 0..=220u16 {
            let line = compose(w, &b, &s);
            assert!(
                line.width() <= usize::from(w),
                "{} columns composed for {w}: {:?}",
                line.width(),
                plain(&line)
            );
            let out = plain(&line);
            // Wherever a cell's label made it, its number made it whole.
            for (marker, whole) in [
                ("TOKENS", "12,842/500,000"),
                ("%", "68%"),
                ("\u{2191}", "\u{2191} 8,128"),
                ("\u{2193}", "\u{2193} 4,714"),
                ("1m", "1m12s"),
            ] {
                assert!(
                    !out.contains(marker) || out.contains(whole),
                    "at {w}: {marker:?} present but {whole:?} cut: {out}"
                );
            }
        }
    }

    /// Under pressure the env value is fitted — it is a name, and `clau…`
    /// still identifies a model — while every number on the row stays whole.
    #[test]
    fn a_name_is_fitted_before_any_cell_is_lost_and_no_number_ever_is() {
        let (b, s) = (bar(), skin());
        // The widest row at which the env value had to be shortened. Found by
        // scanning rather than by arithmetic on the composed width, because
        // the help hints are right-aligned: a composed line is always exactly
        // as wide as its row, so the everything-whole width is not observable
        // from the outside.
        let w = (0..=300u16)
            .rev()
            .find(|w| text(*w, &b, &s).contains('…'))
            .expect("no width ever fitted the env value");
        let out = text(w, &b, &s);
        assert!(out.contains("ENV"), "{out}");
        assert!(out.contains("12,842/500,000"), "{out}");
        assert!(out.contains("\u{2191} 8,128"), "{out}");
        assert!(out.contains("68%"), "{out}");
    }

    /// The grouping is the mockup's (`12,842`), pinned digit by digit — and
    /// it never abbreviates, because `12k` beside a `12,842` mockup was the
    /// exact gap the owner reported. The clock stays pinned to the outputs
    /// `render.rs` pins, so the local duplicate cannot drift.
    #[test]
    fn numbers_are_grouped_and_clocks_read_the_way_the_old_row_wrote_them() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000), "1,000");
        assert_eq!(group(12_842), "12,842");
        assert_eq!(group(500_000), "500,000");
        assert_eq!(group(1_234_567), "1,234,567");
        // Negative counts are filtered before display, but the formatter must
        // still be correct on them: a helper that panics or garbles on an
        // input it "never gets" is a trap for the next caller.
        assert_eq!(group(-1_000), "-1,000");
        assert_eq!(clock(std::time::Duration::from_secs(9)), "9s");
        assert_eq!(clock(std::time::Duration::from_secs(72)), "1m12s");
        assert_eq!(clock(std::time::Duration::from_secs(7_300)), "2h01m");
    }
}
