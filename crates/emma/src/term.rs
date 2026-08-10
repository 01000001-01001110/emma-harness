//! Line-oriented terminal output, inside a frame the terminal keeps for us.
//!
//! Three things here are not cosmetic.
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
//! **The question is pinned and the transcript is not.** Emma asks before it
//! writes a file or runs a command, and on the first real run that question
//! scrolled off under the tool output that followed it. `approval.rs` argues
//! that a prompt the user cannot evaluate manufactures consent; a prompt the
//! user cannot *see* is the same defect with the argument already made. So the
//! prompt lives on a row that output structurally cannot reach.
//!
//! **How the frame is built, and what it deliberately is not.** A DEC scroll
//! region (`ESC[{top};{bottom}r`) bounds the rows that scroll. Everything
//! outside those margins is ours to paint and nothing that scrolls can touch
//! it, so one row at the top carries the run's identity and one row at the
//! bottom carries the prompt. This is *not* the alternate screen buffer and
//! must not become it: the alternate screen costs scrollback and, on several
//! terminals, mouse selection — and Emma's output is a transcript people read
//! after the run, in the terminal they ran it in. A `ratatui` app owning the
//! screen was considered and rejected for exactly that.
//!
//! **The frame is a luxury and the fallback is the product.** Nothing below
//! makes a decision that a plain stream of lines could not; if the terminal
//! cannot be verified to support VT processing, if either stream is redirected,
//! if the window is tiny, or if `EMMA_NO_FRAME` is set, [`Term`] degrades to
//! exactly the line-by-line output it had before the frame existed. `-p` never
//! gets a frame at all, and neither does [`Term::silent`].

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
// The second split is `frame`: `Some` means the three-zone layout, `None`
// means the original line-by-line output. Every method below is written so
// that `None` reproduces the old behaviour character for character.
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
    /// The pinned frame, when the terminal was verified able to carry one.
    /// `None` is the whole fallback: every method checks it and takes the
    /// pre-frame path, which is why `printing()` and `silent()` need no special
    /// handling beyond never constructing one.
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

    /// True when the three-zone frame is drawn. For callers that want to say
    /// something once about the layout, and for tests.
    pub fn framed(&self) -> bool {
        self.frame.is_some()
    }

    /// What the top row says: the facts that cannot go stale.
    ///
    /// Model, working directory and transcript path are fixed for the life of
    /// the process, which is the entire reason they are the only things on that
    /// row. Token spend belongs to the loop, changes on every turn, and this
    /// type is not told when it changes — a permanent row carrying a number
    /// that stopped being true is worse than a row without one, so there is no
    /// number here. See the report in the commit that added this.
    pub fn set_status(&self, model: &str, cwd: &std::path::Path, session: &std::path::Path) {
        if let Some(frame) = &self.frame {
            frame.render(
                None,
                None,
                Some(format!(
                    "emma · {model} · {} · {}",
                    cwd.display(),
                    session.display()
                )),
            );
        }
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
            frame.render(Some(&format!("{}\r\n", crlf(line))), None, None);
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
            frame.render(Some(&crlf(text)), None, None);
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
            frame.render(Some("\r\n"), None, None);
        } else {
            println!();
        }
    }

    /// Whole assistant text at once, for `-p` where nothing streamed.
    pub fn text(&self, text: &str) {
        if self.enabled && !text.trim().is_empty() {
            if let Some(frame) = &self.frame {
                frame.render(Some(&format!("{}\r\n", crlf(text.trim_end()))), None, None);
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
            "  allow? [y]es  [n]o  [a]lways {tool} this session: "
        ));
    }

    /// The network question, worded so the grant on offer is the one the answer
    /// actually gives: a host for the session, not a tool and not one call.
    /// There is no third option, because a wider network grant is not offered.
    pub fn prompt_network_question(&self, host: &str) {
        self.question(&format!(
            "  allow? [y]es — and {host} again this session  [n]o: "
        ));
    }

    fn question(&self, q: &str) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            // The whole point of the bottom row: this cannot be scrolled away
            // by whatever the agent prints next, because whatever it prints
            // next is confined to the region above it.
            frame.render(None, Some(Some(q.to_string())), None);
            return;
        }
        // Whichever stream the commentary is on, so the question is never
        // separated from the thing it is asking about.
        if self.quiet {
            eprint!("{q}");
            let _ = std::io::stderr().flush();
        } else {
            print!("{q}");
            let _ = std::io::stdout().flush();
        }
    }

    pub fn goal_prompt(&self) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            frame.render(None, Some(Some("> ".to_string())), None);
            return;
        }
        print!("{} ", self.paint(BOLD, ">"));
        let _ = std::io::stdout().flush();
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

