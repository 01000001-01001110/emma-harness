//! The configured status line, on the screen: how its output is turned into
//! cells, and how running it stays off the paint path.
//!
//! The program, its containment and its timeout live in
//! [`emma_harness::StatusLine`], beside the hooks whose supervisor it reuses.
//! What is here is the two things the terminal owns.
//!
//! # Running it can never be part of drawing
//!
//! `Frame::paint` is called from the keyboard reader, from the clock thread,
//! from every transcript write and from the middle of a streamed answer. If any
//! of those could end up waiting on a subprocess, a status script that blocks —
//! a `git` command against a network share, an `ssh` that will not connect —
//! becomes a terminal that has stopped echoing what the user types. So paint
//! **reads a value and never produces one**: it copies whatever string is in
//! `View::custom_status` and returns.
//!
//! Producing the value is a task, fed by a one-slot channel. Sending is
//! `try_send` and a full channel is dropped on the floor, which is the
//! coalescing: while a run is in flight, all the triggers that arrive collapse
//! to the one request already queued. Claude Code debounces the same way at the
//! same 300ms, for the same reason — a burst of assistant messages should cost
//! one invocation, not eight.
//!
//! **What is on screen while all this is happening:**
//!
//! - Nothing configured — the built-in status, forever. This is the common case.
//! - Configured, first run still in flight — the built-in status. Not a blank
//!   row and not a spinner: the built-in line is *true*, and true-but-about-to-
//!   be-replaced beats an empty row that reads as a crash.
//! - The program failed, timed out or printed nothing — the built-in status
//!   again, and the reason is written to the transcript **once per distinct
//!   message**, so a script that is broken on every invocation says so once
//!   rather than five times a second. "Timed out" here now means the program
//!   itself did not finish. Until 2026-08-14 it also caught a program that had
//!   exited in milliseconds but had started something which inherited its
//!   stdout: the supervisor was waiting for the pipe to close rather than for
//!   the program to exit, so the row said `statusLine timed out after 2000ms`
//!   on every debounced repaint of a script that had answered every time. The
//!   fix is in `emma_harness::hooks::exec`; nothing in this file changed, which
//!   is the argument for the status line reusing that supervisor rather than
//!   owning one.
//! - The program answered — its output, and the built-in status is gone. That
//!   is the deal, and `notes/status-line.md` says so out loud: model, cwd,
//!   elapsed, ctx and spend are Emma's line, and a script that does not print
//!   them has replaced them with nothing.
//!
//! # Why its bytes are parsed rather than printed
//!
//! Claude Code honours ANSI colour in a status script's stdout, so Emma does
//! too. It cannot do it the way Claude Code does — by letting the bytes through
//! — because these bytes go into a ratatui cell buffer, and ratatui draws by
//! diffing what it believes is on screen against what it wants there. **An
//! escape byte written into a cell is a byte ratatui counts as one column and
//! the terminal counts as none**, and from that moment the diff is wrong about
//! every row below it: the frame's whole scrollback design rests on ratatui
//! knowing where its own rows are.
//!
//! So [`to_line`] is a translator with a closed output: every SGR parameter it
//! understands becomes a ratatui [`Style`], every escape sequence it does not
//! understand is **dropped**, and no control byte survives into a cell. A status
//! script emitting an OSC 8 hyperlink gets its text and not its link, which is a
//! feature not arriving — a status script emitting one into the raw buffer would
//! be a corrupted frame, which is the product breaking.

use std::sync::{Arc, Weak};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use emma_harness::{
    StatusContext, StatusCost, StatusLine, StatusModel, StatusPayload, StatusWorkspace,
};

use super::frame::Frame;

// region: The trigger and the worker
// ---------------------------------------------------------------------------

/// How long a burst of triggers is allowed to collapse into one run.
///
/// Claude Code's number, and it is right for the same reason: the events that
/// move a status line — a model call returning, a goal starting — arrive in
/// clumps, and a program run eight times in a second is eight processes to show
/// one row.
const DEBOUNCE_MS: u64 = 300;

