//! The inline viewport, and the rule that keeps scrollback working.
//!
//! # The rule
//!
//! **`Viewport::Inline` renders into the normal screen buffer.** There is no
//! alternate screen — that costs scrollback and mouse selection, and Emma's
//! output is a transcript people read after the run. Everything above the
//! viewport is ordinary terminal output that the terminal owns, scrolls, wraps
//! and keeps.
//!
//! **Transcript lines go out through `Terminal::insert_before`.** In the build
//! Emma uses, that is implemented by putting the cursor on the last row of the
//! screen and printing newlines — the same thing `println!` does, and the only
//! mechanism a terminal actually captures into scrollback. The lines are then
//! drawn into the space that opened up and the viewport is redrawn under them.
//!
//! **ratatui's `scrolling-regions` feature must never be enabled.** With it on,
//! `insert_before` is implemented with `ESC[{top};{bottom}r` instead. A DEC
//! scrolling region whose top margin is below row 1 *discards* the lines that
//! leave it: scrollback capture only happens when the region is the whole
//! screen. That is not a theory — it is exactly what shipped here once before,
//! and the owner's first complaint was that he could not scroll. The feature is
//! off in `Cargo.toml` and a test asserts that it stays off, because the failure
//! it causes is invisible to every other test in this repository.
//!
//! # Why raw mode
//!
//! ratatui draws by diffing what it believes is on screen against what it wants
//! there. Cooked-mode echo writes characters it does not know about, onto rows
//! it thinks it owns, and the diff is wrong from that moment on. So the frame
//! takes raw mode and Emma echoes the input itself — see [`super::input`],
//! which also owns delivering Ctrl-C, because raw mode is precisely the state
//! in which the terminal stops delivering it.
//!
//! # Restoring
//!
//! [`restore_terminal`] is idempotent, global, and needs no reference to
//! anything — a panic hook has no `self`. It undoes exactly four things: raw
//! mode, the rows the viewport is drawn on, the Windows console mode, and the
//! Windows output code page Emma set for itself at startup. The
//! failure it prevents is the one people uninstall over: a shell prompt drawn
//! on top of a viewport we left behind, in a terminal that is no longer echoing
//! what they type.

use std::io::{IsTerminal, Stdout, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, Once, Weak};
use std::time::{Duration, Instant};

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::render::{rows_used, Skin};
use super::view::{Mode, Prompt, View};

// region: Restoring
// ---------------------------------------------------------------------------
// Restoring
//
// Globals rather than fields, because the panic hook, `Drop` and an exit path
// that calls `process::exit` all have to reach this and only one of them has a
// `Frame` in hand.
// ---------------------------------------------------------------------------

/// Set once the viewport has been drawn, cleared once it has been cleaned up.
static FRAME_ON: AtomicBool = AtomicBool::new(false);
static RAW_ON: AtomicBool = AtomicBool::new(false);
/// How many rows the cursor sits below the top of the viewport, so the erase
/// can climb exactly that far. `u16::MAX` means nothing is drawn.
static CURSOR_ROW: AtomicU16 = AtomicU16::new(u16::MAX);
static PANIC_HOOK: Once = Once::new();

/// Take the viewport off the screen and leave the terminal as it was found.
///
/// Only relative cursor movement, and only upwards by a number that was written
/// down when the viewport was last drawn. Absolute addressing is not used
/// anywhere here: on at least one common Windows console the row numbers a
/// program can compute are viewport rows while `SetConsoleCursorPosition` reads
/// them as *screen buffer* rows, which is where a repaint aimed at "row 40" of a
/// nine-thousand-row buffer goes to be invisible.
pub fn restore_terminal() {
    if !FRAME_ON.swap(false, Ordering::SeqCst) {
        return;
    }
    if RAW_ON.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
    }
    let mut out = String::new();
    let row = CURSOR_ROW.swap(u16::MAX, Ordering::SeqCst);
    if row != u16::MAX {
        if row > 0 {
            out.push_str(&format!("\x1b[{row}A"));
        }
        // Back to column one, then wipe everything from here to the bottom of
        // the screen. What is above is the transcript and is not ours.
        out.push_str("\r\x1b[J");
    }
    // Show the cursor and drop every attribute: a frame torn down mid-paint
    // could otherwise leave a shell prompt bold, or invisible.
    out.push_str("\x1b[?25h\x1b[0m");
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    #[cfg(windows)]
    {
        super::restore_console_mode();
        // The code page Emma set for itself. Restored last, after the last
        // byte this function writes has gone out, so the escape sequences above
        // are decoded by the console we were drawing under.
        super::restore_console_cp();
    }
}

