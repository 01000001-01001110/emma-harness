//! Line-oriented terminal output, with an input box the terminal never owns.
//!
//! Four things here are not cosmetic.
//!
//! **Text streams as it arrives.** Forty seconds of blank terminal is
//! indistinguishable from a hang, and a user who cannot tell those apart kills
//! the process partway through a write.
//!
//! **Tool output is summarised, not dumped.** The model gets `content`; the
//! human gets `display` when the tool offered one and a few lines otherwise.
//! A screen of JSON is a screen nobody reads, and the reason to show anything
//! at all is so the user can tell that the agent is doing what they meant.
//!
//! **The question is the last thing on screen and the transcript is not.** Emma
//! asks before it writes a file or runs a command, and on the first real run
//! that question scrolled off under the tool output that followed it.
//! `approval.rs` argues that a prompt the user cannot evaluate manufactures
//! consent; a prompt the user cannot *see* is the same defect with the argument
//! already made. So the question is drawn inside the input box, and the input
//! box is redrawn beneath every single thing that is printed afterwards.
//!
//! **Scrollback is the terminal's and stays the terminal's.**
//!
//! # How the box works, and the design it replaces
//!
//! The transcript is written with ordinary writes. The terminal scrolls it, the
//! terminal wraps it on resize, the terminal keeps it in scrollback. Nothing
//! here confines it, measures it or remembers it. The input box is drawn at the
//! bottom and *redrawn*: before any output, the cursor moves to the box's top
//! row, `ESC[J` clears from there to the end of the screen, the output is
//! written — which scrolls normally, into scrollback — and the box is drawn
//! again at the new bottom.
//!
//! **This replaces a DEC scroll region (`ESC[{top};{bottom}r`), which was
//! wrong at the premise and must not come back.** The reasoning for it was that
//! it preserves scrollback by avoiding the alternate screen. That is false:
//! lines scrolled off the top of a region whose top margin is below row 1 are
//! *discarded*. Scrollback capture only happens when the scrolling region is
//! the whole screen. The owner ran it and could not scroll. There is no
//! `ESC[...r` anywhere in this file and a test asserts there is none in what it
//! emits.
//!
//! **What that costs, deliberately.** The box is not pinned to a fixed row; it
//! is always the *last thing on screen*, which is the achievable version. A
//! fixed status row at the top is not possible this way and is not faked: the
//! run's identity is printed once at startup as an ordinary line and scrolls
//! away like everything else, which is correct, because it is a fact about the
//! run rather than a live readout.
//!
//! **The box is a luxury and the fallback is the product.** Nothing below makes
//! a decision that a plain stream of lines could not; if the terminal cannot be
//! verified to support VT processing, if either stream is redirected, if the
//! window is tiny, or if `EMMA_NO_FRAME` is set, [`Term`] degrades to exactly
//! the line-by-line output it had before the box existed. `-p` never gets one,
//! and neither does [`Term::silent`].

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, Once};

use tokio::sync::mpsc;

// region: The terminal
// ---------------------------------------------------------------------------
// The terminal
//
// Every write to the screen goes through one of these methods. The split that
// matters is `delta`/`text` on stdout against `side` on stderr under `-p`, so
// a script can pipe the answer without filtering the commentary out of it.
//
// The second split is `frame`: `Some` means the input box, `None` means the
// original line-by-line output. Every method below is written so that `None`
// reproduces the old behaviour character for character.
// ---------------------------------------------------------------------------

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// How many lines of a tool result reach the screen.
const RESULT_LINES: usize = 8;

pub struct Term {
    color: bool,
    /// `-p`: assistant prose still goes to stdout, but the running commentary
    /// goes to stderr so a script can pipe the answer without filtering it.
    quiet: bool,
    enabled: bool,
    /// The input box, when the terminal was verified able to carry one. `None`
    /// is the whole fallback: every method checks it and takes the pre-box
    /// path, which is why `printing()` and `silent()` need no special handling
    /// beyond never constructing one.
    frame: Option<Frame>,
}

impl Term {
    pub fn interactive() -> Self {
        Self {
            color: std::io::stdout().is_terminal(),
            quiet: false,
            enabled: true,
            // The only constructor that may draw. Detection is in
            // `Frame::install`, which refuses far more often than it accepts.
            frame: Frame::install(),
        }
    }

    pub fn printing() -> Self {
        Self {
            color: std::io::stderr().is_terminal(),
            quiet: true,
            enabled: true,
            frame: None,
        }
    }

    /// Writes nothing. For tests, which assert on the log and the transcript
    /// rather than on the screen.
    pub fn silent() -> Self {
        Self {
            color: false,
            quiet: true,
            enabled: false,
            frame: None,
        }
    }

    /// True when the input box is drawn. For callers that would otherwise say
    /// the same thing twice — the hint under the box is one such line — and for
    /// tests.
    pub fn framed(&self) -> bool {
        self.frame.is_some()
    }

