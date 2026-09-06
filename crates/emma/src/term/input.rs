//! The one place stdin is read, in either of the two ways it can be read.
//!
//! A single reader behind a channel. The goal prompt and the approval prompt
//! both want lines, and two readers on one stdin race for them.
//!
//! # The property that cannot regress
//!
//! **The queue must be empty before a question is asked.** The reader is eager:
//! it consumes whatever is typed, whenever it is typed, and holds it. Without a
//! drain, a line typed while the agent was working is waiting when the next
//! prompt appears and is handed over as the answer. That is not a stale-input
//! annoyance — it is one question being answered by a keystroke aimed at
//! another, and the place it matters most is the approval gate, where the line
//! in question is a `y`.
//!
//! Observed rather than theorised, on the first real run: an answered `y`
//! outlived a turn that aborted on its token budget and was consumed as the
//! *next goal*. The same path could as easily have approved a `Bash` command
//! the user never saw — and left a log saying they approved it.
//!
//! **The frame did not weaken this; it strengthened it.** Under the old cooked
//! mode there was a second buffer nothing here could reach: the line discipline
//! held a partly-typed line, and a drain could not touch it, so a half-typed
//! `y` followed by return after a question appeared would still answer it. In
//! raw mode Emma holds that buffer itself, so [`LineSource::drain`] now clears
//! *both* — the completed lines in the channel and the characters typed but not
//! yet submitted. After a drain the input box is visibly empty, which is the
//! version of this property a user can check.
//!
//! # What raw mode costs, and what it buys
//!
//! It costs the line discipline: backspace, `Ctrl-U` and the rest are
//! implemented here rather than by the terminal, and there is no history and no
//! completion. It buys the only thing that made the viewport possible — the
//! terminal no longer echoes anything, so nothing lands on rows ratatui thinks
//! it owns. It also means **Ctrl-C is a keystroke rather than a signal**, and
//! delivering it is this file's job: see [`Action::Interrupt`].

use std::sync::Arc;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use tokio::sync::mpsc;

use super::app::{self, SidebarAction};
use super::frame::Frame;
use super::menu::Menu;

// region: The line editor
// ---------------------------------------------------------------------------
// The line editor
//
// Pure, and small on purpose. Everything here is a key that a user pressing it
// would be surprised not to have. Anything more — history, completion, word
// motion beyond `Ctrl-W` — is a text editor, and Emma already has one of those
// as a tool.
// ---------------------------------------------------------------------------

/// What a keystroke did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Redraw and wait for more.
    Edit,
    /// A line was submitted.
    Submit(String),
    /// Ctrl-C. The reader trips the interrupt; it does not end input.
    Interrupt,
    /// Ctrl-D on an empty line, or the terminal going away.
    Eof,
    /// Nothing worth redrawing for.
    Ignore,
}

/// What a paste lost on the way into a one-line editor.
///
/// **A count rather than a flag, and returned rather than performed silently.**
/// Flattening a forty-line stack trace to one line is defensible; doing it
/// without saying so is not, because the user is about to spend a model call on
/// text that is not the text they copied. `DEF-028` is the row, and the half
/// this closes is the silence rather than the flattening -- keeping the breaks
/// needs the multi-line editor of the full-screen design's stage 0(a),
/// which is not built.
///
/// The same correction `Term::clipboard` and `Interrupt::starting_goal` already
/// made in this tree: a function that decides something and returns nothing has
/// made the decision unobservable, to the user and to a test alike.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PasteLoss {
    /// Runs of line breaks turned into one space each. A run counts once,
    /// because one run is what the reader sees as one lost gap -- counting
    /// `\r\n` as two would report a number nobody can match to the clipboard.
    pub breaks_joined: usize,
    /// Tabs turned into a single space. Separate from a break because a tab
    /// surviving as one space is a changed width, not a changed structure, and
    /// somebody pasting an indented block should be told which happened.
    pub tabs_flattened: usize,
    /// Control bytes removed entirely, escape sequences among them.
    pub controls_dropped: usize,
}

impl PasteLoss {
    /// Nothing lost, nothing said.
    pub fn is_clean(&self) -> bool {
        self.breaks_joined == 0 && self.tabs_flattened == 0 && self.controls_dropped == 0
    }

    /// What to tell the user, or `None` when the paste arrived intact.
    ///
    /// Names the counts and then the consequence, in that order: a number on
    /// its own reads as trivia, and the sentence that matters is that the model
    /// will be asked about text the clipboard did not hold.
    pub fn note(&self) -> Option<String> {
        if self.is_clean() {
            return None;
        }
        let mut parts = Vec::new();
        if self.breaks_joined > 0 {
            parts.push(format!(
                "{} line break(s) became spaces",
                self.breaks_joined
            ));
        }
        if self.tabs_flattened > 0 {
            // "became one space" was wrong and a reviewer counted it: three tabs
            // produce three spaces, not one. The whole premise of this note is
            // that the number matches what the reader can see in the box.
            parts.push(format!(
                "{} tab(s) each became a space",
                self.tabs_flattened
            ));
        }
        if self.controls_dropped > 0 {
            parts.push(format!(
                "{} control byte(s) were dropped",
                self.controls_dropped
            ));
        }
        Some(format!(
            "pasted with changes: {} — this input box is one line, so what is sent is not \
             byte-for-byte what was copied",
            parts.join(", ")
        ))
    }
}

/// The parts of the frame the reader thread pushes state into.
///
/// **`ARCH-003`'s pilot, and the reason it is a trait rather than a function.**
/// The reader thread cannot be started from a test — it owns a real terminal —
/// so every line inside it is untested by construction. A reviewer proved the
/// consequence for the paste arm: neutering
/// `if let Some(note) = ed.paste(&text).note()` left the whole workspace green,
/// which is the same shape as `DEF-017`'s `starting_goal` call and `UI-003`'s
/// `Alt+m` dispatch. One defect with three instances.
///
/// Extracting the *decision* does not help, because the decision was already
/// tested; what is untestable is the **effect**, and an effect on `Frame` needs
/// a `Frame`. So the arm takes this instead, `Frame` implements it, and a test
/// hands it a recorder.
///
/// Deliberately narrow: exactly the five methods the paste arm uses. A trait
/// that mirrored `Frame` would be a second interface to keep true, and the next
/// arm extracted should widen this by what it needs rather than by what it
/// might.
pub trait Surface {
    fn note_line(&self, text: &str);
    fn set_input(&self, text: &str, cursor: usize);
    fn prompt_pending(&self) -> bool;
    fn set_menu(&self, menu: Option<super::menu::MenuView>);
    fn draw(&self);
}

impl Surface for super::frame::Frame {
    fn note_line(&self, text: &str) {
        super::frame::Frame::note_line(self, text)
    }
    fn set_input(&self, text: &str, cursor: usize) {
        super::frame::Frame::set_input(self, text, cursor)
    }
    fn prompt_pending(&self) -> bool {
        super::frame::Frame::prompt_pending(self)
    }
    fn set_menu(&self, menu: Option<super::menu::MenuView>) {
        super::frame::Frame::set_menu(self, menu)
    }
    fn draw(&self) {
        super::frame::Frame::draw(self)
    }
}

/// A whole clipboard arriving at once: flatten it, say what that cost, and push
/// the result at the surface.
///
/// The body of the reader thread's `Event::Paste` arm, lifted out so it can be
/// driven. What is worth testing here is not that `paste` flattens — that has
/// its own test — but that the note actually **reaches** the surface, which is
/// the line that was deletable.
pub fn on_paste(ed: &mut Editor, menu: &mut super::menu::Menu, surface: &dyn Surface, text: &str) {
    // **Said, not merely done.** A paste into a one-line box loses structure,
    // and the user is about to pay for a model call on text the clipboard did
    // not hold. The flattening stays until the multi-line editor lands; the
    // silence does not have to.
    if let Some(note) = ed.paste(text).note() {
        surface.note_line(&note);
    }
    surface.set_input(&ed.text(), ed.cursor());
    // A paste beginning `/` is a typed `/` as far as the menu is concerned.
    menu.sync(&ed.text(), surface.prompt_pending());
    surface.set_menu(menu.view());
    surface.draw();
}

/// The line being typed.
#[derive(Debug, Default, Clone)]
pub struct Editor {
    chars: Vec<char>,
    cursor: usize,
}

impl Editor {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Throw away whatever is half-typed. Called by [`LineSource::drain`], and
    /// the reason it exists: see the module doc.
    /// The index one word behind the cursor, by `Ctrl-W`'s rule: skip the
    /// whitespace you are sitting in, then skip the run of non-whitespace
    /// before it.
    ///
    /// **Extracted so that "a word" has one definition.** `Ctrl-W`,
    /// `Ctrl-Left` and `Ctrl-Backspace` all mean the same boundary, and this
    /// project has been bitten more than once by one input shape getting two
    /// answers in two places — a tolerance added to one reader and not its
    /// siblings. Cheaper to share the rule than to reconcile it later.
    fn word_start(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }

    /// The index one word ahead of the cursor — the mirror of [`Self::word_start`],
    /// and deliberately the same two passes in the same order so that moving
    /// right then left across one word is not off by one.
    fn word_end(&self) -> usize {
        let mut at = self.cursor;
        while at < self.chars.len() && self.chars[at].is_whitespace() {
            at += 1;
        }
        while at < self.chars.len() && !self.chars[at].is_whitespace() {
            at += 1;
        }
        at
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Text arriving as one lump from the terminal's clipboard.
    ///
    /// **This exists so that a pasted newline is not a keypress.** Without
    /// bracketed paste the terminal delivers a pasted block as the keystrokes it
    /// resembles, and the first `\n` in it is indistinguishable from Enter: half
    /// a code block is submitted as a goal and the other half is typed into
    /// whatever comes next. `ESC[?2004h` (see [`super::frame`]) makes the
    /// terminal wrap the block in markers instead, and crossterm hands it here
    /// as one `Event::Paste`. Nothing in this function can submit.
    ///
    /// **Line breaks become spaces, because this editor is one line.** The
    /// multi-line editor that keeps them is stage 0(a) of
    /// the full-screen design and is not built yet; until it is, the
    /// choice is between flattening the paste and refusing it, and a flattened
    /// stack trace is still the stack trace the user meant to ask about. A run
    /// of breaks collapses to one space so a paste with blank lines in it does
    /// not arrive full of gaps.
    ///
    /// **Everything else that is not printable is dropped.** A clipboard can
    /// hold escape bytes — from a terminal recording, from a log file, from
    /// somebody who put them there on purpose — and this string is rendered
    /// into cells and, on the fallback path, written to a stream. `\x1b` in a
    /// span is an escape sequence Emma did not author, arriving at the terminal
    /// through a text field. Tabs go too: a tab in a cell is not eight columns,
    /// it is one cell containing a tab.
    /// Returns what was lost. The action is always [`Action::Edit`], which is
    /// why this returns the loss instead: a paste cannot submit, and the only
    /// thing a caller cannot work out for itself is what the text stopped
    /// being.
    pub fn paste(&mut self, text: &str) -> PasteLoss {
        let mut loss = PasteLoss::default();
        let mut last_was_break = false;
        for c in text.chars() {
            if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                if !last_was_break {
                    self.chars.insert(self.cursor, ' ');
                    self.cursor += 1;
                    // Counted on the first break of a run, matching the one
                    // space that replaces it.
                    loss.breaks_joined += 1;
                }
                last_was_break = true;
                continue;
            }
            last_was_break = false;
            let c = if c == '\t' {
                loss.tabs_flattened += 1;
                ' '
            } else {
                c
            };
            if c.is_control() {
                loss.controls_dropped += 1;
                continue;
            }
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
        loss
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        // Windows reports releases as well as presses; acting on both types
        // every character twice.
        if key.kind == KeyEventKind::Release {
            return Action::Ignore;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                // The buffer goes with it. A user who hits Ctrl-C and then
                // types a goal must not find the interrupted line still in
                // front of it.
                self.clear();
                Action::Interrupt
            }
            KeyCode::Char('d') if ctrl => {
                if self.chars.is_empty() {
                    Action::Eof
                } else {
                    Action::Ignore
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.clear();
                Action::Edit
            }
            KeyCode::Char('w') if ctrl => {
                // Computed once: after the drain the buffer is shorter and
                // `self.cursor` is stale, so asking a second time indexes past
                // the end.
                let start = self.word_start();
                self.chars.drain(start..self.cursor);
                self.cursor = start;
                Action::Edit
            }
            KeyCode::Char('a') if ctrl => {
                self.cursor = 0;
                Action::Edit
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = self.chars.len();
                Action::Edit
            }
            // A modifier we do not implement must not type its letter: `Ctrl-R`
            // arriving as an `r` in the middle of a goal is worse than nothing
            // happening.
            KeyCode::Char(_) if ctrl || key.modifiers.contains(KeyModifiers::ALT) => Action::Ignore,
            KeyCode::Char(c) => {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
                Action::Edit
            }
            // **Word motion, and the reason all four arms are here rather than
            // three.** Until 2026-08-27 none of these existed, and none of them
            // failed either: no decoder compares the *whole* modifier set, only
            // the bits its own arm names, so `Ctrl-Left` fell through to the
            // plain `Left` below and moved one column. Every other editor moves
            // a word, so the box quietly did the narrow thing and nothing said
            // otherwise. Found by the 4,270-keystroke sweep in
            // `super::bindings`, not by reading — each arm below is correct in
            // isolation and the defect lived in what fell past them.
            //
            // The two deletes claim their chord with no emptiness guard, as
            // `Ctrl-W` already does. A guard would be tidier and wrong: an
            // unclaimed chord falls through to the plain arm below, which is
            // the exact fall-through this change exists to remove, and it would
            // do so only on an empty box — the state nobody tests by hand.
            //
            // They share `word_start`/`word_end` with `Ctrl-W` above rather
            // than repeating its two loops, because a second definition of
            // "where does a word end" is the one-input-two-answers shape this
            // codebase keeps paying for.
            KeyCode::Left if ctrl => {
                self.cursor = self.word_start();
                Action::Edit
            }
            KeyCode::Right if ctrl => {
                self.cursor = self.word_end();
                Action::Edit
            }
            KeyCode::Backspace if ctrl => {
                // Computed once: after the drain the buffer is shorter and
                // `self.cursor` is stale, so asking a second time indexes past
                // the end.
                let start = self.word_start();
                self.chars.drain(start..self.cursor);
                self.cursor = start;
                Action::Edit
            }
            KeyCode::Delete if ctrl => {
                self.chars.drain(self.cursor..self.word_end());
                Action::Edit
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.chars.remove(self.cursor - 1);
                self.cursor -= 1;
                Action::Edit
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
                Action::Edit
            }
            KeyCode::Left if self.cursor > 0 => {
                self.cursor -= 1;
                Action::Edit
            }
            KeyCode::Right if self.cursor < self.chars.len() => {
                self.cursor += 1;
                Action::Edit
            }
            KeyCode::Home => {
                self.cursor = 0;
                Action::Edit
            }
            KeyCode::End => {
                self.cursor = self.chars.len();
                Action::Edit
            }
            KeyCode::Enter => {
                let line = self.text();
                self.clear();
                Action::Submit(line)
            }
            _ => Action::Ignore,
        }
    }
}

// endregion: The line editor

// region: The menu's keys
// ---------------------------------------------------------------------------
// The menu's keys
//
// Four keys change meaning while the command menu is up, and only while it is
// up. Kept as a pure function so "Esc closes the menu rather than clearing the
// line" and "Enter picks rather than submits" are decisions a test states,
// rather than branches buried in a thread nothing can drive.
// ---------------------------------------------------------------------------

/// A keystroke the menu owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    /// Enter or Tab: take the highlighted command.
    Accept,
    /// Esc. The menu goes and the typed text stays.
    Dismiss,
}

/// Which keys the menu takes, and which fall through to the line editor.
///
/// With the menu shut this is `None` for everything, which is what keeps the
/// editor exactly as it was: arrows still move the cursor, Enter still submits,
/// and Esc still does nothing at all.
pub fn menu_key(key: KeyEvent, open: bool) -> Option<MenuKey> {
    if !open || key.kind == KeyEventKind::Release {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(MenuKey::Up),
        KeyCode::Down => Some(MenuKey::Down),
        KeyCode::Enter | KeyCode::Tab => Some(MenuKey::Accept),
        KeyCode::Esc => Some(MenuKey::Dismiss),
        _ => None,
    }
}

// endregion: The menu's keys

// region: The transcript's keys
// ---------------------------------------------------------------------------
// The transcript's keys
//
// The alternate screen took the terminal's scrolling, so the reader now owns
// the keys that give it back. Kept as a pure function for the same reason
// `menu_key` is — which key acts on which pane is a set of decisions a test
// can state — and consumed in the reader without ever touching the line
// channel, which is what keeps the drain guarantee whole: a scroll cannot
// become a line, so it cannot answer a question. The transcript's scroll
// position is likewise *exempt* from the drain, as a decision rather than an
// omission (design §5): a scroll offset cannot answer a question, and yanking
// the reader's page because a prompt appeared would be hostile.
// ---------------------------------------------------------------------------

/// A keystroke aimed at the transcript pane or the sidebar rather than the
/// line editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKey {
    PageUp,
    PageDown,
    RowUp,
    RowDown,
    Top,
    Tail,
    Sidebar,
}

