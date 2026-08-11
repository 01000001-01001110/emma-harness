//! Everything the person at the keyboard sees.
//!
//! Five things here are not cosmetic.
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
//! already made.
//!
//! **Different events look different.** A tool starting, a tool succeeding, a
//! tool failing, a call the user refused and a call a hook blocked are five
//! things, and the screen says which before anybody reads a word. See
//! [`render`].
//!
//! **Scrollback is the terminal's and stays the terminal's.**
//!
//! # The design, and the two it replaces
//!
//! The transcript is ordinary terminal output. Emma draws a small **inline
//! viewport** — ratatui's [`Viewport::Inline`](ratatui::Viewport::Inline) — in
//! the last few rows of the normal screen buffer: a status line, whatever is
//! being streamed or asked, an input box, and a hint. There is no alternate
//! screen. Transcript lines are pushed above it with `insert_before`, which
//! scrolls the screen the way `println!` does, so the terminal wraps them, keeps
//! them in scrollback and lets a mouse select them. [`frame`] carries the whole
//! argument, including the one feature flag that would silently break it.
//!
//! **The first attempt used a DEC scroll region** (`ESC[{top};{bottom}r`) to pin
//! a status row. Lines that scroll off the top of a region whose top margin is
//! below row 1 are *discarded*, so there was no scrollback; and it addressed
//! rows absolutely, which on a Windows console targets the screen *buffer*
//! rather than the window, so the pinned rows were painted thousands of lines
//! above anything visible. One mechanism, two symptoms.
//!
//! **The second attempt** removed the scroll region and redrew an input block
//! from the bottom with relative movement only. It worked, and it could not have
//! a status line — the run's identity was printed once as an ordinary line that
//! scrolled away — and it had no vocabulary: `●` prefixed every tool line
//! whether it ran, failed or was refused.
//!
//! **The viewport is a luxury and the fallback is the product.** Nothing below
//! makes a decision a plain stream of lines could not. If the terminal cannot be
//! verified to support VT processing, if either stream is redirected, if the
//! window is tiny, or if `EMMA_NO_FRAME` is set, [`Term`] degrades to
//! line-by-line output on the same streams, with the same vocabulary and no
//! escape byte at all when the destination is not a terminal. `-p` never gets a
//! viewport, and neither does [`Term::silent`].

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use ratatui::text::Line;

pub mod frame;
pub mod input;
pub mod menu;
pub mod palette;
pub mod render;
pub mod view;
pub mod welcome;

pub use frame::restore_terminal;
pub use input::LineSource;
pub use welcome::Welcome;

use frame::Frame;
use palette::{Level, Palette};
use render::{for_stream, Skin};
use view::Prompt;

// region: The terminal
// ---------------------------------------------------------------------------
// The terminal
//
// Every write to the screen goes through one of these methods. The split that
// matters is `delta`/`text` on stdout against `side` on stderr under `-p`, so
// a script can pipe the answer without filtering the commentary out of it.
//
// The second split is `frame`: `Some` means the viewport, `None` means plain
// lines. Every method below renders the same `Line`s either way, so the two
// paths cannot drift apart.
// ---------------------------------------------------------------------------

pub struct Term {
    skin: Skin,
    /// `-p`: assistant prose still goes to stdout, but the running commentary
    /// goes to stderr so a script can pipe the answer without filtering it.
    quiet: bool,
    enabled: bool,
    /// The viewport, when the terminal was verified able to carry one. `None`
    /// is the whole fallback: every method checks it and takes the plain path,
    /// which is why `printing()` and `silent()` need no special handling beyond
    /// never constructing one.
    frame: Option<Arc<Frame>>,
    /// The completion marker, on its way off the screen.
    ///
    /// Here rather than in the loop because this is a display decision and the
    /// loop's copy of the text is the one done-detection reads: filtering
    /// upstream of here would mean the thing deciding whether a goal is finished
    /// and the thing showing the answer disagreed about what the model said.
    /// It is `Mutex` because every write method takes `&self` — the same reason
    /// [`Frame`] holds one — and it is state because text arrives in fragments.
    marker: Mutex<crate::goal::MarkerFilter>,
    /// The approval question being assembled. `prompt_header` fills the
    /// evidence and `prompt_question` adds the keys, because the gate calls
    /// them separately; keeping the half-built thing here rather than in the
    /// frame is what lets the fallback path use the same two calls.
    pending: Mutex<Prompt>,
}