    /// The run's identity, printed once, as an ordinary line.
    ///
    /// Model, working directory and transcript path are fixed for the life of
    /// the process, which is why they are the only things here. It scrolls away
    /// with everything else and that is correct: it is a fact about the run,
    /// not a readout, and a permanent row is impossible without a scroll region
    /// — which is what this whole file exists to not have. Token spend belongs
    /// to the loop, changes every turn, and is reported after each goal.
    pub fn set_status(&self, model: &str, cwd: &std::path::Path, session: &std::path::Path) {
        self.side(&self.paint(
            DIM,
            &format!("emma · {model} · {} · {}", cwd.display(), session.display()),
        ));
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// The side channel: tool lines, notes, warnings. stderr in `-p`.
    fn side(&self, line: &str) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            frame.write(&format!("{line}\n"));
        } else if self.quiet {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    /// Assistant text, as it arrives. No newline, flushed every time — a
    /// buffered stream is a blank terminal with the words already in it.
    pub fn delta(&self, text: &str) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            frame.write(text);
            return;
        }
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    pub fn end_of_text(&self) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            frame.write("\n");
        } else {
            println!();
        }
    }

    /// Whole assistant text at once, for `-p` where nothing streamed.
    pub fn text(&self, text: &str) {
        if self.enabled && !text.trim().is_empty() {
            if let Some(frame) = &self.frame {
                frame.write(&format!("{}\n", text.trim_end()));
            } else {
                println!("{}", text.trim_end());
            }
        }
    }

    pub fn note(&self, text: &str) {
        self.side(&self.paint(DIM, &format!("  {text}")));
    }

    pub fn warn(&self, text: &str) {
        self.side(&self.paint(YELLOW, &format!("! {text}")));
    }

    pub fn banner(&self, text: &str) {
        self.side(&self.paint(RED, &format!("!! {text}")));
    }

    /// A tool is about to run. One line, the interesting argument inline.
    pub fn tool_started(&self, name: &str, args: &serde_json::Value) {
        let head = summarise_args(name, args);
        self.side(&format!(
            "{} {head}",
            self.paint(BOLD, &format!("● {name}"))
        ));
    }

    pub fn tool_result(&self, display: Option<&str>, content: &str, error: bool) {
        let body = display.unwrap_or(content);
        let mut lines = body.lines();
        let shown: Vec<&str> = lines.by_ref().take(RESULT_LINES).collect();
        let rest = lines.count();
        for line in shown {
            let text = format!("  {line}");
            self.side(&self.paint(if error { RED } else { DIM }, &text));
        }
        if rest > 0 {
            self.side(&self.paint(DIM, &format!("  … {rest} more lines")));
        }
    }

    pub fn goal_started(&self, goal: &str) {
        self.side(&self.paint(BOLD, &format!("▸ {}", goal.trim())));
    }

    pub fn prompt_header(&self, tool: &str, preview: &str) {
        self.side("");
        self.side(&self.paint(BOLD, &format!("{tool} wants to run:")));
        for line in preview.lines() {
            self.side(&format!("  {line}"));
        }
    }

    pub fn prompt_question(&self, tool: &str) {
        self.question(&format!(
            "allow? [y]es  [n]o  [a]lways {tool} this session: "
        ));
    }

    /// The network question, worded so the grant on offer is the one the answer
    /// actually gives: a host for the session, not a tool and not one call.
    /// There is no third option, because a wider network grant is not offered.
    pub fn prompt_network_question(&self, host: &str) {
        self.question(&format!(
            "allow? [y]es — and {host} again this session  [n]o: "
        ));
    }

    fn question(&self, q: &str) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            // Inside the box, where the eye already is, and — because every
            // subsequent write redraws the box below whatever it printed —
            // still the last thing on screen however much follows it. That is
            // the fix for the question that scrolled away.
            frame.set_label(q);
            return;
        }
        // Whichever stream the commentary is on, so the question is never
        // separated from the thing it is asking about.
        if self.quiet {
            eprint!("  {q}");
            let _ = std::io::stderr().flush();
        } else {
            print!("  {q}");
            let _ = std::io::stdout().flush();
        }
    }

    pub fn goal_prompt(&self) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            frame.set_label(IDLE);
            return;
        }
        print!("{} ", self.paint(BOLD, ">"));
        let _ = std::io::stdout().flush();
    }

    /// A line has been read from the terminal, so the box's contents are now
    /// history.
    ///
    /// Called by whoever consumed the line, and it has to be, because this type
    /// cannot observe the terminal's own echo. Pressing return moved the cursor
    /// off the input row; without being told, the next redraw would erase from
    /// one row too low and leave a stranded box behind. `None` is end of input.
    ///
    /// What it draws is the question and the answer together, as one ordinary
    /// transcript line — so the record of what was approved reads back exactly
    /// as it was asked, and scrolls with everything else.
    pub fn prompt_answered(&self, line: Option<&str>) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            frame.submitted(line);
        }
    }
}

impl Drop for Term {
    /// The ordinary exit path. `restore_terminal` is idempotent and global, so
    /// this racing the panic hook or an explicit call costs nothing.
    fn drop(&mut self) {
        if self.frame.is_some() {
            restore_terminal();
        }
    }
}

// endregion: The terminal

// region: The input box
// ---------------------------------------------------------------------------
// The input box
//
// Four rows at the bottom of the screen: a bordered box whose middle row holds
// the prompt and whatever the user is typing, and a dim hint under it. Nothing
// above it is ours.
//
// The invariant everything here depends on: **`cursor_row` is where the cursor
// is, counted in rows from the top of the drawn block.** Erasing is
// `ESC[{cursor_row}A`, a carriage return and `ESC[J`, which is why that number
// has to be right and why `prompt_answered` exists — the terminal's own echo of
// a newline moves the cursor and nothing else would tell us.
//
// Two absolute rules. No `ESC[...r`, ever: see the module doc. And no cursor
// addressing (`ESC[{row};{col}H`), because the row numbers a program can
// compute are viewport rows and at least one common Windows console applies
// them to the screen *buffer* — which is where a box drawn on "row 40" of a
// 9001-row buffer goes to be invisible. Only relative movement is used here.
// ---------------------------------------------------------------------------

/// Rows of the box proper: top border, input row, bottom border. The hint under
/// it makes four in total, and the input row is index 1.
const INPUT_ROW: u16 = 1;
const ROWS_BELOW_INPUT: u16 = 2;

/// What the input row shows when nothing is being asked.
const IDLE: &str = "> ";

/// Back to column one and wipe everything from here down. Paired with a
/// preceding `ESC[{n}A` whenever the cursor is not already on the block's top
/// row.
const ERASE_TO_END: &str = "\r\x1b[J";

/// Below this the box would be most of the screen, so it is not worth drawing.
const MIN_ROWS: u16 = 8;
const MIN_COLS: u16 = 24;

/// Set once the box has been drawn, cleared once it has been cleaned up.
/// Global rather than owned, because the panic hook has no `self`.
static FRAME_ON: AtomicBool = AtomicBool::new(false);
/// Where the cursor sits inside the drawn block, so `restore_terminal` can
/// erase it from a panic hook that holds no reference to the [`Frame`].
/// `u16::MAX` means nothing is drawn.
static BLOCK_CURSOR_ROW: AtomicU16 = AtomicU16::new(u16::MAX);
static PANIC_HOOK: Once = Once::new();

/// Take the box off the screen and leave the cursor on a clean line.
///
/// Idempotent, and safe to call from anywhere — a `Drop`, a panic hook, or an
/// exit path that is about to call `std::process::exit` and skip both. There is
/// far less to undo than there was under the scroll region, but the failure it
/// prevents is the same one: a shell prompt drawn on top of a box we left
/// behind, over a terminal whose cursor is somewhere nobody put it.
pub fn restore_terminal() {
    if !FRAME_ON.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = String::new();
    let row = BLOCK_CURSOR_ROW.swap(u16::MAX, Ordering::SeqCst);
    if row != u16::MAX {
        if row > 0 {
            out.push_str(&format!("\x1b[{row}A"));
        }
        out.push_str(ERASE_TO_END);
    }
    out.push_str(RESET);
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    #[cfg(windows)]
    restore_console_mode();
}