// region: The pinned frame
// ---------------------------------------------------------------------------
// The pinned frame
//
// Row 1 is the status. Rows 2..h-1 are the scroll region and hold the
// transcript. Row h is the prompt. The margins are set once with `ESC[2;{h-1}r`
// and re-set whenever the window size changes.
//
// The invariant everything here depends on: **`ESC 7` always holds the
// transcript cursor.** Every paint restores it (`ESC 8`), writes the new text —
// which scrolls the region and nothing else — saves it again, and only then
// walks outside the region to repaint the two fixed rows. Nothing else in this
// process may use DECSC/DECRC, and nothing else does.
// ---------------------------------------------------------------------------

/// Below this the three-zone layout has no transcript left to show, so it is
/// not worth the risk of drawing one.
const MIN_ROWS: u16 = 8;
const MIN_COLS: u16 = 24;

/// Set once the margins have been changed, cleared once they have been put
/// back. Global rather than owned, because the panic hook has no `self`.
static FRAME_ON: AtomicBool = AtomicBool::new(false);
/// The height the frame last drew at, so `restore_terminal` can find the
/// prompt row even if the size query fails on the way out.
static FRAME_ROWS: AtomicU16 = AtomicU16::new(0);
static PANIC_HOOK: Once = Once::new();

/// Put the terminal back: full-height scrolling, our two rows blanked, cursor
/// below the transcript.
///
/// Idempotent, and safe to call from anywhere — a `Drop`, a panic hook, or an
/// exit path that is about to call `std::process::exit` and skip both. A shell
/// left with a scroll region set is a shell whose every subsequent command
/// draws in a box, and that is a tool people uninstall rather than debug.
pub fn restore_terminal() {
    if !FRAME_ON.swap(false, Ordering::SeqCst) {
        return;
    }
    let rows = terminal_size()
        .map(|(_, r)| r)
        .unwrap_or_else(|| FRAME_ROWS.load(Ordering::Relaxed).max(1));
    let mut out = String::new();
    // Margins first. Everything after this is an ordinary write to an ordinary
    // terminal, including whatever the shell does next.
    out.push_str("\x1b[r");
    // The two rows we borrowed were somebody else's scrollback before we
    // painted them, and neither is ours to leave behind.
    out.push_str("\x1b[1;1H\x1b[2K");
    out.push_str(&format!("\x1b[{rows};1H\x1b[2K"));
    out.push_str(RESET);
    // Cursor on the blanked prompt row, which is directly under the last line
    // of the transcript: the shell prompt lands there and the session is still
    // on screen above it, which is the entire reason for not using the
    // alternate screen.
    out.push_str(&format!("\x1b[{rows};1H"));
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    #[cfg(windows)]
    restore_console_mode();
}

struct Frame {
    /// Painted on the bottom row when no question is outstanding. It claims
    /// nothing about what the agent is doing, because this type is not told.
    hint: String,
    state: Mutex<FrameState>,
}

struct FrameState {
    rows: u16,
    cols: u16,
    status: String,
    /// `Some` while a question is on screen waiting for a line. Cleared by the
    /// next write to the transcript — see `render`.
    prompt: Option<String>,
}