impl Term {
    /// The interactive terminal, and the one place the console is *changed*
    /// rather than merely asked about.
    ///
    /// **Emma sets the output code page to UTF-8 itself.** A normal PowerShell
    /// window reports 437 or 1252, and detection alone was therefore correct
    /// and useless: it proved the console could not render `╭─╮` and drew
    /// `+---+` forever. Setting the code page is the same shape as the VT
    /// enable beside it — change it, read it back, and believe only the
    /// read-back — and it is what every modern Windows CLI does.
    ///
    /// It is done for a run that will draw the frame and for no other. `-p`, a
    /// pipe and a redirected run get the detection they always had, because
    /// changing a code page on a console nobody is looking at is a change to
    /// somebody's shell for no benefit at all.
    pub fn interactive() -> Self {
        let color = std::io::stdout().is_terminal();
        let opted_out = std::env::var_os("EMMA_ASCII_FRAME").is_some();
        let framing = frame_wanted();
        let glyphs = |utf8: bool| {
            if prefers_ascii(opted_out, utf8) {
                render::ASCII
            } else {
                render::UNICODE
            }
        };
        let palette = Palette::new(Level::detect(color));
        // Only a run that is about to draw asks for UTF-8; everything else
        // takes the console as it found it.
        let skin = Skin::new(
            palette,
            glyphs(if framing {
                adopt_utf8_output()
            } else {
                console_is_utf8()
            }),
        );
        // The only constructor that may draw. Detection is in
        // `Frame::install`, which refuses far more often than it accepts.
        let frame = if framing { Frame::install(skin) } else { None };
        let skin = match frame {
            Some(_) => skin,
            None => {
                // The frame refused after the code page was already changed —
                // raw mode failed, or VT could not be proved. Put the console
                // back, and then re-decide the glyphs against what it is now:
                // a Unicode transcript on a restored 437 console is the
                // mojibake this whole mechanism exists to avoid.
                #[cfg(windows)]
                restore_console_cp();
                Skin::new(palette, glyphs(console_is_utf8()))
            }
        };
        Self {
            skin,
            quiet: false,
            enabled: true,
            frame,
            marker: Mutex::default(),
            pending: Mutex::default(),
        }
    }

    pub fn printing() -> Self {
        let color = std::io::stderr().is_terminal();
        Self {
            skin: Skin::new(
                Palette::new(Level::detect(color)),
                if prefers_ascii(
                    std::env::var_os("EMMA_ASCII_FRAME").is_some(),
                    console_is_utf8(),
                ) {
                    render::ASCII
                } else {
                    render::UNICODE
                },
            ),
            quiet: true,
            enabled: true,
            frame: None,
            marker: Mutex::default(),
            pending: Mutex::default(),
        }
    }

    /// Writes nothing. For tests, which assert on the log and the transcript
    /// rather than on the screen.
    pub fn silent() -> Self {
        Self {
            skin: Skin::new(Palette::new(Level::None), render::ASCII),
            quiet: true,
            enabled: false,
            frame: None,
            marker: Mutex::default(),
            pending: Mutex::default(),
        }
    }

    /// True when the viewport is drawn. For callers that would otherwise say
    /// the same thing twice — the hint row carries two of them — and for tests.
    pub fn framed(&self) -> bool {
        self.frame.is_some()
    }

    /// The one reader of stdin, matched to how this terminal is being driven.
    ///
    /// A viewport means raw mode, which means Emma echoes what is typed and
    /// **owns delivering Ctrl-C** — raw mode is exactly the state in which the
    /// terminal stops generating it. Without a viewport this is the same
    /// line-at-a-time reader it has always been and the signal handler is the
    /// only delivery. `on_interrupt` is wired to both, which costs nothing:
    /// they trip one flag.
    /// `menu` is this run's command vocabulary, for the `/` menu. It is passed
    /// in rather than read here because the harness is the only thing that
    /// knows it, and a menu that lists anything else would be inventing
    /// commands. The fallback reader has no menu: a cooked terminal hands over
    /// whole lines, so there is nothing to react to as the user types.
    pub fn line_source(
        &self,
        menu: menu::Menu,
        on_interrupt: impl Fn() + Send + 'static,
    ) -> LineSource {
        match &self.frame {
            Some(frame) => LineSource::raw(frame.clone(), menu, on_interrupt),
            None => LineSource::stdin(),
        }
    }