struct Frame {
    state: Mutex<FrameState>,
}

/// The glyphs the box is drawn from.
///
/// Two sets rather than one, because the owner is on Windows and a console
/// whose output code page is not UTF-8 renders `╭` as two pieces of mojibake —
/// a border that looks broken is worse than an honest ASCII one. Detection is
/// [`prefers_ascii`], and it errs towards ASCII.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct BoxChars {
    tl: char,
    tr: char,
    bl: char,
    br: char,
    h: char,
    v: char,
    /// The cut marker, and the hint's separator: both are non-ASCII in the
    /// rounded set and both have to travel with it.
    ellipsis: &'static str,
    hint: &'static str,
}

const ROUNDED: BoxChars = BoxChars {
    tl: '╭',
    tr: '╮',
    bl: '╰',
    br: '╯',
    h: '─',
    v: '│',
    ellipsis: "…",
    hint: "/exit quits · Ctrl-C interrupts",
};

const ASCII: BoxChars = BoxChars {
    tl: '+',
    tr: '+',
    bl: '+',
    br: '+',
    h: '-',
    v: '|',
    ellipsis: "...",
    hint: "/exit quits | Ctrl-C interrupts",
};

struct FrameState {
    cols: u16,
    chars: BoxChars,
    /// What the input row shows before the cursor: `"> "`, or a question.
    label: String,
    /// Text written since the last newline — the line the assistant is part way
    /// through streaming. It is part of the block rather than of the transcript
    /// until a newline arrives, because the block is erased and redrawn whole
    /// and a half-written line inside the erased region has to be redrawn with
    /// it. Committed, verbatim and exactly once, the moment its newline lands.
    partial: String,
    /// Rows from the top of the drawn block down to the cursor. `None` when
    /// nothing is drawn, which is only true before the first paint.
    cursor_row: Option<u16>,
}

impl Frame {
    /// Decide whether to draw, and if so, draw the empty box.
    ///
    /// Every arm that returns `None` is a fallback to the pre-box output, and
    /// the bias is heavily towards taking it: a plain-but-correct terminal
    /// beats a pretty-but-broken one.
    fn install() -> Option<Frame> {
        let size = terminal_size();
        if fallback_reason(
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
        // fall back unless it stuck. Legacy `conhost.exe` fails the set and
        // returns here with `false`; Windows Terminal and modern conhost do
        // not. Nothing downstream re-checks, so this is the only place the
        // question is asked.
        #[cfg(windows)]
        if !enable_vt() {
            return None;
        }
        let (cols, _) = size?;

        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                // Before the message, so the message is readable.
                restore_terminal();
                previous(info);
            }));
        });

        let frame = Frame {
            state: Mutex::new(FrameState {
                cols,
                chars: if prefers_ascii(
                    std::env::var_os("EMMA_ASCII_FRAME").is_some(),
                    console_is_utf8(),
                ) {
                    ASCII
                } else {
                    ROUNDED
                },
                label: IDLE.to_string(),
                partial: String::new(),
                cursor_row: None,
            }),
        };

        // A newline first: whatever is on the current line is somebody's shell
        // prompt and the box should not be drawn on top of it. Nothing is
        // anchored, measured or positioned — the box lands wherever the cursor
        // already was, which is the bottom of whatever the terminal is showing.
        let mut out = String::from("\r\n");
        {
            let mut st = frame.state.lock().expect("fresh mutex");
            out.push_str(&st.draw());
        }
        let mut stdout = std::io::stdout();
        stdout.write_all(out.as_bytes()).ok()?;
        stdout.flush().ok()?;

        BLOCK_CURSOR_ROW.store(INPUT_ROW, Ordering::SeqCst);
        FRAME_ON.store(true, Ordering::SeqCst);
        Some(frame)
    }

    /// One repaint, composed whole and written once.
    ///
    /// A repaint split across several writes is a repaint the user watches
    /// happen. Everything except the size query and the write is in
    /// [`FrameState`], which is pure and is therefore the part that can be
    /// asserted on — drawing into a real terminal cannot be tested from here,
    /// because cargo hands the test binary a pipe, but the bytes that would be
    /// drawn can be, and they are.
    fn repaint(&self, f: impl FnOnce(&mut FrameState) -> String) {
        // Re-measured every paint. A stale width is the most visible possible
        // bug — a box that does not reach the edge, or one that wraps and
        // doubles in height — and paints happen at human speed, so one syscall
        // beats learning `SIGWINCH` on unix and a console event thread on
        // Windows to be told something we can simply ask for.
        let cols = terminal_size().map(|(c, _)| c);
        // A poisoned lock here means a panic mid-paint. The frame is already
        // being torn down by the hook; carrying on with the inner value is
        // strictly better than a second panic on the way out.
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cols) = cols {
            st.cols = cols;
        }
        let out = f(&mut st);
        BLOCK_CURSOR_ROW.store(st.cursor_row.unwrap_or(u16::MAX), Ordering::SeqCst);
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    fn write(&self, body: &str) {
        self.repaint(|st| st.write(body));
    }

    fn set_label(&self, label: &str) {
        self.repaint(|st| st.set_label(label));
    }

    fn submitted(&self, line: Option<&str>) {
        self.repaint(|st| st.submitted(line));
    }
}

impl FrameState {
    /// Move to the top of the drawn block and wipe it.
    ///
    /// Only ever relative movement, and only ever upwards by a number this type
    /// wrote down when it drew. Everything above that row is the terminal's.
    fn erase(&mut self) -> String {
        match self.cursor_row.take() {
            None => String::new(),
            Some(0) => ERASE_TO_END.to_string(),
            Some(n) => format!("\x1b[{n}A{ERASE_TO_END}"),
        }
    }