// endregion: Restoring

// region: The frame
// ---------------------------------------------------------------------------

/// How many rows the viewport takes.
///
/// A third of the window, bounded. Ten rows is enough for a status line, a hint,
/// an input box and five rows of a diff; five is the least that can hold a
/// question and the keys to answer it. Below that the fallback path is a better
/// product than a viewport occupying most of the screen — see
/// `super::fallback_reason`, which refuses outright under eight rows.
pub fn view_rows(screen_rows: u16) -> u16 {
    (screen_rows / 3).clamp(5, 10)
}

/// One transcript insert is capped here. A tool that returns fifty thousand
/// lines would otherwise ask the terminal to make fifty thousand rows of room
/// in one call; the render path already caps a *result* at eight lines, and this
/// is the backstop for everything else.
const MAX_INSERT_ROWS: u16 = 500;

pub struct Frame {
    inner: Mutex<Inner>,
    skin: Skin,
    /// Where a request for a fresh status line is posted, once one has been
    /// configured. A separate lock from `inner` on purpose: it is taken from
    /// inside paths that already hold `inner`, and one mutex for two unrelated
    /// things is how a repaint ends up waiting on a subprocess.
    status_requests: Mutex<Option<super::statusline::Requests>>,
}

struct Inner {
    term: Terminal<CrosstermBackend<Stdout>>,
    view: View,
    /// When the running goal started. The elapsed field on the status line is
    /// computed from this at paint time rather than stored, which is the whole
    /// reason it can be trusted: there is no copy of it to go stale.
    started: Option<Instant>,
    /// `(max_context, max_tokens)`, so a measurement arriving before or after
    /// the budgets are set lands against the same caps either way.
    caps: (i64, i64),
    /// The transcript file, for the `transcript_path` a status program is
    /// handed. The status *line* carries only the stem, which is what fits on
    /// screen; a script that wants to read the transcript needs the whole path.
    transcript: String,
}