/// The channel a repaint-adjacent caller pushes work onto.
///
/// One slot, `try_send`, failures ignored. That is not laziness about errors: a
/// full channel means a request is *already waiting* with fresher data behind
/// it, and the correct response to "the status needs updating" when the status
/// is already queued for updating is to do nothing.
pub type Requests = tokio::sync::mpsc::Sender<StatusPayload>;

/// Start the task that keeps [`View::custom_status`](super::view::View) fed.
///
/// A [`Weak`] for the same reason [`super::frame::spawn_clock`] holds one: the
/// task must die with the frame rather than keep it alive, and a status line
/// running for a terminal that has been restored is a process spawned into a
/// session that ended.
pub fn spawn(frame: Weak<Frame>, line: Arc<StatusLine>) -> Requests {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<StatusPayload>(1);
    tokio::spawn(async move {
        // The last thing said out loud, so a program that fails identically on
        // every invocation is reported once rather than on every repaint.
        let mut said: Option<String> = None;
        while let Some(mut payload) = rx.recv().await {
            tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS)).await;
            // Whatever arrived during the debounce wins: the newest measurement
            // is the only one worth drawing.
            while let Ok(newer) = rx.try_recv() {
                payload = newer;
            }
            // Upgraded per iteration rather than held, so a frame torn down
            // mid-sleep ends this task at the next turn of the loop.
            let Some(frame) = frame.upgrade() else {
                return;
            };
            let (cols, rows) = super::terminal_size().unwrap_or((80, 24));
            match line.run(&payload, cols, rows).await {
                Ok(out) => match first_line(&out) {
                    // A program that printed nothing has not produced a status
                    // line, and blanking the row would hide the built-in one
                    // behind an empty string.
                    None => note_once(&frame, &mut said, "statusLine printed nothing"),
                    Some(text) => {
                        said = None;
                        frame.set_custom_status(Some(text));
                    }
                },
                Err(why) => {
                    frame.set_custom_status(None);
                    note_once(&frame, &mut said, &why);
                }
            }
        }
    });
    tx
}

/// Say it if it has not just been said. The transcript is a record somebody
/// reads afterwards; the same sentence forty times is not a record.
fn note_once(frame: &Frame, said: &mut Option<String>, why: &str) {
    if said.as_deref() == Some(why) {
        return;
    }
    *said = Some(why.to_string());
    frame.note_line(&format!(
        "{why}. The built-in status line is being drawn instead."
    ));
}

/// The first line of a program's output, trimmed, or `None` if there is not one.
///
/// **Claude Code renders every line a status script prints as its own row.**
/// Emma takes the first and drops the rest, because the viewport's height is
/// fixed at install and a status line that could claim two rows would be a
/// viewport that resizes under a running program — the class of bug that ate the
/// last two attempts at this file. Stated here rather than discovered by
/// somebody whose two-line script silently lost its second line.
fn first_line(out: &str) -> Option<String> {
    out.lines()
        .map(str::trim_end)
        .find(|l| !l.trim().is_empty())
        .map(str::to_string)
}

// endregion: The trigger and the worker

// region: What Emma can honestly say about itself
// ---------------------------------------------------------------------------