impl Frame {
    /// Decide whether to draw, and if so, draw the empty frame.
    ///
    /// Every arm that returns `None` is a fallback to the pre-frame output, and
    /// the bias is heavily towards taking it: a plain-but-correct terminal
    /// beats a pretty-but-broken one, and a terminal that ignores `ESC[r` does
    /// not degrade gracefully — it interleaves the prompt into the transcript
    /// and paints the status row over whatever scrolls past.
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
        let (cols, rows) = size?;

        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                // Before the message, so the message is readable.
                restore_terminal();
                previous(info);
            }));
        });

        let mut out = String::new();
        // A newline first: whatever is on the current line is somebody's shell
        // prompt and the transcript should not start on top of it.
        out.push_str("\r\n");
        out.push_str(&margins(rows));
        // Anchor the transcript at the bottom of the region. This is a jump —
        // on a freshly opened full-height window it leaves blank rows above the
        // first line of output. That is the cost of not asking the terminal
        // where the cursor is, which would mean reading a reply off stdin, and
        // stdin has exactly one reader in this process for reasons `LineSource`
        // spells out at length. Deterministic and slightly empty beats correct
        // four times out of five.
        out.push_str(&format!("\x1b[{};1H\x1b7", rows - 1));
        let mut stdout = std::io::stdout();
        stdout.write_all(out.as_bytes()).ok()?;
        stdout.flush().ok()?;

        FRAME_ROWS.store(rows, Ordering::Relaxed);
        FRAME_ON.store(true, Ordering::SeqCst);

        let frame = Frame {
            // Two facts, both verified, both otherwise invisible. `/exit` and
            // `/quit` have worked since the loop was written and appeared
            // nowhere a user looks — the owner went looking for the exit
            // command and could not find it, which is the whole argument for
            // this string existing. Nothing here may name a command that does
            // not work; see `main.rs`, where both are matched.
            hint: "/exit quits · Ctrl-C interrupts".to_string(),
            state: Mutex::new(FrameState {
                rows,
                cols,
                status: "emma".to_string(),
                prompt: None,
            }),
        };
        frame.render(None, None, None);
        Some(frame)
    }

    /// One atomic repaint.
    ///
    /// `body` is text for the transcript, `prompt` replaces the bottom row's
    /// question, `status` replaces the top row. All three are composed into a
    /// single `write_all`, because a repaint split across several writes is a
    /// repaint the user watches happen.
    ///
    /// Everything except the size query and the write itself is in
    /// [`FrameState::compose`], which is a pure function and therefore the part
    /// that can be asserted on. Drawing into a real terminal cannot be tested
    /// from here — cargo hands the test binary a pipe — but the bytes that
    /// would be drawn can be, and they are.
    fn render(&self, body: Option<&str>, prompt: Option<Option<String>>, status: Option<String>) {
        // A poisoned lock here means a panic mid-paint. The frame is already
        // being torn down by the hook; carrying on with the inner value is
        // strictly better than a second panic on the way out.
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = prompt {
            st.prompt = p;
        }
        if let Some(s) = status {
            st.status = s;
        }
        // Resize. The margins are absolute rows, so a resized window
        // invalidates them and every subsequent write would scroll the wrong
        // band. Re-querying costs one syscall per paint and paints happen at
        // human speed, so it is simply done every time rather than on a signal
        // — which would be `SIGWINCH` on unix and a console event thread on
        // Windows, two platform paths to learn something we can ask for.
        let size = terminal_size();
        if let Some((_, rows)) = size {
            FRAME_ROWS.store(rows, Ordering::Relaxed);
        }
        let out = st.compose(body, &self.hint, size);
        // The cursor is left where `compose` put it: just past the bottom row's
        // text. The terminal is in cooked mode, so that is where it echoes what
        // the user types, which is what makes this a chat bar rather than a
        // caption.
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
}