    /// Draw the block at the cursor, which must be at column one of a free row,
    /// and leave the cursor on the input row just past the label.
    ///
    /// The last thing emitted is the input row a second time, without its
    /// padding or right border: re-writing the same glyphs is how the cursor
    /// lands exactly past the text without this type having to count columns
    /// through escape sequences. Cooked-mode echo then appears inside the box,
    /// which is what makes typing land in the right place with no raw mode and
    /// therefore no rewrite of `LineSource`.
    fn draw(&mut self) -> String {
        let mut out = String::new();
        let mut row = 0u16;

        // The half-written line, above the box, redrawn because the erase that
        // preceded this took it with the box.
        // `\r\n` rather than `\n` for every row break the box owns. A bare line
        // feed moves down without moving left unless the terminal is adding the
        // carriage return itself, and a box drawn on a terminal that is not
        // would step one column right per row. The transcript above keeps its
        // own newlines exactly as they were handed over — those are ordinary
        // output and not ours to rewrite.
        if !self.partial.is_empty() {
            out.push_str(&self.partial);
            out.push_str("\r\n");
            row += rows_used(&self.partial, self.cols);
        }

        let width = box_width(self.cols);
        let inner = usize::from(width - 4);
        let c = self.chars;
        let rule: String = std::iter::repeat_n(c.h, usize::from(width - 2)).collect();
        let label = fit(&self.label, inner, c.ellipsis);

        out.push_str(&format!("{DIM}{}{rule}{}{RESET}\r\n", c.tl, c.tr));
        out.push_str(&format!(
            "{DIM}{}{RESET} {BOLD}{label:<inner$}{RESET} {DIM}{}{RESET}\r\n",
            c.v, c.v
        ));
        out.push_str(&format!("{DIM}{}{rule}{}{RESET}\r\n", c.bl, c.br));
        // No trailing newline: the hint is the last row, and a newline after it
        // would put the cursor on a fifth row that the erase arithmetic below
        // does not know about.
        out.push_str(&format!(
            "{DIM}{}{RESET}",
            fit(c.hint, usize::from(width), c.ellipsis)
        ));

        out.push_str(&format!(
            "\x1b[{ROWS_BELOW_INPUT}A\r{DIM}{}{RESET} {BOLD}{label}{RESET}",
            c.v
        ));
        self.cursor_row = Some(row + INPUT_ROW);
        out
    }

    /// Transcript out, box back underneath it.
    ///
    /// The body is passed through byte for byte. Nothing rewrites newlines —
    /// the terminal is in cooked mode, so `\n` is already a carriage return and
    /// a line feed — and nothing truncates or wraps it, because wrapping the
    /// transcript is the terminal's job and doing it here is how a program ends
    /// up owning a scrollback buffer.
    fn write(&mut self, body: &str) -> String {
        let mut out = self.erase();
        let mut pending = std::mem::take(&mut self.partial);
        pending.push_str(body);
        match pending.rfind('\n') {
            Some(i) => {
                out.push_str(&pending[..=i]);
                self.partial = pending[i + 1..].to_string();
            }
            None => self.partial = pending,
        }
        out.push_str(&self.draw());
        out
    }

    fn set_label(&mut self, label: &str) -> String {
        let mut out = self.erase();
        self.label = label.to_string();
        out.push_str(&self.draw());
        out
    }

    /// The user pressed return, and the terminal echoed it.
    ///
    /// That echo moved the cursor down one row, off the input row, so the erase
    /// has to reach one row further — which is the whole reason this cannot be
    /// inferred and has to be called. The question and the answer then go into
    /// the transcript together as one line, because a record of what was
    /// approved that shows only the answer is not a record of anything.
    fn submitted(&mut self, line: Option<&str>) -> String {
        if let (Some(row), Some(_)) = (self.cursor_row, line) {
            self.cursor_row = Some(row + 1);
        }
        let mut out = self.erase();
        if let Some(line) = line {
            out.push_str(&format!("{DIM}{}{}{RESET}\r\n", self.label, line));
        }
        self.label = IDLE.to_string();
        out.push_str(&self.draw());
        out
    }
}

/// The box is one column narrower than the window.
///
/// A row of exactly `cols` characters leaves the cursor at the right margin,
/// and terminals disagree about whether the wrap happens then or on the next
/// character. Getting that wrong adds a row to the block, which makes every
/// subsequent erase one row short and leaves a trail of dead borders up the
/// screen. One spare column costs nothing and removes the disagreement.
fn box_width(cols: u16) -> u16 {
    cols.saturating_sub(1).max(8)
}

/// How many screen rows a line of `n` characters occupies at this width.
///
/// Counting characters rather than display width, so a line containing an emoji
/// or a CJK glyph is under-counted and the block can be redrawn one row low for
/// as long as that line is unterminated. It self-corrects the moment the line
/// ends, which for streamed prose is within a few hundred milliseconds; the
/// alternative is a full width table, which is a dependency and a correctness
/// claim this file cannot keep on its own.
fn rows_used(text: &str, cols: u16) -> u16 {
    let cols = usize::from(cols.max(1));
    let n = text.chars().count();
    n.div_ceil(cols).max(1) as u16
}

/// Cut a line to a character budget.
///
/// Only ever called on strings this module built and knows contain no escape
/// sequences, which is why counting characters is a legitimate way to measure
/// them. Colour is applied by the caller, outside the cut.
fn fit(text: &str, budget: usize, ellipsis: &str) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }
    let keep = budget.saturating_sub(ellipsis.chars().count());
    text.chars().take(keep).collect::<String>() + ellipsis
}

/// Whether to draw the box out of ASCII rather than box-drawing characters.
///
/// Pure, and biased: `utf8` has to be *proved* before the rounded set is used.
/// A console that reports a legacy code page renders every one of those glyphs
/// as mojibake, and a border made of question marks reads as a broken program
/// rather than as a stylistic choice.
fn prefers_ascii(opted_out: bool, utf8: bool) -> bool {
    opted_out || !utf8
}

// endregion: The input box

// region: The fallback
// ---------------------------------------------------------------------------
// The fallback
//
// The decision to draw at all, kept pure and separate because it is the part
// that has to be right: drawing is either visible or it is not, but drawing
// *when we should not have* produces a garbled terminal on somebody else's
// machine, or a file full of escape sequences.
// ---------------------------------------------------------------------------

/// Why the box is not being drawn, or `None` to draw it.
fn fallback_reason(
    stdout_tty: bool,
    stdin_tty: bool,
    term_var: Option<&str>,
    opted_out: bool,
    size: Option<(u16, u16)>,
) -> Option<&'static str> {
    if opted_out {
        return Some("EMMA_NO_FRAME is set");
    }
    // Both directions matter and for different reasons. Redirected output means
    // the escape sequences end up in somebody's file; redirected input means
    // there is nobody typing, so an input box is decoration on a batch job.
    if !stdout_tty {
        return Some("stdout is not a terminal");
    }
    if !stdin_tty {
        return Some("stdin is not a terminal");
    }
    // `dumb` is the one value that promises the opposite of what we need.
    if term_var == Some("dumb") {
        return Some("TERM=dumb");
    }
    match size {
        None => Some("the window size could not be read"),
        Some((cols, rows)) if rows < MIN_ROWS || cols < MIN_COLS => Some("the window is too small"),
        Some(_) => None,
    }
}