/// Build the stdin payload out of what the viewport actually knows.
///
/// Every field here is a measurement or a fact, never an estimate — the rule
/// [`Status`](super::render::Status) is written to, applied one layer out. The
/// fields Emma cannot answer are absent from the struct entirely; see
/// [`StatusPayload`].
pub fn payload(
    status: &super::render::Status,
    session_path: &str,
    elapsed_ms: u64,
) -> StatusPayload {
    StatusPayload {
        cwd: status.cwd.clone(),
        session_id: status.session.clone(),
        transcript_path: session_path.to_string(),
        model: StatusModel {
            // Emma has one name for a model and no prettier one to offer, so
            // both fields carry it. A script reading `display_name` — which is
            // what every published example reads — gets something real.
            id: status.model.clone(),
            display_name: status.model.clone(),
        },
        workspace: StatusWorkspace {
            current_dir: status.cwd.clone(),
            // Emma does not change directory mid-session and has no `/add-dir`,
            // so these are the same directory and an empty list. Truthful, and
            // the shape a script expects.
            project_dir: status.cwd.clone(),
            added_dirs: Vec::new(),
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
        cost: StatusCost {
            total_duration_ms: elapsed_ms,
        },
        context_window: status
            .context
            .map(|(used, cap)| StatusContext::new(used, cap)),
        // Against the provider's own input count, and false until there is one.
        // The threshold is fixed at 200k in Claude Code regardless of the actual
        // window, and copying that is the point: a script branching on it here
        // must branch the same way it does there.
        exceeds_200k_tokens: status.context.is_some_and(|(used, _)| used > 200_000),
    }
}

// endregion: What Emma can honestly say about itself

// region: Bytes to cells
// ---------------------------------------------------------------------------
// Bytes to cells
//
// The translator. Pure, and its guarantee is negative: whatever goes in, no
// escape byte and no control character comes out the other side.
// ---------------------------------------------------------------------------

/// Turn a status program's output into a styled line.
///
/// `padding` is Claude Code's `statusLine.padding` — leading spaces, counted in
/// characters, defaulting to none.
pub fn to_line(text: &str, padding: u16, base: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(usize::from(padding))));
    }
    let mut style = base;
    let mut buf = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            // Everything that is not an escape is either a printable character
            // or a control byte, and a control byte in a cell is the same defect
            // as an escape in one: a tab, a carriage return or a bell would be
            // stored as a symbol the terminal then acts on.
            if !c.is_control() {
                buf.push(c);
            }
            continue;
        }
        // An escape sequence. Whatever it is, its bytes end here — the only
        // question is whether it also changes the style.
        match consume_escape(&mut chars) {
            Some(params) => {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), style));
                }
                style = apply_sgr(style, base, &params);
            }
            None => {
                // A sequence that is not SGR — a cursor move, an OSC 8 link, a
                // truncated escape at end of input. Dropped whole. It cannot be
                // honoured in a cell buffer and it must not be stored in one.
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), style));
                }
            }
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    Line::from(spans)
}

/// Eat one escape sequence and return its parameters if it was an SGR (`…m`).
///
/// The iterator is left just past the sequence either way, which is what makes
/// "no escape byte survives" true rather than approximate: an unrecognised
/// sequence is consumed and discarded, never emitted a character at a time.
fn consume_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<Vec<i64>> {
    match chars.next() {
        // CSI: parameters, then one final byte in `@`–`~`.
        Some('[') => {
            let mut params = String::new();
            for c in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&c) {
                    return (c == 'm').then(|| parse_params(&params));
                }
                params.push(c);
            }
            None
        }
        // OSC: runs to a BEL or an ST (`ESC \`). This is where a hyperlink
        // lives, and where an unterminated one would otherwise eat the line.
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
            None
        }
        // Anything else is a two-byte sequence, already consumed.
        _ => None,
    }
}

/// `1;38;5;208` → `[1, 38, 5, 208]`. An empty parameter is zero, which is what
/// terminals do: bare `ESC[m` is a reset.
fn parse_params(raw: &str) -> Vec<i64> {
    let raw = raw.trim_start_matches(['?', '>', '<', '=']);
    if raw.is_empty() {
        return vec![0];
    }
    raw.split(&[';', ':'][..])
        .map(|p| p.parse::<i64>().unwrap_or(0))
        .collect()
}