impl FrameState {
    /// The bytes of one repaint, and the only place the frame's geometry is
    /// decided.
    ///
    /// `size` is passed in rather than queried so this stays pure: the tests
    /// hand it a window that never existed, including one that changed between
    /// two paints, which is the case a real terminal will not reproduce on
    /// demand.
    fn compose(&mut self, body: Option<&str>, hint: &str, size: Option<(u16, u16)>) -> String {
        let mut out = String::new();

        // What a resize does *not* recover: the terminal's own reflow of the
        // lines already on screen. A window that gets narrower rewraps the
        // transcript under margins measured before the rewrap, so the first
        // paint after a resize can land against a line that moved. It
        // self-corrects on the next paint, and the alternative is redrawing a
        // transcript this type does not keep — which would mean keeping one,
        // which is a scrollback buffer, which is the terminal's job.
        if let Some((cols, rows)) = size {
            if rows != self.rows || cols != self.cols {
                self.rows = rows;
                self.cols = cols;
                out.push_str(&margins(rows));
                // The saved transcript cursor may now be outside the region,
                // and a `ESC 8` to a row outside the margins would write the
                // next line of output across the prompt.
                out.push_str(&format!("\x1b[{};1H\x1b7", transcript_bottom(rows)));
            }
        }

        if let Some(text) = body {
            out.push_str("\x1b8");
            out.push_str(text);
            out.push_str("\x1b7");
            // Output means the agent is talking, not asking. The two never
            // overlap — a question is asked from a loop that is blocked until
            // it is answered — so a write is proof the last question was
            // answered and the bottom row can go back to the hint.
            self.prompt = None;
        }

        out.push_str(&format!(
            "\x1b[1;1H\x1b[2K{DIM}{}{RESET}",
            fit(&self.status, self.cols)
        ));
        match &self.prompt {
            Some(q) => out.push_str(&format!(
                "\x1b[{};1H\x1b[2K{BOLD}{}{RESET}",
                self.rows,
                fit(q, self.cols)
            )),
            None => out.push_str(&format!(
                "\x1b[{};1H\x1b[2K{DIM}{}{RESET}",
                self.rows,
                fit(hint, self.cols)
            )),
        }
        out
    }
}

/// `\n` → `\r\n`, and never `\r\r\n`.
///
/// Inside the frame a newline must also return the cursor to column one:
/// `ESC 7` is about to record where the transcript got to, and a position half
/// way along a line that has already been left is not it. The provider streams
/// bare `\n`, a tool result can carry `\r\n` from a Windows program, and
/// doubling the `\r` on the second case is harmless but leaves an odd byte in
/// anything piping the transcript — so both are normalised to one form.
fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// `ESC[{top};{bottom}r`: rows 2..h-1 scroll, rows 1 and h do not.
fn margins(rows: u16) -> String {
    format!("\x1b[2;{}r", transcript_bottom(rows))
}

fn transcript_bottom(rows: u16) -> u16 {
    rows.saturating_sub(1).max(2)
}

