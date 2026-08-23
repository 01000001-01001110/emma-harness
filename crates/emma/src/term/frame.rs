//! The terminal frame: the alternate-screen app it draws by default, and the
//! inline viewport it keeps as the `EMMA_UI=inline` escape hatch.
//!
//! # The rule, and the reversal
//!
//! This file spent its whole life arguing *against* the alternate screen: it
//! costs terminal scrollback and native mouse selection, and Emma's output is
//! a transcript people read after the run. That argument was true and it
//! lost — the owner chose a multi-pane layout (sidebar, pinned input, status
//! bar) that structurally cannot be drawn inline, and reopened the decision on
//! purpose. `notes/design/tui-fullscreen.md` §2 prices every cost;
//! `notes/audits/eval-tui-fullscreen-kimi.md` adds the four the plan missed. The flip
//! is stage 2 of that design, and this is it.
//!
//! What replaces what the terminal used to do for free: the retained
//! [`Transcript`](super::transcript::Transcript) buffer replaces scrollback
//! (scrolled by keys and the wheel — mouse capture is on, which is why plain
//! drag-selection needs Shift now); leaving the alternate screen replaces the
//! erase arithmetic (the terminal restores its own prior content wholesale);
//! and the exit line printed by `Term`'s drop names the session log, because
//! "scroll the shell up an hour later" is the one thing nothing here can give
//! back.
//!
//! **The inline viewport is not deleted.** `EMMA_UI=inline` selects it for one
//! release as a soak-period escape hatch (design §2.3), so everything below
//! about `insert_before`, anchoring and erase-climbing still holds on that
//! path. The plain fallback — pipes, `-p`, `EMMA_NO_FRAME`, no console —
//! remains the product it always was and never sees an escape byte, let alone
//! the alternate screen.
//!
//! **ratatui's `scrolling-regions` feature must still never be enabled.** With
//! it on, the inline path's `insert_before` is implemented with
//! `ESC[{top};{bottom}r` instead, and a DEC scrolling region whose top margin
//! is below row 1 *discards* the lines that leave it. That is not a theory —
//! it is exactly what shipped here once before, and the owner's first
//! complaint was that he could not scroll. The inline path exists for as long
//! as the escape hatch does; after that, "irrelevant and off" costs nothing,
//! while "someone enabled it while the hatch still exists" is exactly the
//! invisible defect the manifest test was written for.
//!
//! **On the inline path, transcript lines still go out through
//! `Terminal::insert_before`** — cursor to the last row, newlines, the one
//! mechanism a terminal captures into scrollback — and the frame still starts
//! where the cursor is and drifts to the bottom rather than being walked
//! there. See the `Anchoring` region.
//!
//! **Blank rows are a boundary between blocks and are decided in one place** —
//! [`super::spacing`] on the inline path, the transcript buffer's own
//! separators on the full-screen one. Nothing in this file writes a
//! `Line::default()` of its own.
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
//! anything — a panic hook has no `self`. It undoes raw mode, mouse capture,
//! the alternate screen (or, on the inline path, the rows the viewport is
//! drawn on), the private modes, the Windows console mode, and the Windows
//! output code page Emma set for itself at startup. The failure it prevents
//! got worse with the flip: a stranded alternate screen is a user staring at a
//! full frozen frame with a shell they cannot see typing into it, in a
//! terminal that is no longer echoing what they type. It is reachable from
//! `Drop`, the panic hook (which restores *before* the message prints, so the
//! message lands on the normal screen), and the one `process::exit` path in
//! `main.rs`. What it cannot cover, named honestly: `SIGKILL`, a stack
//! overflow that skips the hook, and `std::process::abort` — those strand raw
//! mode today and strand the alternate screen now, and no in-process code can
//! do otherwise.

use std::io::{IsTerminal, Stdout, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, Once, Weak};
use std::time::{Duration, Instant};

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::app::App;
use super::markdown::Markdown;
use super::render::{rows_used, Skin};
use super::spacing::Spacing;
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
/// Set once the alternate screen has been entered. The restore path branches
/// on it: leaving the alternate screen restores the prior content wholesale,
/// where the inline path has to erase its own rows.
static ALT_ON: AtomicBool = AtomicBool::new(false);
/// Set once mouse capture is on. Its own latch because on Windows the disable
/// is a console-mode change rather than bytes, so it cannot ride in
/// [`leave_modes`] with the string modes.
static MOUSE_ON: AtomicBool = AtomicBool::new(false);
/// How many rows the cursor sits below the top of the viewport, so the erase
/// can climb exactly that far. `u16::MAX` means nothing is drawn.
static CURSOR_ROW: AtomicU16 = AtomicU16::new(u16::MAX);
static PANIC_HOOK: Once = Once::new();

/// Take the viewport off the screen and leave the terminal as it was found.
///
/// Only relative cursor movement, and only upwards by a number that was written
/// down when the viewport was last drawn. Nothing on this path computes a row
/// number: on a Windows console without VT processing the rows a program can
/// compute are window rows while `SetConsoleCursorPosition` reads them as
/// *screen buffer* rows, which is where a repaint aimed at "row 40" of a
/// nine-thousand-row buffer goes to be invisible — and this path runs on the way
/// out, when whether VT was ever proved is no longer knowable from here.
///
/// Absolute addressing above the frame is a different question and the answer
/// is different: with VT proved, `ESC[y;xH` is window-relative, crossterm's
/// cursor report is window-relative to match, and ratatui addresses every cell
/// of the viewport that way already. [`anchor`] uses it on the resize path for
/// exactly that reason, and only over rows it has just erased.
/// Put the terminal back before a panic message is printed.
///
/// Separate from `install` so it can be called *first*, before any mode is
/// enabled. `call_once`, so repeated frames cost nothing and the previous hook
/// is chained exactly once.
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Before the message, so the message is readable and so the
            // terminal is echoing again by the time anybody reads it.
            restore_terminal();
            previous(info);
        }));
    });
}