impl Frame {
    /// Install the viewport, or decide not to.
    ///
    /// Every `None` is a fallback to plain line output and the bias is heavily
    /// towards taking it: a plain-but-correct terminal beats a pretty-but-broken
    /// one.
    pub fn install(skin: Skin) -> Option<Arc<Frame>> {
        let size = super::terminal_size();
        if super::fallback_reason(
            std::io::stdout().is_terminal(),
            std::io::stdin().is_terminal(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var_os("EMMA_NO_FRAME").is_some(),
            size,
        )
        .is_some()
        {
            return None;
        }
        // Windows is the hazard, so this is a *verified* enable rather than a
        // hopeful one: read the console mode, set the VT bit, read it back, and
        // fall back unless it stuck. It is also what decides which of
        // crossterm's two code paths runs — with VT on it writes escape
        // sequences, and without it makes WinAPI calls in screen-buffer
        // coordinates, which is the bug this whole file is written around.
        #[cfg(windows)]
        if !super::enable_vt() {
            return None;
        }
        let (_, rows) = size?;

        // Whatever is on the current line is somebody's shell prompt, and the
        // viewport should not be drawn on top of it.
        let mut stdout = std::io::stdout();
        stdout.write_all(b"\r\n").ok()?;
        stdout.flush().ok()?;

        enable_raw_mode().ok()?;
        RAW_ON.store(true, Ordering::SeqCst);

        let terminal = match Terminal::with_options(
            CrosstermBackend::new(std::io::stdout()),
            TerminalOptions {
                viewport: Viewport::Inline(view_rows(rows)),
            },
        ) {
            Ok(t) => t,
            Err(_) => {
                // Half-installed is the worst state to leave a terminal in.
                if RAW_ON.swap(false, Ordering::SeqCst) {
                    let _ = disable_raw_mode();
                }
                return None;
            }
        };

        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                // Before the message, so the message is readable and so the
                // terminal is echoing again by the time anybody reads it.
                restore_terminal();
                previous(info);
            }));
        });
        FRAME_ON.store(true, Ordering::SeqCst);

        let frame = Arc::new(Frame {
            inner: Mutex::new(Inner {
                term: terminal,
                view: View::new(skin),
                started: None,
                caps: (0, 0),
                transcript: String::new(),
            }),
            skin,
            status_requests: Mutex::new(None),
        });
        frame.draw();
        spawn_clock(Arc::downgrade(&frame));
        Some(frame)
    }

    /// A poisoned lock means a panic mid-paint. The frame is already being torn
    /// down by the hook; carrying on with the inner value is strictly better
    /// than a second panic on the way out.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Repaint the viewport.
    pub fn draw(&self) {
        let mut inner = self.lock();
        inner.paint();
    }

    /// Ordinary transcript output: written above the viewport, into the
    /// terminal's own scrollback, exactly once.
    pub fn write_lines(&self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }
        let mut inner = self.lock();
        inner.emit(lines);
        inner.paint();
    }

    /// Assistant text as it arrives.
    ///
    /// Complete lines go to the transcript; the fragment after the last newline
    /// stays in the viewport, where it can still be added to. A fragment
    /// inserted above the viewport could never be extended, so a paragraph
    /// streamed without newlines would arrive as a column of one-word lines.
    pub fn prose(&self, text: &str) {
        let mut inner = self.lock();
        inner.view.partial.push_str(text);
        let mut done: Vec<Line<'static>> = Vec::new();
        while let Some(i) = inner.view.partial.find('\n') {
            let line: String = inner.view.partial.drain(..=i).collect();
            done.push(self.skin.prose(line.trim_end_matches(['\r', '\n'])));
        }
        inner.emit(done);
        inner.paint();
    }

    /// The assistant stopped talking: whatever is still in the viewport becomes
    /// a transcript line, followed by the blank line that separates one answer
    /// from what comes next.
    pub fn flush_prose(&self) {
        let mut inner = self.lock();
        let held = std::mem::take(&mut inner.view.partial);
        let mut lines = Vec::new();
        if !held.trim().is_empty() {
            lines.push(self.skin.prose(held.trim_end()));
        }
        lines.push(Line::default());
        inner.emit(lines);
        inner.paint();
    }

    pub fn set_input(&self, text: &str, cursor: usize) {
        let mut inner = self.lock();
        inner.view.input = text.to_string();
        inner.view.cursor = cursor;
    }

    pub fn set_prompt(&self, prompt: Option<Prompt>) {
        let mut inner = self.lock();
        // A question outranks the menu, so putting one up takes the menu down
        // rather than drawing them over each other. The reader also stops
        // syncing it — see [`super::input`] — and this is the second half of
        // the same rule, for the menu that was already open when the question
        // arrived.
        if prompt.is_some() {
            inner.view.menu = None;
        }
        inner.view.prompt = prompt;
        inner.paint();
    }

    /// The command menu, as the viewport should draw it. `None` closes it.
    pub fn set_menu(&self, menu: Option<super::menu::MenuView>) {
        let mut inner = self.lock();
        inner.view.menu = menu;
    }

    /// Whether a question is on screen. Asked by the reader before it lets `/`
    /// open anything: while an approval is up, the keyboard is for answering
    /// it.
    pub fn prompt_pending(&self) -> bool {
        self.lock().view.prompt.is_some()
    }

    pub fn set_identity(&self, model: &str, cwd: &str, session: &str, transcript: &str) {
        {
            let mut inner = self.lock();
            inner.view.status.model = model.to_string();
            inner.view.status.cwd = cwd.to_string();
            inner.view.status.session = session.to_string();
            inner.transcript = transcript.to_string();
            inner.paint();
        }
        // The first invocation. Claude Code runs a status line once when a
        // session starts, before anything else has happened, and so does this —
        // the row is otherwise the built-in one until the first model call
        // returns, which on a slow first turn is a long time to show something
        // the user configured away.
        self.request_status();
    }

    // -----------------------------------------------------------------------
    // The configured status line
    //
    // Two directions, and the split between them is the safety property: a
    // *request* is a non-blocking post to a channel, and a *result* arrives
    // later from a task. Nothing on this side of the boundary ever waits on a
    // process. See `super::statusline`.
    // -----------------------------------------------------------------------

    /// Adopt a configured status program, and start the task that runs it.
    ///
    /// `Weak`, so the task dies with the frame — the same arrangement
    /// [`spawn_clock`] uses and for the same reason. Called once, from
    /// `Term::set_status_source`.
    pub fn set_status_source(self: &Arc<Self>, line: Arc<emma_harness::StatusLine>) {
        let padding = line.padding;
        {
            let mut inner = self.lock();
            inner.view.status_padding = padding;
        }
        let tx = super::statusline::spawn(Arc::downgrade(self), line);
        *self
            .status_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }

    /// Ask for a fresh status line, with the measurements as they stand.
    ///
    /// **This function must never block, and it is the reason the whole feature
    /// is safe.** It builds a small struct, posts it into a one-slot channel and
    /// returns; a full channel means a request with fresher data is already
    /// queued behind the one in flight, and the right answer to that is to do
    /// nothing. It is called from the paths that move the status — a goal
    /// starting or ending, a model call reporting — which are the same events
    /// Claude Code re-runs a status line on.
    pub fn request_status(&self) {
        let guard = self
            .status_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            return;
        };
        let payload = {
            let mut inner = self.lock();
            inner.view.status.elapsed = inner.started.map(|t| t.elapsed());
            super::statusline::payload(
                &inner.view.status,
                &inner.transcript,
                inner
                    .view
                    .status
                    .elapsed
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            )
        };
        let _ = tx.try_send(payload);
    }

    /// What the program printed, or `None` to go back to the built-in line.
    /// Called from the task; the only writer.
    pub fn set_custom_status(&self, text: Option<String>) {
        let mut inner = self.lock();
        inner.view.custom_status = text;
        inner.paint();
    }

    /// One dim transcript line, for the task to explain itself with. The skin is
    /// the frame's, so a status-line failure reads like every other note rather
    /// than like a different program's output.
    pub fn note_line(&self, text: &str) {
        self.write_lines(self.skin.note(text));
    }

    /// A goal started. The clock starts here and is read at paint time, so what
    /// is on screen is the elapsed time and not a copy of it.
    pub fn goal_started(&self) {
        {
            let mut inner = self.lock();
            inner.started = Some(Instant::now());
            inner.view.mode = Mode::Working;
            // The previous goal's spend is not this goal's spend, and a number
            // left over from the last one is exactly the stale readout this line
            // exists to not have.
            inner.view.status.spend = None;
            inner.paint();
        }
        // Outside the lock, always: `request_status` takes it again, and a
        // method that held it across the call would deadlock the first time
        // anybody typed a goal. The scoping is load-bearing, not tidiness.
        self.request_status();
    }

    pub fn goal_ended(&self) {
        {
            let mut inner = self.lock();
            inner.started = None;
            inner.view.mode = Mode::Idle;
            inner.view.status.elapsed = None;
            inner.paint();
        }
        self.request_status();
    }

    /// Measurements from the model call that just returned. Both numbers are
    /// the provider's or the loop's own, never an estimate — see
    /// [`Status`](super::render::Status).
    pub fn spent(&self, context: Option<i64>, spend: Option<i64>) {
        {
            let mut inner = self.lock();
            let (ctx_cap, token_cap) = inner.caps;
            if let Some(context) = context {
                inner.view.status.context = Some((context, ctx_cap));
            }
            if let Some(spend) = spend {
                inner.view.status.spend = Some((spend, token_cap));
            }
            inner.paint();
        }
        // The nearest thing Emma has to Claude Code's "a new assistant message
        // arrived": this is called once per model call, with that call's own
        // measurements. Debouncing downstream is what keeps a fast turn from
        // spawning a process per response.
        self.request_status();
    }

    /// The caps the two meters are measured against. Set once, from the
    /// budgets the run was given.
    pub fn set_budgets(&self, max_context: i64, max_tokens: i64) {
        let mut inner = self.lock();
        inner.caps = (max_context, max_tokens);
        if let Some((used, _)) = inner.view.status.context {
            inner.view.status.context = Some((used, max_context));
        }
        if let Some((used, _)) = inner.view.status.spend {
            inner.view.status.spend = Some((used, max_tokens));
        }
        inner.paint();
    }
}

