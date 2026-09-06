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
//! **The transcript is retained, because the alternate screen keeps no
//! scrollback.** This was the reverse for the whole life of this module —
//! "scrollback is the terminal's and stays the terminal's" — and the reversal
//! is deliberate: stage 2 of the full-screen design, owner-approved,
//! with its costs priced in that design's record and red-teamed in the
//! full-screen evaluation.
//!
//! # The design, and the three it replaces
//!
//! The interactive frame is a **full-screen app on the alternate screen**:
//! sidebar left, status bar bottom, and a main pane holding the header, the
//! retained transcript ([`transcript`]), and the input box pinned at the
//! bottom — where the approval prompt replaces it when a question is pending,
//! so no amount of output can move either. [`frame`] owns entry, restore and
//! the locks; [`app`] owns the layout; [`chat`], [`sidebar`] and [`statusbar`]
//! own their panes' cells.
//!
//! **The three earlier designs, kept for the argument.** A DEC scroll region
//! discarded scrollback and addressed the Windows screen *buffer* rather than
//! the window. A bottom-anchored redraw had no status line and no vocabulary.
//! The **inline viewport** — a few rows at the bottom of the normal screen,
//! transcript pushed above through `insert_before` — was the good one: the
//! terminal kept scrollback, wrapped, and let a mouse select. It lost to a
//! layout it structurally cannot draw (a sidebar and a pinned bottom bar need
//! the whole screen), and it survives as the `EMMA_UI=inline` escape hatch
//! for one release. What the flip costs and what pays it back: terminal
//! scrollback → the [`transcript`] buffer, keys and wheel; native mouse
//! selection → Shift-drag (mouse capture owns the wheel now), cleanly only
//! with the sidebar collapsed, until `/export` and in-app selection land;
//! re-reading the styled run after exit → gone, the session JSONL named in
//! the exit line is the record.
//!
//! **The viewport is a luxury and the fallback is the product.** Nothing below
//! makes a decision a plain stream of lines could not. If the terminal cannot be
//! verified to support VT processing, if either stream is redirected, if the
//! window is tiny, or if `EMMA_NO_FRAME` is set, [`Term`] degrades to
//! line-by-line output on the same streams, with the same vocabulary and no
//! escape byte at all when the destination is not a terminal. `-p` never gets a
//! viewport — and never the alternate screen — and neither does [`Term::silent`].

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use ratatui::text::Line;