pub fn restore_terminal() {
    // **Each latch answers for itself.** This used to return early unless
    // `FRAME_ON` was set — and `FRAME_ON` is set *last*, after raw mode, the
    // alternate screen and mouse capture are already on. So a panic anywhere in
    // that window left every one of them enabled with the only thing that turns
    // them off refusing to run: a shell with no echo, on a screen that is not
    // the user's, which is the failure `CLAUDE.md` says people uninstall over.
    //
    // The comment at the install site claimed the `ALT_ON` latch prevented
    // exactly this. It could not, because it never reached the code that reads
    // it.
    let was_frame = FRAME_ON.swap(false, Ordering::SeqCst);
    if !was_frame
        && !RAW_ON.load(Ordering::SeqCst)
        && !ALT_ON.load(Ordering::SeqCst)
        && !MOUSE_ON.load(Ordering::SeqCst)
    {
        return;
    }
    if RAW_ON.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
    }
    if MOUSE_ON.swap(false, Ordering::SeqCst) {
        // Through crossterm rather than a string on purpose: on Windows the
        // enable was a console-mode change, and only the matching command
        // undoes it. On unix this writes the `?100x` lows.
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
    // Leaving the alternate screen restores the prior shell content wholesale,
    // which is the erase arithmetic's whole job done by the terminal; the
    // inline path still erases its own rows. The `?1049l` is written only when
    // `?1049h` was: a leave on the normal screen carries an implicit cursor
    // restore that nothing saved, and terminals answer that by moving the
    // cursor somewhere surprising.
    let mut out = if ALT_ON.swap(false, Ordering::SeqCst) {
        CURSOR_ROW.store(u16::MAX, Ordering::SeqCst);
        ALT_LEAVE.to_string()
    } else {
        erase_frame()
    };
    out.push_str(&leave_modes());
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

/// The escape that takes the viewport off the screen, climbing to its top row
/// by the number of rows written down at the last paint and wiping from there
/// to the bottom. Empty when nothing is drawn.
///
/// Relative movement only, and upwards only, by a number this file measured
/// itself: what is *above* the viewport is the transcript, and it is the
/// terminal's.
fn erase_frame() -> String {
    let row = CURSOR_ROW.swap(u16::MAX, Ordering::SeqCst);
    if row == u16::MAX {
        return String::new();
    }
    let mut out = String::new();
    if row > 0 {
        out.push_str(&format!("\x1b[{row}A"));
    }
    out.push_str("\r\x1b[J");
    out
}

// endregion: Restoring

// region: Terminal modes
// ---------------------------------------------------------------------------
// Terminal modes
//
// The private modes Emma turns on for the length of a framed run, and the
// matching offs. They are written as strings rather than through crossterm's
// command types for the reason [`synchronized`] gives: they are not drawing
// operations, they share the one stdout buffer with everything ratatui writes,
// and the order they were written in is the order they go out in.
//
// **Every `h` here has to have its `l` there.** A mode left on outlives the
// process: a shell inheriting bracketed paste that nothing will ever read the
// markers of pastes `ESC[200~` into itself.
// `every_terminal_mode_the_frame_sets_is_unset_on_the_way_out` is the assertion
// that the two cannot drift — it reads the enable sequence, extracts every mode
// number from it, and demands the matching disable — and that is why they are
// two functions rather than one with a `bool` nobody would ever pass wrongly.
// ---------------------------------------------------------------------------

/// Bracketed paste: the terminal wraps clipboard text in `ESC[200~`/`ESC[201~`
/// so a program can tell "the user pressed Enter" from "the user pasted
/// something with a newline in it".
///
/// Without it, a pasted code block is delivered as the keystrokes it looks
/// like, and its first newline submits whatever arrived before it as a goal.
/// The evaluation in `notes/audits/eval-tui-fullscreen-kimi.md` ranks that first on
/// the list of things that would sink the redesign, and it is right that it is
/// cheap: one sequence each way, plus [`super::input::Editor::paste`], which is
/// where the guarantee that a paste cannot submit actually lives.
const PASTE_ON: &str = "\x1b[?2004h";
const PASTE_OFF: &str = "\x1b[?2004l";

/// The alternate screen. `?1049h` switches to a cleared second buffer and
/// `?1049l` restores the first, prior content and all — which is what makes
/// the full-screen exit story *simpler* than the inline one: there is nothing
/// to erase. Kept out of [`enter_modes`] because it is entered *before* the
/// `Terminal` is built (ratatui must measure the screen it will draw on) and
/// left conditionally (see [`restore_terminal`]); the pairing test scans both
/// pairs, so the split cannot hide an unmatched `h`.
///
/// The `2J`+`H` after the switch is deliberate: xterm clears on entry, but
/// "cleared" is the terminal's promise, not DEC's, and one explicit erase is
/// cheaper than certifying every console's reading of 1049.
const ALT_ENTER: &str = "\x1b[?1049h\x1b[2J\x1b[H";
const ALT_LEAVE: &str = "\x1b[?1049l";

/// What a framed run turns on, once, after the viewport is up.
fn enter_modes() -> String {
    PASTE_ON.to_string()
}

/// What the way out turns off, whichever way out it is.
///
/// Show the cursor, end any synchronized update that was in flight, and drop
/// every attribute as well: a frame torn down mid-paint could otherwise leave a
/// shell prompt bold, invisible, or — with [`SYNC_END`] unsent — not repainting
/// at all until the terminal's own timeout fired.
fn leave_modes() -> String {
    format!("{PASTE_OFF}{SYNC_END}\x1b[?25h\x1b[0m")
}

// endregion: Terminal modes

// region: Synchronized output
// ---------------------------------------------------------------------------
// Synchronized output
//
// `ESC [ ? 2026 h` asks the terminal to stop presenting frames until the
// matching `l`, so a repaint made of several writes — scroll, draw the inserted
// lines, redraw the viewport, clear — reaches the eye as one picture instead of
// as its steps. That sequence is exactly what `insert_before` does, and it is
// what tears once the viewport is anchored at the bottom: without this, the
// frame is visibly erased and redrawn a row lower on every line of output.
//
// **On a terminal that does not implement it, nothing happens.** DECSET/DECRST
// with an unrecognised parameter is defined to be ignored, and 2026 is
// registered, so a terminal either honours it or drops it — there is no third
// behaviour and no reply to read. Windows Terminal, WezTerm, kitty, foot, iTerm2
// and Ghostty honour it; legacy conhost ignores it.
//
// The failure mode worth naming is an unmatched `h`: a terminal that never sees
// the `l` stops updating. Every terminal that implements the mode implements a
// timeout for that reason, and Emma sends `l` on the restore path as well —
// which covers `Drop`, the panic hook and the `process::exit` route — so the
// only way to strand one is a `SIGKILL`, which strands raw mode too.
//
// Nothing here is reachable without a `Frame`, and a `Frame` exists only when
// the terminal was verified able to carry one. A pipe never sees these bytes.
// ---------------------------------------------------------------------------

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// Hold the picture still for the length of one repaint.
///
/// Written straight to `stdout` rather than through the backend because it is
/// not a drawing operation and ratatui has no notion of one: both go into the
/// same global buffer, so the order they were written in is the order they go
/// out in.
fn synchronized<T>(f: impl FnOnce() -> T) -> T {
    let mut out = std::io::stdout();
    let _ = out.write_all(SYNC_BEGIN.as_bytes());
    let result = f();
    let _ = out.write_all(SYNC_END.as_bytes());
    let _ = out.flush();
    result
}

// endregion: Synchronized output

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

/// Which of the two interactive frames this run is drawing.
///
/// `Full` is the default — the alternate-screen app, with the retained
/// transcript and the multi-pane layout in [`super::app`]. `Inline` is the
/// `EMMA_UI=inline` escape hatch: the old bottom-of-screen viewport with the
/// terminal's own scrollback above it, kept for one release so the owner can
/// discover what the flip costs against the real thing rather than in theory.
enum Ui {
    Inline,
    /// Boxed for the size difference clippy flags — one heap hop per lock,
    /// against an `Inner` that already lives behind a mutex.
    Full(Box<App>),
}

struct Inner {
    term: Terminal<CrosstermBackend<Stdout>>,
    ui: Ui,
    view: View,
    /// The markdown state for the answer being streamed, which is one open
    /// fence. Here rather than in [`View`] because it belongs to the transcript
    /// — the thing being written *above* the viewport — and is reset between
    /// answers. See [`super::markdown`].
    md: Markdown,
    /// The window as it was when the frame last looked. A change means the user
    /// resized — see [`Inner::reanchor`].
    screen: (u16, u16),
    /// Whether the frame has reached the bottom of the window, which it does by
    /// being pushed there one row at a time as the transcript grows.
    ///
    /// A latch, because `insert_before` never moves it back up: once the last
    /// row of the viewport is the last row of the window, every further insert
    /// scrolls the screen instead. What it is *for* is the resize path, which
    /// has to tell "put it back on the bottom, where it was" apart from "leave
    /// it under the two lines of output it is currently sitting under".
    pinned: bool,
    /// Where blank rows come from, and the only thing that decides one. See
    /// [`super::spacing`].
    spacing: Spacing,
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
    /// The user-tool catalogue, as of the last time the working directory was
    /// known. Held here rather than re-probed per keystroke: a chord lookup is
    /// a `find` over this list, and the list only moves when the cwd does.
    /// Empty on the inline path, where no sidebar lists it and no chord fires.
    tools: Vec<crate::usertools::Entry>,
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
        let (cols, rows) = size?;
        // The escape hatch: the inline viewport, for one release. Anything
        // else — including the variable being unset, which is the ordinary
        // case — is the full-screen app.
        let inline = std::env::var("EMMA_UI").is_ok_and(|v| v.eq_ignore_ascii_case("inline"));

        // Whatever is on the current line is somebody's shell prompt, and the
        // viewport should not be drawn on top of it. On the full-screen path
        // the newline also means the exit line printed after the alternate
        // screen restores lands under the prompt rather than on it.
        let mut stdout = std::io::stdout();
        stdout.write_all(b"\r\n").ok()?;
        stdout.flush().ok()?;

        // Installed before the first mode is enabled, not after. A hook that
        // goes on afterwards cannot answer for a panic in between, and the
        // window between raw mode and `FRAME_ON` is several fallible writes
        // long. `call_once` makes this free on every later frame.
        install_panic_hook();

        enable_raw_mode().ok()?;
        RAW_ON.store(true, Ordering::SeqCst);

        if !inline {
            // Entered before the `Terminal` is built, so ratatui's first
            // measurement is of the buffer it will actually draw on. The
            // latch goes on first: a panic between here and `FRAME_ON` would
            // otherwise strand the alternate screen with a restore that
            // refuses to run.
            ALT_ON.store(true, Ordering::SeqCst);
            if stdout.write_all(ALT_ENTER.as_bytes()).is_err() || stdout.flush().is_err() {
                ALT_ON.store(false, Ordering::SeqCst);
                if RAW_ON.swap(false, Ordering::SeqCst) {
                    let _ = disable_raw_mode();
                }
                return None;
            }
        }

        let height = view_rows(rows);
        // Inline: where the cursor already is, directly under whatever the
        // shell last printed — the frame is *not* walked to the bottom (see
        // `Anchoring`). Full: the whole alternate screen.
        let backend = CrosstermBackend::new(std::io::stdout());
        let opened = if inline {
            open_viewport(backend, height)
        } else {
            open_fullscreen(backend)
        };
        let terminal = match opened {
            Ok(t) => t,
            Err(_) => {
                // Half-installed is the worst state to leave a terminal in.
                if ALT_ON.swap(false, Ordering::SeqCst) {
                    let mut stdout = std::io::stdout();
                    let _ = stdout.write_all(ALT_LEAVE.as_bytes());
                    let _ = stdout.flush();
                }
                if RAW_ON.swap(false, Ordering::SeqCst) {
                    let _ = disable_raw_mode();
                }
                return None;
            }
        };

        FRAME_ON.store(true, Ordering::SeqCst);
        // After `FRAME_ON`, never before: that flag is what makes
        // [`restore_terminal`] willing to turn these modes off again, so one
        // enabled ahead of it is one that survives a panic in the next three
        // lines. Being late costs nothing; being early costs a shell that
        // pastes escape markers into itself.
        {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(enter_modes().as_bytes());
            let _ = stdout.flush();
        }
        if !inline {
            // Mouse capture, for the wheel: on the alternate screen the wheel
            // does nothing at all without it — not "less useful", nothing —
            // and the first instinct of anybody reading a long answer is the
            // wheel (`notes/audits/eval-tui-fullscreen-kimi.md` §1.5). The cost is
            // that plain drag-selection now needs Shift; the quick-help table
            // says so, because it is the one fact a user cannot guess. Only
            // on the full-screen path: inline, the terminal owns the wheel
            // and taking it would break scrollback that still works.
            if execute!(std::io::stdout(), EnableMouseCapture).is_ok() {
                MOUSE_ON.store(true, Ordering::SeqCst);
            }
        }

        // The user-tool catalogue, against the directory Emma was started in.
        // `set_identity` refreshes it when the run's real cwd arrives; probing
        // here as well means the TOOLS section is never a placeholder on the
        // first paint. Inline runs skip it: no sidebar lists it there.
        let tools = if inline {
            Vec::new()
        } else {
            std::env::current_dir()
                .map(|d| crate::usertools::catalogue(&d))
                .unwrap_or_default()
        };
        let frame = Arc::new(Frame {
            inner: Mutex::new(Inner {
                term: terminal,
                ui: if inline {
                    Ui::Inline
                } else {
                    let mut app = App::new((cols, rows));
                    app.set_tools(super::app::tool_rows(&tools));
                    Ui::Full(Box::new(app))
                },
                view: View::new(skin),
                md: Markdown::new(),
                screen: (cols, rows),
                pinned: false,
                spacing: Spacing::new(),
                started: None,
                caps: (0, 0),
                transcript: String::new(),
                tools,
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
        synchronized(|| inner.paint());
    }

    /// The goal the user typed. On the full-screen path this keeps its kind —
    /// a `User` entry, so the chat pane's gutter can say `You` beside it —
    /// where `write_lines` would flatten it into anonymous activity. Inline,
    /// the two are the same lines.
    pub fn user_line(&self, text: &str) {
        let mut inner = self.lock();
        synchronized(|| {
            match &mut inner.ui {
                Ui::Full(app) => app.push_user(text, &self.skin),
                Ui::Inline => {
                    let lines = self.skin.goal(text);
                    inner.emit(lines);
                }
            }
            inner.paint();
        });
    }

    /// Ordinary transcript output: written above the viewport, into the
    /// terminal's own scrollback, exactly once.
    pub fn write_lines(&self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }
        let mut inner = self.lock();
        synchronized(|| {
            inner.emit(lines);
            inner.paint();
        });
    }

    /// Assistant text as it arrives, rendered as the markdown it is.
    ///
    /// Complete lines go to the transcript; the fragment after the last newline
    /// stays in the viewport, where it can still be added to. A fragment
    /// inserted above the viewport could never be extended, so a paragraph
    /// streamed without newlines would arrive as a column of one-word lines.
    ///
    /// **The newline is also the boundary the formatting waits for**, and for
    /// the same reason: every block [`super::markdown`] recognises is decided by
    /// the start of a line, so a complete line is all the context it needs and
    /// nothing has to be held back to be styled. The width is asked for here,
    /// at the moment of writing, because it is what the rows reserved by
    /// [`emit_into`] are counted against.
    pub fn prose(&self, text: &str) {
        let mut inner = self.lock();
        if let Ui::Full(app) = &mut inner.ui {
            // The owned buffer deletes the held-back partial line outright:
            // deltas stream into the tail entry, which re-renders whole on
            // every paint, so a code fence that opens three deltas in
            // restyles everything it covers. The "a fragment above the
            // viewport can never be extended" constraint was the inline
            // path's, and it stays there.
            app.stream(text, &self.skin);
            synchronized(|| inner.paint());
            return;
        }
        inner.view.partial.push_str(text);
        if !inner.view.partial.contains('\n') {
            // Nothing to commit: only the viewport's tail changed.
            synchronized(|| inner.paint());
            return;
        }
        let width = inner.term.get_frame().area().width;
        let mut done: Vec<Line<'static>> = Vec::new();
        while let Some(i) = inner.view.partial.find('\n') {
            let line: String = inner.view.partial.drain(..=i).collect();
            let rows = inner.md.line(&line, width, &self.skin);
            done.extend(rows);
        }
        synchronized(|| {
            inner.emit(done);
            inner.paint();
        });
    }

    /// The assistant stopped talking: whatever is still in the viewport becomes
    /// a transcript line, and the answer is declared over.
    ///
    /// **The blank row that used to be pushed here is now a boundary**, which is
    /// the difference between "one blank under the answer" and "one blank under
    /// the answer, plus the one the approval panel pushed, plus the one the
    /// model's own trailing newline already wrote". See [`super::spacing`]: the
    /// gap is drawn only if something follows the answer, so the last answer of
    /// a session no longer leaves a blank row hanging under it.
    ///
    /// The markdown state is dropped here rather than carried, so an answer that
    /// ended inside an unclosed fence cannot render the *next* answer as code.
    pub fn flush_prose(&self) {
        let mut inner = self.lock();
        if let Ui::Full(app) = &mut inner.ui {
            // Nothing is held back on this path — every delta already reached
            // the buffer — so the end of a turn is just the tail entry
            // closing, which is what stops the next answer growing into it.
            app.transcript.finish();
            synchronized(|| inner.paint());
            return;
        }
        let held = std::mem::take(&mut inner.view.partial);
        let width = inner.term.get_frame().area().width;
        let mut lines = Vec::new();
        if !held.trim().is_empty() {
            lines.extend(inner.md.line(&held, width, &self.skin));
        }
        inner.md.reset();
        synchronized(|| {
            inner.emit(lines);
            inner.spacing.separate();
            inner.paint();
        });
    }

    /// Declare a boundary between blocks. On the inline path this is
    /// [`super::spacing`]'s rule — a blank row only if a block turns up on the
    /// other side. On the full-screen path it is a no-op: the transcript
    /// buffer separates every block it holds, so the boundary is structural
    /// rather than declared.
    pub fn separate(&self) {
        let mut inner = self.lock();
        if matches!(inner.ui, Ui::Inline) {
            inner.spacing.separate();
        }
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
            // The catalogue is re-probed against the run's real cwd — the
            // availability of a tool is a fact about a directory. Computed
            // before the `ui` borrow because the refreshed list lands in two
            // places: the frame's lookup copy and the sidebar's rows.
            let refreshed = (!cwd.is_empty() && matches!(inner.ui, Ui::Full(_)))
                .then(|| crate::usertools::catalogue(std::path::Path::new(cwd)));
            if let Ui::Full(app) = &mut inner.ui {
                // The sidebar's SESSIONS slot gets the one row that is
                // honestly known — this run — and the transcript buffer
                // learns where the complete record lives, so the cap marker
                // can name the remedy when it binds.
                app.set_identity(session, transcript, &self.skin);
                if let Some(t) = &refreshed {
                    app.set_tools(super::app::tool_rows(t));
                }
            }
            if let Some(t) = refreshed {
                inner.tools = t;
            }
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

    // -----------------------------------------------------------------------
    // The transcript keys
    //
    // The alternate screen took the terminal's scrolling; these give it back
    // through the retained buffer. Every method is a no-op on the inline
    // path, where the terminal still owns scrollback and taking these keys
    // would break it. They are called from the reader thread — which already
    // holds the `Arc<Frame>` and already acts locally without the line
    // channel (menu sync does exactly this) — so nothing here ever enters
    // the channel and the drain guarantee is untouched by construction.
    // -----------------------------------------------------------------------

    /// Scroll by whole rows. Up breaks the follow latch; arriving back at the
    /// tail re-latches. The decisions live in
    /// [`Transcript`](super::transcript::Transcript) and are tested there.
    pub fn scroll_rows(&self, up: bool, rows: usize) {
        let mut inner = self.lock();
        if let Ui::Full(app) = &mut inner.ui {
            if up {
                app.transcript.scroll_up(rows);
            } else {
                app.transcript.scroll_down(rows);
            }
            synchronized(|| inner.paint());
        }
    }

    /// A page: the chat pane's height less one row of continuity.
    pub fn scroll_page(&self, up: bool) {
        let mut inner = self.lock();
        if let Ui::Full(app) = &mut inner.ui {
            let page = app.page();
            if up {
                app.transcript.scroll_up(page);
            } else {
                app.transcript.scroll_down(page);
            }
            synchronized(|| inner.paint());
        }
    }

    /// Home with an empty input box: as far back as there is.
    pub fn scroll_top(&self) {
        let mut inner = self.lock();
        if let Ui::Full(app) = &mut inner.ui {
            app.transcript.to_top();
            synchronized(|| inner.paint());
        }
    }

    /// End with an empty input box — and every submit, which is what stops a
    /// scrolled-up reader sending a goal and watching nothing happen.
    pub fn scroll_tail(&self) {
        let mut inner = self.lock();
        if let Ui::Full(app) = &mut inner.ui {
            app.transcript.follow_tail();
            synchronized(|| inner.paint());
        }
    }

    /// Ctrl-B. Latches — see [`super::app::Latch`].
    /// Return to the conversation, if a page is showing.
    ///
    /// Answers whether it did anything, so the reader can tell "I handled this"
    /// from "pass it on" — an Esc that was swallowed while no page was up would
    /// take the key away from whatever else wanted it.
    pub fn leave_page(&self) -> bool {
        let mut inner = self.lock();
        let Ui::Full(app) = &mut inner.ui else {
            return false;
        };
        if app.pane() == crate::term::app::Pane::Chat {
            return false;
        }
        app.show(crate::term::app::Pane::Chat);
        synchronized(|| inner.paint());
        true
    }

    pub fn toggle_sidebar(&self) {
        let mut inner = self.lock();
        let cols = inner.screen.0;
        if let Ui::Full(app) = &mut inner.ui {
            app.toggle_sidebar(cols);
            synchronized(|| inner.paint());
        }
    }

    /// A left click, routed to the layout's hit-test ([`App::click`] — today,
    /// the sidebar's collapse affordance and nothing else). Dead while a
    /// question is pending, the same §4.6 rule the `Ctrl-B` key follows: a
    /// click cannot become an answer, but a layout that reshuffles under a
    /// question the user is reading is its own hazard. A no-op click paints
    /// nothing, so idle mouse noise costs no repaint.
    pub fn click(&self, x: u16, y: u16) {
        let mut inner = self.lock();
        if inner.view.prompt.is_some() {
            return;
        }
        let cols = inner.screen.0;
        if let Ui::Full(app) = &mut inner.ui {
            if app.click(x, y, cols) {
                synchronized(|| inner.paint());
            }
        }
    }

    /// `Alt+<key>`: launch the user tool the catalogue lists under `key`, and
    /// put the outcome — `Ok` or `Err`, either way — in the transcript. A key
    /// that silently does nothing is the defect the collapse toggle just paid
    /// for; an unbound chord is the one honest silence (the editor ignored it
    /// before this existed, and there is no label to report under).
    ///
    /// The launch runs on its own thread: `launch` may spawn a process, and a
    /// keystroke handler that waits on one is an event loop that has stopped.
    /// The result comes back through [`Frame::write_lines`] — transcript
    /// lines, which can never enter the line channel, answer a prompt, or
    /// touch the editor: a launch pending across a prompt's appearance stays
    /// outside the drain guarantee by construction.
    pub fn launch_tool(self: &Arc<Self>, key: char) {
        let picked = {
            let inner = self.lock();
            if !matches!(inner.ui, Ui::Full(_)) {
                return;
            }
            inner
                .tools
                .iter()
                .find(|e| e.key == key)
                .map(|e| (e.tool, e.label.clone()))
        };
        let Some((tool, label)) = picked else {
            return;
        };

        // **Settings is a page now, not a program.** The owner reported three
        // times that `Alt+,` opening an editor was wrong; UI-001 ruled it should
        // be an in-app page. The external launch was correct as built — its own
        // comment says "the tool's promise is edit your settings, not open your
        // chosen editor" — and it is simply not what was wanted.
        // **The Data Explorer reads the session store, so the scan happens
        // here and the page is handed a snapshot.** The directory is the one
        // holding this run's log — the frame already knows that path because the
        // status row shows it — and scanning on open rather than on paint keeps
        // a directory walk off the render path.
        if tool == crate::usertools::Tool::DataExplorer {
            let dir = {
                let inner = self.lock();
                std::path::PathBuf::from(&inner.view.status.session)
                    .parent()
                    .map(|p| p.to_path_buf())
            };
            let text = match crate::commands::capture_sessions(dir.as_deref()) {
                Ok(t) => t,
                Err(e) => format!("the session store could not be read: {e:#}"),
            };
            let mut inner = self.lock();
            if let Ui::Full(app) = &mut inner.ui {
                app.show_explorer(&text);
                synchronized(|| inner.paint());
            }
            return;
        }

        if tool == crate::usertools::Tool::Settings {
            let mut inner = self.lock();
            if let Ui::Full(app) = &mut inner.ui {
                app.show(crate::term::app::Pane::Page(
                    crate::term::app::Page::Settings,
                ));
                synchronized(|| inner.paint());
            }
            return;
        }
        let frame = Arc::clone(self);
        std::thread::spawn(move || {
            let cwd = {
                let inner = frame.lock();
                inner.view.status.cwd.clone()
            };
            let cwd = if cwd.is_empty() {
                // Before `set_identity` the view has no cwd; the process's is
                // the honest stand-in — it is the directory Emma is in.
                std::env::current_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
            } else {
                cwd
            };
            let lines = match crate::usertools::launch(tool, std::path::Path::new(&cwd)) {
                Ok(msg) => frame.skin.note(&format!("{label}: {msg}")),
                Err(err) => frame.skin.warn(&format!("{label}: {err}")),
            };
            frame.write_lines(lines);
        });
    }

    /// Where the whole run is written down, for the exit line — the last
    /// thing left on the normal screen after the alternate screen restores,
    /// because it is the only pointer to a transcript the shell no longer
    /// holds. `None` on the inline path, whose transcript *is* the shell's.
    pub fn session_pointer(&self) -> Option<String> {
        let inner = self.lock();
        match &inner.ui {
            Ui::Full(_) if !inner.transcript.is_empty() => Some(inner.transcript.clone()),
            _ => None,
        }
    }
}

impl Inner {
    /// Draw the viewport, and record where the cursor ended up so
    /// [`restore_terminal`] can erase from a panic hook that holds no
    /// reference to any of this.
    fn paint(&mut self) {
        if let Some(size) = super::terminal_size() {
            if size != self.screen {
                match self.ui {
                    // The inline viewport has to be erased and re-anchored by
                    // hand; the full-screen terminal re-measures on every
                    // draw, so a resize is just the next draw at the new
                    // size — the transcript rewraps inside `App::render`.
                    Ui::Inline => self.reanchor(size),
                    Ui::Full(_) => self.screen = size,
                }
            }
        }
        self.view.status.elapsed = self.started.map(|t| t.elapsed());
        match &mut self.ui {
            Ui::Inline => {
                let cursor = paint_into(&mut self.term, &self.view);
                let area = self.term.get_frame().area();
                // The latch. Asked after the draw, of ratatui's own idea of
                // where the viewport is, because that is the number
                // `insert_before` moves and the only one that can answer "has
                // it arrived yet".
                self.pinned |= area.bottom() >= self.screen.1;
                let top = area.y;
                CURSOR_ROW.store(
                    cursor.map(|c| c.y.saturating_sub(top)).unwrap_or(0),
                    Ordering::SeqCst,
                );
            }
            Ui::Full(app) => {
                // The bar's cells map to real measurements or they do not
                // appear. Its contract encodes absence as a non-positive
                // cap, so an unmeasured `context`/`spend` becomes `(0, 0)` —
                // never `(0, real_cap)`, which would draw an invented "0% of
                // the budget" before the first model call reports. The ↑/↓
                // raw token split is `None` because the loop does not carry
                // it yet (design §7): absent, not zero.
                let bar = super::statusbar::Bar {
                    mode: match self.view.mode {
                        Mode::Idle => "IDLE".to_string(),
                        Mode::Working => "WORKING".to_string(),
                    },
                    model: self.view.status.model.clone(),
                    env: super::app::stem(&self.view.status.cwd),
                    ctx_used: self.view.status.context.map(|(u, _)| u).unwrap_or(0),
                    ctx_max: self.view.status.context.map(|(_, c)| c).unwrap_or(0),
                    total_used: self.view.status.spend.map(|(u, _)| u).unwrap_or(0),
                    total_max: self.view.status.spend.map(|(_, c)| c).unwrap_or(0),
                    up: None,
                    down: None,
                    elapsed: self.view.status.elapsed,
                };
                let view = &self.view;
                let term = &mut self.term;
                let mut cursor = None;
                let _ = term.draw(|f| {
                    let area = f.area();
                    cursor = app.render(area, f.buffer_mut(), view, &bar);
                    if let Some(pos) = cursor {
                        f.set_cursor_position(pos);
                    }
                });
                // `CURSOR_ROW` is the inline erase's climb count and stays
                // parked at "nothing drawn": the full-screen restore leaves
                // the alternate screen instead of erasing rows.
            }
        }
    }

    /// One block of transcript output, to whichever transcript this run has.
    ///
    /// Inline: above the viewport, where the terminal owns it, through the
    /// spacing rule. Full-screen: into the retained buffer, whose own
    /// separators are the spacing rule — one blank row between blocks,
    /// decided in one place either way.
    fn emit(&mut self, lines: Vec<Line<'static>>) {
        match &mut self.ui {
            Ui::Inline => emit_block(&mut self.term, &mut self.spacing, lines),
            Ui::Full(app) => app.push_block(lines, &self.view.skin),
        }
    }

    /// The window changed size: put the viewport back where it belongs.
    ///
    /// **Where it belongs depends on whether it had arrived at the bottom yet.**
    /// A frame that is still drifting down under two lines of output belongs
    /// under those two lines, in the new window as in the old one — dragging it
    /// to the bottom of a taller window would open exactly the field of blank
    /// rows this file stopped opening at install. A frame that had reached the
    /// bottom belongs on the bottom of the new window. `pinned` is the
    /// difference, and it is a latch rather than a comparison because a window
    /// dragged taller makes the old bottom row an ordinary middle row and there
    /// is then nothing left on screen to work it out from.
    ///
    /// **Why a fresh `Terminal` rather than `resize`.** ratatui's own
    /// `autoresize` keeps an inline viewport *where it is* and only moves it
    /// when it would not otherwise fit — which is right for a viewport anchored
    /// where it was created and wrong for one anchored to the bottom: a window
    /// dragged taller leaves the frame stranded in the middle with a field of
    /// empty rows under it. There is no way to tell it otherwise, because the
    /// top row it computes comes from the cursor, and the cursor is the one
    /// thing this file can put where it likes. So the frame is erased, the
    /// cursor is placed on the row the viewport should start at, and a new
    /// `Terminal` is built around it — which is exactly what [`Frame::install`]
    /// does, and reuses its argument rather than restating it.
    ///
    /// The viewport's *height* is re-derived too. A window dragged from forty
    /// rows to fifteen should not keep a ten-row frame in it, and `view_rows`
    /// already knows what the right answer is.
    ///
    /// **Nothing above the frame is touched.** The erase climbs by the number of
    /// rows written down at the last paint and wipes from there down; the
    /// transcript is above that line and stays where the terminal put it. What
    /// this cannot promise is that a terminal reflowing a *narrower* window left
    /// the frame where it last saw it — a reflow moves text this file did not
    /// write. In that case the erase lands somewhere near, and the worst outcome
    /// is a few blank rows or a leftover row of the old frame, which the next
    /// line of output scrolls away. It is a repaint being untidy, not scrollback
    /// being lost.
    ///
    /// **The one hazard worth naming.** Asking where the cursor is costs a
    /// round trip through the terminal on unix — Emma writes a DSR query and
    /// reads the answer off stdin — and by the time a resize happens the reader
    /// thread is on stdin too, so the answer can go to the wrong reader. That
    /// exposure is not new: ratatui's own `autoresize` asks the same question on
    /// the first draw after any resize. When it fails, `anchor` moves nothing
    /// and `Terminal::with_options` returns an error, so the frame stays where
    /// it was — untidy, not broken. On Windows, where this is developed and
    /// used, the position comes from a console API call and there is no race at
    /// all.
    fn reanchor(&mut self, size: (u16, u16)) {
        self.screen = size;
        let (_, rows) = size;
        let height = view_rows(rows);
        let mut out = std::io::stdout();
        let _ = out.write_all(erase_frame().as_bytes());
        let _ = out.flush();
        let mut backend = CrosstermBackend::new(std::io::stdout());
        if let Some(target) = resize_target(self.pinned, rows, height) {
            anchor(&mut backend, target);
        }
        if let Ok(term) = open_viewport(backend, height) {
            self.term = term;
        }
    }
}

// region: Anchoring
// ---------------------------------------------------------------------------
// Anchoring
//
// **The input box ends up on the bottom row of the window. It does not start
// there.** ratatui computes the viewport's top row in `compute_inline_size` from
// where the cursor is when the `Terminal` is built, and every `insert_before`
// after that moves it one row further down until it reaches the bottom, where it
// stays. So the box is under the last line of output at all times, which is the
// only definition of "pinned to the bottom" that is true on the first frame as
// well as the hundredth.
//
// **The version this replaces walked the cursor down to `height - view_rows`
// before handing over the backend.** That is one line and it does put the box on
// the last row immediately — and on a fresh shell it also leaves every row it
// walked over blank, so the first thing a new user sees is two thirds of a
// window of nothing with a box under it. The owner sent a screenshot of that,
// too. The walk cost the *transcript* nothing, which was the property that
// mattered and the property that was tested; what it cost was the screen.
//
// Not anchoring costs nothing in return, because ratatui's own construction
// already appends `view_rows - 1` lines to make room for the viewport it is
// about to draw — the same newlines `println!` writes, at the bottom row, which
// is the one movement a terminal captures into scrollback rather than discards.
// Where the shell prompt was already near the bottom, which is the usual case
// for a terminal somebody has been working in, that appending puts the box on
// the last row on the first frame with nothing left over.
//
// **What is left here is the resize path**, which is a different question: the
// rows below the frame have just been erased by this file and are ours, so the
// cursor may be moved to either side of where it is. See [`Inner::reanchor`] for
// which of the two answers a resize gets.
// ---------------------------------------------------------------------------

/// The row the viewport's top belongs on: the window's height, less the frame's.
fn anchor_row(screen_rows: u16, view_rows: u16) -> u16 {
    screen_rows.saturating_sub(view_rows)
}

/// Where a resize should put the viewport's top row, or `None` to leave it on
/// the row the erase left the cursor on.
///
/// The whole of the resize decision, as a function of one bit, so that what
/// [`Inner::reanchor`] does and what the tests assert are the same code rather
/// than two statements of it — `reanchor` itself writes to the process's real
/// stdout and cannot be called from a test binary.
///
/// `None` is not "do nothing safe": it is the answer for a frame still sitting
/// under the two lines of output it belongs to, which a taller window must not
/// drag to the bottom. That is the startup void arriving through the resize
/// path.
fn resize_target(pinned: bool, screen_rows: u16, height: u16) -> Option<u16> {
    pinned.then(|| anchor_row(screen_rows, height))
}

/// Put the cursor on the row the viewport should begin at, before ratatui asks.
///
/// **Only ever called for a frame that had already reached the bottom**, and
/// only after [`erase_frame`] has wiped from the frame's old top row down — so
/// every row this moves over is one Emma just cleared, and nothing above the
/// frame is touched. Walking *down* is newlines over rows that already exist,
/// which scrolls nothing; walking *up* is absolute addressing, which is safe for
/// the reason [`restore_terminal`] spells out and only over rows we own.
///
/// A backend that cannot say where its cursor is gets no anchoring rather than a
/// guess: the viewport still lands somewhere legible.
fn anchor<B: Backend>(backend: &mut B, target: u16) {
    let Ok(pos) = backend.get_cursor_position() else {
        return;
    };
    if pos.y < target {
        let _ = backend.append_lines(target - pos.y);
    } else if pos.y > target {
        let _ = backend.set_cursor_position(Position::new(0, target));
    }
    let _ = backend.flush();
}

// endregion: Anchoring

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

/// Hand the backend to ratatui and let it work out where the viewport goes.
///
/// **The cursor is not moved first, and that is the feature.** ratatui derives
/// the viewport's top row from wherever the cursor happens to be, so this puts
/// the frame directly under the last line of output — see the `Anchoring`
/// region for the version that walked it to the bottom instead and left a
/// window of blank rows above it.
///
/// A function rather than two call sites, so the tests build their `Terminal`
/// through the same call the real install does and a cursor moved back in here
/// fails them.
fn open_viewport<B: Backend>(backend: B, height: u16) -> std::io::Result<Terminal<B>> {
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
}

/// The full-screen terminal, for the alternate-screen path. The other of the
/// exactly two viewports this file may construct — the manifest test counts
/// them — and only reachable after [`Frame::install`]'s fallback gate has
/// said a real terminal is present, which is what keeps the alternate screen
/// out of every pipe, test binary and `-p` run.
fn open_fullscreen<B: Backend>(backend: B) -> std::io::Result<Terminal<B>> {
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fullscreen,
        },
    )
}

/// One block on its way to the transcript: the spacing rule, then the rows.
///
/// The whole of [`Inner::emit`], as a function over any backend, so a test can
/// drive the same code a real run does — the alternative is a test that states
/// the composition a second time and passes when the real one stops doing it.
fn emit_block<B: Backend>(
    term: &mut Terminal<B>,
    spacing: &mut Spacing,
    lines: Vec<Line<'static>>,
) {
    let lines = spacing.apply(lines);
    emit_into(term, lines);
}

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
        open_viewport(TestBackend::new(48, rows), view_height)
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

    /// How many input boxes are on screen — counted by the corner *and the
    /// border beside it*, which is not fussiness.
    ///
    /// **`TestBackend` and a real terminal disagree about one cell, and it is
    /// this one.** ratatui clears a moved inline viewport by putting the cursor
    /// on its top-left and asking the backend for `ClearType::AfterCursor`.
    /// crossterm sends `ESC[J`, which clears *from and including* the cursor
    /// cell; `TestBackend` implements it as `content[index + 1..]`, which leaves
    /// the cell under the cursor alone. So every time the frame moves down a
    /// row, the cell buffer keeps a single stale `╭` at column zero of the row
    /// the border used to be on, and a real console does not.
    ///
    /// It is worth saying which way round that is: for once the test screen
    /// shows a defect the terminal will not have, rather than hiding one it
    /// will. Counting a whole corner rather than one glyph is the honest way
    /// past it — and it is still an assertion, because a genuinely duplicated
    /// box brings its border with it.
    fn boxes_in(rows: &[String]) -> usize {
        let corner = format!(
            "{}{}",
            UNICODE.border.top_left, UNICODE.border.horizontal_top
        );
        rows.iter().filter(|r| r.contains(&corner)).count()
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
        let boxes = boxes_in(&rows);
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

    /// The manifest's invariants — rewritten, not deleted, on 2026-08-12,
    /// when stage 2 of `notes/design/tui-fullscreen.md` entered the alternate
    /// screen on purpose.
    ///
    /// This test used to assert three things: `scrolling-regions` absent,
    /// `Viewport::Inline` only, and no alternate-screen entry anywhere. The
    /// third was a decision the owner reversed — a multi-pane layout cannot
    /// be drawn inline, and the design note carries the full price list — so
    /// the assertion died in this commit, out loud, as the design's §0 said
    /// it must. What still holds, and is asserted below:
    ///
    /// 1. **`scrolling-regions` stays off.** The inline path survives as the
    ///    `EMMA_UI=inline` escape hatch, and with the feature on its
    ///    `insert_before` becomes a DEC scroll region that *discards* the
    ///    lines that leave it — the defect that shipped here once, invisible
    ///    to every other test, and the owner's first complaint.
    /// 2. **Exactly two viewports exist, both constructed in this file**,
    ///    behind the one install gate that refuses pipes, test binaries and
    ///    `-p`. A third construction site is a path around the gate.
    /// 3. **The alternate screen is entered by one constant, paired with its
    ///    leave.** The enter bytes appearing anywhere else would be an entry
    ///    the restore path does not know about.
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
            "ratatui's scrolling-regions feature would implement the inline path's \
             insert_before with a DEC scroll region, which discards scrollback. The inline \
             escape hatch still ships; the feature stays off."
        );
        // The needles are assembled rather than written out, because this file
        // is its own haystack and a literal would match itself.
        let source = include_str!("frame.rs");
        let viewport = format!("View{}", "port::");
        assert!(
            source
                .split(viewport.as_str())
                .skip(1)
                .all(|rest| rest.starts_with("Inline") || rest.starts_with("Fullscreen")),
            "a viewport beyond the sanctioned two is constructed here"
        );
        // The alternate screen's enter sequence exists exactly once — the
        // constant `install` writes and `restore_terminal` undoes. A second
        // occurrence is an entry outside the latch's knowledge. Comments
        // stripped first: this file argues about the sequence by number, as
        // this sentence just did.
        let code: String = source
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join(" ");
        let enter = format!("?10{}h", "49");
        assert_eq!(
            code.matches(enter.as_str()).count(),
            1,
            "the alternate screen is entered somewhere other than ALT_ENTER"
        );
        assert_eq!(
            code.matches(format!("?10{}l", "49").as_str()).count(),
            1,
            "the alternate-screen leave exists somewhere other than ALT_LEAVE"
        );
    }

    /// **Every mode turned on is turned off again.**
    ///
    /// A private mode outlives the process that set it. Bracketed paste left on
    /// means the shell that inherits the terminal receives `ESC[200~` around
    /// everything anybody pastes into it, forever, from a program that has
    /// exited — the same shape of damage as raw mode left on, which this file's
    /// restore path already exists for.
    ///
    /// Asserted as a pairing over the sequences rather than as a literal, so
    /// that adding a mode to the way in without adding it to the way out fails
    /// here rather than on somebody's terminal. `enter_modes` is written to
    /// `stdout` by `install`; that write is the part no test in this process
    /// can see, and it is certified by running the binary.
    #[test]
    fn every_terminal_mode_the_frame_sets_is_unset_on_the_way_out() {
        // The way out is `leave_modes` plus the conditional alternate-screen
        // leave — two strings, because the alt leave must not be written on
        // the inline path (an unpaired `?1049l` carries a cursor restore
        // nothing saved). The way in is likewise two: `enter_modes` after the
        // viewport is up, `ALT_ENTER` before it.
        let leaving = format!("{ALT_LEAVE}{}", leave_modes());
        let mut found = 0;
        for on in format!("{}{ALT_ENTER}", enter_modes()).split_inclusive('h') {
            let Some(number) = on.strip_prefix("\x1b[?").and_then(|s| s.strip_suffix('h')) else {
                continue;
            };
            found += 1;
            assert!(
                leaving.contains(&format!("\x1b[?{number}l")),
                "mode {number} is set on the way in and never unset: {leaving:?}"
            );
        }
        assert!(found >= 2, "the enter sequences set fewer modes than exist");
        // Mouse capture cannot ride in these strings — on Windows it is a
        // console-mode change, not bytes — so its pairing is asserted on the
        // source: the crossterm disable must appear in `restore_terminal`'s
        // reach. Weak as assertions go, and said so; the live pairing is a
        // certification item.
        // **Scoped to `restore_terminal`'s body, not the whole file.** The
        // assertion used to search all of `frame.rs`, which the `use` line at
        // the top satisfies on its own — so deleting the disable from the
        // restore path left this green. The comment above already called it
        // weak; it was worse than weak, it could not fail.
        let source = include_str!("frame.rs");
        let start = source
            .find("pub fn restore_terminal(")
            .expect("restore_terminal was renamed; this assertion is now vacuous");
        let end = source[start..]
            .find(
                "
}",
            )
            .expect("restore_terminal was restructured; this assertion is now vacuous")
            + start;
        assert!(
            source[start..end].contains(&format!("Disable{}", "MouseCapture")),
            "mouse capture is enabled and the restore path never disables it"
        );
        // The three the way out owes regardless of what the way in did: the
        // cursor comes back, a synchronized update in flight is ended, and no
        // attribute outlives the frame.
        assert!(leaving.contains("\x1b[?25h"), "{leaving:?}");
        assert!(leaving.contains(SYNC_END), "{leaving:?}");
        assert!(leaving.ends_with("\x1b[0m"), "{leaving:?}");
    }

    /// Bracketed paste specifically, because it is the one being added and the
    /// number is the whole of it: `2004`, not `2044` or `20004`.
    #[test]
    fn bracketed_paste_is_enabled_by_the_frame_and_disabled_by_the_restore() {
        assert!(enter_modes().contains("\x1b[?2004h"));
        assert!(leave_modes().contains("\x1b[?2004l"));
    }

    /// **Rendered markdown is ordinary scrollback like everything else.**
    ///
    /// The styling happens before `insert_before` and changes nothing about
    /// where the lines go or how many rows they are given. The viewport
    /// underneath is still whole, which is the assertion that matters: a
    /// rendered line that took one row more than was reserved for it would have
    /// written its tail over the input box.
    #[test]
    fn formatted_prose_lands_above_the_viewport_and_leaves_it_whole() {
        let mut term = screen(24, 5);
        let view = view();
        let skin = view.skin;
        paint_into(&mut term, &view);

        let mut md = Markdown::new();
        let doc = "# Heading\n\ntext with **bold** and `code`\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n";
        let width = term.get_frame().area().width;
        for line in doc.lines() {
            emit_into(&mut term, md.line(line, width, &skin));
            paint_into(&mut term, &view);
        }

        let rows = rows_of(&term);
        let shown = rows.join("\n");
        // The viewport is intact and on the bottom five rows.
        assert!(shown.contains("emma"), "{shown}");
        assert!(
            rows.iter().any(|r| r.contains(UNICODE.border.top_left)),
            "the formatted prose wrote over the input box: {shown}"
        );
        // The hashes are gone and the heading is not, and the emphasis markers
        // went with them.
        assert!(shown.contains("Heading"), "{shown}");
        assert!(!shown.contains("# Heading"), "{shown}");
        assert!(shown.contains("text with bold and code"), "{shown}");
        // The code line arrived on one row, unbroken: 48 columns is wider than
        // it, so nothing had any business splitting it.
        assert!(
            rows.iter()
                .any(|r| r.trim() == "fn main() { println!(\"hi\"); }"),
            "the code line was broken up: {shown}"
        );
        // All of it is above the viewport, which still owns the last five rows.
        let box_top = rows
            .iter()
            .position(|r| r.contains(UNICODE.border.top_left))
            .expect("there was no input box");
        let heading = rows.iter().position(|r| r.contains("Heading")).unwrap();
        assert!(heading < box_top, "{shown}");
    }

    // -----------------------------------------------------------------------
    // Anchoring
    //
    // `TestBackend` implements `append_lines` and `get_cursor_position` the way
    // a terminal does — including pushing rows into a scrollback these tests can
    // read — and `compute_inline_size` is ratatui's own. So what is asserted
    // here is the real mechanism: where the viewport lands, and what anchoring
    // costs the rows above it.
    //
    // What it cannot show is a screen. Whether the box *looks* pinned to the
    // bottom of a real window is in the report, not in this file.
    // -----------------------------------------------------------------------

    /// A `Terminal` built the way [`Frame::install`] builds one — which is to
    /// say, built where the cursor already is. The arithmetic under test is
    /// ratatui's rather than a restatement of it.
    fn install_into(backend: TestBackend, rows: u16) -> Terminal<TestBackend> {
        open_viewport(backend, view_rows(rows)).expect("a test backend never fails to size")
    }

    /// **The defect: a fresh shell got a screen of nothing and a box under it.**
    ///
    /// The frame starts under the last line the shell printed, not on the last
    /// row of the window. The rows between it and the bottom belong to the
    /// terminal and are the ones the transcript has not reached yet — the same
    /// empty screen a shell has after two commands, which is what a person
    /// opening a program expects to see.
    #[test]
    fn the_frame_starts_under_the_last_line_of_output_rather_than_at_the_bottom() {
        let mut backend = TestBackend::new(48, 30);
        // A fresh shell: a couple of lines printed, the cursor under them.
        backend.set_cursor_position(Position::new(0, 2)).unwrap();
        let mut term = install_into(backend, 30);
        let area = term.get_frame().area();
        assert_eq!(area.height, view_rows(30));
        assert_eq!(
            area.y, 2,
            "the frame left a gap between itself and the shell's last line: {area:?}"
        );
        // And nothing was scrolled to achieve it, which is the property the
        // walk-to-the-bottom version also had and must not lose.
        term.backend().assert_scrollback_empty();
    }

    /// **Install moves the cursor nowhere**, which is the whole of the fix and
    /// the one part of it a cell buffer cannot witness: `Frame::install` needs a
    /// real terminal and refuses to run in a test binary, so the tests above
    /// reach ratatui through [`open_viewport`] rather than through it.
    ///
    /// So the caller is asserted on its source, the way the `scrolling-regions`
    /// decision already is. A single `anchor` call put back at the top of
    /// A half-installed frame can still be taken down.
    ///
    /// **The window this covers.** `install` enables raw mode, then the
    /// alternate screen, then mouse capture, and sets `FRAME_ON` last. The
    /// teardown used to return early unless `FRAME_ON` was set — so a panic
    /// anywhere in that window left every mode on with the only thing that
    /// turns them off refusing to run: a shell with no echo, on a screen that
    /// is not the user's. `CLAUDE.md` names a stranded alternate screen as the
    /// failure people uninstall over, and the constraint it states is absolute:
    /// *every* exit path leaves it.
    ///
    /// The comment at the install site claimed the `ALT_ON` latch prevented
    /// this. It could not, because the early return meant nothing ever read it.
    #[test]
    fn a_frame_that_never_finished_installing_is_still_torn_down() {
        // Stage the window: modes latched, `FRAME_ON` never reached.
        RAW_ON.store(true, Ordering::SeqCst);
        ALT_ON.store(true, Ordering::SeqCst);
        FRAME_ON.store(false, Ordering::SeqCst);

        restore_terminal();

        assert!(
            !RAW_ON.load(Ordering::SeqCst),
            "raw mode survived a half-installed frame: the shell has no echo"
        );
        assert!(
            !ALT_ON.load(Ordering::SeqCst),
            "the alternate screen survived: the user is looking at the wrong screen"
        );
    }

    /// `install` restores the screen of blank rows the owner photographed, and
    /// every other test in this file passes with it there.
    #[test]
    fn install_hands_the_terminal_to_ratatui_without_moving_the_cursor_first() {
        let source = include_str!("frame.rs");
        let start = source
            .find("pub fn install(")
            .expect("install was renamed; this assertion is now vacuous");
        // The end marker must be something that stays *inside* `install`.
        // `PANIC_HOOK.call_once` was this marker until the hook moved to the
        // top of the function to cover the install window itself, at which
        // point the slice ran on past the end of `install` and swept in
        // unrelated code. The `expect` below caught it, which is the whole
        // reason it is worded the way it is.
        let end = source[start..]
            .find("FRAME_ON.store(true")
            .expect("install was restructured; this assertion is now vacuous")
            + start;
        let body: String = source[start..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !body.contains("anchor("),
            "install anchors the frame to the bottom of the window again, which on a fresh shell \
             leaves every row it walked over blank"
        );
    }

    /// **The frame arrives at the bottom by being pushed there, and stays.**
    ///
    /// This is the whole of "it should scroll up from the bottom": output
    /// accumulates, the box moves down a row per line until it can move no
    /// further, and from then on the screen scrolls under it. Asserted through
    /// ratatui's own `insert_before` rather than a re-implementation of it.
    #[test]
    fn the_frame_drifts_down_to_the_bottom_as_output_arrives_and_stays_there() {
        let mut backend = TestBackend::new(48, 20);
        backend.set_cursor_position(Position::new(0, 1)).unwrap();
        let mut term = install_into(backend, 20);
        let view = view();
        paint_into(&mut term, &view);
        let start = term.get_frame().area().y;

        let mut tops = vec![start];
        for i in 0..30 {
            emit_into(&mut term, vec![view.skin.prose(&format!("line {i}"))]);
            paint_into(&mut term, &view);
            tops.push(term.get_frame().area().y);
        }
        // It only ever moves down…
        assert!(
            tops.windows(2).all(|w| w[1] >= w[0]),
            "the frame moved back up the screen: {tops:?}"
        );
        // …it gets to the bottom…
        let area = term.get_frame().area();
        assert_eq!(
            area.bottom(),
            20,
            "the frame never reached the bottom: {tops:?}"
        );
        // …and once there it stops, rather than the screen growing a second
        // copy of it further down.
        assert_eq!(*tops.last().unwrap(), anchor_row(20, view_rows(20)));
        let rows = rows_of(&term);
        assert_eq!(boxes_in(&rows), 1, "{rows:?}");
    }

    /// Starting the frame must cost nothing above it. This is the one that
    /// would catch the obvious implementation — print a screen of newlines —
    /// which pins the box by scrolling everything that was on screen into
    /// scrollback and filling it with blanks.
    #[test]
    fn starting_the_frame_pushes_nothing_into_scrollback_and_disturbs_nothing_above_it() {
        // A tall window with a shell prompt on its first row and the cursor
        // under it, which is the case the owner photographed.
        let mut lines = vec!["$ emma".to_string()];
        lines.extend((1..20).map(|_| "      ".to_string()));
        let mut backend = TestBackend::with_lines(lines);
        backend.set_cursor_position(Position::new(0, 1)).unwrap();

        let term = install_into(backend, 20);
        let backend = term.backend();
        backend.assert_scrollback_empty();
        // …and the shell's own line is still on the row it was on.
        let row: String = (0..6)
            .map(|x| backend.buffer()[(x, 0u16)].symbol())
            .collect();
        assert_eq!(row, "$ emma", "starting the frame scrolled the screen");
    }

    /// **A shell already at the bottom of its window is the other half of the
    /// rule**, and the one where something does scroll: ratatui appends
    /// `view_rows - 1` lines to make room for the viewport, exactly as
    /// `println!` would.
    ///
    /// What must be true is what scrolls off. Those rows are the terminal's own
    /// output going into scrollback, which is where it belongs — never blank
    /// padding Emma invented, which is the failure this project has shipped
    /// twice.
    #[test]
    fn a_shell_already_at_the_bottom_scrolls_its_own_output_and_never_blanks() {
        let lines: Vec<String> = (0..20).map(|i| format!("history {i:02}")).collect();
        let mut backend = TestBackend::with_lines(lines);
        backend.set_cursor_position(Position::new(0, 19)).unwrap();
        let mut term = install_into(backend, 20);

        let area = term.get_frame().area();
        assert_eq!(area.bottom(), 20, "{area:?}");
        let scrollback = term.backend().scrollback();
        assert!(scrollback.area.height > 0, "nothing scrolled at all");
        for y in 0..scrollback.area.height {
            let row: String = (0..scrollback.area.width)
                .map(|x| scrollback[(x, y)].symbol())
                .collect();
            assert!(
                row.trim_start().starts_with("history"),
                "a blank row entered scrollback: {row:?}"
            );
        }
    }

    /// A frame that had reached the bottom goes back to the bottom of the
    /// window it is now in, at the height that window deserves.
    ///
    /// Written against the two functions `reanchor` is made of, because
    /// `reanchor` itself writes to the process's real stdout — there is no
    /// terminal in a test binary, which is the same reason `install` refuses.
    #[test]
    fn a_resized_window_puts_a_pinned_frame_back_on_its_last_rows() {
        for (before, after) in [(20u16, 40u16), (40, 20), (40, 9)] {
            let mut backend = TestBackend::new(24, after);
            // Where the old frame was: the bottom of the *old* window.
            backend
                .set_cursor_position(Position::new(0, anchor_row(before, view_rows(before))))
                .unwrap();
            let height = view_rows(after);
            let target =
                resize_target(true, after, height).expect("a pinned frame goes back to the bottom");
            anchor(&mut backend, target);
            let mut term = open_viewport(backend, height).unwrap();
            let area = term.get_frame().area();
            assert_eq!(
                area.bottom(),
                after,
                "{before} -> {after} left the frame off the bottom: {area:?}"
            );
            assert_eq!(area.height, view_rows(after), "{before} -> {after}");
        }
    }

    /// **A frame that had not reached the bottom yet stays where it is**, which
    /// is under the output it belongs to.
    ///
    /// The failure this prevents is the startup void coming back through the
    /// resize path: three lines of transcript, the user drags the window
    /// taller, and an unconditional re-anchor drops the box thirty rows below
    /// them.
    #[test]
    fn a_resized_window_leaves_an_unpinned_frame_under_its_own_output() {
        let mut backend = TestBackend::new(24, 40);
        // Three lines of transcript and the frame directly under them, in a
        // window that has just been dragged from twenty rows to forty. The
        // cursor is where `erase_frame` left it: the frame's own top row.
        backend.set_cursor_position(Position::new(0, 3)).unwrap();
        assert_eq!(
            resize_target(false, 40, view_rows(40)),
            None,
            "an unpinned frame was given a row to move to"
        );
        // …so `reanchor` moves nothing, and the viewport is rebuilt where the
        // erase left the cursor.
        let mut term = install_into(backend, 40);
        let area = term.get_frame().area();
        assert_eq!(
            area.y, 3,
            "an unpinned frame was dragged to the bottom, opening the void again: {area:?}"
        );
        term.backend().assert_scrollback_empty();
    }

    /// A window barely taller than the frame is the case where "start where the
    /// cursor is" and "sit on the bottom" are almost the same thing, and where
    /// getting the height wrong would leave no room for anything else.
    #[test]
    fn the_smallest_window_emma_will_draw_in_still_fits_the_frame() {
        // Eight rows is the least `fallback_reason` allows.
        let mut backend = TestBackend::new(24, 8);
        backend.set_cursor_position(Position::new(0, 1)).unwrap();
        let mut term = install_into(backend, 8);
        let area = term.get_frame().area();
        assert_eq!(area.height, 5);
        assert_eq!(area.y, 1);
        assert!(area.bottom() <= 8, "{area:?}");
        term.backend().assert_scrollback_empty();
    }

    // -----------------------------------------------------------------------
    // Spacing
    //
    // The rule itself is `super::spacing` and is tested there, on data. What is
    // asserted here is that the transcript actually goes through it — that the
    // sequence a real turn produces lands on screen with one blank row between
    // blocks rather than two.
    // -----------------------------------------------------------------------

    /// **The screen the owner photographed, as rows.**
    ///
    /// A goal, an answer that ends with the model's own blank line, the
    /// boundary the answer declares, and the approval panel's evidence — which
    /// used to declare a boundary of its own by pushing a `Line::default()`.
    /// Three blank rows' worth of intent, one blank row on screen.
    #[test]
    fn a_turn_lands_with_one_blank_row_between_blocks_and_never_two() {
        let mut term = screen(30, 5);
        let view = view();
        let skin = view.skin;
        let mut spacing = Spacing::new();
        paint_into(&mut term, &view);

        // What `Term::goal_started` does: the boundary belongs *above* the
        // echoed goal, between one turn and the next. Nothing separates the
        // goal from the answer to it — they are the same exchange.
        spacing.separate();
        emit_block(&mut term, &mut spacing, skin.goal("who is president?"));
        let mut md = Markdown::new();
        for line in ["I will look that up.", ""] {
            emit_block(&mut term, &mut spacing, md.line(line, 48, &skin));
        }
        // What `flush_prose` does at the end of an answer.
        spacing.separate();
        // What `prompt_header` does at the start of the panel's evidence.
        spacing.separate();
        emit_block(
            &mut term,
            &mut spacing,
            skin.tool_started("WebSearch", "wants to run:"),
        );
        emit_block(
            &mut term,
            &mut spacing,
            skin.tool_ok("3 results", false, None),
        );
        // What `Term::ending` does. This gap has nothing else to come from —
        // no markdown blank, no trailing newline — so it is the one that fails
        // if the rule is bypassed rather than merely mis-tuned.
        spacing.separate();
        emit_block(
            &mut term,
            &mut spacing,
            skin.ending("goal complete", true, 2, 900),
        );
        paint_into(&mut term, &view);

        let rows: Vec<String> = rows_of(&term)
            .into_iter()
            .take_while(|r| !r.contains(UNICODE.border.top_left))
            .collect();
        let shown = rows.join("\n");
        // The goal is the first thing on screen — no gap above the first block.
        assert_eq!(
            rows[0].trim_start_matches(' '),
            "▶ who is president?",
            "{shown}"
        );
        // The answer follows its own goal with nothing between them.
        assert!(rows[1].contains("look that up"), "{shown}");
        // One blank between the answer and the panel, though three separate
        // boundaries were declared across that gap: the model's own trailing
        // newline, the end of the answer, and the panel's own.
        assert!(rows[2].is_empty(), "{shown}");
        assert!(rows[3].contains("WebSearch"), "{shown}");
        assert!(rows[4].contains("3 results"), "{shown}");
        // The gap with nothing behind it but a declared boundary.
        assert!(
            rows[5].is_empty(),
            "the summary was not separated from the tool result:\n{shown}"
        );
        assert!(rows[6].contains("goal complete"), "{shown}");
        // And the property, stated as itself: nowhere in the transcript do two
        // blank rows sit together.
        assert!(
            rows.windows(2)
                .all(|w| !(w[0].is_empty() && w[1].is_empty())),
            "two blank rows in a row:\n{shown}"
        );
    }

    /// The transcript never opens with a blank row, however many boundaries
    /// were declared before the first block arrived.
    #[test]
    fn the_transcript_never_opens_with_a_blank_row() {
        let mut term = screen(12, 5);
        let view = view();
        let mut spacing = Spacing::new();
        paint_into(&mut term, &view);
        spacing.separate();
        spacing.separate();
        emit_block(&mut term, &mut spacing, view.skin.goal("first"));
        paint_into(&mut term, &view);
        let rows = rows_of(&term);
        assert!(
            rows[0].contains("first"),
            "the transcript began with a gap: {rows:?}"
        );
    }

    /// Synchronized output is two constants and a rule about pairing them: the
    /// end is on the restore path, so `Drop`, the panic hook and `process::exit`
    /// all release a terminal that was told to hold its picture.
    ///
    /// **This test used to be a false receipt and could not fail.** Its third
    /// assertion was `include_str!("frame.rs").contains("out.push_str(SYNC_END)")`
    /// — and that literal occurred exactly once in the file, inside the assertion
    /// itself, so `include_str!` matched the test's own source. The restore path
    /// has said `leave_modes()` for as long as the test has existed. Proven by
    /// mutation: with `SYNC_END` deleted from `leave_modes`, the grep still
    /// passed while `every_terminal_mode_the_frame_sets_is_unset_on_the_way_out`
    /// correctly went red.
    ///
    /// It now asserts the behaviour instead of the spelling. A source grep can
    /// only ever pin how something is written; this repository's rule is that a
    /// test which cannot fail is worse than no test, because it is a receipt for
    /// a guarantee nobody checked.
    #[test]
    fn a_synchronized_update_is_always_ended_including_on_the_way_out() {
        assert_eq!(SYNC_BEGIN, "\x1b[?2026h");
        assert_eq!(SYNC_END, "\x1b[?2026l");
        // The pair is an h/l set on the same private mode; a typo in either
        // number leaves a terminal holding its picture forever.
        assert_eq!(SYNC_BEGIN.replace('h', "l"), SYNC_END);
        assert!(
            leave_modes().contains(SYNC_END),
            "the restore path no longer ends a synchronized update: {:?}",
            leave_modes()
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
        // The alternate-screen latch clears the same way, in the same test
        // rather than its own: these statics are process-global and the tests
        // run in parallel threads, so two tests flipping FRAME_ON would race.
        FRAME_ON.store(true, Ordering::SeqCst);
        ALT_ON.store(true, Ordering::SeqCst);
        restore_terminal();
        assert!(
            !ALT_ON.load(Ordering::SeqCst),
            "the alternate-screen latch survived the restore"
        );
        restore_terminal();
        assert!(!ALT_ON.load(Ordering::SeqCst));
    }
}