impl Inner {
    /// Draw the viewport, and record where the cursor ended up so
    /// [`restore_terminal`] can erase from a panic hook that holds no
    /// reference to any of this.
    fn paint(&mut self) {
        self.view.status.elapsed = self.started.map(|t| t.elapsed());
        let cursor = paint_into(&mut self.term, &self.view);
        let top = self.term.get_frame().area().y;
        CURSOR_ROW.store(
            cursor.map(|c| c.y.saturating_sub(top)).unwrap_or(0),
            Ordering::SeqCst,
        );
    }

    /// Put lines above the viewport, where the terminal owns them.
    fn emit(&mut self, lines: Vec<Line<'static>>) {
        emit_into(&mut self.term, lines);
    }
}

// region: The two operations, over any backend
// ---------------------------------------------------------------------------
// The two operations, over any backend
//
// Everything the viewport does is one of these: put lines above it, or redraw
// it. They are generic over `Backend` for one reason — a `TestBackend` is a
// screen the tests can read, and it runs the same ratatui code a real terminal
// runs. Without that, "output goes above and the question stays below" is a
// claim about a mechanism nobody in this repository can observe.
// ---------------------------------------------------------------------------

/// Redraw the viewport. Returns where the cursor was put.
fn paint_into<B: Backend>(term: &mut Terminal<B>, view: &View) -> Option<Position> {
    let mut cursor = None;
    let _ = term.draw(|f| {
        let area = f.area();
        cursor = view.render(area, f.buffer_mut());
        if let Some(pos) = cursor {
            f.set_cursor_position(pos);
        }
    });
    cursor
}