/// Fold SGR parameters into a style. Unknown ones are ignored rather than
/// guessed at.
///
/// `base` is what a reset returns to — the palette's own idea of a status row —
/// so `ESC[0m` in the middle of a script's output lands somewhere legible
/// instead of on the terminal's raw default.
fn apply_sgr(mut style: Style, base: Style, params: &[i64]) -> Style {
    let mut i = 0;
    while i < params.len() {
        match params[i] {
            0 => style = base,
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            30..=37 => style = style.fg(basic(params[i] - 30, false)),
            90..=97 => style = style.fg(basic(params[i] - 90, true)),
            39 => style = style.fg(Color::Reset),
            40..=47 => style = style.bg(basic(params[i] - 40, false)),
            100..=107 => style = style.bg(basic(params[i] - 100, true)),
            49 => style = style.bg(Color::Reset),
            // Extended colour: `38;5;n` or `38;2;r;g;b`, and the same at 48 for
            // background. The consumed length varies, so `i` moves by hand.
            38 | 48 => {
                let fg = params[i] == 38;
                let (color, used) = extended(&params[i + 1..]);
                if let Some(color) = color {
                    style = if fg { style.fg(color) } else { style.bg(color) };
                }
                i += used;
            }
            _ => {}
        }
        i += 1;
    }
    style
}

fn basic(n: i64, bright: bool) -> Color {
    match (n, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        (_, false) => Color::Gray,
        (_, true) => Color::White,
    }
}

/// The tail of a `38`/`48`, and how many parameters it ate.
fn extended(rest: &[i64]) -> (Option<Color>, usize) {
    match rest.first() {
        Some(5) => match rest.get(1) {
            Some(&n) if (0..=255).contains(&n) => (Some(Color::Indexed(n as u8)), 2),
            _ => (None, 1),
        },
        Some(2) => match (rest.get(1), rest.get(2), rest.get(3)) {
            (Some(&r), Some(&g), Some(&b)) => (Some(Color::Rgb(clamp(r), clamp(g), clamp(b))), 4),
            _ => (None, rest.len().min(3)),
        },
        _ => (None, 0),
    }
}

fn clamp(n: i64) -> u8 {
    n.clamp(0, 255) as u8
}