// endregion: The fallback

// region: Asking the platform
// ---------------------------------------------------------------------------
// Asking the platform
//
// Three questions, none of which can be guessed: how wide is the window, will
// this console honour VT at all, and will it render a box-drawing character.
// ---------------------------------------------------------------------------

/// `(cols, rows)` of the visible window, not the buffer.
///
/// On Windows those differ: the screen buffer is usually far taller than the
/// window. Only `cols` is load-bearing now — nothing here addresses a row — and
/// `rows` is used solely to refuse a window too short to be worth drawing in.
#[cfg(windows)]
fn terminal_size() -> Option<(u16, u16)> {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle: HANDLE = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        let cols = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
        let rows = i32::from(info.srWindow.Bottom) - i32::from(info.srWindow.Top) + 1;
        if cols <= 0 || rows <= 0 {
            return None;
        }
        Some((cols as u16, rows as u16))
    }
}

#[cfg(unix)]
fn terminal_size() -> Option<(u16, u16)> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) != 0 {
            return None;
        }
        if ws.ws_col == 0 || ws.ws_row == 0 {
            return None;
        }
        Some((ws.ws_col, ws.ws_row))
    }
}

#[cfg(not(any(windows, unix)))]
fn terminal_size() -> Option<(u16, u16)> {
    // An unknown platform gets the fallback, which is the whole product.
    None
}

/// Will this console render the UTF-8 bytes Rust is about to write to it.
///
/// On Windows that is a code page, and the answer is usually no: Windows
/// Terminal displays UTF-8 happily but the console's *output code page* is
/// still whatever the system locale says unless somebody changed it, and the
/// bytes are decoded by that code page on the way through. So the honest answer
/// is "only when it is 65001", and the box is ASCII the rest of the time.
#[cfg(windows)]
fn console_is_utf8() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleOutputCP;
    // `CP_UTF8`, spelled out rather than imported: the constant lives behind
    // the `Win32_Globalization` feature of `windows-sys` and pulling a whole
    // module in for one integer is a dependency for a number that has not
    // changed since Windows 95.
    const CP_UTF8: u32 = 65001;
    unsafe { GetConsoleOutputCP() == CP_UTF8 }
}

/// On unix the locale is the only thing that answers this, and it answers it
/// well: a terminal running under a `UTF-8` locale renders these glyphs.
#[cfg(not(windows))]
fn console_is_utf8() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .any(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
}

/// The console mode as we found it, so exit can put it back. `u32::MAX` is the
/// "we never changed it" sentinel — no real console mode has every bit set.
#[cfg(windows)]
static ORIGINAL_MODE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// Turn on VT processing and *prove* it turned on.
///
/// `SetConsoleMode` returning success is not the same claim as the flag being
/// honoured, and the failure this guards against is legacy `conhost.exe`, where
/// the flag is unknown and the call fails outright. Reading the mode back is
/// one syscall and turns "probably" into "yes".
#[cfg(windows)]
fn enable_vt() -> bool {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle: HANDLE = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            // Already on and not ours to restore.
            return true;
        }
        ORIGINAL_MODE.store(mode, Ordering::SeqCst);
        if SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
            ORIGINAL_MODE.store(u32::MAX, Ordering::SeqCst);
            return false;
        }
        let mut check = 0u32;
        if GetConsoleMode(handle, &mut check) == 0 {
            return false;
        }
        check & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
    }
}

#[cfg(windows)]
fn restore_console_mode() {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetConsoleMode, STD_OUTPUT_HANDLE};
    let mode = ORIGINAL_MODE.swap(u32::MAX, Ordering::SeqCst);
    if mode == u32::MAX {
        return;
    }
    unsafe {
        let handle: HANDLE = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return;
        }
        SetConsoleMode(handle, mode);
    }
}

// endregion: Asking the platform

// region: The one place stdin is read
// ---------------------------------------------------------------------------
// The one place stdin is read
//
// A single reader behind a channel. The goal prompt and the approval prompt
// both want lines, and two readers on one stdin race for them.
//
// **The box changed nothing here, deliberately.** The terminal stays in cooked
// mode: the line discipline does the editing, echoes into the box because that
// is where the cursor was left, and hands whole lines to the same thread that
// always read them. Raw-mode key handling would have meant reimplementing
// `drain` as "discard the pending key buffer", and the property it protects was
// a live security bug — the version that cannot regress is the one that was not
// rewritten. What it costs is listed in the report: no history, no completion,
// no editing beyond what the terminal itself offers, and input longer than the
// box is wide runs past the right border until the next redraw tidies it.
// ---------------------------------------------------------------------------

/// The one place stdin is read.
///
/// A single reader, fed by a blocking thread, because the goal prompt and the
/// approval prompt both need lines and two independent readers race for them:
/// the loser buffers the answer to a question the winner asked. One queue
/// means a line is consumed exactly once by whoever asked most recently.
///
/// **The queue must be emptied before a question is asked.** The reader thread
/// is eager — it consumes whatever is typed, whenever it is typed, and holds
/// it. Without a drain, a line typed while the agent was working is waiting in
/// the channel when the next prompt appears, and `next()` hands it over as the
/// answer. That is not a stale-input annoyance; it is one question being
/// answered by a keystroke aimed at another, and the one place it matters most
/// is the approval gate, where the line in question is a `y`.
///
/// Observed rather than theorised, on the first real run: an answered `y`
/// outlived a turn that aborted on its token budget and was consumed as the
/// *next goal*. The same path could as easily have approved a `Bash` command
/// the user never saw — and left a log saying they approved it.
///
/// Type-ahead is a convenience. Answering an unseen question is a hazard.
pub struct LineSource {
    rx: mpsc::Receiver<String>,
}

impl LineSource {
    pub fn stdin() -> Self {
        let (tx, rx) = mpsc::channel(8);
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut stdin.lock(), &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx
                            .blocking_send(line.trim_end_matches(['\r', '\n']).to_string())
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        Self { rx }
    }

    /// Discard anything typed before now, and report how much was dropped.
    ///
    /// Called immediately before a prompt is printed, so the only line that can
    /// answer a question is one typed after seeing it. The count is returned
    /// rather than swallowed because silently eating a line a person typed is
    /// its own small betrayal — the caller says so.
    pub fn drain(&mut self) -> usize {
        let mut dropped = 0;
        while self.rx.try_recv().is_ok() {
            dropped += 1;
        }
        dropped
    }