/// Cut a line to the window width.
///
/// Only ever called on strings this module built and knows contain no escape
/// sequences, which is why counting characters is a legitimate way to measure
/// them. Colour is applied by the caller, outside the cut.
fn fit(text: &str, cols: u16) -> String {
    let budget = cols.saturating_sub(2) as usize;
    if text.chars().count() <= budget {
        text.to_string()
    } else {
        text.chars()
            .take(budget.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

/// Why the frame is not being drawn, or `None` to draw it.
///
/// Split out and pure so the decision can be tested, because the decision is
/// the part that has to be right: drawing is either visible or it is not, but
/// drawing *when we should not have* produces a garbled terminal on somebody
/// else's machine.
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
    // there is nobody typing, so a prompt row is decoration on a batch job.
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

// endregion: The pinned frame

// region: Asking the platform
// ---------------------------------------------------------------------------
// Asking the platform
//
// Two questions, both of which the platform has to answer and neither of which
// can be guessed: how big is the window, and will this console honour VT at
// all. Windows answers both through the console API; unix answers the first
// through an ioctl and does not need the second.
// ---------------------------------------------------------------------------

/// `(cols, rows)` of the visible window, not the buffer.
///
/// On Windows those differ: the screen buffer is usually far taller than the
/// window, and a scroll region measured against the buffer would put the prompt
/// row hundreds of lines below anything the user can see.
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
// **The frame changed nothing here, deliberately.** The terminal stays in
// cooked mode: the line discipline does the editing, echoes into the bottom row
// because that is where the cursor was left, and hands whole lines to the same
// thread that always read them. Raw-mode key handling would have meant
// reimplementing `drain` as "discard the pending key buffer", and the property
// it protects was a live security bug — the version that cannot regress is the
// one that was not rewritten. What it costs is listed in the report: no
// history, no completion, and no editing beyond what the terminal itself
// offers.
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
// Everything here that *decides* something, and nothing that draws. The frame
// cannot be asserted on from a test binary — cargo gives it a pipe, not a
// terminal, which is exactly the condition under which `fallback_reason` says
// no. So the tests are about the fallback, the geometry, the restore being
// idempotent and the drain still draining; the drawing itself was checked by
// eye and the report says so.
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
    // is the requirement: today's behaviour is the default and the frame is
    // the exception that has to earn itself.
    // -----------------------------------------------------------------------

    const BIG: Option<(u16, u16)> = Some((120, 40));

    #[test]
    fn a_real_terminal_of_a_reasonable_size_gets_the_frame() {
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

    // -----------------------------------------------------------------------
    // What a repaint actually emits
    //
    // `compose` is pure, so the escape sequences are assertable even though
    // the drawing is not. These are written against the properties that make
    // the frame correct — the transcript cursor is restored before the write
    // and saved after it, the prompt row is repainted last and is outside the
    // region — rather than against the exact byte string, which would fail on
    // every cosmetic change and teach the next person to update the expected
    // value without reading it.
    // -----------------------------------------------------------------------

    fn state() -> FrameState {
        FrameState {
            rows: 40,
            cols: 120,
            status: "emma · model · dir".to_string(),
            prompt: None,
        }
    }

    #[test]
    fn a_transcript_write_is_bracketed_by_the_saved_cursor() {
        // The invariant the whole frame rests on: `ESC 7` holds the transcript
        // position, so output is restored to it, written, and saved again
        // before anything walks outside the region to repaint a fixed row.
        // Without the restore, every line would be written wherever the last
        // repaint left the cursor — which is on the prompt row.
        let out = state().compose(Some("● Bash cargo test\r\n"), "hint", Some((120, 40)));
        let write = out.find("● Bash").expect("the line was not emitted");
        let restore = out.find("\x1b8").expect("the cursor was never restored");
        let save = out.find("\x1b7").expect("the cursor was never saved");
        assert!(
            restore < write,
            "output was written before the cursor moved"
        );
        assert!(write < save, "the cursor was saved before the output");
        // And the prompt row is repainted after all of it, so a line that
        // scrolled the region cannot leave the bottom row half-drawn.
        assert!(save < out.rfind("\x1b[40;1H").unwrap());
    }

    #[test]
    fn a_question_is_painted_on_the_prompt_row_and_the_cursor_is_left_after_it() {
        let mut st = state();
        st.prompt = Some("  allow? [y]es  [n]o: ".to_string());
        let out = st.compose(None, "hint", Some((120, 40)));
        // Row 40 is outside the region set by `margins(40)` = rows 2..39, so
        // nothing the agent prints next can scroll this away. That is the
        // defect being fixed, expressed as an assertion.
        assert!(out.contains("\x1b[40;1H\x1b[2K"), "{out:?}");
        assert!(out.contains("allow? [y]es"), "{out:?}");
        assert_eq!(transcript_bottom(40), 39);
        // The cursor ends just past the question: cooked-mode echo lands there.
        assert!(out.ends_with(RESET));
        // No transcript write, so the saved cursor is untouched.
        assert!(!out.contains("\x1b8"), "{out:?}");
    }

    #[test]
    fn writing_to_the_transcript_retires_the_question() {
        // A question is asked from a loop that blocks until it is answered, so
        // any output at all is proof the answer arrived. If this did not
        // clear, the bottom row would keep showing an answered `allow?` for
        // the rest of the run.
        let mut st = state();
        st.prompt = Some("  allow? [y]es  [n]o: ".to_string());
        let out = st.compose(Some("  ok\r\n"), "/exit quits", Some((120, 40)));
        assert!(st.prompt.is_none());
        assert!(out.contains("/exit quits"), "{out:?}");
        assert!(!out.contains("allow?"), "{out:?}");
    }

    #[test]
    fn a_resize_resets_the_margins_and_re_anchors_the_transcript() {
        // The margins are absolute rows. A window that grew from 40 to 50 has
        // a region that stops ten rows early and a prompt row stranded in the
        // middle of the screen; a window that shrank has a region that
        // includes the prompt row, which is the version that eats the
        // approval question.
        let mut st = state();
        let out = st.compose(None, "hint", Some((100, 50)));
        assert!(out.starts_with("\x1b[2;49r"), "{out:?}");
        assert!(out.contains("\x1b[49;1H\x1b7"), "{out:?}");
        assert!(out.contains("\x1b[50;1H\x1b[2K"), "{out:?}");
        assert_eq!((st.cols, st.rows), (100, 50));
        // And a paint at an unchanged size does not re-issue them, because
        // re-anchoring throws the transcript cursor to the bottom of the
        // region and would do it on every line.
        let out = st.compose(None, "hint", Some((100, 50)));
        assert!(!out.contains("\x1b[2;49r"), "{out:?}");
    }

    #[test]
    fn a_newline_inside_the_frame_also_returns_to_column_one() {
        // `ESC 7` records the transcript position immediately after the write.
        // A bare `\n` on most terminals moves down without moving left, so the
        // saved position would be mid-line and the next line of output would
        // start under the end of this one.
        assert_eq!(crlf("a\nb\n"), "a\r\nb\r\n");
        // A tool result from a Windows program already has the `\r`, and
        // doubling it puts a stray byte into anything piping the transcript.
        assert_eq!(crlf("a\r\nb"), "a\r\nb");
    }

    #[test]
    fn a_paint_with_no_readable_size_still_paints_both_rows() {
        // `terminal_size` can fail at any time — a console handle closing
        // under us, most plausibly. Falling back to the last known geometry
        // draws a slightly wrong frame; returning early draws nothing at all
        // and the session appears to hang.
        let out = state().compose(Some("x\r\n"), "hint", None);
        assert!(out.contains("\x1b[1;1H\x1b[2K"), "{out:?}");
        assert!(out.contains("\x1b[40;1H\x1b[2K"), "{out:?}");
    }

    // -----------------------------------------------------------------------
    // Geometry
    //
    // The failure these catch is an off-by-one that puts the bottom margin on
    // the prompt row, at which point the transcript scrolls over the question
    // and the defect this whole thing fixes comes back.
    // -----------------------------------------------------------------------

    #[test]
    fn the_region_stops_one_row_above_the_prompt() {
        assert_eq!(margins(40), "\x1b[2;39r");
        assert_eq!(transcript_bottom(40), 39);
        // The prompt row is 40 and is not in the region, so nothing that
        // scrolls can reach it. That is the security-relevant property.
        assert!(transcript_bottom(40) < 40);
    }

    #[test]
    fn a_degenerate_height_still_produces_a_legal_region() {
        // Unreachable through `install`, which refuses under MIN_ROWS, but a
        // resize can report anything and `ESC[2;1r` is a malformed region that
        // some terminals resolve by resetting to full screen.
        for rows in [0u16, 1, 2, 3] {
            assert!(transcript_bottom(rows) >= 2, "rows={rows}");
        }
    }

    #[test]
    fn a_status_line_wider_than_the_window_is_cut_not_wrapped() {
        // Wrapping the top row would push it into the region and it would then
        // scroll, which is the one thing a fixed row must not do.
        let long = "emma · ".to_string() + &"a".repeat(200);
        let cut = fit(&long, 40);
        assert!(cut.chars().count() <= 38, "{}", cut.chars().count());
        assert!(cut.ends_with('…'));
        // Short strings are left exactly alone.
        assert_eq!(fit("emma", 40), "emma");
    }

    // -----------------------------------------------------------------------
    // Restore
    // -----------------------------------------------------------------------

    #[test]
    fn restoring_a_terminal_that_was_never_framed_writes_nothing() {
        // The `Drop`, the panic hook and `main` can all reach this, and the
        // common case — `-p`, a test binary, a pipe — never installed a frame
        // at all. A restore that emitted `ESC[r` regardless would corrupt the
        // output of every non-interactive run.
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
        FRAME_ROWS.store(40, Ordering::Relaxed);
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst), "the latch did not clear");
        restore_terminal();
        assert!(!FRAME_ON.load(Ordering::SeqCst));
    }

    // -----------------------------------------------------------------------
    // The drain
    //
    // Unchanged by the frame, and tested here so that a future move to raw
    // mode has to delete an assertion rather than quietly lose the property.
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