/// Push transcript lines above the viewport.
///
/// The height has to be worked out in advance, because that is what
/// `insert_before` makes room for: guess low and the last row of a wrapped line
/// lands on top of the viewport. [`rows_used`] measures display columns rather
/// than characters, so a CJK glyph counts as the two columns it takes.
fn emit_into<B: Backend>(term: &mut Terminal<B>, lines: Vec<Line<'static>>) {
    if lines.is_empty() {
        return;
    }
    let width = term.get_frame().area().width.max(1);
    let height: u16 = lines
        .iter()
        .map(|l| rows_used(l, width))
        .fold(0u16, |a, b| a.saturating_add(b))
        .min(MAX_INSERT_ROWS);
    if height == 0 {
        return;
    }
    let _ = term.insert_before(height, |buf| {
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(Rect::new(0, 0, buf.area.width, buf.area.height), buf);
    });
}

// endregion: The two operations, over any backend

/// Redraw once a second while a goal is running, so the clock on the status
/// line is a clock rather than a timestamp from the last thing that happened.
///
/// A `Weak` so the thread dies with the frame, and a plain thread rather than a
/// tokio task so it keeps ticking through a blocking tool call.
fn spawn_clock(frame: Weak<Frame>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(500));
        let Some(frame) = frame.upgrade() else {
            return;
        };
        let running = {
            let inner = frame.lock();
            inner.started.is_some()
        };
        if running {
            frame.draw();
        }
    });
}