    /// The run's identity: model, working directory, transcript.
    ///
    /// With a viewport these are the fixed half of the status line and stay on
    /// screen for the life of the process. Without one they are printed once as
    /// an ordinary line and scroll away, which is correct for a fact that never
    /// changes.
    pub fn set_status(&self, model: &str, cwd: &Path, session: &std::path::Path) {
        let session_id = session
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(frame) = &self.frame {
            frame.set_identity(model, &cwd.display().to_string(), &session_id);
            return;
        }
        self.side(self.skin.note(&format!(
            "emma {} {model} {} {} {} {}",
            self.skin.glyphs.sep,
            self.skin.glyphs.sep,
            cwd.display(),
            self.skin.glyphs.sep,
            session.display()
        )));
    }

    /// The caps the status meters are measured against. Called by the loop from
    /// the budgets it was given, so nothing here has to know what a budget is.
    pub fn set_budgets(&self, max_context: i64, max_tokens: i64) {
        if let Some(frame) = &self.frame {
            frame.set_budgets(max_context, max_tokens);
        }
    }

    /// What a new user sees, once. See [`welcome`] and
    /// [`crate::session::first_run`].
    pub fn welcome(&self, w: &Welcome) {
        self.side(self.skin.welcome(w));
    }

    /// The side channel: tool lines, notes, warnings. stderr in `-p`.
    fn side(&self, lines: Vec<Line<'static>>) {
        if !self.enabled {
            return;
        }
        if let Some(frame) = &self.frame {
            frame.write_lines(lines);
            return;
        }
        for line in &lines {
            // `for_stream` writes the text and nothing else when the palette
            // has no colour in it, which is the case for every pipe and every
            // `NO_COLOR` run — so a redirected stream never receives an escape
            // byte.
            let text = for_stream(&self.skin, line);
            if self.quiet {
                eprintln!("{text}");
            } else {
                println!("{text}");
            }
        }
    }

    /// Assistant text, as it arrives. No newline, flushed every time — a
    /// buffered stream is a blank terminal with the words already in it.
    ///
    /// The completion marker is taken out on the way through. See
    /// [`crate::goal::MarkerFilter`] for why that happens here, character by
    /// character, rather than on the finished text.
    pub fn delta(&self, text: &str) {
        if !self.enabled {
            return;
        }
        let shown = self.filtered(|f| f.push(text));
        if shown.is_empty() {
            return;
        }
        self.write_out(&shown);
    }

    pub fn end_of_text(&self) {
        if !self.enabled {
            return;
        }
        // Whatever the filter is still holding, which for a turn ending on the
        // marker with no trailing newline is the marker itself — the ordinary
        // case, and the one that would otherwise reach the screen at the very
        // moment the user is reading the answer.
        let held = self.filtered(|f| f.finish());
        match &self.frame {
            Some(frame) => {
                if !held.is_empty() {
                    frame.prose(&held);
                }
                frame.flush_prose();
            }
            None => {
                if !held.is_empty() {
                    self.write_out(&held);
                }
                println!();
            }
        }
    }

    /// Whole assistant text at once, for `-p` where nothing streamed.
    pub fn text(&self, text: &str) {
        let text = crate::goal::MarkerFilter::once(text);
        if self.enabled && !text.trim().is_empty() {
            match &self.frame {
                Some(frame) => frame.write_lines(
                    text.trim_end()
                        .lines()
                        .map(|l| self.skin.prose(l))
                        .collect(),
                ),
                None => println!("{}", text.trim_end()),
            }
        }
    }