// endregion: Bytes to cells

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn plain(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// **The guarantee the frame depends on: nothing this function returns can
    /// carry an escape byte or a control character into a cell.**
    ///
    /// ratatui draws by diffing what it believes is on screen against what it
    /// wants there, and it counts a stored escape byte as one column while the
    /// terminal counts it as none. One of those in a cell and the frame is wrong
    /// about every row below it — which, in a design whose whole point is that
    /// transcript output above the viewport reaches scrollback intact, is the
    /// product breaking rather than a cosmetic slip.
    ///
    /// Written as "over everything hostile at once" rather than one case per
    /// test, because the property is universal and a per-case test passes the
    /// moment somebody adds a sequence nobody thought of.
    #[test]
    fn no_escape_byte_or_control_character_ever_reaches_a_cell() {
        let hostile = [
            "\x1b[31mred\x1b[0m",
            // A hyperlink: OSC 8, which cannot be honoured in a cell buffer.
            "\x1b]8;;https://example.com\x07click\x1b]8;;\x07",
            // An OSC terminated by ST rather than BEL.
            "\x1b]0;a title\x1b\\after",
            // Cursor movement and screen erase, which would move the frame's
            // idea of where it is.
            "\x1b[2J\x1b[10;20Hmoved\x1b[K",
            // A truncated escape at end of input.
            "trailing\x1b[",
            "just an escape\x1b",
            // Raw control bytes with no escape at all.
            "a\rb\tc\x07d\x08e",
            // A scroll region — the exact sequence this project's scrollback
            // defect was made of. It must not reach the terminal from here.
            "\x1b[2;20rregion",
            "\x1b[38;2;255;0;0mtruecolor\x1b[m",
            "",
        ];
        for input in hostile {
            let line = to_line(input, 0, Style::default());
            let text = plain(&line);
            assert!(
                !text.contains('\x1b'),
                "an escape byte reached a cell from {input:?}: {text:?}"
            );
            assert!(
                !text.chars().any(char::is_control),
                "a control character reached a cell from {input:?}: {text:?}"
            );
        }
    }

    /// The sequences above are dropped, but the *text* around them survives —
    /// otherwise the safe thing would be to render nothing, which is not a
    /// status line.
    #[test]
    fn the_text_around_a_sequence_survives_even_when_the_sequence_does_not() {
        assert_eq!(
            plain(&to_line(
                "\x1b]8;;https://example.com\x07click\x1b]8;;\x07",
                0,
                Style::default()
            )),
            "click"
        );
        assert_eq!(
            plain(&to_line(
                "\x1b[2J\x1b[10;20Hmoved\x1b[K",
                0,
                Style::default()
            )),
            "moved"
        );
        assert_eq!(
            plain(&to_line("trailing\x1b[", 0, Style::default())),
            "trailing"
        );
    }

    /// Colour is honoured, which is the half of the contract that makes the
    /// dropping above acceptable: a status script's `\033[32m` is why anybody
    /// writes one.
    #[test]
    fn ansi_colour_becomes_style_rather_than_being_thrown_away() {
        let line = to_line("\x1b[32mok\x1b[0m fail", 0, Style::default());
        let green = line
            .spans
            .iter()
            .find(|s| s.content.contains("ok"))
            .expect("the coloured text is missing");
        assert_eq!(green.style.fg, Some(Color::Green));
        // …and the reset really resets, so the rest of the line is not green.
        let after = line
            .spans
            .iter()
            .find(|s| s.content.contains("fail"))
            .expect("text after the reset is missing");
        assert_ne!(after.style.fg, Some(Color::Green));
    }

    #[test]
    fn bold_dim_reverse_and_both_extended_colour_forms_are_understood() {
        let bold = to_line("\x1b[1mx", 0, Style::default());
        assert!(bold.spans[0].style.add_modifier.contains(Modifier::BOLD));

        let indexed = to_line("\x1b[38;5;208mx", 0, Style::default());
        assert_eq!(indexed.spans[0].style.fg, Some(Color::Indexed(208)));

        let truecolor = to_line("\x1b[38;2;250;189;47mx", 0, Style::default());
        assert_eq!(truecolor.spans[0].style.fg, Some(Color::Rgb(250, 189, 47)));

        // A background, and the bright range, which a real script uses for a
        // context meter.
        let chip = to_line("\x1b[41;97mx", 0, Style::default());
        assert_eq!(chip.spans[0].style.bg, Some(Color::Red));
        assert_eq!(chip.spans[0].style.fg, Some(Color::White));
    }

    /// A reset returns to the palette's status style, not to the terminal's raw
    /// default — otherwise `\033[0m` halfway through a script's output would
    /// drop the row onto whatever colours the user's shell happens to have.
    #[test]
    fn a_reset_returns_to_the_style_the_row_would_have_had() {
        let base = Style::default().fg(Color::Cyan);
        let line = to_line("\x1b[31mred\x1b[0mback", 0, base);
        let back = line
            .spans
            .iter()
            .find(|s| s.content.contains("back"))
            .expect("text after the reset");
        assert_eq!(back.style.fg, Some(Color::Cyan));
    }

    /// `padding` is Claude Code's field and it means leading characters.
    #[test]
    fn padding_indents_the_row_by_the_characters_it_asks_for() {
        assert_eq!(plain(&to_line("x", 3, Style::default())), "   x");
        assert_eq!(plain(&to_line("x", 0, Style::default())), "x");
    }

    /// The viewport's height is fixed at install, so a status line may claim one
    /// row and never two. Claude Code renders every printed line as its own row;
    /// Emma takes the first non-empty one, which is a stated limit rather than a
    /// silent loss.
    #[test]
    fn a_program_that_prints_several_lines_yields_the_first_of_them() {
        assert_eq!(first_line("one\ntwo\nthree").as_deref(), Some("one"));
        // Leading blank lines are somebody's `echo`, not their status line.
        assert_eq!(first_line("\n\n  real  \n").as_deref(), Some("  real"));
        // A program that printed nothing has not produced a status line, and
        // must not blank the row that would otherwise carry the built-in one.
        assert_eq!(first_line(""), None);
        assert_eq!(first_line("\n  \n"), None);
    }
}