/// The pages that take keys while they are open, in the order the reader
/// offers a key to them.
///
/// **A seam, and the reason it exists is that the three methods behind it had
/// no caller.** `Frame::{harness_key, memory_key, settings_key}` were public,
/// tested at the `App` level, and nothing in the reader thread called any of
/// them, so at runtime every page was read-only: `Alt+h` opened the Harness
/// page and `j` did nothing on it. A whole-tree search for the three names
/// found only their definitions and their tests. The fork found the identical
/// defect on its side and wrote the same block; this is that block, behind a
/// trait so the *order* can be tested without a terminal, which `Frame`
/// cannot be built without.
///
/// Order is part of the contract. Help sits on top because it is the one page
/// that can be opened over any other. A page that is not open answers `false`
/// and costs nothing; chords and releases fall through every page to the
/// global layer below, which is what keeps `Alt+,` able to close the page it
/// opened.
pub trait PageKeys {
    /// The Help page, once it exists. `false` until then.
    fn help_key(&self, key: KeyEvent) -> bool;
    fn harness_key(&self, key: KeyEvent) -> bool;
    fn memory_key(&self, key: KeyEvent) -> bool;
    /// The Code page, once it exists. `false` until then.
    fn code_key(&self, key: KeyEvent) -> bool;
    fn settings_key(&self, key: KeyEvent) -> bool;
}

impl PageKeys for Arc<Frame> {
    fn help_key(&self, _key: KeyEvent) -> bool {
        false
    }
    fn harness_key(&self, key: KeyEvent) -> bool {
        Frame::harness_key(self, key)
    }
    fn memory_key(&self, key: KeyEvent) -> bool {
        Frame::memory_key(self, key)
    }
    fn code_key(&self, _key: KeyEvent) -> bool {
        false
    }
    fn settings_key(&self, key: KeyEvent) -> bool {
        Frame::settings_key(self, key)
    }
}

/// Offer one key to the open pages, top page first. `true` when a page took
/// it, in which case the reader is done with the key; `false` hands it to the
/// pane layer.
pub fn run_page_key<P: PageKeys>(pages: &P, key: KeyEvent) -> bool {
    pages.help_key(key)
        || pages.harness_key(key)
        || pages.memory_key(key)
        || pages.code_key(key)
        || pages.settings_key(key)
}

/// Which keys the panes take, and which fall through to the editor.
///
/// - `PgUp`/`PgDn` and `Ctrl-↑`/`Ctrl-↓` are unconditionally the
///   transcript's: the editor never used them, and they work **while a goal
///   runs and while a question is pending** — reading the evidence above an
///   approval prompt is exactly when scrolling matters most, and the prompt
///   itself is fixed chrome that scrolling cannot move.
/// - `Home`/`End` belong to the editor while there is a line to move within;
///   on an empty box they have nothing to do there and go to the transcript.
///   One rule, no mode: the keys act on the thing that can act.
/// - `Ctrl-B` toggles the sidebar, except while a question is pending — the
///   design's §4.6 keeps sidebar keys dead under a prompt so nothing aimed at
///   a pane can ever read as part of an answer.
pub fn pane_key(key: KeyEvent, editor_empty: bool, prompt_pending: bool) -> Option<PaneKey> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::PageUp => Some(PaneKey::PageUp),
        KeyCode::PageDown => Some(PaneKey::PageDown),
        KeyCode::Up if ctrl => Some(PaneKey::RowUp),
        KeyCode::Down if ctrl => Some(PaneKey::RowDown),
        KeyCode::Home if editor_empty => Some(PaneKey::Top),
        KeyCode::End if editor_empty => Some(PaneKey::Tail),
        KeyCode::Char('b') if ctrl && !prompt_pending => Some(PaneKey::Sidebar),
        _ => None,
    }
}

/// Rows per wheel notch. Three is what terminals themselves scroll by.
pub const WHEEL_ROWS: usize = 3;

/// The user-tool chords: `Alt+<key>`, where `<key>` is a catalogue entry's
/// letter (`Alt+s` for Shell, `Alt+,` for Settings, and so on — the sidebar's
/// TOOLS column shows each one). Which letters are bound is the catalogue's
/// knowledge, not this function's: it names the *class*, and the frame looks
/// the letter up.
///
/// **A bare letter is never a shortcut, and the empty-editor gate would not
/// have made it one safely.** The design's §6 refused the mockup's bare
/// `s`/`c`/`f` bindings outright — "a bare letter that acts while the input
/// box exists is a footgun" — and the softer "only when the editor is empty"
/// rule dies on a fact the approval-gate keys never meet: the first character
/// of *every* goal lands on an empty editor, so a bare `s` binding makes any
/// goal beginning with `s` start a shell instead. An Alt chord types nothing,
/// which puts it in `Ctrl-B`'s class: safe whatever the editor holds, so the
/// editor's content is not consulted. AltGr arrives as Ctrl+Alt on Windows
/// layouts and produces real characters, so Ctrl excludes.
///
/// Dead while a question is pending, like every non-scroll key aimed away
/// from the prompt (§4.6) — and like the launches themselves, whose results
/// come back as transcript lines that can never enter the line channel.
pub fn tool_key(key: KeyEvent, prompt_pending: bool) -> Option<char> {
    if key.kind == KeyEventKind::Release || prompt_pending {
        return None;
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if alt && !ctrl => Some(c),
        _ => None,
    }
}

// endregion: The transcript's keys

// region: The reader
// ---------------------------------------------------------------------------
// The reader
//
// Two constructors, one type. `stdin` is the pre-frame reader and is what the
// fallback path still uses, character for character. `raw` is the frame's, and
// the only difference the rest of Emma can see is that `drain` now reaches one
// buffer further.
// ---------------------------------------------------------------------------

pub struct LineSource {
    rx: mpsc::Receiver<String>,
    /// The half-typed line, shared with the reader thread. `None` on the cooked
    /// path, where the terminal's own line discipline holds it and nothing in
    /// this process can reach it.
    editing: Option<Arc<std::sync::Mutex<Editor>>>,
    /// The command menu, shared with the same thread and drained with the same
    /// call. An open menu holds a selection, and a selection is one Enter away
    /// from being a line — so it is a place input can hide, and [`Self::drain`]
    /// closes it for exactly the reason it clears the half-typed line.
    menu: Option<Arc<std::sync::Mutex<Menu>>>,
    frame: Option<Arc<Frame>>,
}