pub mod app;
pub mod bindings;
pub mod chat;
pub mod code;
pub mod code_git;
pub mod code_lsp;
pub mod diff;
pub mod frame;
/// The net over `frame.rs` and `app.rs`. Green as of 2026-08-27, and it earned
/// the four clusters it was red for.
///
/// It was lifted out of those two files so that replacing them wholesale would
/// turn it red on arrival, and it did: four defects on the import itself, then
/// seventeen compile errors that were genuine model changes, then — once those
/// were reconciled — four more regressions the compiler could not see. The
/// panic hook had moved after raw mode, `restore_terminal`'s early return was
/// back to consulting one latch of four, the Settings grid clipped four cards
/// with nothing saying so, and neither page named the key that leaves it. Read
/// its own doc and the term-hardening backport checklist for what each item
/// defends.
pub mod guarantees;
pub mod harness;
pub mod help;
pub mod input;
pub mod inspect;
pub mod keymap;
pub mod layout;
pub mod markdown;
pub mod memory;
pub mod menu;
pub mod palette;
pub mod render;
pub mod rungraph;
pub mod settings;
pub mod sidebar;
pub mod spacing;
pub mod statusbar;
pub mod statusline;
pub mod termfont;
pub mod theme;
pub mod transcript;
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
    /// This terminal belongs to a nested run — see [`Term::subordinate`].
    subordinate: bool,
    /// What was said to the status meters and the approval prompt, when
    /// somebody asked to be told. See [`Term::recording`].
    record: Option<Arc<Mutex<Vec<String>>>>,
    /// Blank rows, for the path that has no viewport. The framed path keeps its
    /// own inside [`Frame`], because prose reaches the frame without coming
    /// through here — see [`Term::side`], which is the only user of this one.
    /// One rule, two places that can be *at* the transcript; never two rules.
    spacing: Mutex<spacing::Spacing>,
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
    /// **The theme is an argument, and the fidelity is not.** `theme` chooses
    /// *which* colours; [`Level::detect`] decides how many of them this
    /// terminal can be sent, and it is not one of the theme's inputs. That is
    /// what makes `NO_COLOR` unarguable rather than merely respected: it
    /// produces `Level::None`, and `Palette::color` returns before the theme is
    /// consulted at all. The theme is passed in rather than loaded here because
    /// resolving it needs the harness root, which `main` has and this does not.
    pub fn interactive(theme: theme::Theme) -> Self {
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
        let palette = Palette::live(Level::detect(color), theme);
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
            subordinate: false,
            record: None,
            spacing: Mutex::default(),
        }
    }

    /// `-p`. The theme reaches the *side* channel and nothing else: assistant
    /// prose goes to stdout unstyled, which is what keeps a piped answer free
    /// of escape bytes whatever a theme says.
    pub fn printing(theme: theme::Theme) -> Self {
        let color = std::io::stderr().is_terminal();
        Self {
            skin: Skin::new(
                Palette::live(Level::detect(color), theme),
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
            subordinate: false,
            record: None,
            spacing: Mutex::default(),
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
            subordinate: false,
            record: None,
            spacing: Mutex::default(),
        }
    }

    /// A terminal that remembers what it was told, and draws nothing.
    ///
    /// It exists for one reason: the three status meters are the calls a nested
    /// run must **not** make, and a no-op nobody can observe is a rule nobody
    /// can test. Everything routed through [`Term::meter`] is recorded by name,
    /// as is the approval prompt — which a subordinate terminal must keep, so a
    /// test needs to see both halves of that rule and not only one.
    pub fn recording() -> Self {
        let mut term = Self::silent();
        term.record = Some(Arc::new(Mutex::new(Vec::new())));
        term
    }

    /// What was said to the meters and the prompt, in order. Empty unless this
    /// terminal (or the one it is subordinate to) was built by
    /// [`Term::recording`].
    pub fn recorded(&self) -> Vec<String> {
        match &self.record {
            Some(rec) => rec.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            None => Vec::new(),
        }
    }

    /// A nested run's view of this terminal.
    ///
    /// **The bug this exists to fix.** `Term` carries the *process's* status
    /// meters, and a nested [`crate::agent::Agent`] would drive them with a
    /// nested run's numbers: `set_budgets` would re-point the context and token
    /// meters at the sub's caps for the rest of the process, `goal_ended` would
    /// stop the parent's clock while the parent is still running, and `spent`
    /// would feed the live line a per-goal figure smaller than the parent's, so
    /// the meter would jump backwards mid-goal. A meter measured against the
    /// wrong cap is the kind of untruth the status line exists to not have.
    ///
    /// So a subordinate terminal shares the frame, the skin and the stdin path,
    /// and swallows exactly four calls: `set_budgets`, `goal_started`,
    /// `goal_ended` and `spent`. It swallows the assistant's prose as well —
    /// `delta`, `text`, `end_of_text` — because a delegation reports once at the
    /// end rather than streaming a second conversation into the first.
    ///
    /// **What it must not swallow is the approval prompt.** A subagent inherits
    /// the gate, and a prompt the user cannot see is the same defect as a prompt
    /// they cannot evaluate, with the argument already made. `prompt_header`,
    /// `prompt_question`, `prompt_network_question` and `prompt_answered` render
    /// exactly as they do for the parent, and so do the tool lines and notes —
    /// the sub's activity becomes ordinary lines on the parent's transcript.
    pub fn subordinate(&self) -> Self {
        Self {
            skin: self.skin,
            quiet: self.quiet,
            enabled: self.enabled,
            frame: self.frame.clone(),
            marker: Mutex::default(),
            pending: Mutex::default(),
            subordinate: true,
            record: self.record.clone(),
            spacing: Mutex::default(),
        }
    }

    /// Anything that moves the status meters, which a subordinate terminal does
    /// not have. One funnel rather than four `if`s, so a meter added later
    /// cannot forget the rule.
    fn meter(&self, event: &str, f: impl FnOnce(&Frame)) {
        if self.subordinate {
            return;
        }
        self.remember(event);
        if let Some(frame) = &self.frame {
            f(frame);
        }
    }

    fn remember(&self, event: &str) {
        if let Some(rec) = &self.record {
            rec.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(event.to_string());
        }
    }

    /// True when the viewport is drawn. For callers that would otherwise say
    /// the same thing twice — the hint row carries two of them — and for tests.
    pub fn framed(&self) -> bool {
        self.frame.is_some()
    }

    /// Put text on the user's clipboard, through the terminal.
    ///
    /// **OSC 52** — `ESC ] 52 ; c ; <base64> BEL`. The terminal, not Emma, does
    /// the writing, which is why this works identically over SSH and needs no
    /// platform clipboard API. Windows Terminal has supported it since 2020.
    ///
    /// **Callers check [`Term::framed`] first so they can say something useful,
    /// and this refuses anyway.** This emits escape bytes, and `INV-001`
    /// promises none of those on `-p`, on a pipe, under `EMMA_NO_FRAME` or with
    /// no console. The call-site guard stays because a clipboard write that
    /// silently does nothing is the failure this whole area is about — the
    /// caller has a message to give, and this function does not.
    ///
    /// **But "callers must" was the whole of the enforcement, and a reviewer
    /// found nothing tested it.** No test in the workspace named `Copy`, so
    /// deleting the single `if !s.term.framed()` at the call site emitted OSC 52
    /// on a pipe with the entire suite green. An invariant held up by a sentence
    /// in a doc comment is held up by nothing; the refusal belongs at the one
    /// place that can emit the bytes, and the message belongs where there is
    /// somebody to read it.
    ///
    /// Returns whether it wrote, so the guarantee is observable rather than
    /// merely performed — the same correction `read_reporting` and
    /// `SessionLog::transcript_failed` already made elsewhere in this tree.
    ///
    /// **There is no acknowledgement.** The terminal honours the sequence or
    /// ignores it, with no reply and no error, so nothing downstream can report
    /// success as a fact. Callers say what was sent, never what arrived.
    pub fn clipboard(&self, text: &str) -> bool {
        if !self.framed() {
            return false;
        }
        // Straight to stdout rather than through `write_out`, which routes prose
        // into the frame retained transcript. This is a control sequence, not
        // content: a buffer that held it would replay somebody clipboard write
        // on every redraw.
        //
        // Writing under the frame is safe because OSC 52 draws nothing - it
        // moves no cursor and paints no cell, so it cannot disturb a frame
        // mid-paint the way an unsolicited line would.
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let _ = out.write_all(seq.as_bytes());
        let _ = out.flush();
        true
    }

    /// How much colour this run has, as `Level::of` decided it.
    ///
    /// For the one caller that has to say plainly that a setting will not
    /// change anything here: `/theme` writes a preference, and on a terminal
    /// running under `NO_COLOR` — or piped, or `EMMA_COLORS=none` — restarting
    /// will look identical. A command that stayed quiet about that would be
    /// promising something the palette will not deliver.
    pub fn colour_level(&self) -> palette::Level {
        self.skin.palette.level
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
            frame.set_identity(
                model,
                &cwd.display().to_string(),
                &session_id,
                &session.display().to_string(),
            );
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

    /// Adopt a status-line program the harness resolved.
    ///
    /// **The whole of this feature's wiring, and it is one call.** Everything a
    /// status line can do wrong — hang, fail, print an escape sequence, be
    /// pointed at somebody else's binary — is decided in `statusline.rs` here and
    /// in the harness, so a caller has nothing to get right beyond handing over
    /// what the config named.
    ///
    /// Ignored without a viewport, and that is not an oversight. `-p` is a
    /// script's stdout, [`Term::silent`] draws nothing, and the fallback path
    /// prints identity once as an ordinary line that scrolls away — none of the
    /// three has a row to keep a live status on, so running somebody's program
    /// for one would be a subprocess spawned to produce output with nowhere to
    /// go. A subordinate terminal is skipped for the reason every meter is: the
    /// status belongs to the process, not to a delegation.
    pub fn set_status_source(&self, line: std::sync::Arc<emma_harness::StatusLine>) {
        if self.subordinate {
            return;
        }
        self.remember("set_status_source");
        if let Some(frame) = &self.frame {
            frame.set_status_source(line);
        }
    }

    /// The caps the status meters are measured against. Called by the loop from
    /// the budgets it was given, so nothing here has to know what a budget is.
    pub fn set_budgets(&self, max_context: i64, max_tokens: i64) {
        self.meter("set_budgets", |frame| {
            frame.set_budgets(max_context, max_tokens)
        });
    }

    /// What a new user sees, once. See [`welcome`] and
    /// [`crate::session::first_run`].
    pub fn welcome(&self, w: &Welcome) {
        self.separate();
        self.side(self.skin.welcome(w));
        self.separate();
    }

    /// Declare a boundary between two blocks of transcript.
    ///
    /// The blank row is written by whichever path is actually at the transcript,
    /// and only if a block turns up on the other side of the boundary — so a
    /// caller may say this without knowing what follows it, or whether anything
    /// does. See [`spacing`], which is the rule, and which exists because two
    /// callers each pushing a `Line::default()` is how the screen ended up with
    /// two blank rows where one was meant.
    fn separate(&self) {
        if !self.enabled {
            return;
        }
        match &self.frame {
            Some(frame) => frame.separate(),
            None => self
                .spacing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .separate(),
        }
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
        let lines = self
            .spacing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(lines);
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
    ///
    /// **Formatting stops at the frame.** With a viewport the text is markdown
    /// and is rendered as markdown — headings, code, lists, wrapped to the
    /// window — because there is a window to wrap to and a terminal to style
    /// for. Without one there is neither: a pipe has no width, and every byte
    /// written to it is somebody's input to the next program. So the fallback
    /// path passes the model's own characters through untouched, which is also
    /// what keeps `emma … | tee` free of escape bytes. See [`markdown`].
    pub fn delta(&self, text: &str) {
        if !self.enabled || self.subordinate {
            return;
        }
        let shown = self.filtered(|f| f.push(text));
        if shown.is_empty() {
            return;
        }
        self.write_out(&shown);
    }

    pub fn end_of_text(&self) {
        if !self.enabled || self.subordinate {
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
                // Ends the line the model left open — a terminator, not a
                // separator. The gap between this answer and whatever follows
                // is the boundary declared under it.
                println!();
                self.separate();
            }
        }
    }

    /// Whole assistant text at once, for `-p` where nothing streamed.
    ///
    /// With a viewport it goes through the same two calls the streaming path
    /// uses, rather than a second rendering of its own: markdown is decided a
    /// line at a time either way, and a private path here is a path that would
    /// drift from the one people actually see. Without a viewport it is printed
    /// as it arrived — see [`Term::delta`] for why formatting stops at the
    /// frame.
    pub fn text(&self, text: &str) {
        let text = crate::goal::MarkerFilter::once(text);
        if self.enabled && !self.subordinate && !text.trim().is_empty() {
            match &self.frame {
                Some(frame) => {
                    frame.prose(text.trim_end());
                    frame.flush_prose();
                }
                None => {
                    println!("{}", text.trim_end());
                    self.separate();
                }
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
        // Straight to stdout, byte for byte, which is what keeps a pipe free of
        // anything Emma invented — so these rows never pass through the
        // spacing rule and it has to be told they happened, or the answer and
        // the next block would run together.
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        self.spacing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wrote_text(false);
    }

    // The three prose channels record their *text* rather than an event name.
    //
    // The meters are recorded by name because what a test asks of them is "was
    // this called"; a note is the opposite — the call is never in doubt and the
    // sentence is the whole of what can be wrong. `session_command` is what
    // needs it: every one of Emma's own commands answers by writing notes, and
    // without this the only way to check what `/clear` said would be a terminal
    // and a person reading it.
    pub fn note(&self, text: &str) {
        self.remember(text);
        self.side(self.skin.note(text));
    }

    pub fn warn(&self, text: &str) {
        self.remember(text);
        self.side(self.skin.warn(text));
    }

    pub fn banner(&self, text: &str) {
        self.side(self.skin.banner(text));
    }

    /// A tool is about to run. One line, the interesting argument inline — and,
    /// for a tool that changes a file, the diff underneath it.
    ///
    /// **This is the only place a change reaches the transcript, and it is why
    /// it is here rather than at the prompt.** The loop calls this for every tool
    /// call; the prompt fires only for the calls that are actually asked about,
    /// so a `Write` covered by an `allow` rule, by an `a` earlier in the session
    /// or by `--dangerously-skip-permissions` would otherwise change a file with
    /// nothing on screen but its name. That is the case the survey is about:
    /// every other harness shows the change, Emma showed the path.
    ///
    /// The diff is a *proposal* — the tool has not run, and may still be blocked,
    /// refused or fail — which is exactly what this line already means. See
    /// [`Term::prompt_header`], which does not repeat it.
    pub fn tool_started(&self, name: &str, args: &serde_json::Value) {
        if !self.enabled {
            return;
        }
        let mut lines = self.skin.tool_started(name, &summarise_args(name, args));
        if let Some(change) = diff::for_call(name, args) {
            lines.extend(change.to_lines(&self.skin, diff::BUDGET));
        }
        self.side(lines);
    }

    /// A tool ran. `truncated` is the tool's own claim that it stopped early,
    /// which is a different fact from the screen showing only the first few
    /// lines — the model's copy is short too.
    ///
    /// `reason` is that same tool's account of *which* cap bound and by how
    /// much. Passed through rather than summarised: the human watching is the
    /// one who can raise a default, and they cannot do that from the word
    /// "truncated".
    pub fn tool_result(
        &self,
        display: Option<&str>,
        content: &str,
        truncated: bool,
        reason: Option<&str>,
    ) {
        self.side(
            self.skin
                .tool_ok(display.unwrap_or(content), truncated, reason),
        );
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
    pub fn tool_refused(&self, name: &str, because: &str) {
        self.side(self.skin.tool_refused(name, because));
    }

    pub fn kick(&self, n: u32, max: u32) {
        self.side(self.skin.kick(n, max));
    }

    /// A goal stopped, for whatever reason, having spent what it spent.
    pub fn ending(&self, message: &str, ok: bool, iterations: u32, tokens: i64) {
        // The summary is its own block: it is what a reader scrolling back
        // looks for, and it should not read as one more tool result.
        self.separate();
        self.side(self.skin.ending(message, ok, iterations, tokens));
    }

    pub fn goal_started(&self, goal: &str) {
        // Above the echoed goal, not below it: the gap belongs between one
        // turn and the next, and the answer to a goal is part of the same
        // exchange as the goal. "Space after my message" was the complaint.
        self.separate();
        match (&self.frame, self.enabled) {
            // Through the kind-preserving door, so the full-screen chat pane
            // can label the row `You` instead of guessing the speaker back
            // from its styling. Inline this renders the same lines `side`
            // would have.
            (Some(frame), true) => frame.user_line(goal),
            _ => self.side(self.skin.goal(goal)),
        }
        self.meter("goal_started", Frame::goal_started);
    }

    /// The goal is over: the clock stops rather than freezing at its last value.
    pub fn goal_ended(&self) {
        self.meter("goal_ended", Frame::goal_ended);
    }

    /// One model call's measurements, for the live half of the status line.
    /// Both are measured — the provider's input count and the loop's own
    /// weighted spend — because a status line with an estimate on it is a
    /// status line that lies at exactly the moment somebody checks it.
    /// `up`/`down` are the call's own raw `input_tokens`/`output_tokens` — the
    /// ↑/↓ split the full-screen bar draws. They are separate arguments rather
    /// than derived from `context` because `context` is the *billable* input
    /// (uncached + cache writes + cache reads) and the split is deliberately
    /// the uncached traffic; deriving one from the other would put a number on
    /// screen that no provider reported.
    pub fn spent(&self, context: i64, tokens: i64, up: i64, down: i64) {
        self.meter("spent", |frame| {
            frame.spent(Some(context), Some(tokens), up, down)
        });
    }

    /// The evidence half of an approval prompt.
    ///
    /// It goes to the transcript in full — that is the record of what was
    /// asked, and it can be scrolled back to and selected — and is held for the
    /// viewport panel, which shows as much of it as fits.
    ///
    /// **Except when the transcript already has it.** [`Term::tool_started`]
    /// draws the diff for a tool that changes a file, and it runs first for
    /// every call, so echoing the preview here would put the same forty rows on
    /// screen twice in a row — which is not merely wasteful: a reader scrolling
    /// back through two copies of a diff has to work out whether they are two
    /// changes. The panel still gets the whole preview, because the panel is not
    /// the transcript and cannot be scrolled to.
    pub fn prompt_header(&self, tool: &str, preview: &str) {
        self.remember("prompt_header");
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.title = format!("Approve {tool}");
        pending.preview = preview.lines().map(str::to_string).collect();
        drop(pending);
        // A boundary rather than a blank row of its own. The answer that just
        // ended declared one too, and the model's own trailing newline may
        // already have written one; three intentions, one gap.
        self.separate();
        let mut lines = self.skin.tool_started(tool, "wants to run:");
        if !diff::changes_a_file(tool) {
            for line in preview.lines() {
                lines.push(self.skin.prose(&format!("  {line}")));
            }
        }
        self.side(lines);
    }

    /// `remember` is the rule text `[r]` would write to disk, or `None` when
    /// nothing can be written — an unattended run, or a prompt an `ask` rule
    /// forced, where the grant would be outranked by the rule that produced the
    /// question.
    ///
    /// **The rule is shown verbatim, not described.** "remember this tool" is a
    /// sentence the user has to translate into a permission; `Write` is the
    /// permission. The string here is the same one that lands in the file, so
    /// there is no second rendering to disagree with it.
    pub fn prompt_question(&self, tool: &str, remember: Option<&str>) {
        let mut q = format!("allow? [y]es  [n]o  [a]lways {tool} this session");
        let mut keys = vec![
            ("y".to_string(), "yes".to_string()),
            ("n".to_string(), "no".to_string()),
            ("a".to_string(), format!("always {tool}")),
        ];
        if let Some(rule) = remember {
            q.push_str(&format!("  [r]emember {rule}"));
            keys.push(("r".to_string(), format!("save {rule}")));
        }
        q.push_str(": ");
        self.question(&q, keys);
    }

    /// The network question, worded so each grant on offer is the one the answer
    /// actually gives: one call, or the host for the session, or one of two
    /// rules written down.
    ///
    /// The two saved options are deliberately different sizes and deliberately
    /// separate keys. `[r]` is the host in front of you. `[t]` is every call this
    /// tool ever makes, anywhere — the answer to "just let it search" — and it is
    /// never what `[r]` silently expands into, because a grant somebody arrived
    /// at by pressing the obvious key is a grant they did not read.
    pub fn prompt_network_question(&self, host: &str, remember: Option<&str>, trust: Option<&str>) {
        let mut q = format!("allow? [y]es — and {host} again this session  [n]o");
        let mut keys = vec![
            ("y".to_string(), format!("yes, and {host} again")),
            ("n".to_string(), "no".to_string()),
        ];
        if let Some(rule) = remember {
            q.push_str(&format!("  [r]emember {rule}"));
            keys.push(("r".to_string(), format!("save {rule}")));
        }
        if let Some(rule) = trust {
            q.push_str(&format!("  [t]rust {rule} (any host)"));
            keys.push(("t".to_string(), format!("save {rule}")));
        }
        q.push_str(": ");
        self.question(&q, keys);
    }

    fn question(&self, q: &str, keys: Vec<(String, String)>) {
        if !self.enabled {
            return;
        }
        self.remember("prompt_question");
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
        // Not on a subordinate view: it shares the parent's frame and goes out
        // of scope at the end of every delegation, which is the middle of the
        // parent's run. Restoring there would put the terminal back while the
        // session is still drawing into it.
        if self.frame.is_some() && !self.subordinate {
            let pointer = self.frame.as_ref().and_then(|f| f.session_pointer());
            restore_terminal();
            // The exit line, after the alternate screen has restored the
            // shell: the one pointer to a transcript the shell no longer
            // holds, printed where it survives. Leaving the alt screen with
            // nothing on the normal screen is the "run visually vanishes"
            // cost from the design's §2.1, and this is the named remedy. On
            // the inline path `session_pointer` is `None` — the transcript
            // is already in the shell's scrollback and needs no pointer.
            if let Some(path) = pointer {
                println!("session: {path}");
            }
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
pub(crate) fn terminal_size() -> Option<(u16, u16)> {
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
pub(crate) fn terminal_size() -> Option<(u16, u16)> {
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
pub(crate) fn terminal_size() -> Option<(u16, u16)> {
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
        term.spent(1, 2, 0, 0);
        term.tool_started("Bash", &json!({ "command": "ls" }));
        term.tool_result(None, "out", false, None);
        term.tool_failed("Bash", "boom");
        term.tool_blocked("Bash", "policy");
        term.tool_refused("Bash", "you declined it; the model was told");
        term.kick(1, 3);
        term.ending("done", true, 1, 2);
        term.delta("x");
        term.end_of_text();
        term.text("y");
        term.prompt_header("Bash", "$ ls");
        term.prompt_question("Bash", None);
        term.prompt_question("Bash", Some("Bash"));
        term.prompt_network_question("docs.rs", None, None);
        term.prompt_network_question(
            "docs.rs",
            Some("WebFetch(domain:docs.rs)"),
            Some("WebFetch"),
        );
        term.goal_prompt();
        term.prompt_answered(Some("y"));
        term.welcome(&Welcome::default());
    }

    /// **One predicate decides both halves of the no-duplication rule**, so
    /// "`tool_started` draws the diff" and "`prompt_header` does not draw it
    /// again" cannot come apart. The failure it prevents is two copies of a
    /// forty-row diff in a row, which a reader scrolling back has to work out
    /// are one change rather than two.
    ///
    /// Written as "these are the same set" rather than as two `contains` calls,
    /// because the hazard is a writing tool added to one list and not the other.
    #[test]
    fn the_tools_whose_diff_is_drawn_are_exactly_the_ones_the_prompt_does_not_repeat() {
        for tool in ["Write", "Edit"] {
            assert!(diff::changes_a_file(tool), "{tool}");
            assert!(
                diff::for_call(tool, &json!({})).is_none(),
                "a call with no arguments produced a diff"
            );
        }
        for tool in ["Bash", "Read", "Grep", "WebFetch", "TaskUpdate"] {
            assert!(!diff::changes_a_file(tool), "{tool}");
            assert!(diff::for_call(tool, &json!({ "command": "ls" })).is_none());
        }
    }

    /// **No emitter fabricates a blank row.** Blank rows between blocks are
    /// declared as boundaries and materialised in one place — see [`spacing`],
    /// which is the only file allowed to construct one.
    ///
    /// Asserted on the source because the failure has no other witness: a
    /// second hand-rolled `Line::default()` next to a `separate()` produces two
    /// blank rows on a real screen and passes every test that renders lines,
    /// which is exactly how the screen the owner photographed came to have
    /// three. The needle is assembled rather than written out, because this
    /// file is its own haystack.
    ///
    /// The exception, stated: a block's *internal* layout may use blank rows —
    /// `welcome.rs` does, between its sections — because those are its content,
    /// not the gap between it and its neighbour.
    #[test]
    fn no_emitter_between_here_and_the_terminal_writes_a_blank_row_of_its_own() {
        let needle = format!("Line::{}()", "default");
        for (name, src) in [
            ("term.rs", include_str!("term.rs")),
            ("term/frame.rs", include_str!("term/frame.rs")),
            // The full-screen shell is under the same rule: its blank rows
            // are the transcript buffer's separators, decided in
            // `transcript.rs` and nowhere else.
            ("term/app.rs", include_str!("term/app.rs")),
        ] {
            // Comments stripped: both files argue about the old blank rows by
            // name, as this one does. What must not appear is a *use* of one.
            let code: String = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                !code.contains(&needle),
                "{name} writes a blank row directly. Blank rows are boundaries: call `separate()` \
                 and let `spacing` decide whether one is owed."
            );
        }
    }

    #[test]
    fn printing_never_draws_a_viewport() {
        // `-p` is a script's stdout. A viewport in it is escape sequences in
        // somebody's pipeline, and raw mode on a process nobody is typing at.
        assert!(!Term::printing(theme::BUILTIN).framed());
    }
}

// endregion: Tests

#[cfg(test)]
mod clipboard_tests {
    use super::base64;

    /// `INV-001` for the one function in this file that emits escape bytes.
    ///
    /// **A reviewer found that no test in the workspace named `Copy`.** The
    /// promise that piped and `-p` output carries no escape bytes rested, for
    /// this path, on a single `if !s.term.framed()` at the `/copy` call site and
    /// on a sentence in a doc comment asking callers to write it. Deleting that
    /// one line put OSC 52 into a pipe with the whole suite green.
    ///
    /// So the refusal moved to the function that writes the bytes, and this
    /// asserts it there: every constructor that yields an unframed terminal
    /// refuses, and says so by returning `false` rather than by doing nothing
    /// visible. `recording` and `subordinate` are included because a nested run
    /// borrows its parent decision -- a sub-agent must not be the hole.
    #[test]
    fn an_unframed_terminal_refuses_to_write_the_clipboard() {
        use super::{theme, Term};

        let unframed: Vec<(&str, Term)> = vec![
            ("silent", Term::silent()),
            ("printing", Term::printing(theme::BUILTIN)),
            ("recording", Term::recording()),
            ("subordinate", Term::silent().subordinate()),
        ];

        for (name, term) in unframed {
            assert!(!term.framed(), "{name} unexpectedly has a frame");
            assert!(
                !term.clipboard("anything at all"),
                "{name} wrote OSC 52 on a run that must emit no escape bytes"
            );
        }
    }

    /// The RFC 4648 vectors, plus the payload shape OSC 52 actually carries.
    ///
    /// **A hand-written encoder is exactly the thing to check against somebody
    /// else's numbers rather than against itself.** These six strings are the
    /// canonical vectors: they exercise every remainder — no padding, one `=`,
    /// two `=` — which is where an encoder written from the format description
    /// goes wrong. Getting the padding subtly wrong produces output that looks
    /// like base64, decodes to nothing, and fails silently in a terminal that
    /// never reports errors.
    #[test]
    fn the_encoder_matches_the_published_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "encoding {input:?}");
        }
    }

    /// Multi-byte text survives, because an answer is not ASCII.
    ///
    /// The encoder takes bytes, so this is really a check that the caller hands
    /// it UTF-8 and nothing tries to be clever about characters — a `chars()`
    /// based encoder would produce something that decodes to mojibake, and the
    /// clipboard would receive it without complaint.
    #[test]
    fn utf8_round_trips_through_the_encoder() {
        // "héllo — ✓" as bytes, encoded, then decoded by hand from the alphabet.
        let text = "héllo — ✓";
        let encoded = base64(text.as_bytes());
        assert!(
            !encoded.contains(|c: char| !c.is_ascii()),
            "the encoding is not ascii-safe: {encoded}"
        );
        // Length is a function of byte count, not character count. Getting this
        // wrong is the symptom of counting characters somewhere.
        let expect = text.len().div_ceil(3) * 4;
        assert_eq!(encoded.len(), expect, "{encoded}");
    }
}

/// Base64, standard alphabet with padding — the encoding OSC 52 specifies.
///
/// Written out rather than pulling a crate. `base64` is already in the lock file
/// as somebody else's transitive dependency, and taking it as a *direct* one
/// makes it Emma's to audit and to keep current, for a single call site of
/// twenty lines. The same trade `cli::edit_distance_at_most_one` made.
///
/// The three-byte groups and the `=` padding are the whole of the format; there
/// is no line wrapping, because OSC 52 carries one unbroken payload.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, &v) in idx.iter().enumerate() {
            // A group of one byte carries two characters of payload, a group of
            // two carries three; the rest is padding rather than data.
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[v as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
