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
    /// `notes/design/tui-fullscreen.md` and is not built yet; until it is, the
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
    pub fn paste(&mut self, text: &str) -> Action {
        let mut last_was_break = false;
        for c in text.chars() {
            if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                if !last_was_break {
                    self.chars.insert(self.cursor, ' ');
                    self.cursor += 1;
                }
                last_was_break = true;
                continue;
            }
            last_was_break = false;
            let c = if c == '\t' { ' ' } else { c };
            if c.is_control() {
                continue;
            }
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
        Action::Edit
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
                while self.cursor > 0 && self.chars[self.cursor - 1].is_whitespace() {
                    self.chars.remove(self.cursor - 1);
                    self.cursor -= 1;
                }
                while self.cursor > 0 && !self.chars[self.cursor - 1].is_whitespace() {
                    self.chars.remove(self.cursor - 1);
                    self.cursor -= 1;
                }
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
                    MouseEventKind::ScrollUp => thread_frame.scroll_rows(true, WHEEL_ROWS),
                    MouseEventKind::ScrollDown => thread_frame.scroll_rows(false, WHEEL_ROWS),
                    MouseEventKind::Down(MouseButton::Left)
                        if !mouse.modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        thread_frame.click(mouse.column, mouse.row);
                    }
                    _ => {}
                },
                // A whole clipboard at once. It reaches the editor and stops
                // there: no `Action::Submit` can come out of a paste, which is
                // the entire point of turning the mode on. The menu is synced
                // afterwards for the same reason typing syncs it — a paste
                // beginning `/` is a typed `/` as far as the menu is concerned.
                Ok(Event::Paste(text)) => {
                    {
                        let mut ed = thread_editor.lock().unwrap_or_else(|e| e.into_inner());
                        ed.paste(&text);
                        thread_frame.set_input(&ed.text(), ed.cursor());
                        let mut m = thread_menu.lock().unwrap_or_else(|e| e.into_inner());
                        m.sync(&ed.text(), thread_frame.prompt_pending());
                        thread_frame.set_menu(m.view());
                    }
                    thread_frame.draw();
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

    /// **The sink risk from the evaluation, as an assertion.** A fifty-line
    /// paste produces no `Submit`, whatever is in it.
    #[test]
    fn pasting_a_block_full_of_newlines_never_submits_it() {
        let mut ed = Editor::default();
        let block: String = (0..50)
            .map(|i| format!("    line {i} of a code block\n"))
            .collect();
        assert_eq!(ed.paste(&block), Action::Edit);
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
}