impl LineSource {
    /// The cooked reader: whole lines, edited by the terminal.
    ///
    /// Unchanged from before the frame existed, deliberately. It is what every
    /// fallback run uses — no TTY, `TERM=dumb`, a tiny window, `EMMA_NO_FRAME`
    /// — and the fallback is the product.
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
        Self {
            rx,
            editing: None,
            menu: None,
            frame: None,
        }
    }

    /// The raw reader, for a run that has a viewport.
    ///
    /// `on_interrupt` is Ctrl-C. Raw mode turns off the terminal's own signal
    /// generation on both platforms — `ISIG` on unix, `ENABLE_PROCESSED_INPUT`
    /// on Windows — so `tokio::signal` will not fire while this reader is
    /// installed and the keystroke is the only delivery there is. The signal
    /// handler stays installed anyway: it is what a fallback run uses, and two
    /// paths tripping one flag costs nothing.
    pub fn raw(frame: Arc<Frame>, menu: Menu, on_interrupt: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel(8);
        let editing = Arc::new(std::sync::Mutex::new(Editor::default()));
        let menu = Arc::new(std::sync::Mutex::new(menu));
        let thread_editor = editing.clone();
        let thread_menu = menu.clone();
        let thread_frame = frame.clone();
        std::thread::spawn(move || loop {
            // A poll rather than a blocking read, so a frame that has been torn
            // down does not leave a thread wedged inside the console driver
            // holding a handle nobody can close.
            match event::read() {
                Err(_) => break,
                Ok(Event::Key(key)) => {
                    // The open page's keys, before the pane layer. Reader-local
                    // like the pane keys: a page mutates through the frame and
                    // nothing here enters the line channel, so the drain
                    // guarantee is untouched. See [`run_page_key`] for why this
                    // line was missing and what it cost.
                    if run_page_key(&thread_frame, key) {
                        continue;
                    }
                    // The pane keys act first and locally — they mutate view
                    // state through the frame and never enter the line
                    // channel, so the drain guarantee cannot be touched by
                    // anything they hold. Inline runs get a no-op from every
                    // one of them: there the terminal still owns scrollback.
                    let pane = {
                        let ed = thread_editor.lock().unwrap_or_else(|e| e.into_inner());
                        pane_key(key, ed.is_empty(), thread_frame.prompt_pending())
                    };
                    if let Some(cmd) = pane {
                        match cmd {
                            PaneKey::PageUp => thread_frame.scroll_page(true),
                            PaneKey::PageDown => thread_frame.scroll_page(false),
                            PaneKey::RowUp => thread_frame.scroll_rows(true, 1),
                            PaneKey::RowDown => thread_frame.scroll_rows(false, 1),
                            PaneKey::Top => thread_frame.scroll_top(),
                            PaneKey::Tail => thread_frame.scroll_tail(),
                            PaneKey::Sidebar => thread_frame.toggle_sidebar(),
                        }
                        continue;
                    }
                    // A tool chord is consumed here, reader-locally, exactly
                    // like the pane keys above: the launch runs on its own
                    // thread inside the frame and its result arrives as
                    // transcript lines, so nothing on this path can enter the
                    // line channel and the drain guarantee is untouched by
                    // construction. An unbound chord launches nothing, which
                    // is also what the editor did with it before this branch
                    // existed (`Action::Ignore`).
                    if let Some(c) = tool_key(key, thread_frame.prompt_pending()) {
                        thread_frame.launch_tool(c);
                        continue;
                    }
                    // **Esc leaves a page, and only after the menu has had it.**
                    // Ordered below `menu_key` deliberately: while the menu is
                    // open Esc dismisses the menu, which is what it has always
                    // done and what a user pressing it expects. A page is the
                    // outer thing and gives its key up to the inner one.
                    //
                    // Reader-local like every other pane key: it is consumed
                    // here and mutates through the frame, so it never reaches
                    // the line channel and the drain guarantee is untouched.
                    if key.kind != KeyEventKind::Release
                        && key.code == KeyCode::Esc
                        && !thread_frame.prompt_pending()
                        && !{
                            let m = thread_menu.lock().unwrap_or_else(|e| e.into_inner());
                            m.is_open()
                        }
                        && thread_frame.leave_page()
                    {
                        continue;
                    }
                    // The menu takes four keys, and only while it is open. With
                    // it shut this is `None` for everything and the editor
                    // below is reached exactly as it always was.
                    let owned = {
                        let m = thread_menu.lock().unwrap_or_else(|e| e.into_inner());
                        menu_key(key, m.is_open())
                    };
                    if let Some(owned) = owned {
                        let picked = {
                            let mut m = thread_menu.lock().unwrap_or_else(|e| e.into_inner());
                            let mut ed = thread_editor.lock().unwrap_or_else(|e| e.into_inner());
                            let picked = match owned {
                                MenuKey::Up => {
                                    m.move_by(-1);
                                    None
                                }
                                MenuKey::Down => {
                                    m.move_by(1);
                                    None
                                }
                                // The typed text is not touched: Esc is for the
                                // popup, not for the sentence behind it.
                                MenuKey::Dismiss => {
                                    m.dismiss(&ed.text());
                                    None
                                }
                                MenuKey::Accept => {
                                    let name = m.selection().map(|e| format!("/{}", e.name));
                                    m.close();
                                    if name.is_some() {
                                        // The picked command replaces whatever
                                        // was being typed towards it, so the
                                        // box is empty for the next line.
                                        ed.clear();
                                        thread_frame.set_input("", 0);
                                    }
                                    name
                                }
                            };
                            thread_frame.set_menu(m.view());
                            picked
                        };
                        thread_frame.draw();
                        if let Some(line) = picked {
                            // A menu-accepted command is a submit, and gets
                            // the same snap to the tail a typed one does.
                            thread_frame.scroll_tail();
                            if tx.blocking_send(line).is_err() {
                                break;
                            }
                        }
                        continue;
                    }

                    let action = {
                        let mut ed = thread_editor.lock().unwrap_or_else(|e| e.into_inner());
                        let action = ed.key(key);
                        thread_frame.set_input(&ed.text(), ed.cursor());
                        // What is typed decides whether the menu is up. A
                        // pending question suppresses it outright — while an
                        // approval is on screen, `/` is just a character.
                        let mut m = thread_menu.lock().unwrap_or_else(|e| e.into_inner());
                        match action {
                            // A submitted or interrupted line takes the menu
                            // with it rather than leaving a selection behind.
                            Action::Submit(_) | Action::Interrupt => m.close(),
                            _ => m.sync(&ed.text(), thread_frame.prompt_pending()),
                        }
                        thread_frame.set_menu(m.view());
                        action
                    };
                    match action {
                        Action::Ignore => {}
                        Action::Edit => thread_frame.draw(),
                        Action::Interrupt => {
                            thread_frame.draw();
                            on_interrupt();
                        }
                        Action::Eof => break,
                        Action::Submit(line) => {
                            // A submit snaps the view to the tail (a no-op
                            // inline): the answer to what was just sent is
                            // about to arrive there, and a reader parked
                            // fifty rows up would watch nothing happen.
                            thread_frame.scroll_tail();
                            thread_frame.draw();
                            if tx.blocking_send(line).is_err() {
                                break;
                            }
                        }
                    }
                }
                // The wheel, which mouse capture exists for: without capture,
                // on the alternate screen, it does nothing at all. Beyond the
                // wheel, exactly one click is routed: a plain left *press*,
                // to the frame's single click target (the sidebar's collapse
                // affordance — the route the owner actually tried when the
                // toggle "did not work"). Drags, releases and every other
                // button fall through untouched, and anything Shift-modified
                // is refused even where a terminal forwards it: Shift-drag is
                // the terminal's own selection — the one copy route left on
                // the alternate screen — and this arm must never bid for it.
                // A click mutates view state through the frame and never
                // enters the line channel, same as the wheel.
                Ok(Event::Mouse(mouse)) => match mouse.kind {
                    // The Harness page's Run Graph gets first refusal on the
                    // wheel: while it is open the transcript under it cannot be
                    // seen, so scrolling it would move something nobody is
                    // looking at.
                    MouseEventKind::ScrollUp => {
                        if !thread_frame.harness_scroll(true) {
                            thread_frame.scroll_rows(true, WHEEL_ROWS);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if !thread_frame.harness_scroll(false) {
                            thread_frame.scroll_rows(false, WHEEL_ROWS);
                        }
                    }
                    // **The Shift guard is this tree's and it stays.** The
                    // imported reader had no such arm: it bid for every plain
                    // left press. `CLAUDE.md` names Shift-drag as the one
                    // native selection route left on the alternate screen, so a
                    // Shift-modified press is refused here even where a
                    // terminal forwards it, and the in-app selection below
                    // takes only the unmodified one.
                    MouseEventKind::Down(MouseButton::Left)
                        if !mouse.modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        // The pages go first: while one is open the chat pane
                        // under it cannot be selected anyway, so a press that
                        // lands on a run box or a settings chevron is that
                        // control being used and nothing else.
                        if thread_frame.harness_click(mouse.column, mouse.row) {
                            continue;
                        }
                        if thread_frame.settings_click(mouse.column, mouse.row) {
                            continue;
                        }
                        // Then the sidebar's one control, on the same rule: its
                        // `[+]` and the chat pane's selection answer the same
                        // event, and a press on the affordance is not the start
                        // of a drag.
                        match thread_frame.sidebar_click(mouse.column, mouse.row) {
                            Some(SidebarAction::NewSession) => {
                                thread_frame.note_line(app::NEW_SESSION_NOTICE);
                                thread_frame.scroll_tail();
                                thread_frame.draw();
                                if tx
                                    .blocking_send(app::NEW_SESSION_COMMAND.to_string())
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            // Then the scrollbar, on the same rule: its column
                            // is chrome, and a press on it is the bar being
                            // used. The frame answers because the frame is what
                            // knows where the bar was painted.
                            None if thread_frame.bar_press(mouse.column, mouse.row) => {}
                            None => thread_frame.select_begin(mouse.column, mouse.row),
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left)
                        if !mouse.modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        if !thread_frame.bar_drag(mouse.row) {
                            thread_frame.select_extend(mouse.column, mouse.row);
                        }
                    }
                    // A release that ends a scrollbar drag is not a selection
                    // being finished, so nothing goes to the clipboard for it.
                    // Clippy (1.98) wants `bar_release` folded into the arm's
                    // guard; it has a side effect, and a guard is the one place
                    // a reader does not expect one.
                    #[allow(clippy::collapsible_match)]
                    MouseEventKind::Up(MouseButton::Left) => {
                        if !thread_frame.bar_release() {
                            thread_frame.select_finish();
                        }
                    }
                    _ => {}
                },
                // A whole clipboard at once. It reaches the editor and stops
                // there: no `Action::Submit` can come out of a paste, which is
                // the entire point of turning the mode on. The menu is synced
                // afterwards for the same reason typing syncs it — a paste
                // beginning `/` is a typed `/` as far as the menu is concerned.
                Ok(Event::Paste(text)) => {
                    let mut ed = thread_editor.lock().unwrap_or_else(|e| e.into_inner());
                    let mut m = thread_menu.lock().unwrap_or_else(|e| e.into_inner());
                    on_paste(&mut ed, &mut m, thread_frame.as_ref(), &text);
                }
                // A resize is a redraw and nothing else: ratatui re-measures on
                // every draw, so there is no state here to update.
                Ok(Event::Resize(..)) => thread_frame.draw(),
                Ok(_) => {}
            }
        });
        Self {
            rx,
            editing: Some(editing),
            menu: Some(menu),
            frame: Some(frame),
        }
    }

    /// Discard anything typed before now, and report how much was dropped.
    ///
    /// Called immediately before a prompt is shown, so the only line that can
    /// answer a question is one typed after seeing it. The count is returned
    /// rather than swallowed because silently eating a line a person typed is
    /// its own small betrayal — the caller says so.
    ///
    /// On the raw path this also clears the half-typed line, which the cooked
    /// path never could. A partly-typed `y` is one keystroke from being an
    /// answer to a question it was not aimed at.
    pub fn drain(&mut self) -> usize {
        let mut dropped = 0;
        while self.rx.try_recv().is_ok() {
            dropped += 1;
        }
        if let Some(editing) = &self.editing {
            let mut ed = editing.lock().unwrap_or_else(|e| e.into_inner());
            if !ed.is_empty() {
                dropped += 1;
                ed.clear();
            }
        }
        // The menu is a third place a keystroke can sit: an open one has a
        // highlighted command, and Enter would turn it into a line. It closes
        // with the rest, and is not counted — nothing was typed into it that
        // the editor above did not already account for.
        if let Some(menu) = &self.menu {
            menu.lock().unwrap_or_else(|e| e.into_inner()).close();
        }
        if let Some(frame) = &self.frame {
            frame.set_input("", 0);
            frame.set_menu(None);
            frame.draw();
        }
        dropped
    }

    pub async fn next(&mut self) -> Option<String> {
        self.rx.recv().await
    }

    /// A queue fed from a list rather than from stdin, for tests that need to
    /// assert on the drain rather than on a terminal.
    ///
    /// `pub(crate)` because `approval.rs` needs it too: the goal prompt reads
    /// through `Approvals::read_line`, and what that says about the lines it
    /// drops is the difference between a command that visibly did not run and
    /// one that silently vanished.
    #[cfg(test)]
    pub(crate) fn scripted(lines: &[&str]) -> Self {
        let (tx, rx) = mpsc::channel(16);
        for line in lines {
            tx.try_send((*line).to_string())
                .expect("test queue is big enough");
        }
        Self {
            rx,
            editing: None,
            menu: None,
            frame: None,
        }
    }

    /// A queue whose lines arrive **after** the drain, the way a person does.
    ///
    /// **Without this, the approval gate's keystroke parser cannot be tested at
    /// all, and on 2026-08-23 that was measured rather than supposed.** Changing
    /// `ask`'s `"" => Answer::No` arm to `Yes` — so that pressing Return at a
    /// prompt approves the call — left all 85 tests in `approval` and
    /// `permissions` green. So did accepting `r` (write a persistent rule) and
    /// `t` (trust a whole tool) when the prompt had not offered them.
    ///
    /// The reason is mechanical and worth stating, because it looks like a
    /// missing test and is not. `ask` calls [`LineSource::drain`] before it
    /// prints the question — deliberately, so type-ahead cannot answer a prompt
    /// the user has not read. [`LineSource::scripted`] pre-fills the channel and
    /// drops the sender, so every scripted line is type-ahead by construction:
    /// the drain eats all of them and `next()` then finds a closed channel. A
    /// gate tested only that way has tested its policy and never its interface,
    /// and the interface is where a person's consent is actually read.
    ///
    /// So this one keeps the sender and hands it back. The test drains first,
    /// then sends — which is exactly the order a human at a terminal produces,
    /// and the only order under which the parser is reachable.
    #[cfg(test)]
    pub(crate) fn answering() -> (Self, mpsc::Sender<String>) {
        let (tx, rx) = mpsc::channel(16);
        (
            Self {
                rx,
                editing: None,
                menu: None,
                frame: None,
            },
            tx,
        )
    }

    /// The same, with a half-typed line and a menu behind it — the raw path's
    /// shape, without a terminal.
    #[cfg(test)]
    fn scripted_raw(lines: &[&str], typing: &str) -> Self {
        let mut source = Self::scripted(lines);
        let mut editor = Editor::default();
        for c in typing.chars() {
            editor.key(KeyEvent::from(KeyCode::Char(c)));
        }
        let mut menu = Menu::for_project(&["review"]);
        menu.sync(&editor.text(), false);
        source.editing = Some(Arc::new(std::sync::Mutex::new(editor)));
        source.menu = Some(Arc::new(std::sync::Mutex::new(menu)));
        source
    }
}