// endregion: The frame

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    use crate::term::palette::{Level, Palette};
    use crate::term::render::UNICODE;
    use crate::term::view::Prompt;

    /// A screen the tests can read, driven by the same two functions a real
    /// terminal is driven by.
    fn screen(rows: u16, view_height: u16) -> Terminal<TestBackend> {
        Terminal::with_options(
            TestBackend::new(48, rows),
            TerminalOptions {
                viewport: Viewport::Inline(view_height),
            },
        )
        .expect("a test backend never fails to size")
    }

    fn rows_of(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn view() -> View {
        View::new(Skin::new(Palette::new(Level::None), UNICODE))
    }

    /// The whole design in one assertion: transcript above, viewport below,
    /// and no amount of the first can move the second.
    ///
    /// This is the closest anything in this repository gets to looking at a
    /// terminal. `TestBackend` is a screen with the real ratatui `draw` and
    /// `insert_before` running against it — so what is asserted here is the
    /// mechanism rather than a re-implementation of it. What it cannot show is
    /// whether a *real* terminal captures the scrolled-off rows into
    /// scrollback; that depends on `append_lines` emitting newlines at the
    /// bottom row, which is asserted separately, in the manifest.
    #[test]
    fn output_goes_above_the_viewport_and_the_question_stays_below_it() {
        let mut term = screen(12, 5);
        let mut view = view();
        view.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: vec!["$ rm -rf build/".into()],
            keys: vec![("y".into(), "yes".into()), ("n".into(), "no".into())],
            question: "allow? ".into(),
        });
        paint_into(&mut term, &view);

        let skin = view.skin;
        for i in 0..20 {
            emit_into(&mut term, vec![skin.prose(&format!("output line {i}"))]);
            paint_into(&mut term, &view);
        }

        let rows = rows_of(&term);
        let shown = rows.join(
            "
",
        );
        // The last thing printed is directly above the viewport, which sits on
        // the bottom five rows of the screen.
        assert!(
            rows[12 - 5 - 1].contains("output line 19"),
            "the transcript is not directly above the viewport: {shown}"
        );
        // …and everything older than that has scrolled off the top, which is
        // what "the terminal owns it" means: it is in scrollback now, not in
        // anything Emma is holding.
        assert!(!shown.contains("output line 0"), "{shown}");
        // …and the question is still on screen after twenty lines of output,
        // which is the defect this whole design exists to fix.
        assert!(shown.contains("Approve Bash"), "{shown}");
        assert!(shown.contains("rm -rf build/"), "{shown}");
        assert!(
            shown.contains(" y "),
            "the answer keys went off screen: {shown}"
        );
    }

    /// The viewport is redrawn under each insert rather than accumulating
    /// copies of itself up the screen — the failure the second attempt at this
    /// file spent its erase arithmetic on.
    #[test]
    fn the_viewport_is_drawn_once_however_much_is_printed_under_it() {
        let mut term = screen(12, 5);
        let view = view();
        paint_into(&mut term, &view);
        let skin = view.skin;
        for i in 0..8 {
            emit_into(&mut term, vec![skin.prose(&format!("line {i}"))]);
            paint_into(&mut term, &view);
        }
        let rows = rows_of(&term);
        let boxes = rows
            .iter()
            .filter(|r| r.contains(UNICODE.border.top_left))
            .count();
        assert_eq!(boxes, 1, "the input box was drawn {boxes} times: {rows:?}");
    }

    /// A line wider than the screen takes more than one row, and the room made
    /// for it has to match — or its tail is written over the viewport.
    #[test]
    fn a_wrapped_transcript_line_gets_all_the_rows_it_needs() {
        let mut term = screen(12, 5);
        let view = view();
        paint_into(&mut term, &view);
        emit_into(&mut term, vec![view.skin.prose(&"x".repeat(100))]);
        paint_into(&mut term, &view);
        let rows = rows_of(&term);
        // Every character of it arrived, across three rows…
        let xs: usize = rows
            .iter()
            .take_while(|r| r.chars().all(|c| c == 'x'))
            .map(|r| r.chars().count())
            .sum();
        assert_eq!(xs, 100, "the wrapped line lost characters: {rows:?}");
        // …and the viewport underneath is whole rather than written over.
        assert!(
            rows.iter().any(|r| r.contains(UNICODE.border.top_left)),
            "the tail of the wrapped line landed on the viewport: {rows:?}"
        );
        assert!(rows.iter().any(|r| r.starts_with("emma")), "{rows:?}");
    }

    /// The defect the first attempt shipped, as an assertion nothing else can
    /// catch.
    ///
    /// With ratatui's `scrolling-regions` feature on, `insert_before` is
    /// implemented with `ESC[{top};{bottom}r`. A scrolling region whose top
    /// margin is below row 1 *discards* the lines that scroll off it, so the
    /// transcript stops reaching scrollback — and every other test in this
    /// repository passes anyway, because none of them can look at a terminal.
    /// The only place the decision is visible is the manifest.
    #[test]
    fn the_scrolling_regions_feature_is_never_enabled() {
        // Comments stripped: this file argues about the feature by name several
        // times over, and the manifest explains why it is off. What must not
        // appear is a *declaration* of it.
        let manifest: String = include_str!("../../Cargo.toml")
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !manifest.contains("scrolling-regions"),
            "ratatui's scrolling-regions feature would implement insert_before with a DEC \
             scroll region, which discards scrollback. That is the bug this design exists to \
             not have."
        );
        // And the alternate screen, which costs scrollback outright and has
        // been ruled out three times, is not reachable either: the only
        // viewport this file ever constructs is an inline one.
        //
        // The needles are assembled rather than written out, because this file
        // is its own haystack and a literal would match itself.
        let source = include_str!("frame.rs");
        let viewport = format!("View{}", "port::");
        assert!(
            source
                .split(viewport.as_str())
                .skip(1)
                .all(|rest| rest.starts_with("Inline")),
            "a viewport other than the inline one is constructed here"
        );
        assert!(
            !source.contains(&format!("Enter{}", "AlternateScreen")),
            "the alternate screen is one API call away from costing every user their scrollback"
        );
    }

    #[test]
    fn the_viewport_is_a_third_of_the_window_within_reason() {
        assert_eq!(view_rows(40), 10);
        assert_eq!(view_rows(24), 8);
        // Small windows get the least that can hold a question and its keys;
        // anything smaller than this is refused outright by `fallback_reason`.
        assert_eq!(view_rows(9), 5);
        assert_eq!(view_rows(120), 10);
    }

    #[test]
    fn restoring_a_terminal_that_was_never_drawn_on_writes_nothing() {
        // `Drop`, the panic hook and `main` can all reach this, and the common
        // case — `-p`, a test binary, a pipe — never drew a viewport at all. A
        // restore that emitted escapes regardless would corrupt the output of
        // every non-interactive run.
        assert!(!FRAME_ON.load(Ordering::SeqCst));
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst));
    }

    #[test]
    fn restore_runs_once_however_many_times_it_is_called() {
        // Set by hand: a test binary has no terminal, so `install` correctly
        // refuses and cannot set this for us. What is under test is the latch,
        // which is what makes `Drop` + panic hook + an explicit call safe.
        FRAME_ON.store(true, Ordering::SeqCst);
        CURSOR_ROW.store(3, Ordering::SeqCst);
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst), "the latch did not clear");
        assert_eq!(CURSOR_ROW.load(Ordering::SeqCst), u16::MAX);
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst));
    }
}