    pub async fn next(&mut self) -> Option<String> {
        self.rx.recv().await
    }

    /// A queue fed from a list rather than from stdin, for tests that need to
    /// assert on the drain rather than on a terminal.
    #[cfg(test)]
    fn scripted(lines: &[&str]) -> Self {
        let (tx, rx) = mpsc::channel(16);
        for line in lines {
            tx.try_send((*line).to_string())
                .expect("test queue is big enough");
        }
        Self { rx }
    }
}

// endregion: The one place stdin is read

// region: The banner argument
// ---------------------------------------------------------------------------
// The banner argument
//
// Which single argument identifies a tool call on one line. Per tool rather
// than by rule, so a `Write` never puts its content on the screen.
// ---------------------------------------------------------------------------

/// The one argument worth putting on the tool's own line.
///
/// Chosen per tool rather than "the first string field": a `Write` whose banner
/// showed its content would push the next twenty tool calls off the screen.
fn summarise_args(name: &str, args: &serde_json::Value) -> String {
    let s = |k: &str| {
        args.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let one_line = |t: String| {
        let t = t.replace('\n', " ");
        if t.chars().count() > 88 {
            format!("{}…", t.chars().take(88).collect::<String>())
        } else {
            t
        }
    };
    match name {
        "Bash" => one_line(s("command")),
        "Read" | "Write" | "Edit" => one_line(s("file_path")),
        "Glob" => one_line(s("pattern")),
        "Grep" => one_line(s("pattern")),
        "Skill" => one_line(s("name")),
        "WebFetch" => one_line(s("url")),
        "WebSearch" => one_line(s("query")),
        _ => one_line(args.to_string()),
    }
}

// endregion: The banner argument

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Everything here that *decides* something, and nothing that requires eyes.
// The box cannot be looked at from a test binary — cargo gives it a pipe, not a
// terminal, which is exactly the condition under which `fallback_reason` says
// no — so what is asserted is the byte stream: that the transcript passes
// through it untouched and in order, that no scroll region is ever set, that
// every write erases before it prints and redraws after, and that a question
// survives whatever is printed under it.
//
// What none of this can confirm is that the result *looks* right. It has not
// been seen. The report says so.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_banner_argument_is_the_one_a_human_would_look_at() {
        assert_eq!(
            summarise_args("Bash", &json!({ "command": "cargo test", "timeout_ms": 1 })),
            "cargo test"
        );
        // The failure this prevents: a Write banner that prints the file.
        let big = json!({ "file_path": "src/a.rs", "content": "x".repeat(5_000) });
        assert_eq!(summarise_args("Write", &big), "src/a.rs");
    }

    #[test]
    fn a_long_command_is_cut_to_one_line() {
        let out = summarise_args("Bash", &json!({ "command": "a\nb\n".repeat(200) }));
        assert!(!out.contains('\n'));
        assert!(out.chars().count() <= 89, "{}", out.chars().count());
    }

    // -----------------------------------------------------------------------
    // The fallback
    //
    // Written as "which condition sends us back to plain output", because that
    // is the requirement: today's behaviour is the default and the box is the
    // exception that has to earn itself.
    // -----------------------------------------------------------------------

    const BIG: Option<(u16, u16)> = Some((120, 40));

    #[test]
    fn a_real_terminal_of_a_reasonable_size_gets_the_box() {
        assert_eq!(
            fallback_reason(true, true, Some("xterm-256color"), false, BIG),
            None
        );
        // No TERM at all is the normal Windows case and must not disqualify it.
        assert_eq!(fallback_reason(true, true, None, false, BIG), None);
    }

    #[test]
    fn anything_that_is_not_a_terminal_falls_back() {
        // `emma … | tee` and `emma < script` respectively. Both produce a file
        // full of escape sequences if this is wrong.
        assert!(fallback_reason(false, true, Some("xterm"), false, BIG).is_some());
        assert!(fallback_reason(true, false, Some("xterm"), false, BIG).is_some());
    }

    #[test]
    fn a_terminal_that_says_it_is_dumb_is_believed() {
        assert!(fallback_reason(true, true, Some("dumb"), false, BIG).is_some());
    }

    #[test]
    fn the_escape_hatch_wins_over_everything_else() {
        // First arm on purpose: somebody setting this has a broken terminal in
        // front of them and no interest in the other five checks agreeing.
        assert!(fallback_reason(true, true, Some("xterm"), true, BIG).is_some());
    }

    #[test]
    fn an_unmeasurable_or_tiny_window_falls_back() {
        assert!(fallback_reason(true, true, Some("xterm"), false, None).is_some());
        assert!(fallback_reason(true, true, Some("xterm"), false, Some((120, 4))).is_some());
        assert!(fallback_reason(true, true, Some("xterm"), false, Some((10, 40))).is_some());
    }

    #[test]
    fn the_box_is_ascii_unless_utf8_is_proved() {
        assert!(
            prefers_ascii(false, false),
            "an unproved console got glyphs"
        );
        assert!(prefers_ascii(true, true), "the opt-out was ignored");
        assert!(!prefers_ascii(false, true));
    }

    // -----------------------------------------------------------------------
    // What a repaint emits
    //
    // Written against the properties that make the box correct rather than
    // against exact byte strings, which would fail on every cosmetic change and
    // teach the next person to update the expected value without reading it.
    // -----------------------------------------------------------------------

    fn state() -> FrameState {
        FrameState {
            cols: 40,
            chars: ASCII,
            label: IDLE.to_string(),
            partial: String::new(),
            cursor_row: None,
        }
    }

    /// A drawn state: what every method other than the first paint starts from.
    fn drawn() -> FrameState {
        let mut st = state();
        st.draw();
        st
    }

    /// The defect the previous design shipped, as an assertion.
    ///
    /// `ESC[{top};{bottom}r` with a top margin below row 1 does not preserve
    /// scrollback — the terminal *discards* lines that scroll off the top of
    /// the region. The owner could not scroll. Nothing this file emits may ever
    /// set a scrolling region again, and the cheapest way to keep that true is
    /// to assert it over every kind of paint there is.
    #[test]
    fn nothing_this_emits_ever_sets_a_scrolling_region() {
        let mut st = state();
        let mut all = st.draw();
        all.push_str(&st.write("● Bash cargo test\n"));
        all.push_str(&st.set_label("allow? [y]es  [n]o: "));
        all.push_str(&st.write("  running 12 tests\n"));
        all.push_str(&st.submitted(Some("y")));
        all.push_str(&st.write("half a line with no newline"));
        all.push_str(&st.write(" and the rest of it\n"));

        // `ESC[...r` in any form: `ESC[2;39r`, `ESC[r`, `ESC[?1049h` for the
        // alternate screen. None of them.
        for (i, _) in all.match_indices('\x1b') {
            let seq = &all[i..];
            assert!(
                !is_scroll_region(seq),
                "a scrolling region was set: {:?}",
                &seq[..seq.len().min(12)]
            );
        }
        assert!(!all.contains("\x1b[?1049"), "the alternate screen was used");
    }

    /// `ESC[` … `r` — DECSTBM, in any parameter form.
    fn is_scroll_region(seq: &str) -> bool {
        let Some(rest) = seq.strip_prefix("\x1b[") else {
            return false;
        };
        rest.chars()
            .find(|c| !c.is_ascii_digit() && *c != ';')
            .map(|c| c == 'r')
            .unwrap_or(false)
    }

    /// The property the whole design rests on: output is *ordinary output*. It
    /// goes to the terminal exactly as it was handed over, in order, once — so
    /// the terminal scrolls it, wraps it and keeps it in scrollback the way it
    /// keeps every other program's output.
    ///
    /// Delete this and the tempting regression is a frame that "helpfully"
    /// rewrites newlines, wraps long lines to the window, or buffers a
    /// transcript of its own. Each of those is a step back towards owning the
    /// screen, and owning the screen is what cost the owner his scrollback.
    #[test]
    fn the_transcript_passes_through_untouched_and_in_order() {
        let mut st = drawn();
        let lines = [
            "● Bash cargo test\n",
            "  running 12 tests\r\n",
            "  a line far longer than the forty columns this window claims to have\n",
            "\n",
            "Ported the middleware.\n",
        ];
        let mut out = String::new();
        for line in lines {
            out.push_str(&st.write(line));
        }
        let mut at = 0;
        for line in lines {
            let found = out[at..]
                .find(line)
                .unwrap_or_else(|| panic!("{line:?} was not written verbatim"));
            at += found + line.len();
        }
        // Once each, not once per redraw: a line that is re-emitted every time
        // the box repaints is a transcript being rewritten rather than scrolled.
        for line in lines {
            if line != "\n" {
                assert_eq!(out.matches(line).count(), 1, "{line:?} was written twice");
            }
        }
    }

    /// The redraw itself, in the order that makes it work: erase upward from
    /// the cursor to the top of the block, clear to the end of the screen,
    /// write, draw again underneath.
    ///
    /// Without the erase the old box stays on screen and a new one is drawn
    /// under it, once per line of output. Without the redraw the box is gone
    /// after the first thing that is printed, which is where this started.
    #[test]
    fn every_write_erases_the_box_first_and_draws_it_again_after() {
        let mut st = drawn();
        let out = st.write("● Bash cargo test\n");
        assert!(
            out.starts_with(&format!("\x1b[{INPUT_ROW}A{ERASE_TO_END}")),
            "{out:?}"
        );
        let erased = out.find(ERASE_TO_END).unwrap();
        let wrote = out.find("● Bash").expect("the line was not emitted");
        let redrew = out.rfind(ASCII.tl).expect("the box was not drawn again");
        assert!(erased < wrote, "the box was erased after the output");
        assert!(wrote < redrew, "the box was drawn before the output");
        // And the cursor is back on the input row, so cooked-mode echo lands
        // inside the box rather than under it.
        assert_eq!(st.cursor_row, Some(INPUT_ROW));
        assert!(out.ends_with(RESET));
    }

    /// R6, the reason any of this exists: the `allow?` question scrolled away
    /// under the tool output that followed it, and a security prompt nobody can
    /// read manufactures consent.
    ///
    /// The fix has to hold *while output is still arriving*, so that is what is
    /// tested — write under a pending question and the question must still be
    /// the last thing on the screen.
    #[test]
    fn a_pending_question_survives_everything_printed_under_it() {
        let mut st = drawn();
        st.set_label("allow? [y]es  [n]o: ");
        let mut out = String::new();
        for i in 0..5 {
            out.push_str(&st.write(&format!("  output line {i}\n")));
        }
        let last_output = out.rfind("output line 4").expect("the output vanished");
        let question = out.rfind("allow?").expect("the question vanished");
        assert!(
            question > last_output,
            "the question was printed above the output that followed it"
        );
        // Twice per redraw — once in the padded box row, once in the reposition
        // that puts the cursor after it — and the last of those is the end of
        // the stream, which is where the user's eye and the terminal's cursor
        // both are.
        assert!(out.trim_end_matches(RESET).ends_with(": "), "{out:?}");
    }

    /// Streamed prose arrives in fragments with no newlines in them, and the
    /// box has to be redrawn under each one. The half-written line is therefore
    /// part of the block: erased with it, redrawn with it, and committed to the
    /// transcript exactly once — when its newline lands.
    ///
    /// The failure without this is the one that makes streaming unusable: every
    /// fragment forced onto its own line, so a paragraph arrives as a column.
    #[test]
    fn a_half_written_line_is_redrawn_with_the_box_and_committed_once() {
        let mut st = drawn();
        st.write("Ported the ");
        st.write("middleware. ");
        assert_eq!(st.partial, "Ported the middleware. ");
        // One row of half-written prose sits above the box, so the block is a
        // row taller and the erase has to climb past it.
        assert_eq!(st.cursor_row, Some(1 + INPUT_ROW));
        let out = st.write("Tests are green.\n");
        assert!(
            out.contains("Ported the middleware. Tests are green.\n"),
            "the line was not committed whole: {out:?}"
        );
        assert_eq!(st.partial, "");
    }

    /// A line that wraps sits on more than one row, and the erase has to climb
    /// all of them or it leaves a dead border behind on every repaint.
    #[test]
    fn the_erase_climbs_a_wrapped_half_written_line() {
        let mut st = drawn();
        st.cols = 20;
        st.write(&"x".repeat(45)); // three rows at twenty columns
        assert_eq!(st.cursor_row, Some(3 + INPUT_ROW));
        assert!(st.write("\n").starts_with("\x1b[4A"));
    }

    /// The terminal echoes the newline itself, one row below where the cursor
    /// was. Nothing in this process can observe that, so `submitted` is told —
    /// and if it were not, every erase afterwards would be one row short and
    /// the screen would fill with stranded boxes.
    #[test]
    fn a_submitted_line_accounts_for_the_echoed_newline() {
        let mut st = drawn();
        st.set_label("allow? [y]es  [n]o: ");
        let out = st.submitted(Some("y"));
        assert!(
            out.starts_with(&format!("\x1b[{}A", INPUT_ROW + 1)),
            "the echoed newline was not accounted for: {out:?}"
        );
        // The question and its answer become one ordinary transcript line, so
        // what was approved reads back the way it was asked.
        assert!(out.contains("allow? [y]es  [n]o: y"), "{out:?}");
        // And the box goes back to waiting for a goal.
        assert_eq!(st.label, IDLE);
    }

    /// End of input — Ctrl-D, or a closed pipe. No newline was echoed, so the
    /// cursor did not move, and erasing a row too high would eat a line of the
    /// user's transcript.
    #[test]
    fn end_of_input_does_not_pretend_a_newline_was_echoed() {
        let mut st = drawn();
        let out = st.submitted(None);
        assert!(out.starts_with(&format!("\x1b[{INPUT_ROW}A")), "{out:?}");
    }

    /// The box spans the window and is re-measured on every paint, because a
    /// stale width is the most visible bug available: a border that stops
    /// short, or one that wraps and silently adds a row to the block.
    #[test]
    fn the_box_is_redrawn_to_the_current_width() {
        let mut st = drawn();
        st.cols = 100;
        let out = st.write("x\n");
        let rule: String = std::iter::repeat_n(ASCII.h, usize::from(box_width(100) - 2)).collect();
        assert!(out.contains(&rule), "the box did not span the new width");
        // One column short of the window, deliberately: see `box_width`.
        assert_eq!(box_width(100), 99);
        assert!(box_width(100) < 100);
    }

    /// A question longer than the box is cut rather than wrapped. Wrapping adds
    /// a row the erase arithmetic does not know about, and the box walks up the
    /// screen one row per repaint from then on.
    #[test]
    fn a_label_wider_than_the_box_is_cut_not_wrapped() {
        let mut st = drawn();
        st.cols = 30;
        let long = "allow? ".to_string() + &"a".repeat(200);
        let out = st.set_label(&long);
        // `\r` starts a row over as surely as `\n` starts a new one, and the
        // reposition that leaves the cursor after the label uses one.
        for row in strip_escapes(&st.set_label(&long)).split(['\n', '\r']) {
            assert!(
                row.chars().count() <= usize::from(st.cols),
                "a row was {} columns wide: {row:?}",
                row.chars().count()
            );
        }
        assert!(out.contains(ASCII.ellipsis));
    }

    /// Everything a terminal would not print as a glyph, removed — so what is
    /// left is what occupies columns.
    fn strip_escapes(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Every sequence this file emits is `ESC[` … one letter.
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Every row break the box owns carries its own carriage return.
    ///
    /// A bare line feed moves the cursor down and, on a terminal that is not
    /// adding the return itself, leaves it where it was horizontally. The box
    /// would then step one column right per row and the whole thing would lean.
    /// Cheap to emit, invisible when the terminal was going to do it anyway,
    /// and the class of bug it prevents is one nobody can reproduce on the
    /// machine where it was written.
    #[test]
    fn every_row_the_box_owns_ends_with_a_carriage_return() {
        let mut st = drawn();
        st.write("streaming prose with no newline in it");
        let out = st.draw();
        for (i, _) in out.match_indices('\n') {
            assert!(
                out[..i].ends_with('\r'),
                "a bare line feed at {i}: {:?}",
                &out[i.saturating_sub(20)..i]
            );
        }
        // And the transcript's own newlines are still passed through untouched
        // — those belong to whoever wrote the line, not to this file.
        assert!(st.write("a\nb\n").contains("a\nb\n"));
    }

    #[test]
    fn a_short_line_is_left_exactly_alone_and_a_long_one_is_cut() {
        assert_eq!(fit("emma", 40, "…"), "emma");
        let cut = fit(&"a".repeat(200), 10, "…");
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_wrapped_line_is_counted_in_rows() {
        assert_eq!(rows_used("", 20), 1);
        assert_eq!(rows_used(&"x".repeat(20), 20), 1);
        assert_eq!(rows_used(&"x".repeat(21), 20), 2);
        // A zero width would divide by zero, and a resize can report anything.
        assert!(rows_used("xx", 0) >= 1);
    }

    // -----------------------------------------------------------------------
    // Restore
    // -----------------------------------------------------------------------

    #[test]
    fn restoring_a_terminal_that_was_never_drawn_on_writes_nothing() {
        // The `Drop`, the panic hook and `main` can all reach this, and the
        // common case — `-p`, a test binary, a pipe — never drew a box at all.
        // A restore that emitted escapes regardless would corrupt the output of
        // every non-interactive run.
        assert!(!FRAME_ON.load(Ordering::SeqCst));
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst));
    }

    #[test]
    fn restore_runs_once_however_many_times_it_is_called() {
        // Set by hand: a test binary has no terminal, so `install` correctly
        // refuses and cannot set this for us. What is under test is the latch,
        // which is what makes `Drop` + panic hook + explicit call safe.
        FRAME_ON.store(true, Ordering::SeqCst);
        BLOCK_CURSOR_ROW.store(INPUT_ROW, Ordering::SeqCst);
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst), "the latch did not clear");
        assert_eq!(BLOCK_CURSOR_ROW.load(Ordering::SeqCst), u16::MAX);
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst));
    }

    // -----------------------------------------------------------------------
    // The drain
    //
    // Unchanged by the box, and tested here so that a future move to raw mode
    // has to delete an assertion rather than quietly lose the property.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_line_typed_before_the_question_cannot_answer_it() {
        // The bug, in miniature: `y` was typed at some earlier moment, a
        // question is now being asked, and the answer must be the line typed
        // after it — not the one already in the queue.
        let mut lines = LineSource::scripted(&["y", "y"]);
        assert_eq!(lines.drain(), 2, "the queue was not emptied");
        // Nothing left to hand over, so a question asked now waits for a
        // person instead of consuming their old keystroke.
        assert_eq!(lines.rx.try_recv().ok(), None);
    }

    #[tokio::test]
    async fn draining_an_empty_queue_drops_nothing_and_says_so() {
        // The count is what the caller turns into "ignoring N lines"; a false
        // positive there tells a user their input was eaten when it was not.
        let mut lines = LineSource::scripted(&[]);
        assert_eq!(lines.drain(), 0);
    }
}

// endregion: Tests