// endregion: The reader

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn ctrl_press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// The text to the right of the cursor. `cursor()` counts characters and
    /// `text()` indexes bytes, so this walks rather than slices — the two
    /// coincide only while the fixture stays ASCII.
    fn after(ed: &Editor) -> String {
        ed.text().chars().skip(ed.cursor()).collect()
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            editor.key(press(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_and_submitting_a_line_gives_back_exactly_what_was_typed() {
        let mut ed = Editor::default();
        typed(&mut ed, "port the middleware");
        assert_eq!(ed.text(), "port the middleware");
        assert_eq!(
            ed.key(press(KeyCode::Enter)),
            Action::Submit("port the middleware".into())
        );
        // …and the box is empty afterwards, rather than holding the line that
        // was just sent.
        assert!(ed.is_empty());
    }

    /// `Ctrl` with an arrow or a delete moves and deletes by **word**.
    ///
    /// **All four were missing and none of them failed.** No decoder compares
    /// the whole modifier set — only the bits its own arm names — so
    /// `Ctrl-Left` fell past every `ctrl` arm and landed on the plain `Left`
    /// below it, moving one column. The box did the narrow thing quietly, on
    /// the chord every other editor uses for the wide one. Found on
    /// 2026-08-27 by the 4,270-keystroke sweep in [`super::super::bindings`],
    /// not by reading: each arm is right in isolation, and the defect was in
    /// what fell past them.
    ///
    /// The fixture has two spaces between words, because the rule is
    /// `Ctrl-W`'s — skip the whitespace you are in, *then* the run of
    /// non-whitespace — and a single space cannot tell that apart from "skip
    /// one character then a word".
    #[test]
    fn ctrl_with_an_arrow_or_a_delete_works_a_word_at_a_time() {
        let text = "alpha  beta gamma";

        // Left, from the end: onto the start of the last word, then the one
        // before it. Not one column, which is what this used to do.
        let mut ed = Editor::default();
        for c in text.chars() {
            ed.key(press(KeyCode::Char(c)));
        }
        ed.key(ctrl_press(KeyCode::Left));
        assert_eq!(after(&ed), *"gamma", "one word back");
        ed.key(ctrl_press(KeyCode::Left));
        assert_eq!(after(&ed), *"beta gamma", "two words back");

        // Right is the mirror, and lands in the same place coming back — the
        // reason `word_end` walks whitespace-then-word in the same order.
        ed.key(ctrl_press(KeyCode::Right));
        assert_eq!(after(&ed), *" gamma", "one word forward");

        // Backspace takes the word behind, leaving the separator it walked.
        let mut ed = Editor::default();
        for c in text.chars() {
            ed.key(press(KeyCode::Char(c)));
        }
        ed.key(ctrl_press(KeyCode::Backspace));
        assert_eq!(
            ed.text(),
            "alpha  beta ",
            "the last word, not the last char"
        );

        // Delete takes the word ahead. Cursor at the very start.
        let mut ed = Editor::default();
        for c in text.chars() {
            ed.key(press(KeyCode::Char(c)));
        }
        ed.key(press(KeyCode::Home));
        ed.key(ctrl_press(KeyCode::Delete));
        assert_eq!(
            ed.text(),
            "  beta gamma",
            "the first word, not the first char"
        );

        // The control that stops all of the above passing on a plain arrow:
        // without `ctrl` the same keys still move and delete by one.
        let mut ed = Editor::default();
        for c in text.chars() {
            ed.key(press(KeyCode::Char(c)));
        }
        ed.key(press(KeyCode::Left));
        assert_eq!(after(&ed), *"a", "plain Left still moves one");
        ed.key(press(KeyCode::Backspace));
        // The cursor sits before the final  after the Left above, so this
        // takes the second  rather than the last character of the line.
        assert_eq!(
            ed.text(),
            "alpha  beta gama",
            "plain Backspace still takes one"
        );
    }

    /// An empty box swallows the word chords rather than letting them fall
    /// through to the plain arms.
    ///
    /// The guard this pins is one line and reads like clutter: the two `ctrl`
    /// deletes claim their chord whether or not there is anything to remove.
    /// Adding the obvious emptiness check sends the keystroke to the plain arm
    /// below — the fall-through this whole change removes — and only on an
    /// empty box, which is the state nobody tries by hand.
    #[test]
    fn the_word_chords_are_claimed_even_with_nothing_to_delete() {
        for code in [KeyCode::Backspace, KeyCode::Delete] {
            let mut ed = Editor::default();
            assert_eq!(
                ed.key(ctrl_press(code)),
                Action::Edit,
                "{code:?} with ctrl was not claimed on an empty box"
            );
            assert_eq!(ed.text(), "", "and it invented text out of nothing");
        }
    }

    #[test]
    fn the_editing_keys_a_person_will_reach_for_all_work() {
        let mut ed = Editor::default();
        typed(&mut ed, "cargo tesr");
        ed.key(press(KeyCode::Backspace));
        typed(&mut ed, "t");
        assert_eq!(ed.text(), "cargo test");

        ed.key(press(KeyCode::Home));
        assert_eq!(ed.cursor(), 0);
        typed(&mut ed, "$ ");
        assert_eq!(ed.text(), "$ cargo test");

        ed.key(press(KeyCode::End));
        ed.key(ctrl('w'));
        assert_eq!(ed.text(), "$ cargo ");
        ed.key(ctrl('u'));
        assert!(ed.is_empty());
    }

    /// A shortcut Emma does not implement must do nothing, rather than typing
    /// its letter into the middle of a goal.
    #[test]
    fn an_unimplemented_control_key_types_nothing() {
        let mut ed = Editor::default();
        typed(&mut ed, "abc");
        assert_eq!(ed.key(ctrl('r')), Action::Ignore);
        assert_eq!(ed.text(), "abc");
    }

    /// **An Alt chord types nothing, and `tool_key` is not what stops it.**
    ///
    /// Found by mutation, not by reading: deleting
    /// `|| key.modifiers.contains(KeyModifiers::ALT)` from the guard in
    /// [`Editor::key`] left the whole `emma` lib suite green — 527 passed, 0
    /// failed. Every existing assertion about Alt is about [`tool_key`], which
    /// is a *different function on a different branch of the reader*, and the
    /// editor is what runs when that branch declines.
    ///
    /// The branch declines more often than it looks. `tool_key` returns `None`
    /// while a question is pending (§4.6), and it returns `None` for a chord
    /// the catalogue does not bind. In both cases the keystroke falls through
    /// to here — so without the guard, `Alt+s` at an approval prompt puts an
    /// `s` in the answer box, and the user's next Return sends it as their
    /// reply.
    ///
    /// **What this establishes and what it does not.** It establishes that the
    /// pure editor refuses the chord. It does not establish that a real
    /// terminal delivers `Alt+s` as `KeyModifiers::ALT` rather than as
    /// `ESC` followed by `s` — some terminals send the latter, crossterm
    /// normalises what it can, and nothing in a cell buffer can tell the
    /// difference. That is a certification item, on a real terminal.
    #[test]
    fn an_alt_chord_types_nothing_even_when_no_tool_takes_it() {
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        let mut ed = Editor::default();
        typed(&mut ed, "y");
        for c in ['s', 'c', 'z', ','] {
            assert_eq!(ed.key(alt(c)), Action::Ignore, "Alt+{c:?} was not ignored");
        }
        assert_eq!(
            ed.text(),
            "y",
            "an Alt chord typed its letter into the line"
        );

        // The reachable route, stated as itself: under a prompt `tool_key`
        // hands the chord back, and the editor is the next thing to see it.
        let key = alt('s');
        assert_eq!(tool_key(key, true), None, "the fixture proves nothing");
        assert_eq!(ed.key(key), Action::Ignore);
        assert_eq!(ed.text(), "y");

        // The control: the same letter with no modifier is an ordinary
        // character, so this is not passing by refusing everything.
        assert_eq!(ed.key(press(KeyCode::Char('s'))), Action::Edit);
        assert_eq!(ed.text(), "ys");
    }

    /// Ctrl-C in raw mode is a keystroke, not a signal — this file is the only
    /// thing that can deliver it. It also takes the half-typed line with it.
    #[test]
    fn ctrl_c_asks_for_an_interrupt_and_clears_what_was_being_typed() {
        let mut ed = Editor::default();
        typed(&mut ed, "half a goal");
        assert_eq!(ed.key(ctrl('c')), Action::Interrupt);
        assert!(ed.is_empty());
    }

    #[test]
    fn ctrl_d_ends_input_only_when_there_is_nothing_to_lose() {
        let mut ed = Editor::default();
        typed(&mut ed, "x");
        assert_eq!(ed.key(ctrl('d')), Action::Ignore);
        ed.key(ctrl('u'));
        assert_eq!(ed.key(ctrl('d')), Action::Eof);
    }

    #[test]
    fn a_key_release_is_not_a_second_keypress() {
        // Windows reports both edges. Acting on both types everything twice.
        let mut ed = Editor::default();
        let mut release = press(KeyCode::Char('x'));
        release.kind = KeyEventKind::Release;
        assert_eq!(ed.key(release), Action::Ignore);
        assert!(ed.is_empty());
    }

    // -----------------------------------------------------------------------
    // Paste
    //
    // The failure being prevented is specific: without bracketed paste the
    // terminal sends a pasted block as keystrokes, its first newline reads as
    // Enter, and half a code block is submitted as a goal. The mode is enabled
    // in `frame.rs`; the guarantee is here.
    // -----------------------------------------------------------------------

    /// A recording [`Surface`], so the reader thread's effects can be asserted.
    ///
    /// The whole point of `ARCH-003`'s pilot: there was no way to observe what
    /// the paste arm *did*, only what `paste` decided, so the line carrying the
    /// decision to the screen could be deleted with the suite green.
    #[derive(Default)]
    struct Recorder {
        notes: std::sync::Mutex<Vec<String>>,
        input: std::sync::Mutex<Vec<(String, usize)>>,
        draws: std::sync::atomic::AtomicUsize,
        pending: bool,
    }

    impl Surface for Recorder {
        fn note_line(&self, text: &str) {
            self.notes.lock().unwrap().push(text.to_string());
        }
        fn set_input(&self, text: &str, cursor: usize) {
            self.input.lock().unwrap().push((text.to_string(), cursor));
        }
        fn prompt_pending(&self) -> bool {
            self.pending
        }
        fn set_menu(&self, _menu: Option<super::super::menu::MenuView>) {}
        fn draw(&self) {
            self.draws.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The note reaches the screen, and a clean paste puts nothing there.
    ///
    /// **This is the assertion that could not be written before.** A reviewer
    /// neutered `if let Some(note) = ed.paste(&text).note()` inside the reader
    /// thread and the entire workspace stayed green — `DEF-028`'s decision was
    /// covered and its *effect* was not, which is `ARCH-003`'s shape and the
    /// same one `DEF-017` and `UI-003` have.
    ///
    /// The clean case is here for the reason it is everywhere in this area: a
    /// note on every paste is noise, and noise is what gets ignored.
    #[test]
    fn a_paste_puts_its_note_on_the_surface_and_a_clean_one_puts_nothing() {
        let surface = Recorder::default();
        let mut ed = Editor::default();
        let mut menu = super::super::menu::Menu::default();

        on_paste(&mut ed, &mut menu, &surface, "one line, nothing lost");
        assert!(
            surface.notes.lock().unwrap().is_empty(),
            "a clean paste put a note on the screen: {:?}",
            surface.notes.lock().unwrap()
        );
        assert_eq!(
            surface.input.lock().unwrap().last().unwrap().0,
            "one line, nothing lost",
            "the paste never reached the input box"
        );

        let surface = Recorder::default();
        let mut ed = Editor::default();
        on_paste(&mut ed, &mut menu, &surface, "first\nsecond\tthird");
        let notes = surface.notes.lock().unwrap().clone();
        assert_eq!(
            notes.len(),
            1,
            "a paste that lost structure said {} thing(s) on screen",
            notes.len()
        );
        assert!(notes[0].contains("line break"), "{}", notes[0]);
        assert!(notes[0].contains("tab"), "{}", notes[0]);

        // And the arm still does its other work, so a future edit cannot make
        // this pass by doing nothing at all.
        assert_eq!(
            surface.input.lock().unwrap().last().unwrap().0,
            "first second third"
        );
        assert!(surface.draws.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }

    /// A paste that changed says so, and one that did not stays quiet.
    ///
    /// **`DEF-028`, the half that is fixable without the multi-line editor.**
    /// Flattening a stack trace to one line is defensible; doing it silently is
    /// not, because the next thing that happens is a model call billed against
    /// text the clipboard never held. The row's other half -- keeping the
    /// breaks -- needs stage 0(a) of the full-screen design and is
    /// still open.
    ///
    /// **The clean case is the load-bearing half of this test.** A note on
    /// every paste is noise, and noise is what gets ignored, which would leave
    /// the real notes unread. Without this assertion an implementation that
    /// always warned would pass.
    #[test]
    fn a_paste_that_lost_something_says_what_and_a_clean_one_says_nothing() {
        // Clean: no breaks, no tabs, no control bytes.
        let mut ed = Editor::default();
        let loss = ed.paste("an ordinary sentence from the clipboard");
        assert!(loss.is_clean(), "{loss:?}");
        assert!(
            loss.note().is_none(),
            "a paste that lost nothing still warned: {:?}",
            loss.note()
        );
        assert_eq!(ed.text(), "an ordinary sentence from the clipboard");

        // Structure lost: two runs of breaks, one tab, one escape sequence.
        let mut ed = Editor::default();
        let loss = ed.paste("first\r\n\r\nsecond\tthird\x1b[31m");
        assert_eq!(loss.breaks_joined, 1, "a run counts once: {loss:?}");
        assert_eq!(loss.tabs_flattened, 1, "{loss:?}");
        // `\x1b` and the rest of the sequence: ESC is the control byte, the
        // `[31m` are printable and survive as text.
        assert_eq!(loss.controls_dropped, 1, "{loss:?}");

        let note = loss
            .note()
            .expect("a paste that lost structure said nothing");
        assert!(note.contains("1 line break"), "{note}");
        assert!(note.contains("1 tab"), "{note}");
        // Not "became one space": N tabs become N spaces, and a reviewer counted
        // three against a note that said one.
        assert!(
            !note.contains("became one space"),
            "the note claims N tabs collapse to a single space: {note}"
        );
        assert!(note.contains("1 control byte"), "{note}");
        // The consequence, not just the counts. A number on its own reads as
        // trivia; what the user needs to know is that the send will not match
        // the copy.
        assert!(
            note.contains("not") && note.contains("copied"),
            "the note gave counts and never said what they cost: {note}"
        );

        // Two separated runs are two, so the count tracks the text rather than
        // being a boolean wearing a number.
        let mut ed = Editor::default();
        let loss = ed.paste("a\nb\nc");
        assert_eq!(loss.breaks_joined, 2, "{loss:?}");
    }

    /// **The sink risk from the evaluation, as an assertion.** A fifty-line
    /// paste produces no `Submit`, whatever is in it.
    #[test]
    fn pasting_a_block_full_of_newlines_never_submits_it() {
        let mut ed = Editor::default();
        let block: String = (0..50)
            .map(|i| format!("    line {i} of a code block\n"))
            .collect();
        // A paste cannot submit -- that is what bracketed paste buys -- so
        // there is no `Action` to assert on any more. The loss is the return
        // value now, and 50 lines is 50 joined breaks.
        let loss = ed.paste(&block);
        assert_eq!(loss.breaks_joined, 50, "{loss:?}");
        assert!(!ed.is_empty());
        assert!(
            !ed.text().contains('\n'),
            "a newline survived into the line"
        );
        // The text is all still there, minus the structure this editor cannot
        // hold yet.
        assert!(ed.text().contains("line 0 of a code block"));
        assert!(ed.text().contains("line 49 of a code block"));
        // And it takes an actual Enter to send it.
        assert!(matches!(ed.key(press(KeyCode::Enter)), Action::Submit(_)));
    }

    /// Windows clipboards carry `\r\n`, and a run of breaks is one gap rather
    /// than one space per byte.
    #[test]
    fn line_breaks_collapse_to_single_spaces_however_they_are_written() {
        let mut ed = Editor::default();
        ed.paste("first\r\n\r\n\r\nsecond\rthird\nfourth");
        assert_eq!(ed.text(), "first second third fourth");
    }

    /// **The two breaks nothing else in this file mentions**: U+2028 LINE
    /// SEPARATOR and U+2029 PARAGRAPH SEPARATOR.
    ///
    /// Found by mutation: dropping them from [`Editor::paste`]'s `matches!`
    /// left the whole lib suite green. Every existing paste test writes `\n`
    /// and `\r`, which is the shape of a clipboard that came from a file — and
    /// these two arrive from the ones that came from a browser, a PDF viewer,
    /// or a JavaScript string, which is most of what a person pastes into a
    /// terminal.
    ///
    /// **They do not fall through to the control-byte arm**, which is why the
    /// omission would be silent rather than merely wrong: `char::is_control`
    /// is `false` for both (they are `Zl`/`Zp`, not `Cc`), so an unhandled
    /// U+2028 is *inserted into the line verbatim* and reported as a paste
    /// that lost nothing. The note the user reads would say the paste arrived
    /// intact while the line held a separator the terminal draws as it
    /// pleases.
    ///
    /// A cell buffer cannot show what a terminal draws for U+2028. What is
    /// asserted is the string that leaves the editor, which is the thing that
    /// goes to the model and to the screen.
    #[test]
    fn a_unicode_line_separator_is_a_line_break_like_any_other() {
        let mut ed = Editor::default();
        let loss = ed.paste("first\u{2028}second\u{2029}third");
        assert_eq!(ed.text(), "first second third", "{:?}", ed.text());
        assert_eq!(
            loss.breaks_joined, 2,
            "a Unicode line separator was not counted as a break: {loss:?}"
        );
        assert!(
            !ed.text().contains('\u{2028}') && !ed.text().contains('\u{2029}'),
            "a separator survived into the line: {:?}",
            ed.text()
        );
        // And it joins a run with the ASCII breaks rather than being counted
        // beside them: `\r\n` then U+2028 is one gap, not three.
        let mut ed = Editor::default();
        let loss = ed.paste("a\r\n\u{2028}b");
        assert_eq!(ed.text(), "a b");
        assert_eq!(loss.breaks_joined, 1, "{loss:?}");

        // **The control.** An ordinary space is not a break, so a paste of
        // plain spaced words reports nothing — without this, an implementation
        // that counted every whitespace character would pass.
        let mut ed = Editor::default();
        let loss = ed.paste("a b c");
        assert_eq!(loss.breaks_joined, 0, "{loss:?}");
        assert!(loss.is_clean(), "{loss:?}");
        assert!(loss.note().is_none());
    }

    /// **A clipboard is untrusted bytes.** Its contents are rendered into cells
    /// and, on the fallback path, written to a stream — so an escape sequence
    /// in a paste is an escape sequence Emma did not author reaching the
    /// terminal through a text field.
    #[test]
    fn control_bytes_in_a_paste_never_reach_the_line() {
        let mut ed = Editor::default();
        ed.paste("safe\x1b[31mred\x07\x00 text\ttabbed");
        let text = ed.text();
        assert!(!text.contains('\x1b'), "{text:?}");
        assert!(!text.contains('\x07'), "{text:?}");
        assert!(!text.contains('\0'), "{text:?}");
        assert!(!text.contains('\t'), "{text:?}");
        assert!(!text.chars().any(char::is_control), "{text:?}");
        // What was printable survived, including the `[31m` that was only ever
        // dangerous because of the escape in front of it.
        assert_eq!(text, "safe[31mred text tabbed");
    }

    /// A paste lands where the cursor is, and leaves it after what arrived.
    #[test]
    fn a_paste_goes_in_at_the_cursor_and_the_cursor_follows_it() {
        let mut ed = Editor::default();
        typed(&mut ed, "review  now");
        for _ in 0..4 {
            ed.key(press(KeyCode::Left));
        }
        ed.paste("the diff");
        assert_eq!(ed.text(), "review the diff now");
        // Typing continues where the paste ended, not where it started.
        typed(&mut ed, "!");
        assert_eq!(ed.text(), "review the diff! now");
    }

    /// The half-typed line is drained before a question is asked, and a pasted
    /// line is a half-typed line — it must not be a second hiding place.
    #[tokio::test]
    async fn a_pasted_line_is_drained_like_a_typed_one() {
        let mut lines = LineSource::scripted_raw(&[], "");
        lines
            .editing
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .paste("y\nrm -rf /");
        assert_eq!(lines.drain(), 1, "a pasted answer survived the drain");
        assert!(lines.editing.as_ref().unwrap().lock().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // The drain
    //
    // The security property, and the one thing in this file that is not
    // allowed to change. The first two tests are the ones that existed before
    // the frame; the third is the ground the raw path gained.
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

    /// What raw mode added: the half-typed line is drained too.
    ///
    /// Under cooked mode this buffer lived in the terminal's line discipline
    /// and nothing in this process could reach it, so a user who had typed `y`
    /// and not yet pressed return could answer the *next* question with one
    /// keystroke. That hole is closed here, and the test is written from the
    /// hole rather than from the implementation.
    #[tokio::test]
    async fn a_half_typed_answer_is_drained_as_well_as_a_submitted_one() {
        let mut lines = LineSource::scripted_raw(&["y"], "y");
        assert_eq!(lines.drain(), 2, "the half-typed line survived the drain");
        let Some(editing) = &lines.editing else {
            panic!("the raw path lost its editor")
        };
        assert!(
            editing.lock().unwrap().is_empty(),
            "the input box still held an answer to an unasked question"
        );
    }

    /// The menu is the newest place input can hide, so the drain reaches it
    /// too.
    ///
    /// An open menu holds a highlighted command, and one Enter turns that into
    /// a submitted line. A question asked while a menu was up would otherwise
    /// have a `/exit` sitting one keystroke from answering it.
    #[tokio::test]
    async fn draining_closes_the_command_menu_as_well() {
        let mut lines = LineSource::scripted_raw(&[], "/re");
        let Some(menu) = &lines.menu else {
            panic!("the raw path lost its menu")
        };
        assert!(
            menu.lock().unwrap().is_open(),
            "the fixture did not open a menu, so this proves nothing"
        );
        // The half-typed `/re` counts as the one thing dropped; the menu is not
        // a second line, it is a view of that one.
        assert_eq!(lines.drain(), 1);
        let menu = lines.menu.as_ref().unwrap().lock().unwrap();
        assert!(!menu.is_open(), "a menu survived the drain");
        assert_eq!(menu.selection(), None);
    }

    // -----------------------------------------------------------------------
    // The menu's keys
    // -----------------------------------------------------------------------

    #[test]
    fn the_menu_takes_four_keys_and_only_while_it_is_open() {
        for (code, expected) in [
            (KeyCode::Up, Some(MenuKey::Up)),
            (KeyCode::Down, Some(MenuKey::Down)),
            (KeyCode::Enter, Some(MenuKey::Accept)),
            (KeyCode::Tab, Some(MenuKey::Accept)),
            (KeyCode::Esc, Some(MenuKey::Dismiss)),
            (KeyCode::Char('x'), None),
            (KeyCode::Backspace, None),
            (KeyCode::Left, None),
        ] {
            assert_eq!(menu_key(press(code), true), expected, "{code:?}");
            // Closed, the editor keeps every one of them — including Enter,
            // which must still submit, and Esc, which must still do nothing.
            assert_eq!(menu_key(press(code), false), None, "{code:?}");
        }
    }

    // -----------------------------------------------------------------------
    // The transcript's keys
    // -----------------------------------------------------------------------

    /// The routing table for the pane keys, stated whole. The two rules with
    /// teeth: Home/End go to the transcript only when the editor has nothing
    /// for them to do, and Ctrl-B dies while a question is pending.
    #[test]
    fn the_pane_keys_route_to_the_transcript_and_nothing_else_does() {
        for (code, expected) in [
            (KeyCode::PageUp, Some(PaneKey::PageUp)),
            (KeyCode::PageDown, Some(PaneKey::PageDown)),
            (KeyCode::Char('x'), None),
            (KeyCode::Up, None),
            (KeyCode::Down, None),
            (KeyCode::Enter, None),
        ] {
            assert_eq!(pane_key(press(code), true, false), expected, "{code:?}");
        }
        // Ctrl turns the arrows into row scrolling; plain arrows stay with
        // history and the menu.
        assert_eq!(
            pane_key(
                KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL),
                true,
                false
            ),
            Some(PaneKey::RowUp)
        );
        assert_eq!(
            pane_key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL),
                false,
                true
            ),
            Some(PaneKey::RowDown),
            "scrolling must work mid-prompt: the evidence is what is being read"
        );
    }

    /// Home and End act on the thing that can act: the editor while a line is
    /// under the cursor, the transcript when the box is empty.
    #[test]
    fn home_and_end_go_to_the_transcript_only_when_the_editor_is_empty() {
        assert_eq!(
            pane_key(press(KeyCode::Home), true, false),
            Some(PaneKey::Top)
        );
        assert_eq!(
            pane_key(press(KeyCode::End), true, false),
            Some(PaneKey::Tail)
        );
        assert_eq!(pane_key(press(KeyCode::Home), false, false), None);
        assert_eq!(pane_key(press(KeyCode::End), false, false), None);
    }

    /// §4.6: sidebar keys are dead while a question is pending, so nothing
    /// aimed at a pane can read as part of an answer. Scrolling is the stated
    /// exception — it cannot answer anything and the evidence is above.
    #[test]
    fn the_sidebar_toggle_is_dead_while_a_question_is_pending() {
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_eq!(pane_key(ctrl_b, true, false), Some(PaneKey::Sidebar));
        assert_eq!(pane_key(ctrl_b, true, true), None);
        assert_eq!(
            pane_key(press(KeyCode::PageUp), true, true),
            Some(PaneKey::PageUp)
        );
    }

    // -----------------------------------------------------------------------
    // The tool chords
    // -----------------------------------------------------------------------

    /// **A bare letter is never a tool shortcut.** The design's §6 refused the
    /// mockup's bare `s`/`c`/`f` bindings — "a bare letter that acts while the
    /// input box exists is a footgun" — and the first character of every goal
    /// lands on an empty editor, so even an empty-editor gate would turn
    /// "ship the fix" into a shell launch plus "hip the fix". If this test
    /// goes red, that refuted idea is back.
    #[test]
    fn a_bare_letter_is_never_a_tool_shortcut() {
        for c in ['s', 'c', 'f', 'm', 'd', ',', '/'] {
            assert_eq!(
                tool_key(press(KeyCode::Char(c)), false),
                None,
                "bare {c:?} acted as a shortcut"
            );
        }
    }

    /// The chords that do launch: Alt+letter, dead while a question pends,
    /// dead on the release edge, and dead under Ctrl+Alt — which is AltGr on
    /// Windows layouts, where it produces real characters.
    #[test]
    fn alt_chords_launch_tools_and_die_under_a_prompt() {
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        assert_eq!(tool_key(alt('s'), false), Some('s'));
        assert_eq!(tool_key(alt(','), false), Some(','));
        assert_eq!(
            tool_key(alt('s'), true),
            None,
            "a tool chord acted while a question was pending"
        );
        let mut release = alt('s');
        release.kind = KeyEventKind::Release;
        assert_eq!(tool_key(release, false), None);
        assert_eq!(
            tool_key(
                KeyEvent::new(
                    KeyCode::Char('s'),
                    KeyModifiers::ALT | KeyModifiers::CONTROL
                ),
                false
            ),
            None,
            "AltGr (Ctrl+Alt) must type, not launch"
        );
    }

    /// A release edge is not a keystroke here either — the same Windows
    /// double-fire the editor already guards against.
    #[test]
    fn a_key_release_never_scrolls() {
        let mut release = press(KeyCode::PageUp);
        release.kind = KeyEventKind::Release;
        assert_eq!(pane_key(release, true, false), None);
    }

    /// Esc reaches the menu and nothing else: the editor never sees it, so the
    /// line survives.
    #[test]
    fn esc_with_a_menu_open_leaves_the_typed_text_where_it_was() {
        let mut ed = Editor::default();
        typed(&mut ed, "/re");
        let mut menu = Menu::for_project(&["review"]);
        menu.sync(&ed.text(), false);
        assert_eq!(
            menu_key(press(KeyCode::Esc), menu.is_open()),
            Some(MenuKey::Dismiss)
        );
        menu.dismiss(&ed.text());
        assert!(!menu.is_open());
        assert_eq!(ed.text(), "/re", "Esc took the line with the menu");
    }

    /// A page double that records which pages were offered the key, in order,
    /// and answers `true` from one of them.
    struct Pages {
        answers_from: Option<&'static str>,
        offered: std::cell::RefCell<Vec<&'static str>>,
    }

    impl Pages {
        fn answering(page: Option<&'static str>) -> Self {
            Self {
                answers_from: page,
                offered: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn offer(&self, page: &'static str) -> bool {
            self.offered.borrow_mut().push(page);
            self.answers_from == Some(page)
        }
    }

    impl PageKeys for Pages {
        fn help_key(&self, _: KeyEvent) -> bool {
            self.offer("help")
        }
        fn harness_key(&self, _: KeyEvent) -> bool {
            self.offer("harness")
        }
        fn memory_key(&self, _: KeyEvent) -> bool {
            self.offer("memory")
        }
        fn code_key(&self, _: KeyEvent) -> bool {
            self.offer("code")
        }
        fn settings_key(&self, _: KeyEvent) -> bool {
            self.offer("settings")
        }
    }

    /// Every page is offered the key, in the documented order, when none of
    /// them is open. Removing any arm from `run_page_key` fails this.
    #[test]
    fn a_key_is_offered_to_every_page_in_order_when_none_is_open() {
        let pages = Pages::answering(None);
        assert!(!run_page_key(&pages, KeyEvent::from(KeyCode::Char('j'))));
        assert_eq!(
            *pages.offered.borrow(),
            vec!["help", "harness", "memory", "code", "settings"]
        );
    }

    /// The first page to take the key ends the offer: the pages below it never
    /// see the key, and the reader is told it was handled.
    #[test]
    fn the_page_that_takes_the_key_stops_the_offer() {
        let pages = Pages::answering(Some("memory"));
        assert!(run_page_key(&pages, KeyEvent::from(KeyCode::Char('j'))));
        assert_eq!(*pages.offered.borrow(), vec!["help", "harness", "memory"]);
    }

    /// The reader thread calls the dispatcher. This is the half a double cannot
    /// prove: `Frame` needs a terminal to exist, so the call site is checked in
    /// the source. Delete the call and this fails, which is the whole defect
    /// this seam was written to make impossible to reintroduce silently.
    #[test]
    fn the_raw_reader_offers_keys_to_the_pages() {
        let src = include_str!("input.rs");
        let raw = src.find("pub fn raw(").expect("the raw reader");
        let call = src[raw..]
            .find("if run_page_key(&thread_frame, key)")
            .expect("the raw reader does not offer keys to the pages");
        let pane = src[raw..]
            .find("pane_key(key, ed.is_empty()")
            .expect("the pane layer");
        assert!(
            call < pane,
            "the pages must be offered the key before the pane layer"
        );
    }
}