    /// The marker filter, which is stateful and behind `&self`. A poisoned lock
    /// means a panic mid-write; carrying on with the inner value shows the user
    /// their text rather than adding a second panic to the first.
    fn filtered(&self, f: impl FnOnce(&mut crate::goal::MarkerFilter) -> String) -> String {
        f(&mut self.marker.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Assistant prose, straight through, wherever it goes.
    fn write_out(&self, text: &str) {
        if let Some(frame) = &self.frame {
            frame.prose(text);
            return;
        }
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    pub fn note(&self, text: &str) {
        self.side(self.skin.note(text));
    }

    pub fn warn(&self, text: &str) {
        self.side(self.skin.warn(text));
    }

    pub fn banner(&self, text: &str) {
        self.side(self.skin.banner(text));
    }

    /// A tool is about to run. One line, the interesting argument inline.
    pub fn tool_started(&self, name: &str, args: &serde_json::Value) {
        self.side(self.skin.tool_started(name, &summarise_args(name, args)));
    }

    /// A tool ran. `truncated` is the tool's own claim that it stopped early,
    /// which is a different fact from the screen showing only the first few
    /// lines — the model's copy is short too.
    pub fn tool_result(&self, display: Option<&str>, content: &str, truncated: bool) {
        self.side(self.skin.tool_ok(display.unwrap_or(content), truncated));
    }

    /// A `ToolError`, or a fault the tool could not continue past.
    pub fn tool_failed(&self, name: &str, detail: &str) {
        self.side(self.skin.tool_failed(name, detail));
    }

    /// A `PreToolUse` hook said no. Distinct from [`Term::tool_refused`] on
    /// purpose: no answer at the prompt could have allowed this one.
    pub fn tool_blocked(&self, name: &str, reason: &str) {
        self.side(self.skin.tool_blocked(name, reason));
    }

    /// The human said no.
    pub fn tool_refused(&self, name: &str) {
        self.side(self.skin.tool_refused(name));
    }

    pub fn kick(&self, n: u32, max: u32) {
        self.side(self.skin.kick(n, max));
    }

    /// A goal stopped, for whatever reason, having spent what it spent.
    pub fn ending(&self, message: &str, ok: bool, iterations: u32, tokens: i64) {
        self.side(self.skin.ending(message, ok, iterations, tokens));
    }

    pub fn goal_started(&self, goal: &str) {
        self.side(self.skin.goal(goal));
        if let Some(frame) = &self.frame {
            frame.goal_started();
        }
    }

    /// The goal is over: the clock stops rather than freezing at its last value.
    pub fn goal_ended(&self) {
        if let Some(frame) = &self.frame {
            frame.goal_ended();
        }
    }

    /// One model call's measurements, for the live half of the status line.
    /// Both are measured — the provider's input count and the loop's own
    /// weighted spend — because a status line with an estimate on it is a
    /// status line that lies at exactly the moment somebody checks it.
    pub fn spent(&self, context: i64, tokens: i64) {
        if let Some(frame) = &self.frame {
            frame.spent(Some(context), Some(tokens));
        }
    }

    /// The evidence half of an approval prompt.
    ///
    /// It goes to the transcript in full — that is the record of what was
    /// asked, and it can be scrolled back to and selected — and is held for the
    /// viewport panel, which shows as much of it as fits.
    pub fn prompt_header(&self, tool: &str, preview: &str) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.title = format!("Approve {tool}");
        pending.preview = preview.lines().map(str::to_string).collect();
        drop(pending);
        let mut lines = vec![Line::default()];
        lines.extend(self.skin.tool_started(tool, "wants to run:"));
        for line in preview.lines() {
            lines.push(self.skin.prose(&format!("  {line}")));
        }
        self.side(lines);
    }

    pub fn prompt_question(&self, tool: &str) {
        self.question(
            &format!("allow? [y]es  [n]o  [a]lways {tool} this session: "),
            vec![
                ("y".to_string(), "yes".to_string()),
                ("n".to_string(), "no".to_string()),
                ("a".to_string(), format!("always {tool}")),
            ],
        );
    }

    /// The network question, worded so the grant on offer is the one the answer
    /// actually gives: a host for the session, not a tool and not one call.
    /// There is no third option, because a wider network grant is not offered.
    pub fn prompt_network_question(&self, host: &str) {
        self.question(
            &format!("allow? [y]es — and {host} again this session  [n]o: "),
            vec![
                ("y".to_string(), format!("yes, and {host} again")),
                ("n".to_string(), "no".to_string()),
            ],
        );
    }

    fn question(&self, q: &str, keys: Vec<(String, String)>) {
        if !self.enabled {
            return;
        }
        let prompt = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.question = q.to_string();
            pending.keys = keys;
            pending.clone()
        };
        if let Some(frame) = &self.frame {
            // In the viewport, which is below everything that scrolls. There is
            // no amount of output that can push it off, which is the fix for
            // the question that scrolled away.
            frame.set_prompt(Some(prompt));
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
            frame.set_prompt(None);
            return;
        }
        print!("> ");
        let _ = std::io::stdout().flush();
    }

    /// A line has been read from the terminal, so the question it answered is
    /// history.
    ///
    /// What it writes is the question and the answer together, as one ordinary
    /// transcript line — so the record of what was approved reads back exactly
    /// as it was asked, and scrolls with everything else. `None` is end of
    /// input.
    pub fn prompt_answered(&self, line: Option<&str>) {
        if !self.enabled {
            return;
        }
        let Some(frame) = &self.frame else {
            return;
        };
        let question = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut pending.question)
        };
        if let (Some(line), false) = (line, question.is_empty()) {
            frame.write_lines(self.skin.answered(&question, line));
        }
        frame.set_prompt(None);
        frame.set_input("", 0);
        frame.draw();
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

// region: The fallback
// ---------------------------------------------------------------------------
// The fallback
//
// The decision to draw at all, kept pure and separate because it is the part
// that has to be right: drawing is either visible or it is not, but drawing
// *when we should not have* produces a garbled terminal on somebody else's
// machine, or a file full of escape sequences.
// ---------------------------------------------------------------------------

/// Below this the viewport would be most of the screen, so it is not worth
/// drawing.
const MIN_ROWS: u16 = 8;
const MIN_COLS: u16 = 24;

/// Why the viewport is not being drawn, or `None` to draw it.
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
    // there is nobody typing — and raw mode on a pipe is meaningless.
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

/// Whether to draw out of ASCII rather than box-drawing characters.
///
/// Pure, and biased: `utf8` has to be *proved* before the Unicode set is used.
/// A console that reports a legacy code page renders every one of those glyphs
/// as mojibake, and a transcript made of question marks reads as a broken
/// program rather than as a stylistic choice.
fn prefers_ascii(opted_out: bool, utf8: bool) -> bool {
    opted_out || !utf8
}

/// Whether this run is one that would draw a viewport.
///
/// The same question [`Frame::install`] asks, asked earlier: the code page is
/// changed only for a run that will draw, and the glyph set is chosen from
/// what the console says *after* that. `install` still asks it again for
/// itself — a check that lives in two places is cheaper than an installer that
/// trusts its caller.
fn frame_wanted() -> bool {
    fallback_reason(
        std::io::stdout().is_terminal(),
        std::io::stdin().is_terminal(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("EMMA_NO_FRAME").is_some(),
        terminal_size(),
    )
    .is_none()
}

// endregion: The fallback

// region: Asking the platform
// ---------------------------------------------------------------------------
// Asking the platform
//
// Three questions, none of which can be guessed: how big is the window, will
// this console honour VT at all, and will it render a box-drawing character.
//
// The third one changed shape. It used to be pure detection, and it was right
// and useless: a normal PowerShell window reports code page 437, so the answer
// was always no and the box was always `+---+`. A code page is a setting, not
// a property of the terminal, so Emma now sets it — and asks afterwards, which
// is the only part of the answer worth having.
// ---------------------------------------------------------------------------

/// `(cols, rows)` of the visible window, not the buffer.
///
/// On Windows those differ: the screen buffer is usually far taller than the
/// window, and a viewport sized from the buffer would be drawn off screen.
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

/// Will this console render the UTF-8 bytes Rust is about to write to it, as
/// it stands, with nothing changed.
///
/// On Windows that is a code page, and the answer is usually no: Windows
/// Terminal displays UTF-8 happily but the console's *output code page* is
/// still whatever the system locale says unless somebody changed it, and the
/// bytes are decoded by that code page on the way through. So the honest answer
/// is "only when it is 65001", and the glyphs are ASCII the rest of the time.
///
/// This is now the question a run that will *not* draw asks — `-p`, a pipe, a
/// redirected run. A run that will draw does not ask it; it changes the answer.
/// See [`adopt_utf8_output`].
#[cfg(windows)]
fn console_is_utf8() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleOutputCP;
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

/// `CP_UTF8`, spelled out rather than imported: the constant lives behind the
/// `Win32_Globalization` feature of `windows-sys` and pulling a whole module in
/// for one integer is a dependency for a number that has not changed since
/// Windows 95.
#[cfg(windows)]
const CP_UTF8: u32 = 65001;

/// Ask the console to render UTF-8, and prove that it will.
///
/// The premise the old detection got wrong: a legacy code page is not a
/// property of the terminal, it is a setting, and it is ours to set for the
/// duration of the run. Windows Terminal, conhost and the VS Code terminal all
/// display UTF-8 perfectly — what was stopping them is that the bytes were
/// being decoded as 437 on the way through.
///
/// **The read-back is the whole of the safety.** `SetConsoleOutputCP` can
/// succeed against a console that then reports something else, and a frame that
/// draws `╭─╮` into a 437 console is worse than the `+---+` it replaced: the
/// ASCII is plain, and the mojibake reads as a broken program. So the glyph set
/// follows `GetConsoleOutputCP`, never the return value of the set.
///
/// The original is written down for [`restore_console_cp`]. A console that was
/// *already* 65001 is not ours and is left alone on the way out.
#[cfg(windows)]
fn adopt_utf8_output() -> bool {
    use std::sync::atomic::Ordering;

    use windows_sys::Win32::System::Console::{GetConsoleOutputCP, SetConsoleOutputCP};
    unsafe {
        let original = GetConsoleOutputCP();
        if original == CP_UTF8 {
            return true;
        }
        if original == 0 {
            // No console attached — a redirected handle, or a service. Nothing
            // to set and nothing to render.
            return false;
        }
        SetConsoleOutputCP(CP_UTF8);
        // Believe the console, not the call.
        if GetConsoleOutputCP() != CP_UTF8 {
            // Whatever it did with the request, it is not UTF-8 now, and we
            // may have moved it somewhere it did not start.
            SetConsoleOutputCP(original);
            return false;
        }
        ORIGINAL_CP.store(original, Ordering::SeqCst);
        true
    }
}

/// Everywhere else the locale is the answer and there is nothing to set.
#[cfg(not(windows))]
fn adopt_utf8_output() -> bool {
    console_is_utf8()
}

/// The output code page as we found it. `0` is the "not ours to restore"
/// sentinel — zero is not a code page, and `GetConsoleOutputCP` returns it only
/// when there is no console at all.
#[cfg(windows)]
static ORIGINAL_CP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Put the code page back, once, whoever calls.
///
/// Reached from [`frame::restore_terminal`], which is reached from `Drop`, from
/// the panic hook and from the explicit call on the `process::exit` path — so
/// a normal exit, `/exit`, Ctrl-C and a panic all land here. Leaving somebody's
/// shell on a code page they did not choose is the same class of rudeness as
/// leaving a scroll region set.
#[cfg(windows)]
fn restore_console_cp() {
    use std::sync::atomic::Ordering;

    use windows_sys::Win32::System::Console::SetConsoleOutputCP;
    let original = ORIGINAL_CP.swap(0, Ordering::SeqCst);
    if original == 0 {
        return;
    }
    unsafe {
        SetConsoleOutputCP(original);
    }
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
///
/// It has a second job now. crossterm picks between writing escape sequences
/// and making WinAPI calls based on whether VT could be enabled, and the WinAPI
/// path positions the cursor in **screen buffer** coordinates — which is the
/// exact failure the first attempt at this file shipped. Proving VT here is
/// what keeps every later write on the escape-sequence path.
#[cfg(windows)]
fn enable_vt() -> bool {
    use std::sync::atomic::Ordering;

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
    use std::sync::atomic::Ordering;

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
// The viewport cannot be looked at from a test binary — cargo gives it a pipe,
// not a terminal, which is exactly the condition under which `fallback_reason`
// says no — so what is asserted is the decision, the bytes and the widget tree.
// The submodules carry the rest: `render` for what each event looks like,
// `view` for what the viewport draws, `input` for the drain, `frame` for the
// one feature flag that would break scrollback.
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
    // is the requirement: plain output is the default and the viewport is the
    // exception that has to earn itself.
    // -----------------------------------------------------------------------

    const BIG: Option<(u16, u16)> = Some((120, 40));

    #[test]
    fn a_real_terminal_of_a_reasonable_size_gets_the_viewport() {
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
        // full of escape sequences if this is wrong — and the second would put
        // a pipe into raw mode, which is worse.
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

    /// The code page is changed only for a run that will draw, and a test
    /// binary is never one: cargo hands it a pipe.
    ///
    /// This is the assertion that keeps `cargo test` from leaving the
    /// developer's shell on a code page it chose for them — and it is the same
    /// gate `-p` and `emma … | tee` go through.
    #[test]
    fn a_run_that_will_not_draw_never_asks_the_console_to_change() {
        assert!(
            !frame_wanted(),
            "a test binary decided it was going to draw a viewport"
        );
        // Nothing has been adopted, so the restore is a no-op — which is what
        // makes it safe on the panic path, on `Drop` and on an explicit call.
        #[cfg(windows)]
        {
            use std::sync::atomic::Ordering;
            assert_eq!(ORIGINAL_CP.load(Ordering::SeqCst), 0);
            restore_console_cp();
            restore_console_cp();
            assert_eq!(ORIGINAL_CP.load(Ordering::SeqCst), 0);
        }
    }

    /// The read-back is what the glyph set follows, not the request.
    ///
    /// Written against `prefers_ascii` because that is where the decision
    /// lands: a `SetConsoleOutputCP` that silently did not take reaches here as
    /// `false`, and the box stays ASCII. Mojibake is worse than plainness.
    #[test]
    fn a_code_page_change_that_did_not_take_still_gets_ascii() {
        assert!(prefers_ascii(false, false));
        assert!(!prefers_ascii(false, true));
        // …and the opt-out is above all of it, including a console that was
        // successfully moved to UTF-8.
        assert!(prefers_ascii(true, true));
    }

    #[test]
    fn the_glyphs_are_ascii_unless_utf8_is_proved() {
        assert!(
            prefers_ascii(false, false),
            "an unproved console got glyphs"
        );
        assert!(prefers_ascii(true, true), "the opt-out was ignored");
        assert!(!prefers_ascii(false, true));
    }

    // -----------------------------------------------------------------------
    // The silent and printing terminals
    //
    // Both are load-bearing for tests elsewhere in this workspace and for
    // `emma -p` in somebody's script.
    // -----------------------------------------------------------------------

    #[test]
    fn a_silent_terminal_never_draws_and_never_takes_a_terminal() {
        let term = Term::silent();
        assert!(!term.framed());
        // Every write method, to prove none of them reaches for stdout on the
        // disabled path — the test harness's stdout is captured, so a
        // regression here shows up as noise in every other test's output.
        term.set_status("m", Path::new("."), Path::new("s.jsonl"));
        term.note("n");
        term.warn("w");
        term.banner("b");
        term.goal_started("g");
        term.goal_ended();
        term.spent(1, 2);
        term.tool_started("Bash", &json!({ "command": "ls" }));
        term.tool_result(None, "out", false);
        term.tool_failed("Bash", "boom");
        term.tool_blocked("Bash", "policy");
        term.tool_refused("Bash");
        term.kick(1, 3);
        term.ending("done", true, 1, 2);
        term.delta("x");
        term.end_of_text();
        term.text("y");
        term.prompt_header("Bash", "$ ls");
        term.prompt_question("Bash");
        term.prompt_network_question("docs.rs");
        term.goal_prompt();
        term.prompt_answered(Some("y"));
        term.welcome(&Welcome::default());
    }

    #[test]
    fn printing_never_draws_a_viewport() {
        // `-p` is a script's stdout. A viewport in it is escape sequences in
        // somebody's pipeline, and raw mode on a process nobody is typing at.
        assert!(!Term::printing().framed());
    }
}

// endregion: Tests
